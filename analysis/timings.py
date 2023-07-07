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

import json
from pprint import pprint
import pathlib


def cmd(event):
    cmd = event["command"]["command"]
    assert len(cmd.keys()) == 1
    return next(iter(cmd.keys()))


def cond(cond):
    if cond == "None":
        return "None"
    assert len(cond.keys()) == 1
    return next(iter(cond.keys()))


def precond(event):
    return cond(event["command"]["precondition"])


def postcond(event):
    return cond(event["command"]["postcondition"])


# path to the event file
root = pathlib.Path(__file__).parent.resolve().parent
results = "results"
measurement = "lab_atomic_2023-01-22_09-54-24"
file = "event.json"

event_file = f"{root}/{results}/{measurement}/{file}"
with open(event_file, "r") as fp:
    event_list = json.load(fp)

events = {}

for event in event_list:
    id = tuple(event["id"])
    kind = event["event"]
    elapsed = event["elapsed"][0] + event["elapsed"][1] / 1_000_000_000
    if id not in events:
        events[id] = {"command": event["command"]}

    if kind in events[id]:
        raise ValueError(f"Received twice the event kind {kind} for event {id}")

    events[id][kind] = elapsed

for event in events.values():
    event["timePre"] = event["PreconditionSatisfied"] - event["Scheduled"]
    event["timePost"] = event["PostConditionSatisfied"] - event["PreconditionSatisfied"]

for event in events.values():
    if event["timePre"] < 5:
        continue

    event_cmd = cmd(event)
    event_cond = precond(event)
    others = [
        int(round(e["timePre"] * 10)) / 10
        for e in events.values()
        if cmd(e) == event_cmd and precond(event) == event_cond
    ]
    print(f"\nSatisfying precondition took {event['timePre']} time:")
    print(f"  command:  {event_cmd}")
    print(f"  precond:  {event_cond}")
    print(f"  others:   {others}")

    pprint(event["command"])

for event in events.values():
    if event["timePost"] < 5:
        continue

    event_cmd = cmd(event)
    event_cond = postcond(event)
    others = [
        int(round(e["timePost"] * 10)) / 10
        for e in events.values()
        if cmd(e) == event_cmd and postcond(event) == event_cond
    ]

    print(f"\nSatisfying postcondition took {event['timePost']} time:")
    print(f"  command:  {event_cmd}")
    print(f"  postcond: {event_cond}")
    print(f"  others:   {others}")
