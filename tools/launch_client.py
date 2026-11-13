#!/usr/bin/env python3
"""Launch the EVE Online V24.01 client against an EvoMu server.

The installed client encrypts its login so that only CCP's servers can read it.
EvoMu uses the client's built-in "Placebo" setting instead, which has to be
switched on in the client's own folder. This script never touches your
installed client: it makes a private copy, switches the setting on there, and
starts that copy pointed at your server.

    python3 tools/launch_client.py --login pilot1:my-password

Needs Python 3.9+ and nothing else.
"""
import argparse
import os
import platform
import shutil
import socket
import subprocess
import sys
from pathlib import Path

IS_WINDOWS = platform.system() == "Windows"
IS_MACOS = platform.system() == "Darwin"
EXE_NAME = "exefile.exe" if IS_WINDOWS else "exefile"
MACOS_DEFAULT_CLIENT = Path.home() / "Library/Application Support/EVE Online/SharedCache/tq/EVE.app"
DEFAULT_STAGE_DIR = Path.home() / ".evomu" / "client"


def fail(message):
    print("error: " + message, file=sys.stderr)
    sys.exit(1)


def find_build_dir(client):
    """Returns the folder that holds bin64/ and start.ini, given an EVE.app bundle or that folder itself."""
    candidates = [client / "Contents" / "Resources" / "build", client, client / "tq"]
    for candidate in candidates:
        if (candidate / "bin64" / EXE_NAME).is_file() and (candidate / "start.ini").is_file():
            return candidate
    fail(
        "no EVE client found at %s\n"
        "       Pass --client with the path to EVE.app (macOS) or to the folder that contains bin64 (Windows)."
        % client
    )


def is_stale(build_dir, stage_dir):
    staged_ini = stage_dir / "start.ini"
    if not (stage_dir / "bin64" / EXE_NAME).is_file() or not staged_ini.is_file():
        return True
    return (build_dir / "start.ini").stat().st_mtime > staged_ini.stat().st_mtime


def copy_client(build_dir, stage_dir):
    """Copies the client folder to stage_dir, replacing any earlier copy only once the new one is complete."""
    stage_dir.parent.mkdir(parents=True, exist_ok=True)
    partial = stage_dir.with_name(stage_dir.name + ".partial")
    if partial.exists():
        shutil.rmtree(partial)
    cloned = False
    if IS_MACOS:
        # APFS clone: instant and takes no extra disk space.
        cloned = subprocess.run(["/bin/cp", "-cR", str(build_dir), str(partial)], capture_output=True).returncode == 0
        if not cloned and partial.exists():
            shutil.rmtree(partial)
    if not cloned:
        shutil.copytree(build_dir, partial, symlinks=True)
    if stage_dir.exists():
        shutil.rmtree(stage_dir)
    partial.rename(stage_dir)


def server_is_listening(host, port):
    try:
        with socket.create_connection((host, port), timeout=2):
            return True
    except OSError:
        return False


def main():
    parser = argparse.ArgumentParser(
        description="Launch the EVE Online V24.01 client against an EvoMu server.",
        epilog="Anything after -- is passed to the client unchanged.",
    )
    parser.add_argument("--login", metavar="USER:PASSWORD", help="account to log in with (required to launch)")
    parser.add_argument("--host", default="127.0.0.1", help="server address (default: %(default)s)")
    parser.add_argument("--port", type=int, default=26000, help="server game port (default: %(default)s)")
    parser.add_argument("--gateway-port", type=int, default=26005, help="server gateway port; 0 to leave unset (default: %(default)s)")
    parser.add_argument("--character", type=int, metavar="ID", help="character ID to select automatically")
    parser.add_argument("--gm", action="store_true",
                        help="enable the client's game-master menus and slash commands (/localdevtools)")
    parser.add_argument("--client", type=Path, default=MACOS_DEFAULT_CLIENT if IS_MACOS else None,
                        help="EVE.app (macOS) or the folder containing bin64 (Windows)")
    parser.add_argument("--stage-dir", type=Path, default=DEFAULT_STAGE_DIR,
                        help="where the private copy lives (default: %(default)s)")
    parser.add_argument("--refresh", action="store_true", help="make a fresh copy of the client even if one exists")
    parser.add_argument("--prepare-only", action="store_true", help="make the copy and stop; do not launch")
    parser.add_argument("--no-server-check", action="store_true", help="launch even if the server is not reachable")
    parser.add_argument("client_args", nargs="*", help=argparse.SUPPRESS)
    args = parser.parse_args()

    if args.client is None:
        fail("--client is required on this platform (the folder that contains bin64)")
    if not args.prepare_only:
        if not args.login or ":" not in args.login:
            fail("--login USER:PASSWORD is required")
        if not args.no_server_check and not server_is_listening(args.host, args.port):
            fail(
                "no server is listening on %s:%d\n"
                "       Start one first:  cargo run --release -p eve-server" % (args.host, args.port)
            )

    build_dir = find_build_dir(args.client.expanduser())
    stage_dir = args.stage_dir.expanduser()
    if args.refresh or is_stale(build_dir, stage_dir):
        print("Copying the client to %s ..." % stage_dir)
        copy_client(build_dir, stage_dir)
    (stage_dir / "common.ini").write_text("[main]\ncryptoPack = Placebo\n", encoding="utf-8")
    if args.prepare_only:
        print("Client copy is ready at %s" % stage_dir)
        return 0

    command = [str(stage_dir / "bin64" / EXE_NAME), "/server:%s" % args.host, "/port:%d" % args.port,
               "/login:%s" % args.login]
    if args.character:
        command.append("/autoSelectCharacter:%d" % args.character)
    if args.gm:
        command.append("/localdevtools")
    command += args.client_args

    env = dict(os.environ)
    if args.gateway_port:
        env["EVE_PUBLIC_GATEWAY_ADDRESS"] = "%s:%d" % (args.host, args.gateway_port)

    user = args.login.split(":", 1)[0]
    print("Starting the client as %s against %s:%d (close the client window to exit)" % (user, args.host, args.port))
    try:
        return subprocess.call(command, cwd=str(stage_dir), env=env)
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    sys.exit(main())
