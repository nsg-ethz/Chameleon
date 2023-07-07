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

use chameleon::{decompose, runtime, specification::SpecificationBuilder, P};
use bgpsim::{
    builder::*,
    config::{ConfigExpr, ConfigModifier},
    prelude::*,
    topology_zoo::TopologyZoo,
};

/// The topology to test things on.
const TOPO: TopologyZoo = TopologyZoo::Abilene;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    pretty_env_logger::init_timed();

    eprintln!(
        "Running on {:?} with {} routers.",
        TOPO,
        TOPO.num_internals()
    );

    let p = P::from(1);
    let mut net = TOPO.build(BasicEventQueue::new());
    let se = net.get_router_id("Seattle")?;
    let ny = net.get_router_id("NewYork")?;
    let la = net.get_router_id("LosAngeles")?;
    let sn = net.get_router_id("Sunnyvale")?;
    let ka = net.get_router_id("KansasCity")?;
    let at = net.get_router_id("Atlanta")?;
    net.build_external_routers(|_, _| vec![se, ny, la], ())?;
    let e_se = net.get_router_id("Seattle_ext_11")?;
    let e_ny = net.get_router_id("NewYork_ext_12")?;
    let e_la = net.get_router_id("LosAngeles_ext_13")?;
    net.build_link_weights(uniform_integer_link_weight, (10, 100))?;
    net.build_ibgp_route_reflection(|_, _| vec![sn, ka, at], ())?;
    net.build_ebgp_sessions()?;
    let ads = net.build_advertisements(p, |_, _| vec![vec![e_ny], vec![e_se, e_la]], 3)?;

    let e = ads[0][0];
    let r = net
        .get_device(e)
        .unwrap_external()
        .get_bgp_sessions()
        .iter()
        .next()
        .copied()
        .unwrap();

    let command = ConfigModifier::Remove(ConfigExpr::BgpSession {
        source: r,
        target: e,
        session_type: BgpSessionType::EBgp,
    });

    let spec = SpecificationBuilder::EgressWaypoint.build_all(&net, Some(&command), [p]);

    let decomposition = decompose(&net, command, &spec)?;

    // write to file if feature is enabled
    #[cfg(feature = "export-web")]
    {
        atomic_bgp::export_web(&net, &spec, decomposition.clone(), "abilene")?;
    }

    // perform the simulation
    runtime::sim::run(net.clone(), decomposition.clone(), &spec)?;

    #[cfg(feature = "cisco-lab")]
    {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                // normal run
                let lab = runtime::lab::setup_cisco_lab(&net, Some(TOPO)).await?;
                let mut lab = lab.connect().await?;
                lab.wait_for_convergence().await?;
                runtime::lab::run(net.clone(), &mut lab, decomposition.clone(), None).await?;

                // drop the lab
                std::mem::drop(lab);

                // baseline run
                let lab = runtime::lab::setup_cisco_lab(&net, Some(TOPO)).await?;
                let mut lab = lab.connect().await?;
                lab.wait_for_convergence().await?;
                runtime::lab::run_baseline(net.clone(), &mut lab, decomposition.clone(), None)
                    .await?;

                Ok::<(), runtime::lab::LabError>(())
            })?;
    }

    Ok(())
}
