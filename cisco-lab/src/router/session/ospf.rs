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

//! Module to parse OSPF route output of cisco routers

use std::{collections::HashMap, net::Ipv4Addr};

use ipnet::Ipv4Net;
use roxmltree::Node;

use super::{table_parser::parse_table, ParseError};

/// Structure that captrues a specific OSPF Route. This structure contains infromation from
/// executing the command `show ip ospf route` on a cisco router.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OspfRoute {
    pub net: Ipv4Net,
    pub area: Ipv4Addr,
    pub nh_addr: Ipv4Addr,
    pub nh_iface: String,
}

impl OspfRoute {
    /// Parse the XML output from executing the command `show ip ospf route | xml`. This function
    /// will parse the output and create a mapping of each known destination prefix rowards an Opsf
    /// Route.
    ///
    /// The trailing `]]>]]>` of the XML output will be removed in this function (if it exists).
    pub fn from_xml_output(input: &str) -> Result<HashMap<Ipv4Net, OspfRoute>, ParseError> {
        // first, remove a possible "]]>]]>".
        let input = input.trim().trim_end_matches("]]>]]>");
        let tree = roxmltree::Document::parse(input)?;
        // walk the tree until we ses a node called __readonly__
        let table = decend_xml_tree(
            tree.root(),
            [
                "rpc-reply",
                "data",
                "show",
                "ip",
                "ospf",
                "__XML__OPT_Cmd_show_ip_ospf_cmd_tag",
                "route",
                "__XML__OPT_Cmd_show_ip_ospf_route_ip-addr",
                "__XML__OPT_Cmd_show_ip_ospf_route_vrf",
                "__XML__OPT_Cmd_show_ip_ospf_route___readonly__",
                "__readonly__",
                "TABLE_ctx",
                "ROW_ctx",
                "TABLE_route",
            ],
        )?;

        let mut results = HashMap::new();
        for row in table.children() {
            if row.is_element() {
                let ospf = Self::from_node(row)?;
                results.insert(ospf.net, ospf);
            }
        }
        Ok(results)
    }

    /// Create an OSPF Route from a node
    fn from_node(node: Node<'_, '_>) -> Result<Self, ParseError> {
        let mut addr = None;
        let mut masklen = None;
        let mut area = None;
        let mut nh_addr = None;
        let mut nh_iface = None;

        for child in node.children() {
            if child.tag_name().name() == "addr" {
                addr = Some(
                    child
                        .text()
                        .ok_or(ParseError::NoText)?
                        .parse::<Ipv4Addr>()?,
                )
            }
            if child.tag_name().name() == "masklen" {
                masklen = Some(child.text().ok_or(ParseError::NoText)?.parse::<u8>()?);
            }
            if child.tag_name().name() == "area" {
                area = Some(
                    child
                        .text()
                        .ok_or(ParseError::NoText)?
                        .parse::<Ipv4Addr>()?,
                )
            }
        }

        let ubest_nh = decend_xml_tree(node, ["TABLE_route_ubest_nh", "ROW_route_ubest_nh"])?;

        for child in ubest_nh.children() {
            if child.tag_name().name() == "ubest_nh_addr" {
                nh_addr = Some(
                    child
                        .text()
                        .ok_or(ParseError::NoText)?
                        .parse::<Ipv4Addr>()?,
                )
            }
            if child.tag_name().name() == "ubest_nh_intf" {
                nh_iface = Some(child.text().ok_or(ParseError::NoText)?.to_string());
            }
        }

        let addr = if let Some(addr) = addr.take() {
            addr
        } else {
            return Err(ParseError::MissingXmlTag("addr"));
        };
        let masklen = if let Some(masklen) = masklen.take() {
            masklen
        } else {
            return Err(ParseError::MissingXmlTag("masklen"));
        };
        let area = if let Some(area) = area.take() {
            area
        } else {
            return Err(ParseError::MissingXmlTag("area"));
        };
        let nh_addr = if let Some(nh_addr) = nh_addr.take() {
            nh_addr
        } else {
            return Err(ParseError::MissingXmlTag("nh_addr"));
        };
        let nh_iface = if let Some(nh_iface) = nh_iface.take() {
            nh_iface
        } else {
            return Err(ParseError::MissingXmlTag("nh_iface"));
        };

        Ok(Self {
            net: Ipv4Net::new(addr, masklen)?,
            area,
            nh_addr,
            nh_iface,
        })
    }
}

/// Structure that contains informations about OSPF Neighbors. This is the parsed output of the
/// command `show ip ospf neigbors`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OspfNeighbor {
    pub id: Ipv4Addr,
    pub address: Ipv4Addr,
    pub iface: String,
}

impl OspfNeighbor {
    /// Generate a list of OSPF neighbors from the output of the command `show ip ospf neighbors`.
    pub fn from_table(table: &str) -> Result<Vec<OspfNeighbor>, ParseError> {
        // Allow a completely empty output
        if table.trim().is_empty() {
            return Ok(Vec::new());
        }

        // skip the first two lines
        if !table.trim().starts_with("OSPF Process ID") {
            log::warn!(
                "Missing `OSPF Process ID` line when parsing OSPF neighbors:\n{}",
                table
            );
            return Err(ParseError::InvalidPreamble(table.to_string()));
        }
        let next_line = table
            .find('\n')
            .ok_or_else(|| ParseError::InvalidPreamble(table.to_string()))?;
        let table = &table[(next_line + 1)..];
        if !table.trim().starts_with("Total number of neighbors: ") {
            log::warn!(
                "Missing `Total number of neighbors` line when parsing OSPF neighbors:\n{}",
                table
            );
            return Err(ParseError::InvalidPreamble(table.to_string()));
        }
        let next_line = table
            .find('\n')
            .ok_or_else(|| ParseError::InvalidPreamble(table.to_string()))?;
        let table = &table[(next_line + 1)..];

        // parse the table
        let fields = [
            "Neighbor ID",
            "Pri",
            "State",
            "Up Time",
            "Address",
            "Interface",
        ];

        let mut result = Vec::new();
        for (_, row) in parse_table(table, fields)? {
            result.push(OspfNeighbor {
                id: row[0].parse()?,
                address: row[4].parse()?,
                iface: row[5].replace("Eth", "Ethernet"),
            })
        }

        Ok(result)
    }
}

/// Decend an xml tree to find a specific node, following a path of node names.
pub(self) fn decend_xml_tree<'a, 'b, const N: usize>(
    root: roxmltree::Node<'a, 'b>,
    path: [&'static str; N],
) -> Result<roxmltree::Node<'a, 'b>, ParseError> {
    let mut node = root;
    for next_name in path {
        let mut next_node = None;
        for x in node.children() {
            if x.tag_name().name() == next_name {
                next_node = Some(x);
                break;
            }
        }
        if let Some(next_node) = next_node {
            node = next_node
        } else {
            return Err(ParseError::MissingXmlTag(next_name));
        }
    }
    Ok(node)
}
