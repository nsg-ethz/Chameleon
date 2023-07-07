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

//! Runtime for the real-world system in the [`cisco_lab`]

use std::{fs::OpenOptions, io::Write, path::PathBuf, time::Duration};

use atomic_command::{AtomicCommand, AtomicCondition, AtomicModifier};
use bgpsim::{
    config::NetworkConfig, event::EventQueue, export::ExportError, prelude::*,
    topology_zoo::TopologyZoo,
};
use cisco_lab::{export_capture_to_csv, Active, CiscoLab, CiscoLabError, Inactive};
use thiserror::Error;
use tokio::{sync::broadcast::error::RecvError, task::JoinError};

use super::controller::Controller;
use crate::{decomposition::Decomposition, P};

mod executor;
pub use executor::{Event, EventKind};

/// Number of pings per second per flow.
const CAPTURE_FREQ: u64 = 500;

/// Create the [`CiscoLab`] instance from the given network.
pub async fn setup_cisco_lab<Q>(
    net: &'_ Network<P, Q>,
    topo: Option<TopologyZoo>,
) -> Result<CiscoLab<'_, P, Q, Inactive>, LabError>
where
    Q: Clone + EventQueue<P> + PartialEq + std::fmt::Debug,
{
    let mut lab = CiscoLab::new(net)?;

    // set the link delay if given a TopologyZoo Object
    if let Some(topo) = topo {
        lab.set_link_delays_from_geolocation(topo.geo_location());
    }

    Ok(lab)
}

/// Perform the decomposed update on the network using the cisco lab. This function returns the
/// folder where the experiment results were stored.
pub async fn run<'a, 'n: 'a, Q>(
    net: Network<P, Q>,
    lab: &'a mut CiscoLab<'n, P, Q, Active>,
    decomp: Decomposition,
    event: Option<ExternalEvent>,
) -> Result<PathBuf, LabError>
where
    Q: Clone + EventQueue<P> + PartialEq + std::fmt::Debug,
{
    run_and_save_results(
        net,
        lab,
        decomp,
        event.map(|x| (x, Duration::from_secs(30))),
        "lab_chameleon",
    )
    .await
}

/// Perform the decomposed update on the network using the cisco lab. This function returns the
/// folder where the experiment results were stored.
async fn run_and_save_results<'a, 'n: 'a, Q>(
    mut net: Network<P, Q>,
    lab: &'a mut CiscoLab<'n, P, Q, Active>,
    decomp: Decomposition,
    event: Option<(ExternalEvent, Duration)>,
    target_dir_base: impl AsRef<str>,
) -> Result<PathBuf, LabError>
where
    Q: Clone + EventQueue<P> + PartialEq + std::fmt::Debug,
{
    // do the update on the simulated net
    net.apply_modifier(&decomp.original_command)?;

    // create the controller
    let controller = Controller::new(decomp);

    // start the measurement
    let meas_handle = lab.start_capture(CAPTURE_FREQ).await?;

    // wait for 10 seconds before doing anything
    std::thread::sleep(Duration::from_secs(10));

    // now, schedule the external event (if some)
    if let Some((event, delay)) = event {
        event.schedule(lab, delay)?;
    }

    // execute the controller
    let event_log = controller.execute_lab(lab, &net).await?;

    // wait for 10 seconds after the update was complete
    std::thread::sleep(Duration::from_secs(20));

    // end the measurement
    let result = lab.stop_capture(meas_handle).await?;

    // store the capture to disk
    let mut folder = export_capture_to_csv(&net, &result, "results", target_dir_base)?;

    // create the logfile
    folder.push("event.log");
    let mut logfile = OpenOptions::new().create(true).write(true).open(&folder)?;
    for event in event_log.iter() {
        writeln!(logfile, "{}", event.fmt(&net))?;
    }
    folder.pop();

    // create the logfile as json
    #[cfg(feature = "serde")]
    {
        folder.push("event.json");
        let log_content = serde_json::to_string_pretty(&event_log).unwrap();
        let mut logfile = OpenOptions::new().create(true).write(true).open(&folder)?;
        writeln!(logfile, "{log_content}")?;
        folder.pop();
    }

    // store all router configuration
    for r in net.get_routers() {
        let vdc = lab.get_router_device(r)?;
        let config = lab.generate_router_config(r)?;
        folder.push(format!("{vdc}-{}.config", r.fmt(&net)));
        let mut config_file = OpenOptions::new().create(true).write(true).open(&folder)?;
        writeln!(config_file, "{config}")?;
        folder.pop();
    }

    // compare the state
    if event.is_none() {
        log::debug!("Comparing the final state...");
        if !lab.equal_bgp_state(&net).await? {
            return Err(LabError::WrongFinalState);
        }
    }
    Ok(folder)
}

/// run the baseline, which is simply applying the command on the live network.
pub async fn run_baseline<'a, 'n: 'a, Q>(
    net: Network<P, Q>,
    lab: &'a mut CiscoLab<'n, P, Q, Active>,
    decomp: Decomposition,
    event: Option<ExternalEvent>,
) -> Result<PathBuf, LabError>
where
    Q: Clone + EventQueue<P> + PartialEq + std::fmt::Debug,
{
    let cmd = decomp.original_command;
    let tmp_decomp = Decomposition {
        original_command: cmd.clone(),
        setup_commands: Default::default(),
        cleanup_commands: Default::default(),
        atomic_before: Default::default(),
        main_commands: vec![vec![AtomicCommand {
            command: AtomicModifier::Raw(cmd),
            precondition: AtomicCondition::None,
            postcondition: AtomicCondition::None,
        }]],
        atomic_after: Default::default(),
        bgp_deps: Default::default(),
        schedule: Default::default(),
        fw_state_trace: Default::default(),
    };

    run_and_save_results(
        net,
        lab,
        tmp_decomp,
        event.map(|x| (x, Duration::from_secs_f64(5.0))),
        "lab_baseline",
    )
    .await
}

/// Trigger unexpected external events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExternalEvent {
    /// A change in external routing, implemented by performing a step in the exabgp script.
    RoutingInput,
    /// Failure of a link, implemented by disabling two interfaces of the tofino.
    LinkFailure(RouterId, RouterId),
}

impl ExternalEvent {
    /// Schwedule the event on the lab
    fn schedule<Q>(
        self,
        lab: &mut CiscoLab<'_, P, Q, Active>,
        delay: Duration,
    ) -> Result<(), CiscoLabError> {
        // do the event
        match self {
            Self::RoutingInput => lab.step_exabgp_scheduled(delay),
            Self::LinkFailure(a, b) => lab.disable_link_scheduled(a, b, delay),
        }
    }
}

/// Error of the simulated runtime.
#[derive(Debug, Error)]
pub enum LabError {
    /// The simulated Network has thrown an error
    #[error("{0}")]
    NetworkError(#[from] NetworkError),
    /// Cisco lab had an error.
    #[error("{0}")]
    CiscoLab(#[from] CiscoLabError),
    /// Export error
    #[error("{0}")]
    ExportError(#[from] ExportError),
    /// The initial network is not equal to the expected network!
    #[error("The emulated network does not match the network!")]
    WrongInitialState,
    /// The resulting network is not equal to the expected network!
    #[error("The resulting network is not equal to the expected network!")]
    WrongFinalState,
    /// The controller cannot make any progress.
    #[error("The controller cannot make any progress")]
    CannotProgress,
    /// Error while joining threads
    #[error("Error while joining threads: {0:?}")]
    ThreadError(JoinError),
    /// Synchronization error
    #[error("Synchronization error: {0}")]
    SyncError(#[from] RecvError),
    /// Error while dealing with IO
    #[error("IO Error: {0}")]
    IoError(#[from] std::io::Error),
}
