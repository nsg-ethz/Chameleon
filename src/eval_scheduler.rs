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

use std::{
    fs::{read_to_string, remove_file, File, OpenOptions},
    io::Write,
    path::Path,
    time::{Duration, Instant},
};

use chameleon::{
    decomposition::{
        bgp_dependencies::find_dependencies,
        ilp_scheduler::{schedule_with_max_steps, NodeSchedule},
        CommandInfo,
    },
    experiment::{Scenario as Event, _TopologyZoo},
    specification::{Specification, SpecificationBuilder},
    P,
};
use bgpsim::{config::ConfigModifier, prelude::*, topology_zoo::TopologyZoo};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

/// Evaluate the ILP scheduler.
#[derive(Debug, Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Prepare a migration scenario and store the scenario to file.
    Prepare {
        /// Which topology from TopologyZoo should be used.
        topo: _TopologyZoo,
        /// Which kind of invariants are present
        #[clap(short, long, default_value = "reachability")]
        spec_kind: SpecificationBuilder,
        /// Event that should be simulated
        #[clap(short, long, default_value = "del-best-route")]
        event: Event,
        /// File to store the output
        output: String,
        /// Randomize the configuration
        #[clap(short, long)]
        rand: bool,
    },

    /// Run the prepared scenario
    Run {
        /// The scenario file, previously generated with `prepare`.
        scenario: String,
        /// output file, where to store the generated CSV
        output: String,
        /// How many iterations should be done per step
        #[clap(short, long, default_value = "4")]
        iter: usize,
        /// Timeout in seconds before killing a thread.
        #[clap(short, long)]
        timeout: Option<usize>,
        /// Show the output as the program runs
        #[clap(short, long)]
        verbose: bool,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    pretty_env_logger::init_timed();

    let cli = Cli::parse();

    match cli.command {
        Command::Prepare {
            topo,
            spec_kind,
            event,
            output,
            rand,
        } => prepare(topo.0, spec_kind, event, output, rand)?,
        Command::Run {
            scenario,
            output,
            iter,
            timeout,
            verbose,
        } => run(scenario, output, iter, timeout, verbose)?,
    }

    Ok(())
}

fn prepare(
    topo: TopologyZoo,
    spec_kind: SpecificationBuilder,
    event: Event,
    output: String,
    rand: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (net, p, command) = event.build(topo, BasicEventQueue::new(), rand)?;
    let spec = spec_kind.build_all(&net, Some(&command), [p]);

    // store the structure
    let scenario = Scenario {
        topo,
        net,
        event,
        spec_kind,
        spec,
        command,
    };
    let scenario_str = serde_json::to_string(&scenario)?;
    let path = Path::new(&output);
    if path.exists() {
        remove_file(path)?;
    }
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    write!(file, "{scenario_str}")?;
    Ok(())
}

fn run(
    scenario: String,
    output: String,
    n_iter: usize,
    timeout: Option<usize>,
    verbose: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // read the scenario file
    let scenario_str = read_to_string(scenario)?;
    let s: Scenario = serde_json::from_str(&scenario_str)?;

    // prepare all the command stuff
    let info = CommandInfo::new(&s.net, s.command.clone(), &s.spec)?;
    let bgp_deps = find_dependencies(&info);

    // check that there is only one prefix
    if bgp_deps.len() != 1 {
        panic!("Only a single prefix is allowed! got {}", bgp_deps.len())
    }
    let prefix = *bgp_deps.keys().next().unwrap();

    // open the file to write
    let path = Path::new(&output);
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;

    // prepare the iterator
    let max_steps: usize = info.fw_diff.get(&prefix).map(|x| x.len()).unwrap_or(0);
    if verbose {
        eprintln!("Iterating over {max_steps} steps");
    }
    write_line(&mut file, "steps,iter,cost,time", verbose)?;

    for steps in 1..=max_steps {
        for iter in 0..n_iter {
            let start_time = Instant::now();
            let (result, _) = schedule_with_max_steps(
                &info,
                &bgp_deps,
                prefix,
                steps,
                timeout.map(|x| Duration::from_secs(x as u64)),
            );
            let cost: String = match result.as_ref() {
                Ok((r, _)) => r
                    .values()
                    .map(NodeSchedule::cost)
                    .sum::<usize>()
                    .to_string(),
                Err(_) => String::from("nan"),
            };
            let elapsed = start_time.elapsed().as_secs_f64();
            write_line(
                &mut file,
                format!("{steps},{iter},{cost},{elapsed}"),
                verbose,
            )?;
        }
    }

    Ok(())
}

fn write_line(file: &mut File, line: impl AsRef<str>, verbose: bool) -> Result<(), std::io::Error> {
    let line = line.as_ref();
    if verbose {
        eprintln!("{line}")
    }
    writeln!(file, "{line}")
}

/// Scenario structure that contains all information to repeat the same experiment
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Scenario {
    /// The base topology
    topo: TopologyZoo,
    net: Network<P, BasicEventQueue<P>>,
    event: Event,
    spec_kind: SpecificationBuilder,
    spec: Specification,
    command: ConfigModifier<P>,
}
