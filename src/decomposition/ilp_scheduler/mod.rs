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

//! Scheduler using ILP that does satisfy all fw properties during convergence, while minimizing
//! soft dependencies.

use std::{
    collections::{HashMap, HashSet},
    iter::repeat_with,
    ops::Range,
    time::{Duration, Instant},
};

use bgpsim::{forwarding_state::ForwardingState, prelude::*};
use good_lp::{
    constraint,
    solvers::coin_cbc::{coin_cbc as create_solver, CoinCbcProblem},
    variable, ProblemVariables, ResolutionError, Solution, SolverModel, Variable,
};
use itertools::Itertools;
use log::info;

use super::{bgp_dependencies::BgpDependencies, CommandInfo};
use crate::{
    specification::{Checker, Property},
    P,
};

mod bgp_cost;
mod conditions;
mod has_changed;
#[cfg(feature = "explicit-loop-checker")]
mod loop_protection;
mod or_tools;

use bgp_cost::*;
use conditions::*;
use has_changed::*;
#[cfg(feature = "explicit-loop-checker")]
use loop_protection::*;

/// The schedule of an individual node, storing when it will change its forwarding, up to when it
/// will know the old route, and from when it will know the new route.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct NodeSchedule {
    /// Round in which the router must change its next-hop
    pub fw_state: usize,
    /// Round, up to when it will see (and select) the old route
    pub old_route: usize,
    /// Round, from when it will see (and select) the new route
    pub new_route: usize,
}

impl NodeSchedule {
    /// Compute the cost for this scheudle, i.e., the number of temporary BGP sessions needed.
    pub fn cost(&self) -> usize {
        (if self.old_route == self.fw_state {
            0
        } else {
            1
        }) + (if self.new_route == self.fw_state {
            0
        } else {
            1
        })
    }
}

impl std::fmt::Debug for NodeSchedule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("NodeSchedule")
            .field(&self.old_route)
            .field(&self.fw_state)
            .field(&self.new_route)
            .finish()
    }
}

impl std::fmt::Display for NodeSchedule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} <= {} <= {}",
            self.old_route, self.fw_state, self.new_route
        )
    }
}

/// Schedule, that is, a mapping from each router to its schedule.
pub type Schedule = HashMap<RouterId, NodeSchedule>;
/// The forwarding state trace, that is, a sequence of forwarding state changes.
pub type FwStateTrace = Vec<HashSet<(RouterId, Vec<RouterId>)>>;

/// Find the optimal schedule for a given prefix. We are using the maximal number of steps here.
pub fn schedule<Q>(
    info: &CommandInfo<'_, Q>,
    bgp_deps: &HashMap<P, BgpDependencies>,
    prefix: P,
) -> Result<(Schedule, FwStateTrace), ResolutionError> {
    // let max_steps: usize = info.fw_diff.get(&prefix).map(|x| x.len()).unwrap_or(0);
    // schedule_with_max_steps(info, bgp_deps, prefix, max_steps, None).0
    schedule_smart(
        info,
        bgp_deps,
        prefix,
        Duration::from_secs(24 * 60 * 60),
        usize::MAX,
    )
    .0
}

/// Structure containing the resulting problem size before solving.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ProblemSize {
    /// Number of constraints (equations)
    pub rows: usize,
    /// Number of variablee
    pub cols: usize,
    /// Number of rounds (steps)
    pub steps: usize,
}

impl std::fmt::Display for ProblemSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}({}x{})", self.steps, self.rows, self.cols)
    }
}

/// Find the optimal schedule for a given prefix in a smart way. We increase the number of steps
/// until either we use less than the allowed number of temporary sessions, or we exceed the time
/// budget.
pub fn schedule_smart<Q>(
    info: &CommandInfo<'_, Q>,
    bgp_deps: &HashMap<P, BgpDependencies>,
    prefix: P,
    time_budget: Duration,
    allowed_temp_sessions: usize,
) -> (
    Result<(Schedule, FwStateTrace), ResolutionError>,
    ProblemSize,
) {
    let max_steps: usize = info.fw_diff.get(&prefix).map(|x| x.len()).unwrap_or(0);
    if max_steps == 0 {
        return schedule_with_max_steps(info, bgp_deps, prefix, max_steps, None);
    }

    let mut largest_size = Default::default();
    let start_time = Instant::now();
    let deadline = start_time + time_budget;

    for num_steps in 1..=max_steps {
        let remaining_budget = deadline.duration_since(Instant::now());
        log::info!("Solving model with {num_steps}/{max_steps} steps");
        let (result, size) =
            schedule_with_max_steps(info, bgp_deps, prefix, num_steps, Some(remaining_budget));
        match result {
            Ok(x) => {
                log::info!("Found a solution!");
                // compute the cost
                let cost: usize = x.0.values().map(NodeSchedule::cost).sum();
                if cost <= allowed_temp_sessions {
                    // Found an acceptable solution!
                    log::info!(
                        "Found a solution with {num_steps} steps and {cost} temporary sessions after {}s",
                        start_time.elapsed().as_secs_f64()
                    );
                    return (Ok(x), size);
                }
            }
            Err(_) if Instant::now() >= deadline => {
                // we reached our deadline! return the last solution
                return (
                    Err(ResolutionError::Str(format!(
                        "Time budget is not large enough! Explored {}/{max_steps} steps",
                        num_steps - 1
                    ))),
                    size,
                );
            }
            Err(_) => {
                // could not find a solution yet. Simply retry.
                log::info!("No solutoin yet! try with more steps.");
            }
        }
        largest_size = size;
    }
    (Err(ResolutionError::Infeasible), largest_size)
}

/// Find the optimal schedule for a given prefix
pub fn schedule_with_max_steps<Q>(
    info: &CommandInfo<'_, Q>,
    bgp_deps: &HashMap<P, BgpDependencies>,
    prefix: P,
    num_steps: usize,
    timeout: Option<Duration>,
) -> (
    Result<(Schedule, FwStateTrace), ResolutionError>,
    ProblemSize,
) {
    // check if the update is empty
    info!("Prepare the ILP problem to schedule {}", prefix);
    if bgp_deps.get(&prefix).map(|x| x.is_empty()).unwrap_or(true)
        && info
            .fw_diff
            .get(&prefix)
            .map(|x| x.is_empty())
            .unwrap_or(true)
    {
        return (Ok(Default::default()), Default::default());
    }

    // create the variables
    let (problem, vars) = setup_vars(info, bgp_deps.get(&prefix), prefix, num_steps);

    // create the coin_cbc problem
    let mut problem = create_solver(problem.minimise(vars.cost));

    // disable logging during tests
    #[cfg(any(test, feature = "hide-cbc-output"))]
    {
        problem.set_parameter("logLevel", "0");
    }

    #[cfg(feature = "cbc-parallel")]
    problem.set_parameter("threads", &format!("{}", num_cpus::get()));

    if let Some(t) = timeout {
        problem.set_parameter("seconds", &t.as_secs().to_string());
    }

    // create all constraints
    setup_constraints(&mut problem, &vars, info, bgp_deps.get(&prefix), prefix);

    let model = problem.as_inner();
    let size = ProblemSize {
        cols: model.num_cols() as usize,
        rows: model.num_rows() as usize,
        steps: num_steps,
    };

    // solve the problem
    info!("Solving the ILP model...");
    let solution = match problem.solve() {
        Ok(s) => s,
        Err(e) => return (Err(e), size),
    };

    // validate the solution
    info!("Found a solution! Validating the solution...");
    validate_solution(&vars, &solution);
    let fw_state_trace = check_properties(info, &vars, &solution, prefix);

    // build the schedule
    let schedule = vars
        .r
        .keys()
        .map(|r_id| {
            (
                *r_id,
                NodeSchedule {
                    fw_state: solution.value(vars.r[r_id]).round() as usize,
                    old_route: solution.value(vars.r_old[r_id]).round() as usize,
                    new_route: solution.value(vars.r_new[r_id]).round() as usize,
                },
            )
        })
        .collect();

    (Ok((schedule, fw_state_trace)), size)
}

/// Setup all variables needed for the ILP thing to work.
fn setup_vars<Q>(
    info: &CommandInfo<'_, Q>,
    bgp_deps: Option<&BgpDependencies>,
    prefix: P,
    max_steps: usize,
) -> (ProblemVariables, IlpVars) {
    // create the problem
    let mut problem = ProblemVariables::new();
    let p = &mut problem;

    // get the set of all routers that will change eventually.
    let nodes: HashSet<RouterId> = info
        .fw_diff
        .get(&prefix)
        .iter()
        .flat_map(|d| d.keys())
        .chain(bgp_deps.into_iter().flat_map(|d| d.keys()))
        .copied()
        .collect();

    let all_nodes: HashSet<RouterId> = info.net_before.get_topology().node_indices().collect();

    // count the number of variables, i.e., the maximum number of steps
    let max_f = max_steps as f64;

    let (c, s) = spec_variables(p, info, &all_nodes, max_steps, prefix);

    // Create a variable that tracks the maximum round.
    let vars = IlpVars {
        max_steps,
        max_steps_v: p.add(variable().integer().min(0).max(max_f - 1.0)),
        cost: p.add(variable().integer().min(0)),
        session_needed: session_needed_variables(p, &nodes),
        r: round_variables(p, &nodes, max_f),
        r_old: round_variables(p, &nodes, max_f),
        r_new: round_variables(p, &nodes, max_f),
        b: has_changed_variables(p, &nodes, max_steps),
        n: has_changed_variables(p, &nodes, max_steps),
        p: has_changed_path_variables(p, &all_nodes, max_steps),
        c,
        s,
        min_max: min_max_variables(p, bgp_deps, max_steps),
        #[cfg(feature = "explicit-loop-checker")]
        loop_protection: loop_protection_variables(p, &all_nodes, max_steps),
    };

    (problem, vars)
}

/// create the boolean variables to express wether a temporary bgp session is needed in either the
/// initial or final state.
fn session_needed_variables(
    p: &mut ProblemVariables,
    nodes: &HashSet<RouterId>,
) -> HashMap<RouterId, (Variable, Variable)> {
    nodes
        .iter()
        .copied()
        .zip(repeat_with(|| {
            (p.add(variable().binary()), p.add(variable().binary()))
        }))
        .collect()
}

/// Create all round variables, used for `r`, as well as `r_old` and `r_new`.
fn round_variables(
    p: &mut ProblemVariables,
    nodes: &HashSet<RouterId>,
    max_f: f64,
) -> HashMap<RouterId, Variable> {
    nodes
        .iter()
        .copied()
        .zip(repeat_with(|| {
            p.add(variable().integer().min(0).max(max_f - 1.0))
        }))
        .collect()
}

/// Setup all constraints needed for the problem.
fn setup_constraints<Q>(
    problem: &mut CoinCbcProblem,
    vars: &IlpVars,
    info: &CommandInfo<'_, Q>,
    bgp_deps: Option<&BgpDependencies>,
    prefix: P,
) {
    // setup the cost constraint
    let mut rows = problem.as_inner().num_rows();
    log::debug!("{rows} equations before start");

    setup_cost_constraints(problem, vars);

    let new_rows = problem.as_inner().num_rows();
    let delta = new_rows - rows;
    rows = new_rows;
    log::debug!("{delta} equations for `setup_cost_constraints`");

    // setup the bgp propagation constraints
    bgp_propagation_constraints(problem, vars, bgp_deps);

    let new_rows = problem.as_inner().num_rows();
    let delta = new_rows - rows;
    rows = new_rows;
    log::debug!("{delta} equations for `bgp_propagation_constraints`");

    // create constraints for all minimum and maximum variables
    min_max_deps_constraints(problem, vars);

    let new_rows = problem.as_inner().num_rows();
    let delta = new_rows - rows;
    rows = new_rows;
    log::debug!("{delta} equations for `min_max_deps_constraints`");

    // create all conditions for `vars.changed`.
    has_changed_constraints(problem, vars);

    let new_rows = problem.as_inner().num_rows();
    let delta = new_rows - rows;
    rows = new_rows;
    log::debug!("{delta} equations for `has_changed_constraints`");

    // setup all conditions for `vars.changed_step` and `vars.changed_step_ptah`.
    has_changed_path_constraints(problem, vars, info, prefix);

    let new_rows = problem.as_inner().num_rows();
    let delta = new_rows - rows;
    rows = new_rows;
    log::debug!("{delta} equations for `has_changed_path_constraints`");

    // create all constraints for the forwarding policies and the conditions
    prop_constraints(problem, vars, info, prefix);

    let new_rows = problem.as_inner().num_rows();
    let delta = new_rows - rows;
    rows = new_rows;
    log::debug!("{delta} equations for `prop_constraints`");

    // create all constraints to satisfy all forwarding policies at every step.
    spec_constraints(problem, vars, info, prefix);

    let new_rows = problem.as_inner().num_rows();
    let delta = new_rows - rows;
    rows = new_rows;
    log::debug!("{delta} equations for `spec_constraints`");

    // create all constraints for loop protection
    #[cfg(feature = "explicit-loop-checker")]
    {
        loop_protection_constraints(problem, vars, info, prefix);
        let new_rows = problem.as_inner().num_rows();
        let delta = new_rows - rows;
        rows = new_rows;
        log::debug!("{delta} equations for `loop_protection_constraints`");
    }

    // create the temporary BGP session constraints such that a router can only make a static route
    // (which means using the route from the temporary session) if the router on the border has
    // already chosen the old or new route.
    temp_bgp_sessions_constraints(problem, vars, info, prefix);

    let new_rows = problem.as_inner().num_rows();
    let delta = new_rows - rows;
    rows = new_rows;
    log::debug!("{delta} equations for `temp_bgp_session_constraints`");

    log::debug!("{rows} total equations");
}

/// Setup the cost function constraints
fn setup_cost_constraints(problem: &mut impl SolverModel, vars: &IlpVars) {
    // add the constraints to make max_steps_v be the biggest of all rounds used..
    for a in vars.r.values() {
        problem.add_constraint(constraint!(*a <= vars.max_steps_v));
    }

    // add constraints to build the temporary sessions needed variables.
    temp_session_needed_constraints(problem, vars);

    // compute the value for the cost
    let bgp_cost = bgp_cost_expression(vars);

    // add the constraint by weighten the bgp cost twice, and the number of steps only once.
    problem.add_constraint(constraint!(vars.cost == vars.max_steps_v + 2 * bgp_cost));
}

/// Require the two following constraints for each router:
///
/// - If the router in the initial state is not a border router, and if its initial egress router is
///   no longer an egress router in the final state, make sure that the router must change its
///   forwarding **before** the egress router changes its forwarding.
/// - If the router in the final state is not a border router, and if its final egress router is not
///   an egress router in the initial state, make sure that the router must have changed its
///   forwarding **after** the egress router has changed its forwarding.
fn temp_bgp_sessions_constraints<Q>(
    problem: &mut impl SolverModel,
    vars: &IlpVars,
    info: &CommandInfo<'_, Q>,
    prefix: P,
) {
    /// If the router in the initial state is not a border router, and if its initial egress router
    /// is no longer an egress router in the final state, make sure that the router must change its
    /// forwarding **before** the egress router changes its forwarding.
    fn handle_initial_state<Q>(
        problem: &mut impl SolverModel,
        vars: &IlpVars,
        info: &CommandInfo<'_, Q>,
        prefix: P,
        router: RouterId,
    ) -> Option<()> {
        let net = info.net_before;
        let bgp_before = info.bgp_before.get(&prefix)?;
        let bgp_after = info.bgp_after.get(&prefix)?;

        // Get the initial egress router
        let (egress, border_router) = bgp_before.ingress_session(router)?;

        // skip if border_router is the router itself
        if border_router == router {
            return None;
        }

        // assert that the initial egress point is an internal router
        assert!(net.get_device(egress).is_external());

        // If the border router uses the same egress before and after the migration, do not add any
        // constraints.
        if Some(egress) == bgp_after.get(border_router).map(|(x, _)| x) {
            return None;
        }

        // add the condition that the router must chagne its forwarding (`r`) before the egress
        // router has changed its routing decision (`r_old`).
        let r = vars.r[&router];
        let r_old_egress = vars.r_old[&border_router];
        problem.add_constraint(constraint!(r + 1 <= r_old_egress));

        Some(())
    }

    /// If the router in the final state is not a border router, and if its final egress router is
    /// not an egress router in the initial state, make sure that the router must have changed its
    /// forwarding **after** the egress router has changed its forwarding.
    fn handle_final_state<Q>(
        problem: &mut impl SolverModel,
        vars: &IlpVars,
        info: &CommandInfo<'_, Q>,
        prefix: P,
        router: RouterId,
    ) -> Option<()> {
        let net = info.net_before;
        let bgp_before = info.bgp_before.get(&prefix)?;
        let bgp_after = info.bgp_after.get(&prefix)?;

        let (egress, border_router) = bgp_after.ingress_session(router)?;

        // check that the final egress point is an internal router
        assert!(net.get_device(egress).is_external());

        // skip if border_router is the router itself
        if border_router == router {
            return None;
        }

        // If the border router uses the same egress before and after the migration, do not add any
        // constraints.
        if Some(egress) == bgp_before.get(border_router).map(|(x, _)| x) {
            return None;
        }

        // add the condition that the router must chagne its forwarding (`r`) after the egress
        // router has changed its routing decision (`r_new`).
        let r = vars.r[&router];
        let r_new_egress = vars.r_new[&border_router];
        problem.add_constraint(constraint!(r >= r_new_egress + 1));

        Some(())
    }

    for router in vars.r.keys().cloned() {
        let _ = handle_initial_state(problem, vars, info, prefix, router);
        let _ = handle_final_state(problem, vars, info, prefix, router);
    }
}

/// Validate that the solution makes any sense.
fn validate_solution(vars: &IlpVars, solution: &impl Solution) {
    for router in vars.r.keys().copied() {
        let r = solution.value(vars.r[&router]).round() as usize;
        let s = format!(", with r[{}] = {}", router.index(), r);
        assert!(
            r < vars.max_steps,
            "r[{}] = {} >= {}",
            router.index(),
            r,
            vars.max_steps
        );
        for i in vars.steps() {
            let b = solution.value(vars.get_b(router, i)).round() as usize;
            let n = solution.value(vars.get_n(router, i)).round() as usize;
            match i.cmp(&r) {
                std::cmp::Ordering::Less => {
                    assert_eq!(b, 0, "invalid b[{}][{}] = {}{}", router.index(), i, b, s);
                    assert_eq!(n, 0, "invalid b[{}][{}] = {}{}", router.index(), i, n, s);
                }
                std::cmp::Ordering::Equal => {
                    assert_eq!(b, 1, "invalid b[{}][{}] = {}{}", router.index(), i, b, s);
                    assert_eq!(n, 1, "invalid n[{}][{}] = {}{}", router.index(), i, n, s);
                }
                std::cmp::Ordering::Greater => {
                    assert_eq!(b, 1, "invalid b[{}][{}] = {}{}", router.index(), i, b, s);
                    assert_eq!(n, 0, "invalid n[{}][{}] = {}{}", router.index(), i, n, s);
                }
            }
        }
    }
}

/// Make sure that all properties are satisfied properly. This is done by building a forwarding
/// state and checking the conditions.
fn check_properties<Q>(
    info: &CommandInfo<'_, Q>,
    vars: &IlpVars,
    solution: &impl Solution,
    prefix: P,
) -> FwStateTrace {
    /// check the invariants for the given prefix. if an invariant is violated, log an error and panic.
    fn check(checker: &mut Checker<'_>, fw: &mut ForwardingState<P>) {
        if !checker.step(fw) {
            log::error!("Specification violated at step {}", checker.num_steps());
            panic!("Specification violated at step {}", checker.num_steps());
        }
    }

    let mut plan: HashMap<usize, HashSet<(RouterId, Vec<RouterId>)>> = HashMap::new();
    let mut fw_state = info.fw_before.clone();
    let mut checker = Checker::new(info.spec);

    // check the initial forwarding state.
    check(&mut checker, &mut fw_state);

    // check each forwarding state. During this time, also generate a nicely formatted logging
    // string that prints all steps and their forwarding delta.
    for step in vars.steps() {
        // perform all fw deltas
        for (router, r) in vars.r.iter() {
            if step == solution.value(*r).round() as usize {
                let next_hops = info.fw_after.get_next_hops(*router, prefix).to_vec();
                let prev_hops = info.fw_before.get_next_hops(*router, prefix);
                if next_hops != prev_hops {
                    fw_state.update(*router, prefix, next_hops.clone());
                    plan.entry(step).or_default().insert((*router, next_hops));
                }
            }
        }
        // check the conditions
        check(&mut checker, &mut fw_state);
    }

    // check the final state
    if !checker.check_prefix(prefix) {
        log::error!("Specification violated on complete trace");
        panic!("Specification violated on complete trace");
    }

    // transfor te plan into an FwStateTrace
    let plan: FwStateTrace = plan
        .into_iter()
        .sorted_by_key(|(i, _)| *i)
        .map(|(_, x)| x)
        .collect();

    plan
}

/// Structure for maintaining all variables of the ILP Scheduler
#[derive(Debug)]
pub(self) struct IlpVars {
    /// Number of steps in the model.
    max_steps: usize,
    /// Number of steps in the model, as a symbolic variable
    max_steps_v: Variable,
    /// Cost to be minimized
    cost: Variable,
    /// Boolean variables telling that a temporary_bgp_session is needed for the given router in initial or
    /// in final state.
    session_needed: HashMap<RouterId, (Variable, Variable)>,
    /// Integer variables for capturing the round at which a router will update
    r: HashMap<RouterId, Variable>,
    /// Integer variables for capturing up to which round the router will get the old advertisement
    /// in BGP.
    r_old: HashMap<RouterId, Variable>,
    /// Integer variables for capturing at which round the router will get the new advertisement in
    /// BGP.
    r_new: HashMap<RouterId, Variable>,
    /// Boolean variables encoding if a router has already changed its forwarding after step i
    b: HasChangedType,
    /// Boolean variable encoding if the router changes the next hop exactly at step i.
    n: HasChangedType,
    /// Boolean variable counting the number of changes on that path that happened in that step.
    p: HasChangedPathType,
    /// Boolean variables for encoding the conditions to be satisfied.
    c: CondsType,
    /// Boolean variables for encoding the specification expressions.
    s: SpecExprType,
    /// Variables expressing the minimum or the maximum of sets of other variables
    min_max: MinMaxDepsType,
    // /// Variables to protect against loops.
    #[cfg(feature = "explicit-loop-checker")]
    loop_protection: LoopProtectionType,
}

impl IlpVars {
    /// Get the boolean variable for a given condition, router and round.
    fn get_c(&self, prop: &Property, router: RouterId, round: usize) -> Variable {
        self.c[prop][&router][round]
    }

    /// Get the boolean variable for a given specification
    fn get_s(&self, spec: &SpecExprExt, round: usize) -> Variable {
        self.s[spec][&round]
    }

    /// Get the boolean variable to check if a router has already updated its routing decision in
    /// this round.
    fn get_b(&self, router: RouterId, round: usize) -> Variable {
        self.b[&router][round]
    }

    /// Get the boolean variable to check if a router changes its forwarding in this specific round.
    fn get_n(&self, router: RouterId, round: usize) -> Variable {
        self.n[&router][round]
    }

    /// Get an iterator over all properties in the model.
    fn props(&self) -> impl Iterator<Item = &Property> {
        self.c.keys()
    }

    /// Get the range of all steps in the model
    fn steps(&self) -> Range<usize> {
        0..self.max_steps
    }
}
