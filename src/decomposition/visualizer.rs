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

//! This module visualizes a single schedule using graphviz. This module will do nothing when used
//! in test configuration.

use bgpsim::prelude::*;
use std::{collections::HashMap, fmt::Display};

use super::CommandInfo;
use super::{bgp_dependencies::BgpDependencies, ilp_scheduler::NodeSchedule};
use crate::P;

#[cfg(not(test))]
use super::bgp_dependencies::BgpDependency;

#[cfg(not(test))]
use std::{
    cmp::Ordering,
    collections::HashSet,
    fs::{remove_file, OpenOptions},
    io::Write,
    iter::repeat_with,
    path::PathBuf,
    process::Command,
};

#[cfg(not(test))]
use itertools::Itertools;

/// Create a PDF that visualizes the schedule using graphviz DOT. The `filename_base` should not
/// contain any file type, as the prefix will be appended automatically.
pub fn visualize<F, S, Q>(
    info: &CommandInfo<'_, Q>,
    schedules: &HashMap<P, HashMap<RouterId, NodeSchedule>>,
    bgp_deps: &HashMap<P, BgpDependencies>,
    prefix: P,
    name: F,
    filename_base: impl Into<String>,
) where
    F: Fn(RouterId) -> S,
    S: Display,
{
    // skip if the schedule is empty
    if schedules.get(&prefix).map(|x| x.is_empty()).unwrap_or(true) {
        return;
    }

    #[cfg(test)]
    {
        let _ = (info, schedules, bgp_deps, prefix, name, filename_base);
    }
    #[cfg(not(test))]
    {
        let mut dot_name: String = filename_base.into();
        let mut pdf_name: String = dot_name.clone();
        dot_name.push_str(&format!("_{}.dot", prefix.as_num()));
        pdf_name.push_str(&format!("_{}.pdf", prefix.as_num()));

        let dot_path = PathBuf::from(dot_name);
        let pdf_path = PathBuf::from(pdf_name);

        // delete existing files
        if dot_path.exists() {
            remove_file(&dot_path).unwrap();
        }
        if pdf_path.exists() {
            remove_file(&pdf_path).unwrap();
        }

        // write the dot file
        let mut dot_file = OpenOptions::new()
            .create(true)
            .write(true)
            .open(&dot_path)
            .unwrap();

        // create the schedule
        let schedule = schedules.get(&prefix).unwrap();
        let mut schedule_vec: Vec<Vec<RouterId>> = repeat_with(Vec::new)
            .take(schedule.values().map(|x| x.fw_state).max().unwrap() + 1)
            .collect();
        schedule
            .iter()
            .for_each(|(r, x)| schedule_vec[x.fw_state].push(*r));
        write_dot(info, &schedule_vec, bgp_deps, prefix, &mut dot_file, name);

        // call `dot`
        Command::new("dot")
            .arg("-Tpdf")
            .arg("-o")
            .arg(&pdf_path)
            .arg(&dot_path)
            .output()
            .unwrap();

        // delete the temporary dot file
        if dot_path.exists() {
            remove_file(&dot_path).unwrap();
        }
    }
}

/// Visualize the schedule for a given prefix using graphviz. This function will write the `dot`
/// file into the provided `output`.
#[cfg(not(test))]
pub fn write_dot<W: Write, S: Display, F, Q>(
    info: &CommandInfo<'_, Q>,
    schedule: &[Vec<RouterId>],
    bgp_deps: &HashMap<P, BgpDependencies>,
    prefix: P,
    output: &mut W,
    name: F,
) where
    F: Fn(RouterId) -> S,
{
    let bgp_deps = bgp_deps.get(&prefix);
    let fw_deps = get_fw_deps(info, schedule, prefix);

    writeln!(output, "digraph D {{").unwrap();
    writeln!(output, "  splines=true").unwrap();

    for (round, nodes) in schedule.iter().enumerate() {
        for node in nodes {
            writeln!(
                output,
                "  r{} [label=\"{} [{}]\"]",
                node.index(),
                name(*node),
                round
            )
            .unwrap();
        }
    }

    let mut rank = HashMap::new();

    for (i, step) in schedule.iter().enumerate() {
        for r in step {
            assert!(rank.insert(r, i).is_none());
        }
        writeln!(
            output,
            "  {{ rank = same; {} }}",
            step.iter().map(|r| format!("r{}", r.index())).join("; ")
        )
        .unwrap();
    }

    for (r, deps) in fw_deps.iter() {
        for dep in deps {
            writeln!(
                output,
                "  r{} -> r{} [style=bold, linewidth = 3.0]",
                r.index(),
                dep.index()
            )
            .unwrap()
        }
    }

    for (r, BgpDependency { old_from, new_from }) in bgp_deps.iter().flat_map(|x| x.iter()) {
        let rank_r = rank[r];
        if let Some(max_dep) = old_from.iter().max_by_key(|x| rank[x]) {
            let rank_max = rank[max_dep];
            let color = match rank_r.cmp(&rank_max) {
                Ordering::Less => "green",
                Ordering::Equal | Ordering::Greater => "red",
            };
            writeln!(
                output,
                "  r{} -> r{} [constraint = false, color = {}, dir=back]",
                r.index(),
                max_dep.index(),
                color,
            )
            .unwrap()
        }
        if let Some(min_dep) = new_from.iter().min_by_key(|x| rank[x]) {
            let rank_min = rank[min_dep];
            let color = match rank_r.cmp(&rank_min) {
                Ordering::Less | Ordering::Equal => "red",
                Ordering::Greater => "green",
            };
            writeln!(
                output,
                "  r{} -> r{} [constraint = false, color = {}, style=dashed, dir=back]",
                min_dep.index(),
                r.index(),
                color,
            )
            .unwrap()
        }
    }

    writeln!(output, "}}").unwrap();
}

/// Visualize the schedule for a given prefix using graphviz. This function will write the `dot`
/// file into the provided `output`.
#[cfg(not(test))]
pub fn get_fw_deps<Q>(
    info: &CommandInfo<'_, Q>,
    schedule: &[Vec<RouterId>],
    prefix: P,
) -> HashMap<RouterId, HashSet<RouterId>> {
    let mut result: HashMap<RouterId, HashSet<RouterId>> = HashMap::new();

    // build dependencies
    let mut fw_state = info.fw_before.clone();
    let mut round: HashMap<RouterId, usize> = HashMap::new();
    let mut changed: HashSet<RouterId> = HashSet::new();
    let mut unchanged: HashSet<RouterId> = schedule.iter().flatten().copied().collect();

    for (r, step) in schedule.iter().enumerate() {
        for router in step {
            round.insert(*router, r);
            changed.insert(*router);
            unchanged.remove(router);

            if info
                .fw_diff
                .get(&prefix)
                .and_then(|x| x.get(router))
                .is_none()
            {
                continue;
            }

            let old_path = match fw_state.get_paths(*router, prefix) {
                Ok(mut paths) => paths.pop().unwrap_or_default(),
                Err(NetworkError::ForwardingLoop(p))
                | Err(NetworkError::ForwardingBlackHole(p)) => p,
                _ => unreachable!(),
            };
            fw_state.update(
                *router,
                prefix,
                info.fw_after.get_next_hops(*router, prefix).to_vec(),
            );
            let new_path = match fw_state.get_paths(*router, prefix) {
                Ok(mut paths) => paths.pop().unwrap_or_default(),
                Err(NetworkError::ForwardingLoop(p))
                | Err(NetworkError::ForwardingBlackHole(p)) => p,
                _ => unreachable!(),
            };
            let reach: HashSet<RouterId> =
                old_path.into_iter().chain(new_path.into_iter()).collect();

            for dep in reach.intersection(&changed) {
                if info.fw_diff.get(&prefix).and_then(|x| x.get(dep)).is_some() && dep != router {
                    result.entry(*dep).or_default().insert(*router);
                }
            }
            for dep in reach.intersection(&unchanged) {
                if info.fw_diff.get(&prefix).and_then(|x| x.get(dep)).is_some() && dep != router {
                    result.entry(*router).or_default().insert(*dep);
                }
            }
        }
    }

    for r in result.keys().copied().collect::<Vec<_>>() {
        let mut sorted_deps: Vec<RouterId> = result[&r]
            .iter()
            .copied()
            .sorted_by_key(|r| round[r])
            .collect();
        while let Some(dep) = sorted_deps.pop() {
            if sorted_deps
                .iter()
                .any(|x| result.get(x).map(|y| y.contains(&dep)).unwrap_or(false))
            {
                result.get_mut(&r).unwrap().remove(&dep);
            }
        }
    }

    result
}
