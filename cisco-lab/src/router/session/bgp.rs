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

//! Module to parse BGP route output of cisco routers

use std::{
    collections::{BTreeSet, HashMap},
    iter::Peekable,
    net::Ipv4Addr,
    str::Lines,
};

use bgpsim::types::AsId;
use ipnet::Ipv4Net;
use lazy_static::lazy_static;
use regex::Regex;
use thiserror::Error;

use super::{table_parser::parse_table_with_alignment, ParseError};

/// Structure containing a BGP Route in detail. It is parsed from showing the detailed route list on
/// cisco routers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BgpRoute {
    /// The network of this route.
    pub net: Ipv4Net,
    /// The next-hop attribute
    pub next_hop: Ipv4Addr,
    /// The MED (Multi-Exit Discriminator) attribute
    pub med: Option<u32>,
    /// The local-pref attribute
    pub local_pref: Option<u32>,
    /// The weight of this path (local attribute)
    pub weight: u32,
    /// The IGP cost to reach the next-hop (local attribute)
    pub igp_cost: u32,
    /// The AS Path stored in the route
    pub path: Vec<AsId>,
    /// The set of communities of that route.
    pub communities: BTreeSet<(AsId, u32)>,
    /// The neighbor that advertised that route (IP Address of the session with that neighbor).
    pub neighbor: Ipv4Addr,
    /// The neighbor that advertised that route (BGP Router ID of that neighbor).
    pub neighbor_id: Ipv4Addr,
    /// Whether this route is valid.
    pub valid: bool,
    /// Whether this route is selected
    pub selected: bool,
    /// The type of this path.
    pub path_type: BgpPathType,
}

impl BgpRoute {
    /// Parse the output of `show bgp ipv4 unicast detail` into a mapping of a destination prefix to
    /// a vector of routes.
    pub fn from_detail(s: impl AsRef<str>) -> Result<HashMap<Ipv4Net, Vec<BgpRoute>>, ParseError> {
        let mut result = HashMap::new();
        let mut s = s.as_ref().lines().peekable();
        while let Some((net, routes)) = Self::from_detail_single_route(&mut s)? {
            result.insert(net, routes);
        }
        Ok(result)
    }

    /// Parse a single route destination from the output of `show bgp ipb4 unicast detail`,
    /// consuming all lines that are part of that route. This function will search for the first
    /// occurrence of any route.
    fn from_detail_single_route(
        s: &mut Peekable<Lines<'_>>,
    ) -> Result<Option<(Ipv4Net, Vec<BgpRoute>)>, ParseError> {
        lazy_static! {
            static ref TABLE_START: Regex =
                Regex::new(r"^BGP routing table entry for (\d+\.\d+\.\d+\.\d+/\d+), version \d+$")
                    .unwrap();
            static ref ROUTE_START: Regex = Regex::new(r"^  Path type: ").unwrap();
        }
        // skip lines until we find a line that starts with "BGP routing tagle entry for ..."
        let net: Ipv4Net = loop {
            let l = match s.next() {
                Some(l) => l,
                None => return Ok(None),
            };
            if let Some(c) = TABLE_START.captures(l) {
                break c.get(1).map_or("", |m| m.as_str()).parse()?;
            }
        };

        // skip until we see the first `Path type` line.
        let start_line = loop {
            let l = s.next().ok_or(BgpRoutesDetailError::RouteHasNoPaths)?;
            if ROUTE_START.is_match(l) {
                break l;
            }
        };

        // extract all routes into their separate string
        let mut routes: Vec<String> = vec![start_line.to_string() + "\n"];
        while s.peek().map_or(false, |l| !TABLE_START.is_match(l)) {
            let l = s.next().unwrap();
            if ROUTE_START.is_match(l) {
                routes.push(l.to_string() + "\n");
            } else {
                let last = routes.last_mut().unwrap();
                last.push_str(l);
                last.push('\n');
            }
        }

        let mut result = Vec::new();
        for route_str in routes {
            match Self::from_detail_single_path(net, &route_str) {
                Ok(r) => result.push(r),
                Err(e) => log::warn!("Cannot parse a BGP path: {}\n{}", e, route_str),
            }
        }
        Ok(Some((net, result)))
    }

    /// Parse a single path from the output of `show bgp ipb4 unicast detail`, consuming all lines
    /// that are part of that path.
    fn from_detail_single_path(net: Ipv4Net, s: &str) -> Result<BgpRoute, ParseError> {
        lazy_static! {
            static ref NEXT_HOP_LINE: Regex = Regex::new(
                r"    (\d+\.\d+\.\d+\.\d+) \(metric (\d+)\) from (\d+\.\d+\.\d+\.\d+|0::) \((\d+\.\d+\.\d+\.\d+)\)"
            ).unwrap();
            static ref MED_LP_W_LINE: Regex = Regex::new(
                r"      Origin [a-zA-Z ]+, MED (not set|\d+), localpref (\d+), weight (\d+)"
            ).unwrap();
            static ref AS_PATH_LINE: Regex = Regex::new(
                r"  AS-Path: (NONE|[0-9 ]*), path"
            ).unwrap();
            static ref COMMUNITY_LINE: Regex = Regex::new(
                r"      Community: ([0-9: ]*)"
            ).unwrap();
            static ref PATH_TYPE_LINE: Regex = Regex::new(
                r"  Path type: ([a-z ]*), ([a-z \(\)/]*), (is best path|not best|is deleted|no labeled nexthop)"
            ).unwrap();
        }

        let next_hop_line = NEXT_HOP_LINE
            .captures(s)
            .ok_or(BgpRoutesDetailError::NoNextHopLine)?;
        let med_lp_w_line = MED_LP_W_LINE
            .captures(s)
            .ok_or(BgpRoutesDetailError::NoMedLpWeightLine)?;
        let path = match AS_PATH_LINE
            .captures(s)
            .ok_or(BgpRoutesDetailError::NoAsPathLine)?
            .get(1)
            .unwrap()
            .as_str()
            .trim()
        {
            "NONE" => Default::default(),
            x => x
                .split(' ')
                .map(|x| x.parse().map(AsId))
                .collect::<Result<_, _>>()?,
        };
        let communities = COMMUNITY_LINE
            .captures(s)
            .and_then(|c| c.get(1))
            .map_or("", |m| m.as_str())
            .trim()
            .split(' ')
            .filter_map(|x| x.split_once(':'))
            .map(|(id, x)| Ok::<_, ParseError>((AsId(id.parse()?), x.parse()?)))
            .collect::<Result<_, _>>()?;
        let path_type_line = PATH_TYPE_LINE
            .captures(s)
            .ok_or(BgpRoutesDetailError::NoPathTypeLine)?;

        Ok(Self {
            net,
            next_hop: next_hop_line.get(1).unwrap().as_str().parse()?,
            med: match med_lp_w_line.get(1).unwrap().as_str() {
                "not set" => None,
                med => Some(med.parse()?),
            },
            local_pref: Some(med_lp_w_line.get(2).unwrap().as_str().parse()?),
            weight: med_lp_w_line.get(3).unwrap().as_str().parse()?,
            igp_cost: next_hop_line.get(2).unwrap().as_str().parse()?,
            path,
            communities,
            neighbor: {
                let neighbor = next_hop_line.get(3).unwrap().as_str();
                if neighbor == "0::" {
                    Ipv4Addr::new(0, 0, 0, 0)
                } else {
                    neighbor.parse()?
                }
            },
            neighbor_id: next_hop_line.get(4).unwrap().as_str().parse()?,
            valid: path_type_line.get(2).unwrap().as_str() == "path is valid",
            selected: path_type_line.get(3).unwrap().as_str() == "is best path",
            path_type: match path_type_line.get(1).unwrap().as_str() {
                "internal" => BgpPathType::Internal,
                "external" => BgpPathType::External,
                "confederation" => BgpPathType::Confederation,
                "local" => BgpPathType::Local,
                "aggregated" => BgpPathType::Aggregated,
                "redistributed" => BgpPathType::Redistributed,
                "injected" => BgpPathType::Injected,
                "incomplete" => BgpPathType::Incomplete,
                x => Err(BgpRoutesDetailError::UnknownPathType(x.to_string()))?,
            },
        })
    }
}

/// From where was the route learned?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BgpPathType {
    Internal,
    External,
    Confederation,
    Local,
    Aggregated,
    Redistributed,
    Injected,
    Incomplete,
}

/// Parser error for parsing `BgpRoutes` from the detailed table.
#[derive(Debug, Error)]
pub enum BgpRoutesDetailError {
    /// Detailed routing table could not be parsed
    #[error("Detailed routing table could not be parsed.")]
    TableParseError,
    /// Detailed routing table has a route without any paths
    #[error("Detailed routing table has a route without any paths.")]
    RouteHasNoPaths,
    /// BGP Path is missing next-hop line
    #[error("The BGP Path is missing the next-hop line.")]
    NoNextHopLine,
    /// BGP Path is missing the line containing the MED, local-preference and the weight.
    #[error("The BGP Path is missing the line containing the MED, LP and Weight.")]
    NoMedLpWeightLine,
    /// BGP Path is missing the AS-Path line.
    #[error("The BGP Path is missing the AS-Path line.")]
    NoAsPathLine,
    /// BGP Path is missing the Path-type line
    #[error("The BGP Path is missing the Path-type line.")]
    NoPathTypeLine,
    /// Read an unknown path type
    #[error("Unknown path type: {0}")]
    UnknownPathType(String),
}

/// Structure that contains informations about BGP Neighbors. This is the parsed output of the
/// command `show ip bgp summary`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BgpNeighbor {
    /// ID of the BGP neighbor (IP Address).
    pub id: Ipv4Addr,
    /// AS ID of the neighbor
    pub as_id: AsId,
    /// Wether the session to the neighbor is properly established, and routes can be exchanged.
    pub connected: bool,
    /// Number of routes received from that neighbor
    pub routes_received: u64,
}

impl BgpNeighbor {
    /// Generate a list of OSPF neighbors from the output of the command `show ip ospf neighbors`.
    pub fn from_table(table: &str) -> Result<Vec<BgpNeighbor>, ParseError> {
        // check the first row
        if !table
            .trim()
            .starts_with("BGP summary information for VRF default, address family IPv4 Unicast")
        {
            log::warn!("Invalid preamble line when parsing BGP summary:\n{}", table);
            return Err(ParseError::InvalidPreamble(table.to_string()));
        }
        // skip until the empty line
        let empty_line = match table.find("\n\n") {
            Some(pos) => pos,
            None => return Err(BgpRoutesDetailError::TableParseError.into()),
        };
        let table = &table[(empty_line + "\n\n".chars().count())..];

        // parse the table
        let fields = [
            ("Neighbor", false),
            ("V", false),
            ("AS", true),
            ("MsgRcvd", true),
            ("MsgSent", true),
            ("TblVer", true),
            ("InQ", true),
            ("OutQ", true),
            ("Up/Down", false),
            ("State/PfxRcd", false),
        ];

        let mut result = Vec::new();
        for row in parse_table_with_alignment(table, fields)? {
            result.push(BgpNeighbor {
                id: row[0].parse()?,
                as_id: AsId(row[2].parse()?),
                connected: row[9].parse::<usize>().is_ok(),
                routes_received: row[9].parse().unwrap_or_default(),
            })
        }

        Ok(result)
    }
}
