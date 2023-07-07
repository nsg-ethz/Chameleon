// BgpSim: BGP Network Simulator written in Rust
// Copyright (C) 2022-2023 Tibor Schneider <sctibor@ethz.ch>
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

#![allow(dead_code)]

use bgpsim::types::Prefix;
use cisco_lab::{CiscoLab, CiscoLabError};
use std::{
    fmt::Display,
    fs::{create_dir, remove_file, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

/// Write out the configuraiton to string.
pub fn write_config<P: Prefix, Q>(lab: &mut CiscoLab<P, Q>) -> Result<(), CiscoLabError> {
    // generate the folder with all the output
    let mut out_dir = PathBuf::from("example_data");
    if !out_dir.exists() {
        create_dir(&out_dir)?;
    }

    // generate the configuration for all routers
    for (router, config) in lab.generate_router_config_all()?.into_values() {
        out_dir.push(format!("{router}.conf"));
        if out_dir.exists() {
            remove_file(&out_dir)?;
        }
        OpenOptions::new()
            .create(true)
            .write(true)
            .open(&out_dir)?
            .write_all(config.as_bytes())?;
        out_dir.pop();
    }

    // generate the netplan config
    out_dir.push("netplan.conf");
    if out_dir.exists() {
        remove_file(&out_dir)?;
    }
    OpenOptions::new()
        .create(true)
        .write(true)
        .open(&out_dir)?
        .write_all(lab.generate_exabgp_netplan_config()?.as_bytes())?;
    out_dir.pop();

    // generate the exabgp config
    out_dir.push("exabgp.conf");
    if out_dir.exists() {
        remove_file(&out_dir)?;
    }
    OpenOptions::new()
        .create(true)
        .write(true)
        .open(&out_dir)?
        .write_all(lab.generate_exabgp_config()?.as_bytes())?;
    out_dir.pop();

    // generate the exabgp runner
    out_dir.push("run_exabgp.py");
    if out_dir.exists() {
        remove_file(&out_dir)?;
    }
    OpenOptions::new()
        .create(true)
        .write(true)
        .open(&out_dir)?
        .write_all(lab.generate_exabgp_runner()?.as_bytes())?;
    out_dir.pop();

    // generate the tofino controller
    out_dir.push("controller.py");
    if out_dir.exists() {
        remove_file(&out_dir)?;
    }
    OpenOptions::new()
        .create(true)
        .write(true)
        .open(&out_dir)?
        .write_all(lab.generate_tofino_controller()?.as_bytes())?;
    out_dir.pop();

    Ok(())
}

/// Write out the configuraiton to string.
pub fn write_lines<S>(
    lines: impl IntoIterator<Item = S>,
    filename: impl AsRef<Path>,
) -> Result<(), CiscoLabError>
where
    S: Display,
{
    // generate the folder with all the output
    let mut out_dir = PathBuf::from("example_data");
    if !out_dir.exists() {
        create_dir(&out_dir)?;
    }

    out_dir.push(filename);

    // delete if it exists
    if out_dir.exists() {
        remove_file(&out_dir)?;
    }

    // create file
    let mut file = OpenOptions::new().create(true).write(true).open(&out_dir)?;

    for line in lines {
        writeln!(file, "{line}")?;
    }

    Ok(())
}

fn main() {}
