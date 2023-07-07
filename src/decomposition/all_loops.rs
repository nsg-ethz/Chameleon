// Chameleon: Taming the transient while reconfiguring BGP
// Copyright (C) 2023 Tibor Schneider <sctibor@ethz.ch>
//
// This program is free software; you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation; either version 2 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License along
// with this program; if not, write to the Free Software Foundation, Inc.,
// 51 Franklin Street, Fifth Floor, Boston, MA 02110-1301 USA.

//! Module to compute all possible loops in the graph.

use std::collections::{BTreeSet, HashMap, HashSet};

use bgpsim::{forwarding_state::ForwardingState, prelude::*};
use itertools::Itertools;
use petgraph::{algo::tarjan_scc, stable_graph::StableGraph};

use crate::{
    decomposition::{CommandInfo, FwDiff},
    P,
};

/// Compute the set of all simple loops in the overlapped forwarding graph. This function was ported
/// from [`networkx::algorithms::cycles::simple_cycles`](https://networkx.org/documentation/stable/reference/algorithms/generated/networkx.algorithms.cycles.simple_cycles.html#simple-cycles).
///
/// Find simple cycles (elementary circuits) of a directed graph. A simple cycle, or elementary
/// circuit, is a closed path where no node appears twice. Two elementary circuits are distinct if
/// they are not cyclic permutations of each other. This is a nonrecursive, iterator/generator
/// version of Johnson’s algorithm [\[1\]](https://doi.org/10.1137/0204007).
pub fn all_loops<Q>(info: &CommandInfo<'_, Q>, prefix: P) -> HashSet<Vec<RouterId>> {
    let delta = if let Some(d) = info.fw_diff.get(&prefix) {
        d
    } else {
        return Default::default();
    };
    let mut result = HashSet::new();

    // subG = type(G)(G.edges())
    let subg = graph(info.net_before, &info.fw_before, delta, prefix);

    // sccs = [scc for scc in nx.strongly_connected_components(subG) if len(scc) > 1]
    let mut sccs: Vec<HashSet<RouterId>> = tarjan_scc(&subg)
        .into_iter()
        .filter(|scc| scc.len() > 1)
        .map(|scc| scc.into_iter().collect())
        .collect();

    // while sccs:
    while let Some(mut scc) = sccs.pop() {
        // scc = sccs.pop()
        // sccG = subG.subgraph(scc)
        let sccg = subgraph(&subg, &scc);
        // startnode = scc.pop()
        let startnode = starting_node(&sccg);
        scc.remove(&startnode);
        // path = [startnode]
        let mut path = vec![startnode];
        // blocked = set()  # vertex: blocked from search?
        let mut blocked = HashSet::new();
        // closed = set()  # nodes involved in a cycle
        let mut closed = HashSet::new();
        // blocked.add(startnode)
        blocked.insert(startnode);
        // B = defaultdict(set)
        let mut no_circuits: HashMap<RouterId, HashSet<RouterId>> = HashMap::new();
        // stack = [(startnode, list(sccG[startnode]))]  # sccG gives comp nbrs
        let mut stack = vec![(startnode, succ(&sccg, startnode))];
        // while stack:
        while !stack.is_empty() {
            // thisnode, nbrs = stack[-1]
            let (node, nhs) = stack.last_mut().unwrap();
            let thisnode = *node;
            // if nbrs:
            if !nhs.is_empty() {
                // nextnode = nbrs.pop()
                let nextnode = nhs.pop().unwrap();
                // if nextnode == startnode:
                if nextnode == startnode {
                    // yield path[:]
                    result.insert(path.clone());
                    // closed.update(path)
                    closed.extend(path.iter().copied());
                // elif nextnode not in blocked:
                } else if !blocked.contains(&nextnode) {
                    // path.append(nextnode)
                    path.push(nextnode);
                    // stack.append((nextnode, list(sccG[nextnode])))
                    stack.push((nextnode, succ(&sccg, nextnode)));
                    // closed.discard(nextnode)
                    closed.remove(&nextnode);
                    // blocked.add(nextnode)
                    blocked.insert(nextnode);
                    // continue
                    continue;
                }
            }
            // if not nbrs:
            if nhs.is_empty() {
                // if thisnode in closed:
                if closed.contains(&thisnode) {
                    // _unblock(thisnode, blocked, B)
                    unblock(thisnode, &mut blocked, &mut no_circuits);
                // else:
                } else {
                    // for nbr in sccG[thisnode]:
                    for nbr in succ(&sccg, thisnode) {
                        let b_nbr = no_circuits.entry(nbr).or_default();
                        // if thisnode not in B[nbr]:
                        if !b_nbr.contains(&thisnode) {
                            // B[nbr].add(thisnode)
                            b_nbr.insert(thisnode);
                        }
                    }
                    no_circuits
                        .entry(thisnode)
                        .or_insert_with(|| HashSet::from_iter(succ(&subg, thisnode).into_iter()));
                }
                // stack.pop()
                stack.pop();
                assert!(path.last() == Some(&thisnode));
                // path.pop()
                path.pop();
            }
        }

        // H = subG.subgraph(scc)
        let h = subgraph(&sccg, &scc);
        // sccs.extend(scc for scc in nx.strongly_connected_components(H) if len(scc) > 1)
        sccs.extend(
            tarjan_scc(&h)
                .into_iter()
                .filter(|scc| scc.len() > 1)
                .map(|scc| scc.into_iter().collect()),
        );
    }

    result
}

/// Unblock function, as described in [`networkx::algorithms::cycles::simple_cycles`](https://networkx.org/documentation/stable/reference/algorithms/generated/networkx.algorithms.cycles.simple_cycles.html#simple-cycles).
fn unblock(
    start: RouterId,
    blocked: &mut HashSet<RouterId>,
    no_circuits: &mut HashMap<RouterId, HashSet<RouterId>>,
) {
    // stack = {thisnode}
    let mut stack = BTreeSet::new();
    stack.insert(start);
    // while stack:
    while !stack.is_empty() {
        // node = stack.pop()
        let node = *stack.iter().next().unwrap();
        stack.remove(&node);
        // if node in blocked:
        if blocked.contains(&node) {
            // blocked.remove(node)
            blocked.remove(&node);
            let b_node = no_circuits.entry(node).or_default();
            // stack.update(B[node])
            stack.extend(b_node.iter().copied());
            // B[node].clear()
            b_node.clear();
        }
    }
}

/// Graph type without any node or edge labels.
type Graph = StableGraph<(), (), petgraph::Directed, u32>;

/// Generate a new graph from a forwarding delta.
pub fn graph<Q>(
    net: &Network<P, Q>,
    fw: &ForwardingState<P>,
    delta: &HashMap<RouterId, FwDiff>,
    prefix: P,
) -> Graph {
    let mut graph = Graph::new();

    (0..=net
        .get_topology()
        .node_indices()
        .map(|x| x.index())
        .max()
        .unwrap_or_default())
        .for_each(|_| {
            graph.add_node(());
        });

    for node in net.get_routers() {
        if let Some(FwDiff { old: a, new: b }) = delta.get(&node) {
            vec![*a, *b]
        } else {
            vec![fw.get_next_hops(node, prefix).first().copied()]
        }
        .into_iter()
        .flatten()
        .for_each(|nh| {
            graph.add_edge(node, nh, ());
        });
    }

    graph
}

/// Extract the subgraph of `g` that only contains nodes in `nodes`.
fn subgraph(g: &Graph, nodes: &HashSet<RouterId>) -> Graph {
    let mut s = g.clone();
    g.node_indices()
        .filter(|r| !nodes.contains(r))
        .for_each(|r| {
            s.remove_node(r);
        });
    s
}

/// Get the starting node, which is the node with the highest degree.
fn starting_node(g: &Graph) -> RouterId {
    g.node_indices()
        .max_by_key(|r| g.neighbors(*r).count())
        .unwrap()
}

/// Get the successors of a node, i.e., the possible next-hops.
fn succ(g: &Graph, router: RouterId) -> Vec<RouterId> {
    g.neighbors(router).collect_vec()
}
