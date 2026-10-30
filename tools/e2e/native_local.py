#!/usr/bin/env python3
"""Isolated native service E2E: real jobs, managed enrollment, restart and frpc."""
from __future__ import annotations

import hashlib
import http.cookiejar
import json
import os
from pathlib import Path
import socket
import socketserver
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
AGENT = ROOT / "target" / "release" / ("rustfrp-agent.exe" if os.name == "nt" else "rustfrp-agent")
SHELL = Path(os.environ["RUSTFRP_DESKTOP_SHELL"]) if "RUSTFRP_DESKTOP_SHELL" in os.environ else ROOT / "rustfrp-desktop" / "target" / "debug" / ("rustfrp-desktop.exe" if os.name == "nt" else "rustfrp-desktop")
PROBE = Path(os.environ["RUSTFRP_DESKTOP_PROBE"]) if "RUSTFRP_DESKTOP_PROBE" in os.environ else ROOT / "rustfrp-desktop" / "target" / "debug" / "examples" / ("local_probe.exe" if os.name == "nt" else "local_probe")
BASE = "http://127.0.0.1:9674"


def wait(check, seconds=20):
    deadline = time.monotonic() + seconds
    last = None
    while time.monotonic() < deadline:
        try:
            value = check()
            if value:
                return value
        except (OSError, RuntimeError, ValueError) as error:
            last = error
        time.sleep(0.1)
    raise AssertionError(f"condition did not complete: {last}")


def probe(root, action="read", **values):
    payload = {"root": str(root), "shell": str(SHELL), "action": action, **values}
    result = subprocess.run([str(PROBE)], input=json.dumps(payload), text=True, capture_output=True, timeout=40)
    if result.returncode:
        raise RuntimeError(f"native {action} failed: {result.stderr.strip()}")
    return json.loads(result.stdout)


def port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


class Echo(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.settimeout(3)
        self.request.sendall(self.request.recv(512))


def forwarded(remote):
    with socket.create_connection(("127.0.0.1", remote), timeout=3) as client:
        client.sendall(b"native-runtime-probe")
        assert client.recv(512) == b"native-runtime-probe"
    return True


def stop_owned(root, configuration):
    if not (root / "preferences.json").exists():
        return
    try:
        probe(root, "stop")
    finally:
        if sys.platform == "darwin":
            discovery = Path(configuration["stateDir"]) / "control.json"
            label = "com.rustfrp.desktop." + hashlib.sha256(str(discovery).encode()).hexdigest()
            subprocess.run(["/bin/launchctl", "bootout", f"gui/{os.getuid()}/{label}"], capture_output=True, timeout=10)


def main():
    for path in [AGENT, SHELL, PROBE]:
        assert path.is_file(), f"missing {path.name}; run make desktop-e2e"
    opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
    csrf = ""

    def api(method, route, data=None):
        payload = json.dumps(data).encode() if data is not None else None
        headers = {"content-type": "application/json", "origin": BASE, "x-csrf-token": csrf, "x-rustfrp-admin": "1"}
        with opener.open(urllib.request.Request(BASE + "/api/v1" + route, data=payload, method=method, headers=headers), timeout=10) as response:
            body = response.read(2 * 1024 * 1024 + 1)
            assert len(body) <= 2 * 1024 * 1024
            return json.loads(body) if body else None

    api("POST", "/auth/login", {"token": os.environ.get("RUSTFRP_E2E_ADMIN_TOKEN", "native-ui-test-management-token")})
    csrf = api("GET", "/auth/me")["csrf"]
    node = api("GET", "/node")
    evidence = {"version": 1, "platform": sys.platform, "managed": {}, "standalone": {}}
    with tempfile.TemporaryDirectory(prefix="rustfrp-native-local-e2e-") as temporary, socketserver.TCPServer(("127.0.0.1", 0), Echo) as echo:
        folder = Path(temporary)
        threading.Thread(target=echo.serve_forever, daemon=True).start()
        echo_port = echo.server_address[1]
        name = f"native-local-e2e-{time.time_ns()}"
        enrollment = api("POST", "/enroll-tokens", {"name": name, "ttl_minutes": 5, "reason": "Native lifecycle E2E"})
        managed_root = folder / "managed-desktop"
        configuration = {"binary": str(AGENT), "stateDir": str(folder / "managed-agent"), "controller": f"127.0.0.1:{node['edge_port']}", "name": name, "tlsPin": node["tls_fingerprint"]}
        client_id = None
        try:
            started = probe(managed_root, "start", local=configuration, token=enrollment["token"])
            live = wait(lambda: (value if value["status"]["connected"] else None) if (value := probe(managed_root))["status"] else None)
            first_pid = live["status"]["pid"]
            assert started["status"]["pid"] == first_pid
            clients = api("GET", "/clients")
            client = next(item for item in clients if item["name"] == name and item["online"])
            client_id = client["id"]
            identity = json.loads((Path(configuration["stateDir"]) / "identity.json").read_text())
            assert identity["client_id"] == client_id
            for definition in [managed_root / "preferences.json", Path(configuration["stateDir"]) / "control.desktop-job.json", Path(configuration["stateDir"]) / "control.launchd.plist"]:
                if definition.exists():
                    assert enrollment["token"] not in definition.read_text()
                    if os.name != "nt":
                        assert definition.stat().st_mode & 0o777 == 0o600
            # The command process has exited. A fresh runtime discovers the same Agent.
            assert probe(managed_root, "start")["status"]["pid"] == first_pid
            tunnel = api("POST", "/tunnels", {"name": "native-local-echo", "kind": "tcp", "client_id": client_id, "local_ip": "127.0.0.1", "local_port": echo_port, "remote_port": 0, "enabled": True, "reason": "Native data-plane E2E"})
            current = wait(lambda: item if (item := api("GET", f"/tunnels/{tunnel['id']}"))["state"] == "running" else None)
            wait(lambda: forwarded(current["status_port"]))
            applied = wait(lambda: value if (value := probe(managed_root))["tunnels"]["items"] else None)
            assert applied["tunnels"]["items"][0]["local_port"] == echo_port
            api("POST", f"/clients/{client_id}/restart", {"reason": "Native exit-75 restart E2E"})
            restarted = wait(lambda: value if (value := probe(managed_root))["status"] and value["status"]["connected"] and value["status"]["pid"] != first_pid else None, 25)
            current = wait(lambda: item if (item := api("GET", f"/tunnels/{tunnel['id']}"))["state"] == "running" else None)
            wait(lambda: forwarded(current["status_port"]))
            diagnostic = probe(managed_root, diagnostics=True)
            exported = Path(diagnostic["diagnostic"]["path"])
            contents = exported.read_text()
            assert enrollment["token"] not in contents and identity["key"] not in contents and configuration["controller"] not in contents
            assert isinstance(diagnostic["tail"]["cursor"], int)
            if os.name != "nt":
                assert exported.stat().st_mode & 0o777 == 0o600
            evidence["managed"] = {"enrolled": True, "survives_runtime_exit": True, "same_pid_on_repeat_start": True, "first_pid": first_pid, "restart_pid": restarted["status"]["pid"], "forwarding_before_and_after_restart": True, "private_token_free_jobs": True, "diagnostics_redacted": True}
            assert probe(managed_root, "stop")["status"] is None
        finally:
            stop_owned(managed_root, configuration)
            if not client_id:
                client_id = next((item["id"] for item in api("GET", "/clients") if item["name"] == name), None)
            if client_id:
                api("DELETE", f"/clients/{client_id}?reason=Native%20E2E%20cleanup")

        standalone_root = folder / "standalone-desktop"
        edge_port, remote_port = port(), port()
        edge_config = folder / "frps.toml"
        edge_config.write_text(f'bindAddr="127.0.0.1"\nbindPort={edge_port}\nproxyBindAddr="127.0.0.1"\n[auth]\nmethod="token"\ntoken="native-test-only"\n')
        client_config = folder / "frpc.toml"
        client_config.write_text(f'serverAddr="127.0.0.1"\nserverPort={edge_port}\n[auth]\nmethod="token"\ntoken="native-test-only"\n[[proxies]]\nname="standalone-echo"\ntype="tcp"\nlocalIP="127.0.0.1"\nlocalPort={echo_port}\nremotePort={remote_port}\n')
        edge = subprocess.Popen([str(AGENT), "--mode", "edge", "-c", str(edge_config)], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
        independent = {"binary": str(AGENT), "stateDir": str(folder / "standalone-agent"), "configFile": str(client_config)}
        try:
            started = probe(standalone_root, "start", local=independent)
            wait(lambda: forwarded(remote_port))
            live = probe(standalone_root)
            assert live["status"]["pid"] == started["status"]["pid"]
            assert live["tunnels"]["items"][0]["id"] == "standalone-echo"
            assert live["tunnels"]["items"][0]["remote_port"] == remote_port
            assert probe(standalone_root, "stop")["status"] is None
            evidence["standalone"] = {"configured_without_enrollment": True, "survives_runtime_exit": True, "full_config_applied": True, "real_forwarding": True, "authenticated_stop": True}
        finally:
            stop_owned(standalone_root, independent)
            edge.terminate()
            try:
                edge.wait(timeout=5)
            except subprocess.TimeoutExpired:
                edge.kill()
                edge.wait(timeout=5)
        echo.shutdown()
    output = ROOT / "target" / "ui" / "native-local-e2e.json"
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(evidence, indent=2) + "\n")
    print(json.dumps(evidence, indent=2))


if __name__ == "__main__":
    main()
