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
import re
import os
from utils import select_measurement

REX = re.compile("([a-zA-Z0-9_]*)-(p?[0-9_]*-[0-9_]*)-([a-zA-Z0-9_]*)")
FREQ_BASELINE = (50, 500)
FREQ_CHAMELEON = (10, 500)


def get_component(path, i):
    return {m.group(i) for m in (REX.match(x) for x in os.listdir(path)) if m}


def get_raw_samples(path, src, pfx, dst):
    with open(f"{path}/{src}-{pfx}-{dst}.csv", "r") as fp:
        return [
            float(l.split(",")[0])
            for l in fp.readlines()
            if not (l.startswith("time") or l.startswith("send_time"))
        ]


def read_data(path):
    internals = get_component(path, 1)
    externals = get_component(path, 3)
    prefixes = get_component(path, 2)

    return (
        internals,
        prefixes,
        externals,
        {
            (src, prefix, dst): get_raw_samples(path, src, prefix, dst)
            for src in internals
            for prefix in prefixes
            for dst in externals
        },
    )


def process_data(internals, prefixes, externals, raw, freq):
    t_min = min(min(x) for x in raw.values() if x)
    t_max = max(max(x) for x in raw.values() if x)
    bins = int((t_max - t_min) * freq[0])
    samples = {
        (src, pfx, dst): np.histogram(x, bins=bins, range=(t_min, t_max))[0]
        for (src, pfx, dst), x in raw.items()
    }
    t = np.histogram([], bins=bins, range=(t_min, t_max))[1][:-1]
    if len(t) > 0:
        t_min = min(t)
        t = t - t_min
    assert all(len(t) == len(x) for x in samples.values())
    return (internals, prefixes, externals, samples, t)


def data_per_egress(internals, prefixes, externals, samples, t, freq):
    norm = freq[0] / (freq[1] * len(internals) * len(prefixes))
    data = {
        dst.split("_")[0]: norm * sum(x for (_, _, d), x in samples.items() if d == dst)
        for dst in externals
    }
    data["Sum"] = norm * sum(x for x in samples.values())
    data["t"] = t

    # compute the violations
    tail = int(freq[0])
    violations = sum(
        norm * x
        for dst in externals
        for (_, _, d), x in samples.items()
        if d == dst
        if max(max(x[:tail]), max(x[len(x) - tail :])) == 0
    )
    data["violations"] = violations

    data = pd.DataFrame(data)
    return data


def write(root, filename, data):
    file = root / filename
    data.to_csv(file, sep=",", index=False, float_format="%.3f")


if __name__ == "__main__":
    path = select_measurement(prefix=("lab_baseline_", "lab_chameleon_", "lab_atomic_"))

    if path.name.startswith("lab_baseline"):
        freq = FREQ_BASELINE
    else:
        freq = FREQ_CHAMELEON

    data = read_data(path)
    data = process_data(*data, freq)
    data = data_per_egress(*data, freq)
    write(path, "throughput_per_egress.csv", data)
    print(f"Written {path}/throughput_per_egress.csv")
