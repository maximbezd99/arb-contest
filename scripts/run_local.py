"""Launch arb-contest stacks with docker compose.

Required env vars (no defaults — set them explicitly or this script exits):
  - SIM_NETWORK_MODE: "bridge" (dev / macOS) or "host" (contest / Linux).
  - RUST_LOG: log filter, e.g. "info".

SIM_HTTP_ADDR is derived from SIM_NETWORK_MODE and injected automatically:
  - bridge → "simulation:9003"  (resolved via docker network DNS)
  - host   → "127.0.0.1:9003"   (containers share the host stack)

Commands:
  example-bot   build & run simulation + example-bot contestant
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys
from collections.abc import Callable


REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent

REQUIRED_ENV = ("SIM_NETWORK_MODE", "RUST_LOG")


def require_env() -> None:
    missing = [v for v in REQUIRED_ENV if not os.environ.get(v)]
    if missing:
        sys.exit(f"missing required env vars: {', '.join(missing)}")


def network_mode() -> str:
    mode = os.environ["SIM_NETWORK_MODE"].lower()
    if mode not in ("bridge", "host"):
        sys.exit(f"unknown SIM_NETWORK_MODE={mode!r} (expected 'bridge' or 'host')")
    return mode


def compose_files() -> list[str]:
    return ["docker-compose.yml", f"docker-compose.{network_mode()}.yml"]


def env_for_compose() -> dict[str, str]:
    env = os.environ.copy()
    env["SIM_HTTP_ADDR"] = (
        "127.0.0.1:9003" if network_mode() == "host" else "simulation:9003"
    )
    return env


def compose_base() -> list[str]:
    args = ["docker", "compose"]
    for f in compose_files():
        args += ["-f", f]
    return args


def cmd_example_bot(extra: list[str]) -> int:
    args = compose_base() + ["up", "--build", "simulation", "example-bot"] + extra
    env = env_for_compose()
    print(f"$ SIM_HTTP_ADDR={env['SIM_HTTP_ADDR']} {' '.join(args)}", flush=True)
    return subprocess.call(args, cwd=REPO_ROOT, env=env)


COMMANDS: dict[str, Callable[[list[str]], int]] = {
    "example-bot": cmd_example_bot,
}


def main() -> int:
    if len(sys.argv) < 2:
        sys.exit(f"usage: {sys.argv[0]} {{{','.join(COMMANDS)}}} [extra docker-compose args...]")
    cmd = sys.argv[1]
    if cmd not in COMMANDS:
        sys.exit(f"unknown command {cmd!r} (expected one of: {', '.join(COMMANDS)})")

    require_env()
    return COMMANDS[cmd](sys.argv[2:])


if __name__ == "__main__":
    raise SystemExit(main())
