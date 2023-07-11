# Chameleon: Taming the transient while reconfiguring BGP
# Copyright (C) 2023 Tibor Schneider <sctibor@ethz.ch>
#
# This program is free software; you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation; either version 2 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License along
# with this program; if not, write to the Free Software Foundation, Inc.,
# 51 Franklin Street, Fifth Floor, Boston, MA 02110-1301 USA.

import numpy as np
import pandas as pd
import json
import networkx as nx
import sys
from itertools import chain
from utils import select_measurement


def build_graph(data):
    old = data["data"]["fw_state_before"]["state"]
    new = data["data"]["fw_state_after"]["state"]
    p = next(iter(next(iter(old.values())).keys()))
    old = {int(k): v[p][0] for k, v in old.items()}
    new = {int(k): v[p][0] for k, v in new.items()}
    edges = {(a, b) for a, b in chain(old.items(), new.items())}
    g = nx.DiGraph()
    g.add_edges_from(edges)
    return (g, 4294967295)


def running_time(data):
    try:
        schedule = next(iter(data["decomp"]["schedule"].values()))
        num_rounds = max(s["new_route"] for s in schedule.values())
        steps = 2 + num_rounds
    except TypeError:
        steps = 2 + data["data"]["model_steps"]
    return steps * 12


def summarize_statistics(stats):
    data = {}
    # summarize them
    for stat in stats:
        key = (
            stat["topo"],
            stat["scenario"],
            stat["spec"],
            stat["spec_kind"],
            stat["spec_iter"],
        )
        if key in data:
            data[key].append(stat)
        else:
            data[key] = [stat]

    processed = []
    for stats in data.values():
        times = np.array([s["time"] for s in stats])
        x = stats[0]
        [t10, t25, t50, t75, t90] = np.percentile(
            times, [10, 25, 50, 75, 90], interpolation="nearest"
        )
        x["time"] = times.mean()
        x["time_p10"] = t10
        x["time_p25"] = t25
        x["time_p50"] = t50
        x["time_p75"] = t75
        x["time_p90"] = t90

        processed.append(x)

    return processed


def get_stats(file):
    print(f"working on {file.name}")

    with open(file, "r") as fp:
        data = json.load(fp)

    steps = float("inf")
    mem_baseline = float("inf")
    mem_sitn = float("inf")
    mem = float("inf")
    cost = float("inf")

    if len(sys.argv) > 1:
        timeout = float(sys.argv[1])
        time = min(timeout, data["data"]["time"])
    else:
        time = data["data"]["time"]

    (g, d) = build_graph(data)
    updated_nodes = {n for n in g if g.degree(n) > 1}

    if type(data["data"]["result"]) is dict:
        result = next(iter(data["data"]["result"].keys()))
        r = data["data"]["result"][result]
        steps = int(r["steps"])
        cost = int(r["cost"])
        if result == "Success":
            mem_baseline = int(r["max_routes_baseline"])
            mem_sitn = int(r["routes_before"] + r["routes_after"])
            mem = int(r["max_routes"])
    else:
        result = data["data"]["result"]

    nodes = len(data["net"]["net"]["routers"])
    spec = next(iter(data["spec"].values()))
    try:
        spec_name = f"Scalable-{data['spec_builder']['Scalable']:>03}"
        spec_kind = f"Scalable"
        spec_iter = data["spec_builder"]["Scalable"]
    except:
        try:
            spec_name = (
                f"ScalableNonTemporal-{data['spec_builder']['ScalableNonTemporal']:>03}"
            )
            spec_kind = f"ScalableNonTemporal"
            spec_iter = data["spec_builder"]["ScalableNonTemporal"]
        except:
            spec_name = data["spec_builder"]
            spec_kind = spec_name
            spec_iter = 0

    return {
        "topo": data["topo"],
        "scenario": data["scenario"],
        "spec": spec_name,
        "spec_kind": spec_kind,
        "spec_iter": spec_iter,
        "nodes": nodes,
        "time": time,
        "time_p10": time,
        "time_p25": time,
        "time_p50": time,
        "time_p75": time,
        "time_p90": time,
        "cost": cost,
        "result": result,
        "model_steps": data["data"]["model_steps"],
        "steps": steps,
        "est_time": running_time(data),
        "mem": mem,
        "mem_sitn": mem_sitn,
        "mem_baseline": mem_baseline,
        "num_variables": data["data"]["num_variables"],
        "num_equations": data["data"]["num_equations"],
        "avg_path_length": data["data"]["avg_path_length"],
        "num_fw_updates": len(updated_nodes),
        "num_cycles": sum(1 for _ in nx.cycles.simple_cycles(g)),
        "potential_deps": sum(
            sum(1 for x in nx.descendants(g, s) if x in updated_nodes)
            for s in updated_nodes
        ),
    }


if __name__ == "__main__":
    experiment = select_measurement(prefix="overhead_")
    stats = summarize_statistics(
        [
            get_stats(file)
            for file in experiment.iterdir()
            if not file.name.endswith(".csv")
        ]
    )
    stats = pd.DataFrame(stats).sort_values(["nodes", "topo", "spec"])
    stats.to_csv(experiment / "parsed.csv", index=False)
    print(f"Written {experiment / 'parsed.csv'}")
    with pd.option_context(
        "display.max_rows", None, "display.max_columns", None, "display.width", 200
    ):  # more options can be specified also
        print(stats)
