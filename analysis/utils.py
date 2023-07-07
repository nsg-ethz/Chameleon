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

import pathlib
import sys
import os


def select_measurement(prefix=None, suffix=None, contains=None):
    path = pathlib.Path(__file__).parent.resolve().parent / "results"
    children = sorted(
        p
        for p in path.iterdir()
        if os.path.isdir(p)
        and (prefix is None or p.name.startswith(prefix))
        and (suffix is None or p.name.endswith(suffix))
        and (contains is None or os.path.exists(os.path.join(p, contains)))
    )
    if not children:
        print("No measurements found!")
        sys.exit(1)
    if len(children) == 1:
        return children[0]
    for i, child in enumerate(children):
        print(f"{i: >3}: {child.name}")
    idx = int(input("choose an index: "))
    return children[idx]
