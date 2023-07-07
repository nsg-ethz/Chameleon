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

import os
import pandas as pd
import plotly.graph_objects as go
from plotly.colors import hex_to_rgb

from utils import select_measurement

COLORS = ["#636EFA", "#FFA15A"]

if __name__ == "__main__":
    path = select_measurement(contains="parsed.csv")
    file = os.path.join(path, "parsed.csv")
    plot_file = os.path.join(path, "plot_specification_complexity.html")

    df = pd.read_csv(file, sep=",")
    fig = go.Figure()

    specs = sorted(list(set(df["spec_kind"])))

    for spec, color in zip(specs, COLORS):
        data = df[df["spec_kind"] == spec]
        r, g, b = hex_to_rgb(color)

        fig.add_traces(
            go.Scatter(
                x=data["spec_iter"],
                y=data["time_p50"],
                line={"color": color},
                name=spec,
                error_y=dict(
                    color=f"rgba({r},{g},{b},0.3)",
                    type="data",
                    symmetric=False,
                    array=data["time_p90"] - data["time_p50"],
                    arrayminus=data["time_p50"] - data["time_p10"],
                ),
            )
        )
    fig.write_html(plot_file)
    print(f"Written plot to {plot_file}")
