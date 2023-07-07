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

use bgpsim::{
    config::ConfigModifier,
    export::Addressor,
    net, prefix,
    route_map::{RouteMapBuilder, RouteMapDirection::Incoming},
};
use cisco_lab::CiscoLab;
use clap::Parser;
use ipnet::Ipv4Net;
use itertools::Itertools;
use std::net::Ipv4Addr;

use chameleon::{
    decompose, experiment::Experiment, runtime, specification::SpecificationBuilder, P,
};
use bgpsim::prelude::*;

/// Run the system in simulation and in the testbed.
#[derive(Debug, Parser)]
struct Cli {
    /// Specification to generate.
    #[clap(long = "spec", short = 's', default_value = "reachability")]
    spec_builder: SpecificationBuilder,
    /// Run the experiment in the cisco-lab
    #[clap(long = "lab", short = 'l')]
    cisco_lab: bool,
    /// Specifiy the number of prefixes (Prefix Equivalence Class) to simulate
    #[clap(long = "pecs", short = 'p')]
    pecs: Option<u32>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    pretty_env_logger::init_timed();

    let args = Cli::parse();

    let (net, p, command) = build_example();
    let spec = args.spec_builder.build_all(&net, Some(&command), [p]);
    let decomp = decompose(&net, command, &spec)?;

    // perform the simulation
    runtime::sim::run(net.clone(), decomp.clone(), &spec)?;

    if args.cisco_lab {
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
                let mut lab = CiscoLab::new(&net)?;
                if let Some(pecs) = pecs.clone() {
                    lab.addressor_mut().register_pec(p, pecs);
                }

                // connect to the lab and configure all devices
                let mut lab = lab.connect().await?;
                lab.wait_for_convergence().await?;

                // set the prefix equivalence classes
                let mut path =
                    runtime::lab::run(net.clone(), &mut lab, decomp.clone(), None).await?;

                // store the experiment
                path.push("scenario.json");
                Experiment {
                    net: &net,
                    topo: None,
                    scenario: None,
                    spec_builder: None,
                    spec: &spec,
                    decomp: Some(&decomp),
                    rand: false,
                    data: (),
                }
                .write_json(&path)?;
                path.pop();

                // generate the web export
                let web_export_path = format!("{}/web_export", path.to_string_lossy());
                chameleon::export_web(&net, &spec, decomp.clone(), web_export_path).unwrap();

                // drop the lab
                std::mem::drop(lab);

                // baseline run
                let mut lab = CiscoLab::new(&net)?;
                if let Some(pecs) = pecs {
                    lab.addressor_mut().register_pec(p, pecs);
                }

                // connect to the lab and configure all devices
                let mut lab = lab.connect().await?;
                lab.wait_for_convergence().await?;

                let mut path =
                    runtime::lab::run_baseline(net.clone(), &mut lab, decomp.clone(), None).await?;

                // generate the scenario.json
                path.push("scenario.json");
                Experiment {
                    net: &net,
                    topo: None,
                    scenario: None,
                    spec_builder: None,
                    spec: &spec,
                    decomp: Some(&decomp),
                    rand: false,
                    data: (),
                }
                .write_json(path)?;

                Ok::<(), runtime::lab::LabError>(())
            })?;
    }

    Ok(())
}

fn build_example() -> (Network<P, BasicEventQueue<P>>, P, ConfigModifier<P>) {
    let (mut net, (n1, e1)) = net! {
        Queue = BasicEventQueue<P>;
        links = {
            n1 -> n2: 1;
            n1 -> n4: 1;
            n2 -> n3: 1;
            n3 -> n6: 1;
            n4 -> n5: 1;
            n5 -> n6: 1;
        };
        sessions = {
            n2 -> n5;
            n2 -> n1: client;
            n2 -> n3: client;
            n2 -> n4: client;
            n2 -> n6: client;
            n5 -> n1: client;
            n5 -> n3: client;
            n5 -> n4: client;
            n5 -> n6: client;
            n1 -> e1!(1);
            n6 -> e6!(6);
        };
        routes = {
            e1 -> "100.0.0.0/24" as {path: [1, 100]};
            e6 -> "100.0.0.0/24" as {path: [6, 6, 6, 100]};
        };
        return (n1, e1)
    };

    let p = prefix!("100.0.0.0/24" as);

    net.set_bgp_route_map(
        n1,
        e1,
        Incoming,
        RouteMapBuilder::new()
            .order(20)
            .allow()
            .set_local_pref(200)
            .build(),
    )
    .unwrap();

    let cmd = ConfigModifier::Insert(bgpsim::config::ConfigExpr::BgpRouteMap {
        router: n1,
        neighbor: e1,
        direction: Incoming,
        map: RouteMapBuilder::new()
            .order(10)
            .allow()
            .match_prefix(p)
            .set_local_pref(50)
            .exit()
            .build(),
    });

    (net, p, cmd)
}
