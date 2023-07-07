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

//! # New implementation of the compiler
//!
//! The compiler follows the rules as presented in the paper.

use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};

use atomic_command::{AtomicCommand, AtomicCondition, AtomicModifier};
use bgpsim::{
    config::{
        ConfigExpr, ConfigExprKey,
        ConfigModifier::{self, *},
        RouteMapEdit,
    },
    prelude::{BgpSessionType, NetworkFormatter},
    route_map::{RouteMapBuilder, RouteMapDirection},
    types::RouterId,
};
use lazy_static::lazy_static;

use crate::{Decomposition, P};

use super::{
    bgp_dependencies::BgpDependencies,
    ilp_scheduler::{FwStateTrace, NodeSchedule, Schedule},
    CommandInfo, DecompositionError,
};

/// Type definition for a single stage
type Stage = Vec<Vec<AtomicCommand<P>>>;

/// Build the atomic decomposition of the command.
pub fn build<Q>(
    info: &CommandInfo<'_, Q>,
    bgp_deps: HashMap<P, BgpDependencies>,
    schedules: HashMap<P, (Schedule, FwStateTrace)>,
) -> Result<Decomposition, DecompositionError> {
    log::info!("Generate the final decomposition based on the schedule.");
    match info.command.key() {
        Some(ConfigExprKey::BgpRouteMap { .. }) | Some(ConfigExprKey::BgpSession { .. }) => {
            _build(info, bgp_deps, schedules)
        }
        _ => unimplemented!(),
    }
}

/// Build function with type specialization for each decomposer.
fn _build<Q>(
    info: &CommandInfo<'_, Q>,
    bgp_deps: HashMap<P, BgpDependencies>,
    schedules: HashMap<P, (Schedule, FwStateTrace)>,
) -> Result<Decomposition, DecompositionError> {
    let mut schedule = HashMap::new();
    let mut fw_state_trace = HashMap::new();
    for (p, (sched, trace)) in schedules {
        schedule.insert(p, sched);
        fw_state_trace.insert(p, trace);
    }

    let temp_sessions = get_temp_sessions(info, &schedule)?;

    let (atomic_before, atomic_after) = atomic_commands(info, &schedule, &bgp_deps)?;

    // first, build the basic structure of the composition with the setup and cleanup commands, as
    // well as with the main commands.
    let mut decomposition = Decomposition {
        original_command: info.command.clone(),
        bgp_deps: Default::default(),
        schedule: Default::default(),
        fw_state_trace,
        setup_commands: setup_commands(info, &schedule, &bgp_deps, &temp_sessions)?,
        cleanup_commands: cleanup_commands(info, &schedule, &bgp_deps, &temp_sessions)?,
        atomic_before,
        main_commands: main_command(info),
        atomic_after,
    };

    // finally, set the schedule
    decomposition.bgp_deps = bgp_deps;
    decomposition.schedule = schedule;

    batch_route_map_updates(&mut decomposition);

    log::info!(
        "Created the decomposition:\n{}",
        decomposition.fmt(info.net_before)
    );

    Ok(decomposition)
}

/// order for applying route preferences.
const TEMP_SESSION_ORDER: i16 = i16::MAX;
/// weight for applying route preferences.
const TEMP_SESSION_WEIGHT: u32 = u16::MAX as u32 - 1;
/// weight for preferred routes
const PREF_WEIGHT: u32 = u16::MAX as u32 - 2;

/// Get the order for modifying temporary bgp sessions (outgoing route-maps to specifically allow
/// routes)
fn temp_session_order(prefix: P) -> i16 {
    lazy_static! {
        static ref ASSIGNMENT: Mutex<HashMap<P, i16>> = Mutex::new(HashMap::new());
    }
    let mut ass = ASSIGNMENT.lock().unwrap();
    let next = ass.len();
    *ass.entry(prefix).or_insert(next as i16)
}

/// Get the order for modifying temporary bgp sessions (outgoing route-maps to specifically allow
/// routes)
fn pref_order(prefix: P) -> i16 {
    lazy_static! {
        static ref ASSIGNMENT: Mutex<HashMap<P, i16>> = Mutex::new(HashMap::new());
    }
    let mut ass = ASSIGNMENT.lock().unwrap();
    let next = i16::MIN + 1 + ass.len() as i16;
    *ass.entry(prefix).or_insert(next)
}

/// Get the config expr to prefer a specific route.
fn prefer_route(router: RouterId, neighbor: RouterId, prefix: P) -> ConfigExpr<P> {
    ConfigExpr::BgpRouteMap {
        router,
        neighbor,
        direction: RouteMapDirection::Incoming,
        map: RouteMapBuilder::new()
            .allow()
            .order_sgn(pref_order(prefix))
            .match_prefix(prefix)
            .set_weight(PREF_WEIGHT)
            .build(),
    }
}

/// Generate the atomic modifier to use a temporary session
fn use_temp_session(router: RouterId, egress: RouterId, prefix: P) -> AtomicModifier<P> {
    AtomicModifier::UseTempSession {
        router,
        neighbor: egress,
        prefix,
        raw: Insert(ConfigExpr::BgpRouteMap {
            router,
            neighbor: egress,
            direction: RouteMapDirection::Incoming,
            map: RouteMapBuilder::new()
                .allow()
                .order_sgn(temp_session_order(prefix))
                .match_prefix(prefix)
                .set_weight(TEMP_SESSION_WEIGHT)
                .exit()
                .build(),
        }),
    }
}

/// Generate the atomic modifier to ignore a temporary session
fn ignore_temp_session(router: RouterId, egress: RouterId, prefix: P) -> AtomicModifier<P> {
    AtomicModifier::IgnoreTempSession {
        router,
        neighbor: egress,
        prefix,
        raw: Remove(ConfigExpr::BgpRouteMap {
            router,
            neighbor: egress,
            direction: RouteMapDirection::Incoming,
            map: RouteMapBuilder::new()
                .allow()
                .order_sgn(temp_session_order(prefix))
                .match_prefix(prefix)
                .set_weight(TEMP_SESSION_WEIGHT)
                .exit()
                .build(),
        }),
    }
}

/// Get the neighbor that will announce the old route towards `router` as long as `router` selects
/// the old route.
fn old_neighbor<Q>(
    router: RouterId,
    info: &CommandInfo<'_, Q>,
    schedules: &Schedule,
    bgp_deps: &BgpDependencies,
    prefix: P,
) -> Option<RouterId> {
    bgp_deps
        .get(&router)
        .unwrap()
        .old_from
        .iter()
        .map(|n| {
            (
                *n,
                schedules.get(n).map(|s| s.old_route).unwrap_or(usize::MAX),
            )
        })
        .max_by_key(|(_, x)| *x)
        .map(|(n, _)| n)
        .or_else(|| {
            info.bgp_before
                .get(&prefix)
                .and_then(|bgp| bgp.get(router).map(|(n, _)| n))
        })
}

/// Get the neighbor that will announce the new route route towards `router` as soon as `router`
/// selects the new route.
fn new_neighbor<Q>(
    router: RouterId,
    info: &CommandInfo<'_, Q>,
    schedules: &Schedule,
    bgp_deps: &BgpDependencies,
    prefix: P,
) -> Option<RouterId> {
    bgp_deps
        .get(&router)
        .unwrap()
        .new_from
        .iter()
        .map(|n| (*n, schedules.get(n).map(|s| s.new_route).unwrap_or(0)))
        .min_by_key(|(_, x)| *x)
        .map(|(n, _)| n)
        .or_else(|| {
            info.bgp_after
                .get(&prefix)
                .and_then(|bgp| bgp.get(router).map(|(n, _)| n))
        })
}

/// Get the next-hop attribute of the old route
fn old_nh<Q>(info: &CommandInfo<'_, Q>, router: RouterId, prefix: P) -> Option<RouterId> {
    info.bgp_before
        .get(&prefix)
        .unwrap()
        .get(router)
        .map(|(_, r)| r.next_hop)
}

/// Get the next-hop attribute of the new route
fn new_nh<Q>(info: &CommandInfo<'_, Q>, router: RouterId, prefix: P) -> Option<RouterId> {
    info.bgp_after
        .get(&prefix)
        .unwrap()
        .get(router)
        .map(|(_, r)| r.next_hop)
}

/// Generate the commands for the setup stage
fn setup_commands<Q>(
    info: &CommandInfo<'_, Q>,
    schedules: &HashMap<P, Schedule>,
    bgp_deps: &HashMap<P, BgpDependencies>,
    temp_sessions: &HashSet<(RouterId, RouterId)>,
) -> Result<Stage, DecompositionError> {
    let mut cmds = Vec::new();

    // first, force the initial state
    for (p, s) in schedules {
        let deps = bgp_deps.get(p).unwrap();
        for r in s.keys() {
            if let Some(n) = old_neighbor(*r, info, s, deps, *p) {
                cmds.push(AtomicCommand {
                    command: AtomicModifier::ChangePreference {
                        router: *r,
                        prefix: *p,
                        neighbor: n,
                        raw: vec![Insert(prefer_route(*r, n, *p))],
                    },
                    precondition: AtomicCondition::None,
                    postcondition: AtomicCondition::SelectedRoute {
                        router: *r,
                        prefix: *p,
                        neighbor: Some(n),
                        weight: Some(PREF_WEIGHT),
                        next_hop: old_nh(info, *r, *p),
                    },
                })
            }
        }
    }

    // then, create the temporary sessions
    for (a, b) in temp_sessions {
        let raw = vec![
            Insert(ConfigExpr::BgpSession {
                source: *a,
                target: *b,
                session_type: BgpSessionType::IBgpPeer,
            }),
            Insert(ConfigExpr::BgpRouteMap {
                router: *a,
                neighbor: *b,
                direction: RouteMapDirection::Incoming,
                map: RouteMapBuilder::new()
                    .order_sgn(TEMP_SESSION_ORDER)
                    .deny()
                    .build(),
            }),
            Insert(ConfigExpr::BgpRouteMap {
                router: *b,
                neighbor: *a,
                direction: RouteMapDirection::Incoming,
                map: RouteMapBuilder::new()
                    .order_sgn(TEMP_SESSION_ORDER)
                    .deny()
                    .build(),
            }),
        ];
        cmds.push(AtomicCommand {
            command: AtomicModifier::AddTempSession {
                router: *a,
                neighbor: *b,
                raw,
            },
            precondition: AtomicCondition::None,
            postcondition: AtomicCondition::BgpSessionEstablished {
                router: *a,
                neighbor: *b,
            },
        });
    }

    Ok(vec![cmds])
}

/// Generate the main command for the decomposition
fn main_command<Q>(info: &CommandInfo<'_, Q>) -> Stage {
    vec![vec![AtomicCommand {
        command: AtomicModifier::Raw(info.command.clone()),
        precondition: AtomicCondition::None,
        postcondition: AtomicCondition::None,
    }]]
}

/// Generate the atomic for all prefixes
#[allow(clippy::type_complexity)]
fn atomic_commands<Q>(
    info: &CommandInfo<'_, Q>,
    schedules: &HashMap<P, Schedule>,
    bgp_deps: &HashMap<P, BgpDependencies>,
) -> Result<(HashMap<P, Stage>, HashMap<P, Stage>), DecompositionError> {
    let mut atomic_before = HashMap::new();
    let mut atomic_after = HashMap::new();
    for (p, schedule) in schedules {
        let (cmds_before, cmds_after) =
            atomic_commands_for_prefix(info, schedule, bgp_deps.get(p).unwrap(), *p)?;
        atomic_before.insert(*p, cmds_before);
        atomic_after.insert(*p, cmds_after);
    }
    Ok((atomic_before, atomic_after))
}

/// Get the round at which we must apply the command.
fn get_cmd_round(
    cmd: &ConfigModifier<P>,
    schedules: &Schedule,
    prefix: P,
) -> Result<usize, DecompositionError> {
    let mut cmd_round = None;
    for r in cmd.routers() {
        if let Some(s) = schedules.get(&r) {
            if s.old_route != s.fw_state || s.new_route != s.fw_state {
                return Err(DecompositionError::InconsistentMainCommandRound(
                    prefix,
                    "Target router has `r_old < r_new`",
                ));
            }
            if let Some(old_round) = cmd_round.replace(s.fw_state) {
                if old_round != s.fw_state {
                    return Err(DecompositionError::InconsistentMainCommandRound(
                        prefix,
                        "Target routers migrate in different rounds",
                    ));
                }
            }
        }
    }
    cmd_round.ok_or_else(|| {
        DecompositionError::InconsistentMainCommandRound(prefix, "No target router migrated!")
    })
}

/// If the command removes a session, then we need to apply that command after the router actually
/// have updated.
fn does_cmd_remove_session(cmd: &ConfigModifier<P>) -> bool {
    matches!(cmd, ConfigModifier::Remove(ConfigExpr::BgpSession { .. }))
        || matches!(
            cmd,
            ConfigModifier::Update {
                from: ConfigExpr::BgpSession {
                    session_type: BgpSessionType::IBgpClient,
                    ..
                },
                to: ConfigExpr::BgpSession {
                    session_type: BgpSessionType::IBgpPeer,
                    ..
                }
            }
        )
}

/// Generate the atomic commands for a single prefix.
fn atomic_commands_for_prefix<Q>(
    info: &CommandInfo<'_, Q>,
    schedules: &Schedule,
    bgp_deps: &BgpDependencies,
    prefix: P,
) -> Result<(Stage, Stage), DecompositionError> {
    // check if the schedule is non-empty
    if schedules.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    let cmd_round = get_cmd_round(&info.command, schedules, prefix)?;
    let num_rounds = schedules.values().map(|s| s.new_route).max().unwrap_or(0) + 1;
    let mut stage: Stage = (0..num_rounds).map(|_| Vec::new()).collect();

    // build the stage according to our rules
    for (r, s) in schedules {
        if s.old_route == s.fw_state && s.fw_state == s.new_route {
            apply_rule_1(&mut stage, *r, info, schedules, bgp_deps, prefix)
        } else if s.old_route < s.fw_state && s.fw_state == s.new_route {
            apply_rule_2(&mut stage, *r, info, schedules, bgp_deps, prefix)
        } else if s.old_route == s.fw_state && s.fw_state < s.new_route {
            apply_rule_3(&mut stage, *r, info, schedules, bgp_deps, prefix)
        } else if s.old_route < s.fw_state && s.fw_state < s.new_route {
            apply_rule_4(&mut stage, *r, info, schedules, bgp_deps, prefix)
        }
    }

    // now split the stage at cmd_round and return
    let stage_after = if does_cmd_remove_session(&info.command) {
        stage.split_off(cmd_round + 1)
    } else {
        stage.split_off(cmd_round)
    };
    Ok((stage, stage_after))
}

/// Compilation rule when `r_old == r_fw == r_new`
fn apply_rule_1<Q>(
    stage: &mut Stage,
    router: RouterId,
    info: &CommandInfo<'_, Q>,
    schedules: &Schedule,
    bgp_deps: &BgpDependencies,
    prefix: P,
) {
    let s = schedules.get(&router).unwrap();
    let old_n = old_neighbor(router, info, schedules, bgp_deps, prefix);
    let new_n = new_neighbor(router, info, schedules, bgp_deps, prefix);
    let precondition = AtomicCondition::AvailableRoute {
        router,
        prefix,
        neighbor: new_n,
        weight: None,
        next_hop: new_nh(info, router, prefix),
    };
    let postcondition = AtomicCondition::SelectedRoute {
        router,
        prefix,
        neighbor: new_n,
        weight: Some(PREF_WEIGHT),
        next_hop: new_nh(info, router, prefix),
    };
    // changing the preference must be done in any case:
    match (old_n, new_n) {
        (Some(old_n), Some(new_n)) if old_n != new_n => stage[s.fw_state].push(AtomicCommand {
            command: AtomicModifier::ChangePreference {
                router,
                prefix,
                neighbor: new_n,
                raw: vec![
                    Remove(prefer_route(router, old_n, prefix)),
                    Insert(prefer_route(router, new_n, prefix)),
                ],
            },
            precondition,
            postcondition,
        }),
        // Change from the old route to a black hole.
        // TODO: This must still be implemented, as we would introduce a static route to drop
        // packets.
        (Some(_old_n), None) => todo!("no new neighbor: {:?}", bgp_deps.get(&router)),
        // Change from a black hole to a new route
        (None, Some(new_n)) => stage[s.fw_state].push(AtomicCommand {
            command: AtomicModifier::ChangePreference {
                router,
                prefix,
                neighbor: new_n,
                raw: vec![Insert(prefer_route(router, new_n, prefix))],
            },
            precondition,
            postcondition,
        }),
        _ => {}
    };
}

/// Compilation rule when `r_old < r_fw == r_new`.
fn apply_rule_2<Q>(
    stage: &mut Stage,
    router: RouterId,
    info: &CommandInfo<'_, Q>,
    schedules: &Schedule,
    bgp_deps: &BgpDependencies,
    prefix: P,
) {
    let s = schedules.get(&router).unwrap();
    let old_n = old_neighbor(router, info, schedules, bgp_deps, prefix).unwrap();
    let new_n = new_neighbor(router, info, schedules, bgp_deps, prefix).unwrap();
    let old_egress = info
        .bgp_before
        .get(&prefix)
        .unwrap()
        .ingress_session(router)
        .unwrap()
        .1;

    // In round r_old, use the temporary session by making the old egress advertise its route over
    // the temporary session.
    stage[s.old_route].push(AtomicCommand {
        command: use_temp_session(router, old_egress, prefix),
        precondition: AtomicCondition::None,
        postcondition: AtomicCondition::SelectedRoute {
            router,
            prefix,
            neighbor: Some(old_egress),
            weight: Some(TEMP_SESSION_WEIGHT),
            next_hop: Some(old_egress),
        },
    });

    // After selecting the route from the temp session, push the change in routing decision. Leave
    // the postcondition empty, as this route will only be picked in the r_fw.
    stage[s.old_route].push(AtomicCommand {
        command: AtomicModifier::ChangePreference {
            router,
            prefix,
            neighbor: new_n,
            raw: vec![
                Remove(prefer_route(router, old_n, prefix)),
                Insert(prefer_route(router, new_n, prefix)),
            ],
        },
        precondition: AtomicCondition::SelectedRoute {
            router,
            prefix,
            neighbor: Some(old_egress),
            weight: Some(TEMP_SESSION_WEIGHT),
            next_hop: Some(old_egress),
        },
        postcondition: AtomicCondition::None,
    });

    // in r_fw, remove the temporary bgp session, but only when the new route with increased weight
    // is present (that was changed in fw_old)
    stage[s.fw_state].push(AtomicCommand {
        command: ignore_temp_session(router, old_egress, prefix),
        precondition: AtomicCondition::AvailableRoute {
            router,
            prefix,
            neighbor: Some(new_n),
            weight: Some(PREF_WEIGHT),
            next_hop: new_nh(info, router, prefix),
        },
        postcondition: AtomicCondition::SelectedRoute {
            router,
            prefix,
            neighbor: Some(new_n),
            weight: Some(PREF_WEIGHT),
            next_hop: new_nh(info, router, prefix),
        },
    });
}

/// Compilation rule when `r_old == r_fw < r_new`
fn apply_rule_3<Q>(
    stage: &mut Stage,
    router: RouterId,
    info: &CommandInfo<'_, Q>,
    schedules: &Schedule,
    bgp_deps: &BgpDependencies,
    prefix: P,
) {
    let s = schedules.get(&router).unwrap();
    let old_n = old_neighbor(router, info, schedules, bgp_deps, prefix).unwrap();
    let new_n = new_neighbor(router, info, schedules, bgp_deps, prefix).unwrap();
    let new_egress = info
        .bgp_after
        .get(&prefix)
        .unwrap()
        .ingress_session(router)
        .unwrap()
        .1;

    // In round r_fw, use the temporary bgp session.
    stage[s.fw_state].push(AtomicCommand {
        command: use_temp_session(router, new_egress, prefix),
        precondition: AtomicCondition::None,
        postcondition: AtomicCondition::SelectedRoute {
            router,
            prefix,
            neighbor: Some(new_egress),
            weight: Some(TEMP_SESSION_WEIGHT),
            next_hop: Some(new_egress),
        },
    });

    // after using the temporary session, at r_fw, push the change in routing decision. Leave the
    // postcondition empty, as the new route will only be selected in r_new.
    stage[s.fw_state].push(AtomicCommand {
        command: AtomicModifier::ChangePreference {
            router,
            prefix,
            neighbor: new_n,
            raw: vec![
                Remove(prefer_route(router, old_n, prefix)),
                Insert(prefer_route(router, new_n, prefix)),
            ],
        },
        precondition: AtomicCondition::SelectedRoute {
            router,
            prefix,
            neighbor: Some(new_egress),
            weight: Some(TEMP_SESSION_WEIGHT),
            next_hop: Some(new_egress),
        },
        postcondition: AtomicCondition::None,
    });

    // in r_new, also remove the temporary bgp session, but only when the new route has an increased
    // weight. Then, check that the new route is selected.
    stage[s.new_route].push(AtomicCommand {
        command: ignore_temp_session(router, new_egress, prefix),
        precondition: AtomicCondition::AvailableRoute {
            router,
            prefix,
            neighbor: Some(new_n),
            weight: Some(PREF_WEIGHT),
            next_hop: new_nh(info, router, prefix),
        },
        postcondition: AtomicCondition::SelectedRoute {
            router,
            prefix,
            neighbor: Some(new_n),
            weight: Some(PREF_WEIGHT),
            next_hop: new_nh(info, router, prefix),
        },
    });
}

/// Compilation rule when `r_old < r_fw < r_new`
fn apply_rule_4<Q>(
    stage: &mut Stage,
    router: RouterId,
    info: &CommandInfo<'_, Q>,
    schedules: &Schedule,
    bgp_deps: &BgpDependencies,
    prefix: P,
) {
    let old_egress = info
        .bgp_before
        .get(&prefix)
        .unwrap()
        .ingress_session(router)
        .unwrap()
        .1;
    let new_egress = info
        .bgp_after
        .get(&prefix)
        .unwrap()
        .ingress_session(router)
        .unwrap()
        .1;

    if old_egress == new_egress {
        return apply_rule_4_same_egress(stage, router, info, schedules, bgp_deps, prefix);
    }

    let s = schedules.get(&router).unwrap();
    let old_n = old_neighbor(router, info, schedules, bgp_deps, prefix).unwrap();
    let new_n = new_neighbor(router, info, schedules, bgp_deps, prefix).unwrap();
    // In round r_old, use the old egress via the temporary bgp session
    stage[s.old_route].push(AtomicCommand {
        command: use_temp_session(router, old_egress, prefix),
        precondition: AtomicCondition::None,
        postcondition: AtomicCondition::SelectedRoute {
            router,
            prefix,
            neighbor: Some(old_egress),
            weight: Some(TEMP_SESSION_WEIGHT),
            next_hop: Some(old_egress),
        },
    });

    // Also, at r_old, after selecting the temporary session, already prefer the new route.
    stage[s.old_route].push(AtomicCommand {
        command: AtomicModifier::ChangePreference {
            router,
            prefix,
            neighbor: new_n,
            raw: vec![
                Remove(prefer_route(router, old_n, prefix)),
                Insert(prefer_route(router, new_n, prefix)),
            ],
        },
        precondition: AtomicCondition::SelectedRoute {
            router,
            prefix,
            neighbor: Some(old_egress),
            weight: Some(TEMP_SESSION_WEIGHT),
            next_hop: Some(old_egress),
        },
        postcondition: AtomicCondition::None,
    });

    // then, at r_fw, switch over to the new temporary bgp session. For that, first use the
    // temporary session. then remove the old one as soon as the router sees a route for the new
    // one.
    stage[s.fw_state].push(AtomicCommand {
        command: use_temp_session(router, new_egress, prefix),
        precondition: AtomicCondition::None,
        postcondition: AtomicCondition::SelectedRoute {
            router,
            prefix,
            neighbor: Some(new_egress),
            weight: Some(TEMP_SESSION_WEIGHT),
            next_hop: Some(new_egress),
        },
    });
    stage[s.fw_state].push(AtomicCommand {
        command: ignore_temp_session(router, old_egress, prefix),
        precondition: AtomicCondition::AvailableRoute {
            router,
            prefix,
            neighbor: Some(new_egress),
            weight: Some(TEMP_SESSION_WEIGHT),
            next_hop: Some(new_egress),
        },
        postcondition: AtomicCondition::SelectedRoute {
            router,
            prefix,
            neighbor: Some(new_egress),
            weight: Some(TEMP_SESSION_WEIGHT),
            next_hop: Some(new_egress),
        },
    });

    // in r_fw, also remove the temporary bgp session, but only when the new route has an increased
    // weight. Then, check that the new route is selected.
    stage[s.new_route].push(AtomicCommand {
        command: ignore_temp_session(router, new_egress, prefix),
        precondition: AtomicCondition::AvailableRoute {
            router,
            prefix,
            neighbor: Some(new_n),
            weight: Some(PREF_WEIGHT),
            next_hop: new_nh(info, router, prefix),
        },
        postcondition: AtomicCondition::SelectedRoute {
            router,
            prefix,
            neighbor: Some(new_n),
            weight: Some(PREF_WEIGHT),
            next_hop: new_nh(info, router, prefix),
        },
    });
}

/// Compilation rule when `r_old < r_fw < r_new`
fn apply_rule_4_same_egress<Q>(
    stage: &mut Stage,
    router: RouterId,
    info: &CommandInfo<'_, Q>,
    schedules: &Schedule,
    bgp_deps: &BgpDependencies,
    prefix: P,
) {
    let s = schedules.get(&router).unwrap();
    let old_n = old_neighbor(router, info, schedules, bgp_deps, prefix).unwrap();
    let new_n = new_neighbor(router, info, schedules, bgp_deps, prefix).unwrap();
    let egress = info
        .bgp_after
        .get(&prefix)
        .unwrap()
        .ingress_session(router)
        .unwrap()
        .1;

    // In round r_old, use the egress via temporary bgp session
    stage[s.old_route].push(AtomicCommand {
        command: use_temp_session(router, egress, prefix),
        precondition: AtomicCondition::None,
        postcondition: AtomicCondition::SelectedRoute {
            router,
            prefix,
            neighbor: Some(egress),
            weight: Some(TEMP_SESSION_WEIGHT),
            next_hop: Some(egress),
        },
    });

    // Further, at r_old, change the preference which should only take affect in r_new.
    stage[s.old_route].push(AtomicCommand {
        command: AtomicModifier::ChangePreference {
            router,
            prefix,
            neighbor: new_n,
            raw: vec![
                Remove(prefer_route(router, old_n, prefix)),
                Insert(prefer_route(router, new_n, prefix)),
            ],
        },
        precondition: AtomicCondition::SelectedRoute {
            router,
            prefix,
            neighbor: Some(egress),
            weight: Some(TEMP_SESSION_WEIGHT),
            next_hop: Some(egress),
        },
        postcondition: AtomicCondition::None,
    });

    // then, at r_fw, do nothing.

    // in r_fw, also remove the temporary bgp session, but only when the new route has an increased
    // weight. Then, check that the new route is selected.
    stage[s.new_route].push(AtomicCommand {
        command: ignore_temp_session(router, egress, prefix),
        precondition: AtomicCondition::AvailableRoute {
            router,
            prefix,
            neighbor: Some(new_n),
            weight: Some(PREF_WEIGHT),
            next_hop: old_nh(info, router, prefix),
        },
        postcondition: AtomicCondition::SelectedRoute {
            router,
            prefix,
            neighbor: Some(new_n),
            weight: Some(PREF_WEIGHT),
            next_hop: old_nh(info, router, prefix),
        },
    });
}

/// Generate the commands for the setup stage
fn cleanup_commands<Q>(
    info: &CommandInfo<'_, Q>,
    schedules: &HashMap<P, Schedule>,
    bgp_deps: &HashMap<P, BgpDependencies>,
    temp_sessions: &HashSet<(RouterId, RouterId)>,
) -> Result<Stage, DecompositionError> {
    let mut cmds = Vec::new();

    // first, remove the weight in the final state.
    for (p, s) in schedules {
        let deps = bgp_deps.get(p).unwrap();
        for r in s.keys() {
            let precondition = if let Some(route) = info
                .net_after
                .get_device(*r)
                .internal_or_err()?
                .get_selected_bgp_route(*p)
            {
                let mut good_neighbors = deps.get(r).unwrap().new_from.clone();
                good_neighbors.extend(new_neighbor(*r, info, s, deps, *p));
                AtomicCondition::RoutesLessPreferred {
                    router: *r,
                    prefix: *p,
                    good_neighbors,
                    route: route.clone(),
                }
            } else {
                AtomicCondition::None
            };
            if let Some(n) = new_neighbor(*r, info, s, deps, *p) {
                cmds.push(AtomicCommand {
                    command: AtomicModifier::ClearPreference {
                        router: *r,
                        prefix: *p,
                        raw: vec![Remove(prefer_route(*r, n, *p))],
                    },
                    precondition,
                    postcondition: AtomicCondition::None,
                })
            }
        }
    }

    // then, remove the temporary sessions
    for (a, b) in temp_sessions {
        let raw = vec![
            Remove(ConfigExpr::BgpSession {
                source: *a,
                target: *b,
                session_type: BgpSessionType::IBgpPeer,
            }),
            Remove(ConfigExpr::BgpRouteMap {
                router: *a,
                neighbor: *b,
                direction: RouteMapDirection::Incoming,
                map: RouteMapBuilder::new()
                    .order_sgn(TEMP_SESSION_ORDER)
                    .deny()
                    .build(),
            }),
            Remove(ConfigExpr::BgpRouteMap {
                router: *b,
                neighbor: *a,
                direction: RouteMapDirection::Incoming,
                map: RouteMapBuilder::new()
                    .order_sgn(TEMP_SESSION_ORDER)
                    .deny()
                    .build(),
            }),
        ];
        cmds.push(AtomicCommand {
            command: AtomicModifier::RemoveTempSession {
                router: *a,
                neighbor: *b,
                raw,
            },
            precondition: AtomicCondition::None,
            postcondition: AtomicCondition::None,
        });
    }

    Ok(vec![cmds])
}

/// Compute the set of all necessary static routes during the migration.
///
/// The returned set of static routes are to be read as follows: This is a mapping from two routers
/// `(a, b)`, where a BGP session from `a` to `b` should be added. This pair `(a, b) -> (p_a_b,
/// p_b_a)` is mapped to two list of prefixes: the first one `p_a_b` describes the prefixes for
/// which `a` eventually uses router `b` as a static route target, while `p_b_a` describes those
/// for which router `b` will set-up a static route via `a`.
fn get_temp_sessions<Q>(
    info: &CommandInfo<'_, Q>,
    schedules: &HashMap<P, HashMap<RouterId, NodeSchedule>>,
) -> Result<HashSet<(RouterId, RouterId)>, DecompositionError> {
    /// get a key for session between a and b, by ordering a and b.
    fn key(a: RouterId, b: RouterId) -> (RouterId, RouterId) {
        if a < b {
            (a, b)
        } else {
            (b, a)
        }
    }

    let mut sessions = HashSet::new();

    // generate the sessions
    for (p, s) in schedules {
        let bgp_before = info.bgp_before.get(p).unwrap();
        let bgp_after = info.bgp_after.get(p).unwrap();
        for (r, schedule) in s {
            if schedule.old_route < schedule.fw_state {
                if let Some((_, e)) = bgp_before.ingress_session(*r) {
                    sessions.insert(key(*r, e));
                }
            }
            if schedule.fw_state < schedule.new_route {
                if let Some((_, e)) = bgp_after.ingress_session(*r) {
                    sessions.insert(key(*r, e));
                }
            }
        }
    }

    // check all sessions
    for (a, b) in sessions.iter() {
        if info
            .net_before
            .get_device(*a)
            .internal_or_err()?
            .get_bgp_session_type(*b)
            .is_some()
            || info
                .net_after
                .get_device(*a)
                .internal_or_err()?
                .get_bgp_session_type(*b)
                .is_some()
        {
            return Err(DecompositionError::TemporaryBgpSession(*a, *b));
        }
    }

    Ok(sessions)
}

/// Batch together all similar route-map updates of all commands in the decomposition
fn batch_route_map_updates(decomp: &mut Decomposition) {
    batch_route_map_updates_of_stage(&mut decomp.setup_commands);
    batch_route_map_updates_of_stage(&mut decomp.main_commands);
    batch_route_map_updates_of_stage(&mut decomp.cleanup_commands);
    decomp
        .atomic_before
        .values_mut()
        .for_each(batch_route_map_updates_of_stage);
    decomp
        .atomic_after
        .values_mut()
        .for_each(batch_route_map_updates_of_stage);
}

/// Batch together all similar route-map updates of all commands in the decomposition
#[allow(clippy::ptr_arg)]
fn batch_route_map_updates_of_stage(stage: &mut Vec<Vec<AtomicCommand<P>>>) {
    for step in stage.iter_mut() {
        for cmd in step.iter_mut() {
            match &mut cmd.command {
                AtomicModifier::Raw(_)
                | AtomicModifier::IgnoreTempSession { .. }
                | AtomicModifier::UseTempSession { .. } => {}
                AtomicModifier::AddTempSession { raw, .. }
                | AtomicModifier::RemoveTempSession { raw, .. }
                | AtomicModifier::ChangePreference { raw, .. }
                | AtomicModifier::ClearPreference { raw, .. } => {
                    *raw = batch_route_map_updates_of_commands(raw.clone());
                }
            }
        }
    }
}

/// Batch together all similar route-map updates of all commands in the decomposition
fn batch_route_map_updates_of_commands(cmds: Vec<ConfigModifier<P>>) -> Vec<ConfigModifier<P>> {
    /// key for matching on modifying the same route-map
    type Key = (RouterId, RouteMapDirection, i16);
    let mut route_map_updates: HashMap<RouterId, HashMap<Key, RouteMapEdit<P>>> = HashMap::new();
    let mut result = Vec::new();

    for cmd in cmds {
        match cmd {
            ConfigModifier::Insert(ConfigExpr::BgpRouteMap {
                router,
                neighbor,
                direction,
                map,
            }) => match route_map_updates
                .entry(router)
                .or_default()
                .entry((neighbor, direction, map.order))
            {
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(RouteMapEdit {
                        neighbor,
                        direction,
                        old: None,
                        new: Some(map),
                    });
                }
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    let v = e.get_mut();
                    assert!(v.new.is_none());
                    v.new = Some(map)
                }
            },
            ConfigModifier::Remove(ConfigExpr::BgpRouteMap {
                router,
                neighbor,
                direction,
                map,
            }) => match route_map_updates
                .entry(router)
                .or_default()
                .entry((neighbor, direction, map.order))
            {
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(RouteMapEdit {
                        neighbor,
                        direction,
                        old: Some(map),
                        new: None,
                    });
                }
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    let v = e.get_mut();
                    assert!(v.old.is_none());
                    v.old = Some(map)
                }
            },
            ConfigModifier::Update {
                from: ConfigExpr::BgpRouteMap { map: old_map, .. },
                to:
                    ConfigExpr::BgpRouteMap {
                        router,
                        neighbor,
                        direction,
                        map: new_map,
                    },
            } => match route_map_updates.entry(router).or_default().entry((
                neighbor,
                direction,
                new_map.order,
            )) {
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(RouteMapEdit {
                        neighbor,
                        direction,
                        old: Some(old_map),
                        new: Some(new_map),
                    });
                }
                std::collections::hash_map::Entry::Occupied(_) => {
                    panic!("Cannot modify the same route map twice")
                }
            },
            cmd => {
                result.push(cmd);
            }
        }
    }
    for (router, updates) in route_map_updates {
        result.push(ConfigModifier::BatchRouteMapEdit {
            router,
            updates: updates.into_values().collect(),
        });
    }

    result
}
