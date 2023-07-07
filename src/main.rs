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

use bgpsim::export::Addressor;
use cisco_lab::{CiscoLab, Inactive};
use clap::{Parser, ValueEnum};
use ipnet::Ipv4Net;
use itertools::Itertools;
use rand::prelude::*;
use serde::Serialize;
use std::{collections::HashMap, net::Ipv4Addr};

use chameleon::{
    decompose,
    experiment::{Experiment, Scenario, _TopologyZoo},
    runtime::{self, lab::ExternalEvent},
    specification::SpecificationBuilder,
    P,
};
use bgpsim::{prelude::*, topology_zoo::TopologyZoo};

/// The topology to test things on.
const TOPO: TopologyZoo = TopologyZoo::Abilene;

/// Run the system in simulation and in the testbed.
#[derive(Debug, Parser)]
struct Cli {
    /// Topology to use. If you choose a topology with more than 11 routers, you cannot run it on
    /// the testbed.
    #[clap(long = "topo", short = 't', default_value = "Abilene")]
    topo: _TopologyZoo,
    /// Specification to generate.
    #[clap(long = "spec", short = 's', default_value = "reachability")]
    spec_builder: SpecificationBuilder,
    /// Event (scenario) to generate.
    #[clap(long = "event", short = 'e', default_value = "del-best-route")]
    event: Scenario,
    /// Unexpected event that disturbs the simulation
    #[clap(long = "failure", short = 'f')]
    failure: Option<UnexpectedEvent>,
    /// Run the experiment in the cisco-lab
    #[clap(long = "lab", short = 'l')]
    cisco_lab: bool,
    /// Specifiy the number of prefixes (Prefix Equivalence Class) to simulate
    #[clap(long = "pecs", short = 'p')]
    pecs: Option<u32>,
    /// Use a randomized configuration
    #[clap(short, long)]
    rand: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    pretty_env_logger::init_timed();

    let args = Cli::parse();

    let (mut net, p, command) = args
        .event
        .build(args.topo.0, BasicEventQueue::new(), args.rand)?;
    let spec = args.spec_builder.build_all(&net, Some(&command), [p]);
    let decomp = decompose(&net, command, &spec)?;

    let failure = args.failure.map(|x| x.build(&mut net, p));

    // perform the simulation
    runtime::sim::run(net.clone(), decomp.clone(), &spec)?;

    if args.cisco_lab && args.topo.0.num_internals() <= 12 {
        let pecs = args.pecs.map(|p| {
            (0..p)
                .map(|x| Ipv4Addr::from((200u32 << 24) + (x << 8)))
                .map(|ip| Ipv4Net::new(ip, 24).unwrap())
                .collect_vec()
        });

        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                // normal run
                let mut lab = runtime::lab::setup_cisco_lab(&net, Some(args.topo.0)).await?;
                let event = failure.as_ref().map(|f| f.build(&mut lab));
                if let Some(pecs) = pecs.clone() {
                    lab.addressor_mut().register_pec(p, pecs);
                }

                // connect to the lab and configure all devices
                let mut lab = lab.connect().await?;
                lab.wait_for_convergence().await?;

                // set the prefix equivalence classes
                let mut path =
                    runtime::lab::run(net.clone(), &mut lab, decomp.clone(), event).await?;

                // store the experiment
                path.push("scenario.json");
                Experiment {
                    net: &net,
                    topo: Some(TOPO),
                    scenario: Some(args.event),
                    spec_builder: Some(args.spec_builder),
                    spec: &spec,
                    decomp: Some(&decomp),
                    rand: args.rand,
                    data: Parameters {
                        failure: failure.clone(),
                        pecs: args.pecs,
                    },
                }
                .write_json(&path)?;
                path.pop();

                // generate the web export
                let web_export_path = format!("{}/web_export", path.to_string_lossy());
                chameleon::export_web(&net, &spec, decomp.clone(), web_export_path).unwrap();

                // drop the lab
                std::mem::drop(lab);

                // baseline run
                let mut lab = runtime::lab::setup_cisco_lab(&net, Some(TOPO)).await?;
                let event = failure.as_ref().map(|f| f.build(&mut lab));
                if let Some(pecs) = pecs {
                    lab.addressor_mut().register_pec(p, pecs);
                }

                // connect to the lab and configure all devices
                let mut lab = lab.connect().await?;
                lab.wait_for_convergence().await?;

                let mut path =
                    runtime::lab::run_baseline(net.clone(), &mut lab, decomp.clone(), event)
                        .await?;

                // generate the scenario.json
                path.push("scenario.json");
                Experiment {
                    net: &net,
                    topo: Some(TOPO),
                    scenario: Some(args.event),
                    spec_builder: Some(args.spec_builder),
                    spec: &spec,
                    decomp: Some(&decomp),
                    rand: args.rand,
                    data: Parameters {
                        failure,
                        pecs: args.pecs,
                    },
                }
                .write_json(path)?;

                Ok::<(), runtime::lab::LabError>(())
            })?;
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ValueEnum, Serialize)]
enum UnexpectedEvent {
    LinkFailure,
    NewBestRoute,
}

impl UnexpectedEvent {
    /// build the event as a config modifier. This may change the original network toi prepare the
    /// unexpected event.
    fn build(&self, net: &mut Network<P, BasicEventQueue<P>>, prefix: P) -> ExternalEventPrepared {
        match self {
            UnexpectedEvent::LinkFailure => {
                let new = net.clone();
                let mut fw_old = net.get_forwarding_state();
                let mut fw_new = new.get_forwarding_state();

                // find the link with the most traffic going through
                let mut edges: HashMap<(RouterId, RouterId), usize> = HashMap::new();
                net.get_routers()
                    .into_iter()
                    .flat_map(|r| {
                        fw_old
                            .get_paths(r, prefix)
                            .unwrap()
                            .into_iter()
                            .chain(fw_new.get_paths(r, prefix).unwrap().into_iter())
                    })
                    .flat_map(|p: Vec<RouterId>| p.clone().into_iter().zip(p.into_iter().skip(1)))
                    .map(|(a, b)| if a > b { (a, b) } else { (b, a) })
                    .filter(|(a, b)| {
                        net.get_device(*a).is_internal() && net.get_device(*b).is_internal()
                    })
                    .for_each(|k| *edges.entry(k).or_default() += 1);

                let ((a, b), _) = edges.into_iter().max_by_key(|(_, x)| *x).unwrap();
                ExternalEventPrepared::LinkFailure(a, b)
            }
            UnexpectedEvent::NewBestRoute => {
                let e = net.add_external_router("NewRoute", 666);
                let mut routers = net.get_routers();
                routers.shuffle(&mut thread_rng());
                let r = routers
                    .into_iter()
                    .find(|r| {
                        net.get_device(*r)
                            .unwrap_internal()
                            .get_bgp_sessions()
                            .values()
                            .all(|s| s.is_ibgp())
                    })
                    .unwrap();
                net.add_link(r, e);
                net.set_link_weight(r, e, 1.0).unwrap();
                net.set_link_weight(e, r, 1.0).unwrap();
                net.set_bgp_session(r, e, Some(BgpSessionType::EBgp))
                    .unwrap();
                ExternalEventPrepared::NewRoute(
                    e,
                    BgpRoute {
                        prefix,
                        as_path: vec![666.into()],
                        next_hop: e,
                        local_pref: None,
                        med: None,
                        community: Default::default(),
                        originator_id: None,
                        cluster_list: Default::default(),
                    },
                )
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
enum ExternalEventPrepared {
    LinkFailure(RouterId, RouterId),
    NewRoute(RouterId, BgpRoute<P>),
}

impl ExternalEventPrepared {
    fn build<Q>(&self, lab: &mut CiscoLab<'_, P, Q, Inactive>) -> ExternalEvent {
        match self {
            Self::LinkFailure(a, b) => ExternalEvent::LinkFailure(*a, *b),
            Self::NewRoute(r, route) => {
                lab.step_external_time();
                lab.advertise_route(*r, route).unwrap();
                ExternalEvent::RoutingInput
            }
        }
    }
}

#[derive(Debug, Serialize)]
struct Parameters {
    failure: Option<ExternalEventPrepared>,
    pecs: Option<u32>,
}
