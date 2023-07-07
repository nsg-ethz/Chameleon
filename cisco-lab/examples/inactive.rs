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

use bgpsim::types::SimplePrefix as P;
use cisco_lab::{CiscoLab, CiscoLabError};

mod test_net;
mod utils;

fn main() -> Result<(), CiscoLabError> {
    // create the network
    let net = test_net::generate_net::<P>()?;

    // create the lab
    let mut lab = CiscoLab::new(&net)?;

    utils::write_config(&mut lab)?;

    Ok(())
}
