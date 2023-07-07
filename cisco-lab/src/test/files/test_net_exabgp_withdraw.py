#!/usr/bin/env python3
import sys
import time
from os.path import expanduser as full

def wait_until(x):
    while True:
        try:
            with open(full('/tmp/cisco-lab/run_exabgp_control'), 'r') as f:
                t = int(f.read())
                if t >= x: return
        except FileNotFoundError:
            pass
        except ValueError:
            pass
        time.sleep(0.1)


wait_until(0)
sys.stdout.write("neighbor 1.192.0.1 announce route 100.0.0.0/24 next-hop self as-path [4, 100]\n")
sys.stdout.write("neighbor 1.192.0.5 announce route 100.0.0.0/24 next-hop self as-path [5, 100]\n")
sys.stdout.flush()

wait_until(1)
sys.stdout.write("neighbor 1.192.0.1 withdraw route 100.0.0.0/24\n")
sys.stdout.flush()

wait_until(1_000_000)
