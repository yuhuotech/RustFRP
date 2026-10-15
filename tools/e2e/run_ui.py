#!/usr/bin/env python3
"""Run UI tests against an owned, disposable Controller on the agreed port 9674."""
import argparse
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
URL = "http://127.0.0.1:9674"


def free_port(requested=0):
    with socket.socket() as listener:
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("127.0.0.1", requested))
        return listener.getsockname()[1]


def free_ports(count):
    listeners = []
    try:
        for _ in range(count):
            listener = socket.socket()
            listener.bind(("127.0.0.1", 0))
            listeners.append(listener)
        return [listener.getsockname()[1] for listener in listeners]
    finally:
        for listener in listeners:
            listener.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        parser.error("pass a test command after --")
    if os.environ.get("RUSTFRP_E2E_EXTERNAL_CONTROLLER") == "1":
        return subprocess.call(command, cwd=ROOT)
    try:
        free_port(9674)
    except OSError as error:
        raise SystemExit("9674 is occupied; no process was stopped. Use an isolated existing Controller only with RUSTFRP_E2E_EXTERNAL_CONTROLLER=1.") from error
    server = ROOT / "target/release" / ("rustfrp-server.exe" if os.name == "nt" else "rustfrp-server")
    if not server.is_file() or not (ROOT / "rustfrp-ui/dist/index.html").is_file():
        raise SystemExit("Build the shared UI and release Controller first (make ui-build).")
    (ROOT / "target").mkdir(exist_ok=True)
    edge_port, node_port, vhost_http, vhost_https, tcpmux_port = free_ports(5)
    with tempfile.TemporaryDirectory(prefix="ui-e2e-", dir=ROOT / "target") as folder:
        env = {key: value for key, value in os.environ.items() if not key.startswith("RUSTFRP_")}
        env.update({
            "RUSTFRP_DB": str(Path(folder) / "controller.db"),
            "RUSTFRP_HTTP": "127.0.0.1:9674",
            "RUSTFRP_EDGE": f"127.0.0.1:{edge_port}",
            "RUSTFRP_NODE_ADDR": f"127.0.0.1:{node_port}",
            "RUSTFRP_VHOST_HTTP": str(vhost_http),
            "RUSTFRP_VHOST_HTTPS": str(vhost_https),
            "RUSTFRP_TCPMUX_HTTP_CONNECT_PORT": str(tcpmux_port),
            "RUSTFRP_SUBDOMAIN_HOST": "e2e.example.test",
            "RUSTFRP_PROXY_BIND": "127.0.0.1",
            "RUSTFRP_PUBLIC_HOST": "127.0.0.1",
            "RUSTFRP_PANEL_DIR": str(ROOT / "rustfrp-ui/dist"),
        })
        password = "native ui test password"
        subprocess.run([str(server), "admin", "create", "preview"], input=password + "\n", text=True, env=env, cwd=ROOT, check=True, capture_output=True)
        log_path = ROOT / "target/ui/e2e-controller.log"
        log_path.parent.mkdir(exist_ok=True)
        with log_path.open("wb") as log:
            controller = subprocess.Popen([str(server), "run"], cwd=ROOT, env=env, stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 20
                while True:
                    if controller.poll() is not None:
                        raise RuntimeError(f"Controller exited; inspect {log_path}")
                    try:
                        with urllib.request.urlopen(URL + "/", timeout=1) as response:
                            if response.status == 200:
                                break
                    except (OSError, urllib.error.URLError):
                        pass
                    if time.monotonic() >= deadline:
                        raise RuntimeError(f"Controller did not become ready; inspect {log_path}")
                    time.sleep(0.1)
                test_env = os.environ.copy()
                test_env["RUSTFRP_E2E_PASSWORD"] = password
                return subprocess.call(command, cwd=ROOT, env=test_env)
            finally:
                if controller.poll() is None:
                    controller.terminate()
                    try:
                        controller.wait(timeout=8)
                    except subprocess.TimeoutExpired:
                        controller.kill()
                        controller.wait(timeout=3)


if __name__ == "__main__":
    raise SystemExit(main())
