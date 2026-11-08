"""Install login-session launch agents; quick tunnels offer no uptime guarantee."""

import argparse
import json
import os
import plistlib
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

LABELS = ("org.kiss.paper-trader", "org.kiss.paper-trader-tunnel")
CLOUDFLARED = "/opt/homebrew/bin/cloudflared"
AVAILABILITY = (
    "Best effort only: uptime is not guaranteed. The Mac must be awake, online, "
    "and the user logged in. Quick tunnels have no SLA and change URL on restart. "
    "The public dashboard is read-only but visible to anyone with its URL."
)


def validate_python(python: Path) -> Path:
    """Reject unusable interpreters before touching any running service."""
    python = python.expanduser().resolve()
    if ".kiss-worktrees" in python.parts or not python.is_file() or not os.access(python, os.X_OK):
        raise ValueError("Python must be an executable outside temporary worktrees")
    return python


def stage(home: Path, python: Path) -> list[Path]:
    """Copy application code and generate agents without touching account data."""
    python = validate_python(python)
    root = home / ".kiss/paper-trader"
    app = root / "app"
    logs = root / "logs"
    agents = home / "Library/LaunchAgents"
    logs.mkdir(parents=True, exist_ok=True)
    agents.mkdir(parents=True, exist_ok=True)
    root.chmod(0o700)
    package = app / "projects/paper_trader"
    source = Path(__file__).resolve().parent
    if source != package:
        shutil.copytree(
            source,
            package,
            dirs_exist_ok=True,
            ignore=shutil.ignore_patterns("__pycache__", "tests", "*.pyc"),
        )
    commands = [
        [
            str(python),
            "-m",
            "projects.paper_trader",
            "serve",
            "--db",
            str(root / "account.sqlite"),
            "--host",
            "127.0.0.1",
            "--port",
            "8766",
        ],
        # Fixed shell program; paths are positional parameters, never interpolated.
        # Truncate on every restart so status cannot reuse the previous hostname.
        [
            "/bin/sh",
            "-c",
            'exec "$1" tunnel --no-autoupdate --url http://127.0.0.1:8766 > "$2" 2>&1',
            "paper-trader-tunnel",
            CLOUDFLARED,
            str(logs / "tunnel.log"),
        ],
    ]
    paths = []
    for label, command in zip(LABELS, commands):
        config = {
            "Label": label,
            "ProgramArguments": command,
            "WorkingDirectory": str(app),
            "RunAtLoad": True,
            "KeepAlive": True,
            "ThrottleInterval": 30,
            "ProcessType": "Background",
            "EnvironmentVariables": {"PYTHONUNBUFFERED": "1"},
            "StandardOutPath": str(logs / (label + ".out.log")),
            "StandardErrorPath": str(logs / (label + ".err.log")),
        }
        path = agents / (label + ".plist")
        path.write_bytes(plistlib.dumps(config))
        path.chmod(0o600)
        paths.append(path)
    return paths


def launch_status(label: str) -> dict:
    """Report launchd's actual state, or absence, without promising availability."""
    if sys.platform != "darwin":
        return {"loaded": False, "detail": "launchd unavailable on this platform"}
    result = subprocess.run(
        ["launchctl", "print", f"gui/{os.getuid()}/{label}"],
        capture_output=True,
        text=True,
        check=False,
    )
    return {"loaded": result.returncode == 0, "detail": result.stdout or result.stderr}


def status(home: Path) -> dict:
    """Read the latest advertised URL; it is not proof of external reachability."""
    root = home / ".kiss/paper-trader"
    log = root / "logs/tunnel.log"
    urls = []
    if log.exists():
        # Bounded tail avoids loading a long-running tunnel's entire log.
        with log.open("rb") as stream:
            stream.seek(max(0, log.stat().st_size - 262144))
            urls = re.findall(
                r"https://[a-z0-9]+(?:-[a-z0-9]+)*\.trycloudflare\.com\b",
                stream.read().decode("utf-8", errors="replace"),
            )
    return {
        "runtime": str(root),
        "database": str(root / "account.sqlite"),
        "local_url": "http://127.0.0.1:8766",
        "tunnel_url": urls[-1] if urls else None,
        "url_verified": False,
        "url_note": "Latest advertised URL from this run; may be unavailable if stopped.",
        "availability": AVAILABILITY,
        "agents": {label: launch_status(label) for label in LABELS}
        if home == Path.home().resolve()
        else {},
    }


def service_loaded(service: str) -> bool:
    """Return whether launchd still knows *service* (a ``gui/<uid>/<label>`` target)."""
    result = subprocess.run(["launchctl", "print", service], capture_output=True, check=False)
    return result.returncode == 0


def stop_agents(timeout: float = 30.0) -> None:
    """Unload registered agents and wait until launchd has fully removed them.

    ``launchctl bootout`` can return before the service is gone; bootstrapping
    it again during that window fails with "5: Input/output error".
    """
    for label in reversed(LABELS):
        service = f"gui/{os.getuid()}/{label}"
        if service_loaded(service):
            subprocess.run(["launchctl", "bootout", service], check=False)
        deadline = time.monotonic() + timeout
        while service_loaded(service):
            if time.monotonic() > deadline:
                raise RuntimeError(f"launchd did not unload {service} within {timeout}s")
            time.sleep(0.5)


def main() -> None:
    """Install, uninstall, or inspect the durable paper-only dashboard deployment."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("install", "uninstall", "status"))
    parser.add_argument(
        "--home", type=Path, default=Path.home(), help="Alternate home for portable staging only"
    )
    parser.add_argument(
        "--stage-only",
        action="store_true",
        help="Write/remove configuration without registering launch agents",
    )
    parser.add_argument("--python", type=Path, default=Path(sys.executable).resolve())
    args = parser.parse_args()
    home = args.home.expanduser().resolve()
    if args.command == "status":
        print(json.dumps(status(home), indent=2))
        return
    if args.command == "install":
        args.python = validate_python(args.python)
    if not args.stage_only:
        if home != Path.home().resolve() or sys.platform != "darwin":
            parser.error("Real deployment requires macOS and the current home; use --stage-only")
        if args.command == "install" and not os.access(CLOUDFLARED, os.X_OK):
            parser.error(f"Required executable missing: {CLOUDFLARED}")
        stop_agents()
    agents = home / "Library/LaunchAgents"
    if args.command == "install":
        paths = stage(home, args.python.expanduser())
        if not args.stage_only:
            for path in paths:
                subprocess.run(
                    ["launchctl", "bootstrap", f"gui/{os.getuid()}", str(path)],
                    check=True,
                )
    else:
        for label in LABELS:
            (agents / (label + ".plist")).unlink(missing_ok=True)
    print(
        json.dumps(
            {
                "command": args.command,
                "staged": args.stage_only,
                "account_preserved": True,
                "availability": AVAILABILITY,
            }
        )
    )


if __name__ == "__main__":
    main()
