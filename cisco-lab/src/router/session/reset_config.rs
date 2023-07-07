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

//! This module provides code to reset a configuration.

use std::{cmp::Ordering, iter::once};

use lazy_static::lazy_static;
use regex::Regex;

/// State of the parser.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum State {
    Root,
    Interface,
    Ospf,
    Bgp,
    Skip,
}

/// Action to take on each line
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Action {
    Negate,
    Enter(State),
    Ignore,
}

use Action::*;
use State::*;

macro_rules! transition {
    ($(($name:ident, $state:pat, $rex:literal) => $action:expr,)*) => {
        /// The following function matches on the current state and the line, and produces the next
        /// state. The line is always matched on a regular expression.
        fn transition(state: State, line: &str) -> Action {

            lazy_static! {
                $(static ref $name: Regex = Regex::new($rex).unwrap();)*
            }

            match state {
                $(
                    $state if $name.is_match(line) => $action,
                )*
                _ => Ignore,
            }
        }
    }
}

// The following function matches on the current state and the line, and produces the next
// state. The line is always matched on a regular expression.
//
// This function is implemented as a macro. Each match statement here will be translated into a
// proper match statement on the current state and the regex provided. The regex will be generated
// in a `lazy_static` environment, such that the regular expressions are compiled once. Hence, each
// transition needs a label.
transition! {
 // Label,          State,     Regex on the line             => next state
    (RM_ACCEPT_ALL, Root,      r"^route-map ACCEPT-ALL")     => Ignore,
    (RM,            Root,      r"^route-map ")               => Negate,

    (RM_CL,         Root,      r"^ip community-list ")       => Negate,
    (RM_PL,         Root,      r"^ip prefix-list ")          => Negate,
    (RM_AS_PL,      Root,      r"^bgp as-path access-list ") => Negate,

    (STATIC_ROUTE,  Root,      r"^ip route ")                => Negate,

    (IF_MGNT,       Root,      r"^interface mgmt")           => Ignore,
    (IF_BLOCK,      Root,      r"^interface")                => Enter(Interface),
    (IF_ELEMENT,    Interface, r".*")                        => Negate,

    (OSPF_BLOCK,    Root,      r"^router ospf")              => Enter(Ospf),
    (OSPF_ELEMENT,  Ospf,      r".*")                        => Negate,

    (BGP_BLOCK,     Root,      r"^router bgp")               => Enter(Bgp),
    (BGP_ELEMENT,   Bgp,       r".*")                        => Negate,
}

/// Create a set of commands that will invert the configuration on a device. Configuration that is
/// inverted is only the configuration that is set by `bgpsim` and `cisco_lab`. Any other
/// configuration (like passwords, keys, etc are ignored.
pub fn invert_config(config: impl AsRef<str>) -> String {
    // current state
    let mut state = Root;
    // stack for blocks. Each element stores the state before entering the block, and the
    // indentation.
    let mut blocks: Vec<(State, usize)> = Vec::new();
    // current indentation
    let mut indent = 0;
    // inverted config
    let mut reset = String::new();

    for line in config
        .as_ref()
        .lines()
        .filter(|l| !l.is_empty())
        .chain(once("end"))
    {
        let new_indent = line.chars().take_while(|c| c.is_whitespace()).count();
        match new_indent.cmp(&indent) {
            Ordering::Equal => {}
            Ordering::Greater => {
                // we have entered a new block without explicitly. We thus enter a new block and set
                // the current state to Skip.
                blocks.push((state, indent));
                indent = new_indent;
                state = Skip;
            }
            Ordering::Less => {
                // We have exited a block. Pop states until the block indent is equal to the current
                // indent
                while indent != new_indent {
                    // put an `exit` to the reset config if the current state is not `Skip`.
                    if state != Skip {
                        reset.push_str("exit\n");
                    }
                    (state, indent) = blocks.pop().unwrap();
                }
            }
        }

        // trim all whitespace characters from the line
        let line = line.trim();

        match transition(state, line) {
            Negate => {
                if let Some(line) = line.strip_prefix("no") {
                    reset.push_str(line.trim());
                } else {
                    reset.push_str("no ");
                    reset.push_str(line);
                }
                reset.push('\n');
            }
            Enter(new_state) => {
                // enter the new state
                reset.push_str(line);
                reset.push('\n');
                // push the blocks
                blocks.push((state, indent));
                indent += 2;
                state = new_state;
            }
            Ignore => {}
        }
    }

    // trim empty blocks
    lazy_static! {
        static ref EMPTY_IFACE: Regex =
            Regex::new(r"(?m)\ninterface [A-Za-z0-9 /]*\nexit").unwrap();
        static ref EMPTY_ROUTER: Regex = Regex::new(r"(?m)\nrouter [a-z]* [0-9]*\nexit").unwrap();
    }

    let reset = EMPTY_IFACE.replace_all(&reset, "");
    let reset = EMPTY_ROUTER.replace_all(&reset, "");

    reset.to_string()
}
