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

//! # Chameleon: Taming the transient while reconfiguring BGP
//!
//! This is the implementation of the paper "Taming the transient while reconfiguring BGP",
//! published at SIGCOMM '23. Please cite the following article:
//!
//! ```bibtex
//!
//! @INPROCEEDINGS{schneider2023taming,
//!     year = {2023},
//!     booktitle = {Proceedings of the 2023 ACM Special Interest Group on Data Communication (SIGCOMM)},
//!     type = {Conference Paper},
//!     institution = {EC},
//!     author = {Schneider, Tibor and Schmid, Roland and Vissicchio, Stefano and Vanbever, Laurent},
//!     language = {en},
//!     title = {Taming the transient while reconfiguring BGP},
//!     Note = {37th ACM SiGCOMM Conference (SIGCOMM 2023); Conference Location: New York, NY, USA; Conference Date: September 10-14, 2023}
//!     doi = {10.1145/3603269.3604855}
//!     url = {https://doi.org/10.1145/3603269.3604855}
//! }
//! ```
//!
//! ## Abstract
//!
//! BGP reconfigurations are a daily occurrence for most network operators, especially in large
//! networks. Despite many recent efforts, performing safe and robust BGP reconfiguration changes is
//! still an open problem. Existing techniques are indeed either (i) unsafe, because they ignore the
//! impact of transient states which can easily lead to invariant violations; or (ii) impractical as
//! they duplicate the entire routing and forwarding states and require hard- and software support.
//!
//! This paper introduces Chameleon, the first BGP reconfiguration system capable of maintaining
//! correctness throughout the entire reconfiguration process. Chameleon is akin to concurrency
//! coordination in distributed systems. Specifically, we model the reconfiguration process with
//! happens-before relations; show how to reason about (transient) violations; and how to avoid them
//! by precisely controlling BGP route propagation and convergence during the reconfiguration.
//!
//! We fully implement Chameleon and evaluate it in both testbeds and simulations, on real-world
//! topologies and large-scale reconfiguration scenarios. In most experiments, our system computes
//! reconfiguration plans within a minute, and performs them from start to finish in a few minutes,
//! with minimal overhead and no impact on network resiliency.
//!
//! ## Structure
//! The source code of this program is structured as follows:
//! - The module [`decomposition`] (function [`decompose`] and structure [`Decomposition`]) contains
//!   the entire code for decomposing the command into multiple atomic commands. The module contains
//!   the Analyzer ([`decomposition::bgp_dependencies`]), the Scheduler
//!   ([`decomposition::ilp_scheduler`]), and the Compiler ([`decomposition::compiler`]).
//! - The module `runtime` is responsible for applying the decomposition to the network, and
//!   verifying if the decomposition is valid. It contains an implementation to run Chameleon on
//!   both BgpSim ([`runtime::sim`]) and the test bed ([`runtime::lab`] with the feature
//!   `cisco-lab`).
//! - The module [`experiment`] contains code to quickly genwerate topoligies, configurations, and
//!   reconfiguration scenarios.
//! - The module [`specification`] defines the specification language
//!   ([`specification::Specification`]).
//! - The basic datastructures used for the resulting [`Decomposition`] are defined in a separate
//!   crate: [`atomic_command`].

#![deny(
    missing_docs,
    clippy::missing_docs_in_private_items,
    missing_debug_implementations,
    rust_2018_idioms
)]
#![allow(clippy::result_large_err)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc(html_logo_url = "https://iospf.tibors.ch/images/bgpsim/dark_only.svg")]

pub mod decomposition;
mod formatter;
pub mod runtime;
pub mod specification;
#[cfg(test)]
mod test;

pub use bgpsim::types::{RouterId, SimplePrefix as P};
pub use decomposition::{decompose, Decomposition};

#[cfg(feature = "experiment")]
#[cfg_attr(docsrs, doc(cfg(feature = "experiment")))]
/// Export an experiment result to a json file, including all metadata.
pub mod experiment {
    use std::{
        ffi::OsStr,
        fs::{remove_file, OpenOptions},
        io::Write,
        path::{Path, PathBuf},
    };

    use crate::{
        specification::{Specification, SpecificationBuilder},
        Decomposition,
    };

    use super::P;
    use bgpsim::{
        builder::{
            constant_link_weight, k_random_nodes, uniform_integer_link_weight, NetworkBuilder,
        },
        config::{ConfigExpr, ConfigModifier, NetworkConfig},
        event::{EventQueue, FmtPriority},
        prelude::{BgpSessionType, Network},
        topology_zoo::TopologyZoo,
        types::{NetworkError, RouterId},
    };
    use clap::ValueEnum;
    use serde::{Deserialize, Serialize};
    use thiserror::Error;
    use time::{format_description, OffsetDateTime};

    /// What is the kind of reconfiguration that should be done?
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, ValueEnum, Deserialize, Serialize)]
    pub enum Scenario {
        /// Advertise a new route that is better than all others
        NewBestRoute,
        /// Withdraw the old best route.
        DelBestRoute,
    }

    /// Error thrown while building a scenario
    #[derive(Debug, Error)]
    pub enum ScenarioBuildError {
        /// Network error occurred.
        #[error("{0}")]
        Network(#[from] NetworkError),
        /// external BGP session missing
        #[error("Missing an eBGP session with the external router {0:?}")]
        NoBgpSession(RouterId),
        /// Initial or final forwarding state violate reachability
        #[error("Initial or final forwarding state violate reachability.")]
        ConvergenceViolated,
    }

    impl Scenario {
        /// Generate and configure the network appropriately, and generate the reconfiguration command.
        #[allow(clippy::type_complexity)]
        pub fn build<Q>(
            &self,
            topo: TopologyZoo,
            queue: Q,
            randomized: bool,
        ) -> Result<(Network<P, Q>, P, ConfigModifier<P>), ScenarioBuildError>
        where
            Q: EventQueue<P> + Clone,
            Q::Priority: FmtPriority,
        {
            let p = P::from(1);
            let mut net = topo.build(queue);

            let ads = if randomized {
                let ext = net.build_external_routers(k_random_nodes, 3)?;
                net.build_link_weights(uniform_integer_link_weight, (10, 100))?;
                net.build_ibgp_route_reflection(k_random_nodes, 3)?;
                net.build_ebgp_sessions()?;
                let preferences = vec![vec![ext[0]], vec![ext[1], ext[2]]];
                net.build_advertisements(p, |_, _| preferences, ())?
            } else {
                let mut r = net.get_routers();
                r.sort();
                let (egresses, rrs) = if topo == TopologyZoo::Abilene {
                    (
                        vec![
                            net.get_router_id("NewYork")?,
                            net.get_router_id("Houston")?,
                            net.get_router_id("Seattle")?,
                        ],
                        vec![
                            net.get_router_id("Indianapolis")?,
                            net.get_router_id("Atlanta")?,
                            net.get_router_id("Denver")?,
                        ],
                    )
                } else if r.len() <= 3 {
                    (r.clone(), r)
                } else if r.len() == 4 {
                    (vec![r[0], r[1], r[3]], vec![r[0], r[2], r[3]])
                } else if r.len() == 5 {
                    (vec![r[0], r[2], r[4]], vec![r[0], r[1], r[3]])
                } else {
                    let s = r.len() / 6;
                    (
                        vec![r[0], r[2 * s], r[4 * s]],
                        vec![r[s], r[3 * s], r[5 * s]],
                    )
                };
                let ext = net.build_external_routers(|_, _| egresses, ())?;
                net.build_link_weights(constant_link_weight, 1.0)?;
                net.build_ibgp_route_reflection(|_, _| rrs, ())?;
                net.build_ebgp_sessions()?;
                let preferences = vec![vec![ext[0]], vec![ext[1], ext[2]]];
                net.build_advertisements(p, |_, _| preferences, ())?
            };

            let e = ads[0][0];
            let r = match net
                .get_device(e)
                .unwrap_external()
                .get_bgp_sessions()
                .iter()
                .next()
            {
                Some(r) => *r,
                None => return Err(ScenarioBuildError::NoBgpSession(e)),
            };

            let c = match self {
                Scenario::NewBestRoute => {
                    net.set_bgp_session(r, e, None)?;
                    ConfigModifier::Insert(ConfigExpr::<P>::BgpSession {
                        source: r,
                        target: e,
                        session_type: BgpSessionType::EBgp,
                    })
                }
                Scenario::DelBestRoute => ConfigModifier::Remove(ConfigExpr::<P>::BgpSession {
                    source: r,
                    target: e,
                    session_type: BgpSessionType::EBgp,
                }),
            };

            // check the initial and final states.
            let mut initial_fw = net.get_forwarding_state();
            let mut tmp_net = net.clone();
            tmp_net.apply_modifier(&c)?;
            let mut final_fw = tmp_net.get_forwarding_state();
            if net
                .get_routers()
                .into_iter()
                .any(|r| initial_fw.get_paths(r, p).is_err() || final_fw.get_paths(r, p).is_err())
            {
                return Err(ScenarioBuildError::ConvergenceViolated);
            }

            Ok((net, p, c))
        }
    }

    /// Structure to store an experiment result to file
    #[derive(Debug)]
    pub struct Experiment<'a, T, Q> {
        /// Network generated (in the initial state),
        pub net: &'a Network<P, Q>,
        /// Topology of the network
        pub topo: Option<TopologyZoo>,
        /// Scenario used to generate the network configuration
        pub scenario: Option<Scenario>,
        /// Specification used to build the specification
        pub spec_builder: Option<SpecificationBuilder>,
        /// Specification for the experiment`
        pub spec: &'a Specification,
        /// Decomposed schedule for the experiment
        pub decomp: Option<&'a Decomposition>,
        /// Wether the configuration was randomized
        pub rand: bool,
        /// Data obtained during the experiment.
        pub data: T,
    }

    impl<'a, T, Q> Experiment<'a, T, Q>
    where
        T: Serialize,
        Q: EventQueue<P> + Serialize,
    {
        /// Write the json file adding to the filename `_DATE.json`. If the file already exists,
        /// append an increasing number to the filename.
        pub fn write_json_with_timestamp(
            &self,
            file: impl AsRef<str>,
        ) -> Result<(), std::io::Error> {
            let cur_time = OffsetDateTime::now_local()
                .unwrap_or_else(|_| OffsetDateTime::now_utc())
                .format(
                    &format_description::parse("[year]-[month]-[day]_[hour]-[minute]-[second]")
                        .unwrap(),
                )
                .unwrap();
            let mut offset: Option<usize> = None;
            let file = loop {
                let filename = if let Some(offset) = offset {
                    format!("{}_{cur_time}_{}.json", file.as_ref(), offset)
                } else {
                    format!("{}_{cur_time}.json", file.as_ref())
                };
                let file = PathBuf::from(filename);
                if !file.exists() {
                    break file;
                }
                offset = Some(offset.unwrap_or_default() + 1);
            };

            self.write_json(file)
        }

        /// Write the content of the experiment to a json file.
        ///
        /// This function will overwrite any existing file.
        pub fn write_json(&self, file: impl AsRef<OsStr>) -> Result<(), std::io::Error> {
            #[derive(Debug, Serialize)]
            #[allow(clippy::missing_docs_in_private_items)]
            pub struct ExportExperiment<'a, 'b, S, T> {
                topo: Option<TopologyZoo>,
                scenario: &'b S,
                spec_builder: Option<SpecificationBuilder>,
                spec: &'a Specification,
                decomp: Option<&'a Decomposition>,
                data: &'b T,
                net: serde_json::Value,
            }

            let exp_str = serde_json::to_string_pretty(&ExportExperiment {
                net: serde_json::from_str(&self.net.as_json_str()).unwrap(),
                topo: self.topo,
                scenario: &self.scenario,
                spec_builder: self.spec_builder,
                spec: self.spec,
                decomp: self.decomp,
                data: &self.data,
            })
            .unwrap();

            let file = Path::new(file.as_ref());
            if file.exists() {
                remove_file(file)?;
            }
            let mut file = OpenOptions::new().create(true).write(true).open(file)?;
            writeln!(file, "{exp_str}")?;
            Ok(())
        }
    }

    /// Wrapping type for TopologyZoo that implements ValueEnum
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    pub struct _TopologyZoo(pub TopologyZoo);

    impl ValueEnum for _TopologyZoo {
        fn value_variants<'a>() -> &'a [Self] {
            lazy_static::lazy_static! {
                static ref VARIANTS: Vec<_TopologyZoo> = TopologyZoo::topologies_increasing_nodes()
                    .iter()
                    .copied()
                    // .filter(|t| t.num_externals() == 0)
                    .map(_TopologyZoo)
                    .collect();
            }
            VARIANTS.as_slice()
        }

        fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
            let t = self.0;
            Some(
                clap::builder::PossibleValue::new(t.to_string()).help(format!(
                    "Topology with {} internal nodes, and {} edges",
                    t.num_internals(),
                    t.num_internal_edges()
                )),
            )
        }
    }
}

#[cfg(feature = "export-web")]
pub use export_web::export_web;

/// Module for exporting the migration for `bgpsim-web`.
#[cfg(feature = "export-web")]
#[cfg_attr(docsrs, doc(cfg(feature = "export-web")))]
mod export_web {
    use crate::specification::Specification;

    use super::decomposition::Decomposition;
    use super::P;
    use atomic_command::{AtomicCommand, AtomicCondition, AtomicModifier};
    use bgpsim::{
        event::EventQueue,
        policies::{FwPolicy, Policy, PolicyError},
        prelude::*,
    };
    use serde::ser::Serialize;
    use serde_json::Value;
    use std::{collections::HashMap, fs::OpenOptions, io::Write};

    /// Export the network, the policies and the decomposition to a json file to import into `bgpsim-web`.
    pub fn export_web<Q>(
        net: &Network<P, Q>,
        spec: &Specification,
        decomp: Decomposition,
        filename: impl AsRef<str>,
    ) -> Result<(), Box<dyn std::error::Error>>
    where
        Q: EventQueue<P> + Serialize,
    {
        let Decomposition {
            setup_commands,
            cleanup_commands,
            atomic_before,
            main_commands,
            atomic_after,
            original_command,
            ..
        } = decomp;

        let atomic_migration: Vec<Vec<Vec<AtomicCommand<P>>>> = vec![
            setup_commands,
            atomic_before.into_values().flatten().collect(),
            main_commands,
            atomic_after.into_values().flatten().collect(),
            cleanup_commands,
        ];

        let instant_migration: Vec<Vec<Vec<AtomicCommand<P>>>> = vec![vec![vec![AtomicCommand {
            command: AtomicModifier::Raw(original_command),
            precondition: AtomicCondition::None,
            postcondition: AtomicCondition::None,
        }]]];

        #[allow(clippy::type_complexity)]
        let mut policies: HashMap<
            RouterId,
            Vec<(FwPolicy<P>, Result<(), PolicyError<P>>)>,
        > = HashMap::new();
        for (prefix, expr) in spec.clone() {
            for invariant in expr.as_global_invariants(net) {
                for policy in invariant.as_fw_policies(net, prefix) {
                    let r = policy.router().unwrap();
                    policies
                        .entry(r)
                        .or_default()
                        .push((policy.clone(), Ok(())));
                }
            }
        }

        let mut json_obj = serde_json::from_str::<Value>(&net.as_json_str())?;
        let obj = json_obj.as_object_mut().unwrap();
        obj.insert("spec".to_string(), serde_json::to_value(policies)?);
        obj.insert(
            "migration".to_string(),
            serde_json::to_value(atomic_migration)?,
        );
        let s = serde_json::to_string(&json_obj).unwrap();
        let mut f = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(format!("./{}_atomic.json", filename.as_ref()))?;
        f.write_all(s.as_bytes())?;

        let obj = json_obj.as_object_mut().unwrap();
        obj.insert(
            "migration".to_string(),
            serde_json::to_value(instant_migration)?,
        );
        let s = serde_json::to_string(&json_obj).unwrap();
        let mut f = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(format!("./{}_naive.json", filename.as_ref()))?;
        f.write_all(s.as_bytes())?;

        Ok(())
    }
}
