"""Launch the arb-contest simulation alongside 1 or 2 contestant containers.

Usage:
  python run.py <contestant> [<contestant>]   # e.g. example-bot

Each contestant name must match a service in docker-compose.yml.

Env vars (override any to change behavior; defaults applied here, not in the
compose file, so docker still demands them):
  - RUST_LOG                  (default "info")
  - SIM_HTTP_BIND             (default "0.0.0.0:9003")     simulation HTTP listener
  - SIM_UDP_TARGET            (default "239.42.0.1:9001")  simulation UDP destination
  - SIM_TCP_BIND              (default "0.0.0.0:9002")     simulation submission listener
  - SIM_HTTP_ADDR             (default "127.0.0.1:9003")   what contestants connect to
  - SIM_UDP_GROUP             (default "239.42.0.1:9001")  multicast group contestants join
  - SIM_SUBMISSION_ADDR       (default "127.0.0.1:9002")   where contestants send submissions
  - SIM_INITIAL_BALANCE_USD   (default "100")              starting balance per contestant, in whole USD

SIM_EXPECTED_CONTESTANTS is set automatically from the number of contestants
passed on the command line.
"""

from __future__ import annotations

import argparse
import os
import pathlib
import re
import secrets
import subprocess
import sys


REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
COMPOSE_FILE = REPO_ROOT / "docker-compose.yml"

DEFAULTS = {
    "RUST_LOG": "info",
    "SIM_HTTP_BIND": "0.0.0.0:9003",
    "SIM_UDP_TARGET": "239.42.0.1:9001",
    "SIM_TCP_BIND": "0.0.0.0:9002",
    "SIM_HTTP_ADDR": "127.0.0.1:9003",
    "SIM_UDP_GROUP": "239.42.0.1:9001",
    "SIM_SUBMISSION_ADDR": "127.0.0.1:9002",
    "SIM_INITIAL_BALANCE_USD": "100",
}

# Simulation owns 0-3 (set statically in compose). Contestants are assigned
# 3 consecutive cores each starting here, in the order they appear on argv.
CONTESTANT_CORE_BASE = 4
CORES_PER_CONTESTANT = 3
UNUSED_CPUSET = "0"


def contestant_cpuset_env(name: str) -> str:
    return name.upper().replace("-", "_") + "_CPUSET"


def discover_contestants() -> list[str]:
    """All top-level services in docker-compose.yml except `simulation`."""
    text = COMPOSE_FILE.read_text()
    names = re.findall(r"^  ([a-z][a-z0-9_-]*):\s*$", text, re.MULTILINE)
    return [n for n in names if n != "simulation"]


def env_for_compose(running: list[str], seed: int) -> dict[str, str]:
    env = os.environ.copy()
    for key, value in DEFAULTS.items():
        env.setdefault(key, value)
    env["SIM_EXPECTED_CONTESTANTS"] = str(len(running))
    env["SEED"] = str(seed)
    for name in discover_contestants():
        env[contestant_cpuset_env(name)] = UNUSED_CPUSET
    for idx, name in enumerate(running):
        first = CONTESTANT_CORE_BASE + idx * CORES_PER_CONTESTANT
        last = first + CORES_PER_CONTESTANT - 1
        env[contestant_cpuset_env(name)] = f"{first}-{last}"
    return env


def run(contestants: list[str], seed: int) -> int:
    args = (
        ["docker", "compose", "-f", "docker-compose.yml", "up", "--build",
         "--abort-on-container-exit", "simulation"]
        + contestants
    )
    env = env_for_compose(contestants, seed)
    shown_keys = (
        *DEFAULTS,
        "SIM_EXPECTED_CONTESTANTS",
        *(contestant_cpuset_env(c) for c in contestants),
    )
    shown = " ".join(f"{k}={env[k]}" for k in shown_keys)
    print(f"$ {shown} {' '.join(args)}", flush=True)

    proc = subprocess.Popen(args, cwd=REPO_ROOT, env=env)
    while True:
        try:
            return proc.wait()
        except KeyboardInterrupt:
            continue


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Launch simulation alongside 1 or 2 contestant containers.",
    )
    parser.add_argument("contestants", nargs="+", help="contestant service names from docker-compose.yml")
    parser.add_argument(
        "--seed",
        type=int,
        default=None,
        help="master RNG seed (default: random 64-bit value, printed on launch)",
    )
    ns = parser.parse_args()
    if not 1 <= len(ns.contestants) <= 2:
        parser.error("expected 1 or 2 contestants")
    seed = ns.seed if ns.seed is not None else secrets.randbits(64)
    return run(ns.contestants, seed)


if __name__ == "__main__":
    raise SystemExit(main())
