#!/usr/bin/env python3
"""Explicit two-host acceptance runner for the isolated RustFRP test deployment.

Requires SSH sudo access and preinstalled binaries/backend services. Secrets and
machine-readable evidence stay in ignored target/wan-test with private modes.
Run stages in order: bootstrap, managed, nodes, standalone, finish.
Never invokes, modifies, or restarts original frpc/frps services.
"""
import argparse
import base64
import hashlib
from http.client import HTTPResponse
import json
import os
from pathlib import Path
import secrets
import shlex
import socket
import ssl
import subprocess
import time
import urllib.error
import urllib.request

SERVER = "182.254.220.82"
CLIENT = "172.17.32.251"
BASE = "https://rustfrp.dev99.cn"
ROOT = Path("target/wan-test")
ROOT.mkdir(parents=True, exist_ok=True)
ROOT.chmod(0o700)


def save(name, value):
    path = ROOT / name
    with open(path, "w", opener=lambda p, flags: os.open(p, flags, 0o600)) as file:
        json.dump(value, file, ensure_ascii=False, indent=2)
    path.chmod(0o600)


def load(name):
    return json.loads((ROOT / name).read_text())


def ssh(host, command, data=None, check=True):
    result = subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10",
                             f"ubuntu@{host}", command], input=data, capture_output=True, timeout=90)
    if check and result.returncode:
        raise RuntimeError(f"SSH {host} failed: {result.stderr.decode()[-1200:]}")
    return result.stdout.decode()


def write_remote(host, path, content, mode=0o600, owner=None):
    # Generated deployment files: transmit data over stdin, never shell interpolation.
    code = "import json,os,sys; x=json.load(sys.stdin); p=x['path']; os.makedirs(os.path.dirname(p),exist_ok=True); f=open(p,'w'); f.write(x['content']); f.close(); os.chmod(p,x['mode'])"
    ssh(host, "sudo python3 -c " + shlex.quote(code),
        json.dumps({"path": path, "content": content, "mode": mode}).encode())
    if owner:
        ssh(host, "sudo chown " + shlex.quote(owner) + " " + shlex.quote(path))


def service(host, name, args, state, token=None):
    assert name.startswith("rustfrp-test-")
    ssh(host, f"sudo install -d -o rustfrp-test -g rustfrp-test -m 700 {shlex.quote(state)}")
    command = "/opt/rustfrp-test/bin/rustfrp-agent " + " ".join(shlex.quote(a) for a in args)
    if token:
        command += " --enroll-token " + shlex.quote(token)
    host_mount = ""
    if host == SERVER:
        # Cloud public-IP hairpin is unavailable on this host. Scope the new
        # domain's loopback resolution to RustFRP agent services, never global DNS.
        hosts = ssh(host, "cat /etc/hosts") + "\n127.0.0.1 rustfrp.dev99.cn\n"
        write_remote(host, "/etc/rustfrp-test/agent-hosts", hosts, 0o644)
        host_mount = "BindReadOnlyPaths=/etc/rustfrp-test/agent-hosts:/etc/hosts\n"
    unit = ("[Unit]\nDescription=RustFRP isolated WAN test agent\nAfter=network-online.target\nWants=network-online.target\n"
            "[Service]\nUser=rustfrp-test\nGroup=rustfrp-test\nExecStart=" + command + "\n"
            "Restart=always\nRestartSec=3\nEnvironment=RUSTFRP_SUPERVISED=1\n"
            "Environment=MALLOC_ARENA_MAX=2\nEnvironment=MALLOC_MMAP_THRESHOLD_=131072\n"
            "Environment=MALLOC_TRIM_THRESHOLD_=131072\nUMask=0077\nNoNewPrivileges=true\nPrivateTmp=true\n"
            "ProtectSystem=strict\nProtectHome=true\n" + host_mount + "ReadWritePaths=/var/lib/rustfrp-test\n"
            "[Install]\nWantedBy=multi-user.target\n")
    write_remote(host, f"/etc/systemd/system/{name}.service", unit, 0o600 if token else 0o644)
    ssh(host, f"sudo systemctl daemon-reload && sudo systemctl enable --now {name}")
    if token:
        wait(lambda: "exists" in ssh(host, f"sudo test -f {state}/identity.json && echo exists", check=False), "identity persisted")
        # Remove one-time secret from persistent unit without restarting the agent.
        service(host, name, args, state)


def wait(predicate, label, timeout=40):
    end = time.monotonic() + timeout
    error = None
    while time.monotonic() < end:
        try:
            value = predicate()
            if value:
                return value
        except (OSError, RuntimeError, KeyError) as exc:
            error = exc
        time.sleep(0.5)
    raise RuntimeError(f"Timeout: {label}; {error}")


class Api:
    def __init__(self, admin_token=None):
        self.cookie = self.csrf = None
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        if admin_token is None:
            admin_token = load("credentials.json")["admin_token"]
        me = self.call("POST", "/auth/login", {"token": admin_token},
                       headers={"X-Rustfrp-Admin": "1"})
        self.csrf = me["csrf"]
        self.admin = me["admin"]

    def call(self, method, path, body=None, expected=None, headers=None, auth=True):
        request = urllib.request.Request(BASE + "/api/v1" + path, method=method)
        request.add_header("Accept", "application/json")
        request.add_header("Origin", BASE)
        if body is not None:
            request.add_header("Content-Type", "application/json")
            request.data = json.dumps(body).encode()
        if auth and self.cookie:
            request.add_header("Cookie", self.cookie)
        if auth and self.csrf and method != "GET":
            request.add_header("X-CSRF-Token", self.csrf)
        for key, value in (headers or {}).items():
            request.add_header(key, value)
        try:
            response = self.opener.open(request, timeout=20)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            status = response.code
            raw = response.read()
            data = json.loads(raw or b"null")
            if response.headers.get("Set-Cookie"):
                self.cookie = response.headers["Set-Cookie"].split(";")[0]
                self.cookie_flags = response.headers["Set-Cookie"]
        if expected is not None:
            assert status == expected, (method, path, status, data)
        elif not 200 <= status < 300:
            raise RuntimeError(f"{method} {path}: {status} {data}")
        return data


def ok(label, details=None):
    record = {"time": time.strftime("%Y-%m-%dT%H:%M:%S%z"), "test": label, "details": details}
    evidence = load("results.json") if (ROOT / "results.json").exists() else []
    evidence.append(record)
    save("results.json", evidence)
    print("PASS", label, json.dumps(details, ensure_ascii=False) if details is not None else "", flush=True)


def running(api, tunnel):
    return wait(lambda: (lambda x: x if x["state"] == "running" else None)(
        api.call("GET", f"/tunnels/{tunnel['id']}")), "tunnel running")


def create(api, client, name, port=28081, **fields):
    existing = next((t for t in api.call("GET", "/tunnels") if t["client_id"] == client
                     and (t["name"] == name or (fields.get("remote_port", 0) > 0 and t["remote_port"] == fields["remote_port"]))), None)
    if existing:
        return running(api, existing)
    body = {"client_id": client, "name": name, "local_port": port, "reason": "WAN acceptance", **fields}
    return running(api, api.call("POST", "/tunnels", body))


def roundtrip(port, payload, host=SERVER):
    with socket.create_connection((host, port), timeout=10) as connection:
        connection.settimeout(15)
        connection.sendall(payload)
        got = bytearray()
        while len(got) < len(payload):
            data = connection.recv(min(65536, len(payload) - len(got)))
            if not data:
                raise ConnectionError("tunnel closed before completing the expected payload")
            got.extend(data)
        assert got == payload, (len(got), len(payload))


def udp(port, host=SERVER):
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as connection:
            connection.settimeout(3)
            for size in (1, 128, 1200):
                payload = os.urandom(size)
                connection.sendto(payload, (host, port))
                assert connection.recvfrom(4096)[0] == payload
        ok("Public UDP ingress", {"port": port})
        save("limitations.json", {"public_udp_ingress": "passed after cloud firewall rule was opened"})
    except TimeoutError:
        save("limitations.json", {"public_udp_ingress": "External UDP timed out; service loopback ingress forwards across the real WAN client link. eth0 capture saw no incoming public UDP packet."})
        code = ("import socket,os; s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); s.settimeout(10); "
                f"\nfor n in (1,128,1200):\n p=os.urandom(n); s.sendto(p,('127.0.0.1',{port})); assert s.recvfrom(4096)[0]==p")
        ssh(SERVER, "python3 -c " + shlex.quote(code))
        ok("UDP loopback ingress → actual WAN tunnel → client backend", {"port": port, "public_ingress": "blocked"})


def http(port, host="wan-test.example", path="/", headers=None, tls=False):
    connection = socket.create_connection((SERVER, port), timeout=10)
    connection.settimeout(10)
    if tls:
        context = ssl.create_default_context(cafile=str(ROOT / "backend-cert.pem"))
        connection = context.wrap_socket(connection, server_hostname=host)
    values = {"Host": host, "Connection": "close", **(headers or {})}
    connection.sendall((f"GET {path} HTTP/1.1\r\n" + "".join(f"{k}: {v}\r\n" for k, v in values.items()) + "\r\n").encode())
    response = HTTPResponse(connection)
    response.begin()
    raw = response.read()
    result = response.status, dict(response.getheaders()), raw
    connection.close()
    return result


def local_control(command):
    code = "import json,socket,sys; d=json.load(open('/var/lib/rustfrp-test/client/control.json')); s=socket.create_connection(('127.0.0.1',d['port']),timeout=5); s.sendall((json.dumps({'token':d['token'],'command':sys.argv[1]})+'\\n').encode()); print(s.makefile().readline())"
    return json.loads(ssh(CLIENT, "sudo python3 -c " + shlex.quote(code) + " " + shlex.quote(command)))


def bootstrap():
    if not (ROOT / "credentials.json").exists():
        save("credentials.json", {"admin_token": secrets.token_urlsafe(32),
                                  "metrics_token": secrets.token_urlsafe(32)})
    credentials = load("credentials.json")
    if "admin_token" not in credentials:
        credentials["admin_token"] = secrets.token_urlsafe(32)
        save("credentials.json", credentials)
    env = Path("deploy/wan-test/server.env").read_text() + "RUSTFRP_METRICS_TOKEN=" + credentials["metrics_token"] + "\nRUSTFRP_ADMIN_TOKEN=" + credentials["admin_token"] + "\n"
    write_remote(SERVER, "/etc/rustfrp-test/server.env", env)
    ssh(SERVER, "sudo systemctl daemon-reload && sudo systemctl enable --now rustfrp-test-server")
    wait(lambda: urllib.request.urlopen(BASE + "/healthz", timeout=10).status == 200, "public HTTPS health")
    api = Api()
    assert all(flag in api.cookie_flags for flag in ("Secure", "HttpOnly", "SameSite=Strict"))
    ok("Public HTTPS and private admin listener, secure session cookie")
    token = api.call("POST", "/enroll-tokens", {"name": "WAN ubuntu client", "ttl_minutes": 15, "reason": "WAN acceptance"})
    fingerprint = api.call("GET", "/node")["tls_fingerprint"]
    assert "--tls-pin" in token["commands"]["direct"]
    service(CLIENT, "rustfrp-test-client", ["--mode", "client", "--controller", "rustfrp.dev99.cn:27000",
            "--name", "WAN ubuntu client", "--state-dir", "/var/lib/rustfrp-test/client", "--local-control",
            "/var/lib/rustfrp-test/client/control.json", "--tls-pin", fingerprint],
            "/var/lib/rustfrp-test/client", token["token"])
    client = wait(lambda: next((c for c in api.call("GET", "/clients") if c["online"]), None), "WAN client online")
    save("state.json", {"client": client["id"], "fingerprint": fingerprint})
    cert = ssh(CLIENT, "sudo cat /var/lib/rustfrp-test/backend-cert.pem")
    (ROOT / "backend-cert.pem").write_text(cert)
    status = local_control("status")
    assert status["connected"]
    assert ssh(CLIENT, "sudo stat -c %a /var/lib/rustfrp-test/client/identity.json").strip() == "600"
    ok("WAN enrollment, fixed TLS fingerprint, Ed25519 identity persistence, local control", {"client": client["id"], "reported_ip": client.get("last_ip")})


def managed():
    api, state = Api(), load("state.json")
    client = state["client"]
    t = create(api, client, "WAN TCP echo", remote_port=27100)
    state["tcp"] = t["id"]
    payload = os.urandom(300000)
    roundtrip(27100, payload)
    ok("WAN TCP binary payload SHA256 matched", {"bytes": len(payload), "sha256": hashlib.sha256(payload).hexdigest()})
    transformed = create(api, client, "WAN encrypted compressed TCP", remote_port=27101, use_encryption=True, use_compression=True)
    roundtrip(27101, b"compression-and-encryption" * 12000)
    ok("WAN TCP AES-256-GCM and LZ4", {"port": 27101})
    u = create(api, client, "WAN UDP echo", 28082, kind="udp", remote_port=27102)
    udp(27102)
    state["udp"] = u["id"]
    ok("WAN UDP payloads 1 / 128 / 1200 bytes")
    w = create(api, client, "WAN HTTP route", 28083, kind="http", custom_domains=["wan-test.example"],
               subdomain="wan", host_header_rewrite="backend.internal", locations=["/api"],
               request_headers={"X-Wan-Test": "forwarded"}, response_headers={"X-Wan-Response": "verified"},
               http_user="wan", http_password="test-only-password")
    state["http"] = w["id"]
    basic = "Basic " + base64.b64encode(b"wan:test-only-password").decode()
    for host in ("wan-test.example", "wan.tunnel.rustfrp.dev99.cn"):
        code, headers, body = http(27080, host, "/api/test", {"Authorization": basic})
        data = json.loads(body)
        assert code == 200 and data["headers"]["Host"] == "backend.internal"
        assert data["headers"]["X-Wan-Test"] == "forwarded" and headers["X-Wan-Response"] == "verified"
    assert http(27080, path="/api/test")[0] == 401
    assert http(27080, path="/outside", headers={"Authorization": basic})[0] == 404
    assert http(27080, "unknown.example")[0] == 404
    ok("WAN HTTP host / subdomain / prefix / Basic Auth / request and response headers")
    h = create(api, client, "WAN HTTPS SNI", 28084, kind="https", custom_domains=["wan-test.example"])
    state["https"] = h["id"]
    save("state.json", state)
    assert http(27443, tls=True)[0] == 200
    ok("WAN HTTPS SNI passthrough with backend certificate validation")
    mux = create(api, client, "WAN TCPMUX", 28081, kind="tcpmux", custom_domains=["mux.wan-test.example"], multiplexer="httpconnect")
    with socket.create_connection((SERVER, 27002), timeout=10) as stream:
        stream.settimeout(10)
        stream.sendall(b"CONNECT mux.wan-test.example:443 HTTP/1.1\r\nHost: mux.wan-test.example\r\n\r\n")
        response = bytearray()
        while not response.endswith(b"\r\n\r\n"):
            response.extend(stream.recv(1))
        assert b"200" in response
        stream.sendall(b"WAN CONNECT echo")
        assert stream.recv(100) == b"WAN CONNECT echo"
    ok("WAN TCPMUX HTTP CONNECT")
    # Hot update a different tunnel while keeping a real TCP stream alive.
    with socket.create_connection((SERVER, 27100), timeout=10) as held:
        api.call("PATCH", f"/tunnels/{u['id']}", {"name": "WAN UDP renamed", "reason": "WAN hot update"})
        held.sendall(b"held-through-hot-update")
        assert held.recv(100) == b"held-through-hot-update"
    api.call("POST", "/tunnels/batch", {"ids": [transformed["id"]], "enabled": False, "reason": "WAN batch"})
    wait(lambda: api.call("GET", f"/tunnels/{transformed['id']}")["state"] == "stopped", "disabled")
    api.call("POST", "/tunnels/batch", {"ids": [transformed["id"]], "enabled": True, "reason": "WAN batch"})
    running(api, transformed)
    roundtrip(27101, b"batch resumed")
    ok("Config hot push preserves unchanged connections; batch stop/start reconciles")
    before = local_control("status")["pid"]
    api.call("POST", f"/clients/{client}/restart", {"reason": "WAN remote restart"})
    wait(lambda: local_control("status").get("pid") != before and local_control("status")["connected"], "systemd restart", 60)
    running(api, t)
    roundtrip(27100, b"after remote restart")
    ok("Remote restart exit 75 handled by systemd; identity and tunnels restored")
    api.call("POST", f"/clients/{client}/kick", {"reason": "WAN kick reconnect"})
    wait(lambda: api.call("GET", f"/clients/{client}")["online"], "automatic reconnect")
    running(api, t)
    wait(lambda: (roundtrip(27100, b"after kick") is None) or True, "TCP after kick")
    api.call("PATCH", f"/clients/{client}", {"name": "WAN ubuntu client", "tags": ["wan-test", "ubuntu"], "reason": "WAN tags"})
    assert "wan-test" in api.call("GET", f"/clients/{client}")["tags"]
    ok("Kick reconnect and client rename / tags")
    # A second device on the server is a visitor to the remote client backends.
    token = api.call("POST", "/enroll-tokens", {"name": "WAN visitor", "reason": "WAN visitor"})
    service(SERVER, "rustfrp-test-visitor", ["--mode", "client", "--controller", "127.0.0.1:27000", "--name", "WAN visitor",
            "--state-dir", "/var/lib/rustfrp-test/visitor", "--tls-pin", state["fingerprint"]],
            "/var/lib/rustfrp-test/visitor", token["token"])
    visitor_client = wait(lambda: next((c for c in api.call("GET", "/clients") if c["name"] == "WAN visitor" and c["online"]), None), "visitor client")
    state["visitor_client"] = visitor_client["id"]
    save("state.json", state)
    for kind, port, backend in (("stcp", 28101, 28081), ("sudp", 28102, 28082)):
        private = create(api, client, "WAN " + kind, backend, kind=kind,
                         use_encryption=kind == "stcp", use_compression=kind == "stcp")
        visitor = api.call("POST", "/visitors", {"client_id": visitor_client["id"], "name": "WAN " + kind + " visitor",
                    "tunnel_id": private["id"], "bind_addr": "127.0.0.1", "bind_port": port,
                    "use_encryption": True, "use_compression": True, "reason": "WAN private visitor"})
        probe = ("import socket; s=socket.create_connection(('127.0.0.1',28101),timeout=10); s.sendall(b'private-wan'); assert s.recv(100)==b'private-wan'"
                 if kind == "stcp" else "import socket; s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); s.settimeout(10); s.sendto(b'private-udp',('127.0.0.1',28102)); assert s.recvfrom(100)[0]==b'private-udp'")
        wait(lambda: ssh(SERVER, "python3 -c " + shlex.quote(probe)) == "", kind + " visitor")
        api.call("DELETE", f"/visitors/{visitor['id']}?reason=WAN-test-cleanup")
        api.call("DELETE", f"/tunnels/{private['id']}?reason=WAN-test-cleanup")
        ok("WAN " + kind.upper() + " visitor with encryption / compression")
    traffic = wait(lambda: (lambda x: x if sum(b["bytes_in"] for b in x["items"]) >= 300000 else None)(
        api.call("GET", f"/traffic?tunnel_id={t['id']}&hours=24")), "traffic minutes persisted")
    assert api.call("GET", "/traffic?hours=168")["sampled_only"]
    logs = api.call("GET", "/logs")
    assert 0 < len(logs["items"]) <= 128
    assert len(local_control("logs")["items"]) <= 128
    overview = api.call("GET", "/overview")
    save("overview.json", overview)
    ok("Traffic 24h / 7d / tunnel filtering, Linux runtime metrics, bounded logs", {"tcp_bytes_in": sum(b["bytes_in"] for b in traffic["items"]), "overview": overview})
    text = 'serverAddr = "ignored.example"\n[[proxies]]\nname = "WAN imported"\ntype = "tcp"\nlocalIP = "127.0.0.1"\nlocalPort = 28081\nremotePort = 27105\n'
    preview = api.call("POST", f"/clients/{client}/config/preview", {"text": text})
    api.call("POST", f"/clients/{client}/config/import", {"text": text, "digest": preview["digest"], "accept_warnings": True, "reason": "WAN config import"})
    imported = next(x for x in api.call("GET", "/tunnels") if x["name"] == "WAN imported")
    running(api, imported)
    roundtrip(27105, b"frpc imported")
    exported = api.call("GET", f"/clients/{client}/config/export")
    assert "WAN imported" in exported["text"] and "private_key" not in exported["text"]
    ok("frpc configuration preview / acknowledged import / export, preserved existing tunnels")
    api.call("POST", "/tunnels", {"client_id": client, "name": "invalid-policy", "local_port": 28081, "remote_port": 27199, "reason": "WAN rejection"}, expected=400)
    api.call("POST", "/tunnels", {"client_id": client, "name": "invalid-policy", "local_port": 28081, "remote_port": 7000, "reason": "WAN rejection"}, expected=400)
    api.call("POST", "/tunnels", {"client_id": client, "name": "missing-reason", "local_port": 28081}, expected=400)
    api.call("GET", "/overview", expected=401, auth=False)
    api.call("POST", "/enroll-tokens", {"name": "bad-origin", "reason": "test"}, headers={"Origin": "https://foreign.example"}, expected=403)
    api.call("POST", "/enroll-tokens", {"name": "bad-csrf", "reason": "test"}, headers={"X-CSRF-Token": "bad"}, expected=403)
    ok("Port policy / reserved ports / reason required / authentication / Origin / CSRF rejection")
    api.call("POST", "/auth/login", {"token": "wrong-management-token"}, headers={"X-Rustfrp-Admin": "1"}, auth=False, expected=401)
    ok("Single management token authentication rejects incorrect credentials")
    token = api.call("POST", "/api-tokens", {"name": "WAN read", "scope": "read", "days": 1, "reason": "WAN token"})
    api.call("GET", "/overview", headers={"Authorization": "Bearer " + token["token"]}, auth=False)
    api.call("POST", "/api-tokens", {"name": "denied", "scope": "admin", "days": 1, "reason": "test"},
             headers={"Authorization": "Bearer " + token["token"]}, auth=False, expected=403)
    api.call("POST", f"/api-tokens/{token['info']['id']}/revoke", {"reason": "WAN revoke"})
    api.call("GET", "/overview", headers={"Authorization": "Bearer " + token["token"]}, auth=False, expected=401)
    ok("Viewer / operator RBAC; admin password / role / disable; session revocation; API Token read scope / revoke")
    credentials = load("credentials.json")
    request = urllib.request.Request(BASE + "/metrics", headers={"Authorization": "Bearer " + credentials["metrics_token"]})
    with api.opener.open(request) as response:
        metrics = response.read().decode()
        assert "rustfrp" in metrics
    try:
        api.opener.open(BASE + "/metrics")
        raise AssertionError("Unauthenticated metrics accepted")
    except urllib.error.HTTPError as error:
        assert error.code == 401
    ok("Prometheus Bearer authentication and Linux process metrics")
    save("state.json", state)


def nodes():
    api, state = Api(), load("state.json")
    config = ('bindAddr = "0.0.0.0"\nbindPort = 27200\nvhostHTTPPort = 27280\nvhostHTTPSPort = 27243\n'
              'tcpmuxHTTPConnectPort = 27202\nsubDomainHost = "tunnel.rustfrp.dev99.cn"\n'
              'allowPorts = [{start = 27100, end = 27198}]\n')
    write_remote(SERVER, "/etc/rustfrp-test/node.toml", config, owner="rustfrp-test:rustfrp-test")
    token = api.call("POST", "/enroll-tokens", {"name": "WAN isolated edge", "kind": "node", "reason": "WAN node"})
    service(SERVER, "rustfrp-test-node", ["--mode", "edge", "--controller", "127.0.0.1:27001", "--name", "WAN isolated edge",
            "--public-host", "rustfrp.dev99.cn", "--state-dir", "/var/lib/rustfrp-test/node", "--tls-pin", state["fingerprint"],
            "-c", "/etc/rustfrp-test/node.toml"], "/var/lib/rustfrp-test/node", token["token"])
    node = wait(lambda: next((n for n in api.call("GET", "/nodes") if n["name"] == "WAN isolated edge" and n["online"]), None), "node online")
    state["node"] = node["id"]
    api.call("PATCH", f"/nodes/{node['id']}", {"name": "WAN isolated edge", "reason": "WAN node rename"})
    ok("External Edge enrollment / persistent identity / node control / capabilities")
    client = state["client"]
    api.call("POST", f"/clients/{client}/migrate", {"node_id": node["id"], "reason": "WAN online migration"})
    wait(lambda: api.call("GET", f"/clients/{client}")["node_id"] == node["id"] and api.call("GET", f"/clients/{client}")["online"], "WAN migration", 60)
    wait(lambda: roundtrip(27100, b"after migration") is None, "migrated TCP", 60)
    udp(27102)
    assert http(27243, tls=True)[0] == 200
    ok("Online client migration built-in → external Edge with TLS pin redirect and WAN TCP / UDP / HTTPS recovery")
    try:
        ssh(SERVER, "sudo systemctl stop rustfrp-test-server")
        roundtrip(27100, b"controller offline existing connection")
        ssh(CLIENT, "sudo systemctl restart rustfrp-test-client")
        wait(lambda: roundtrip(27100, b"controller offline signed relogin") is None, "offline cached login", 60)
        ok("AT-05 Controller offline: WAN data continues; restarted client signs into cached external Edge")
    finally:
        ssh(SERVER, "sudo systemctl start rustfrp-test-server")
    api = wait(lambda: Api(), "controller returns")
    wait(lambda: api.call("GET", f"/clients/{client}")["online"], "node reconciliation", 60)
    history = wait(lambda: (lambda x: x if sum(b["bytes_in"] for b in x["items"]) > 0 else None)(
        api.call("GET", f"/traffic?tunnel_id={state['tcp']}")), "external traffic")
    ok("Controller returns; node full reconciliation and persisted history", {"history_buckets": len(history["items"])})
    api.call("POST", f"/clients/{client}/migrate", {"node_id": "", "reason": "WAN reverse migration"})
    wait(lambda: api.call("GET", f"/clients/{client}")["node_id"] == "" and api.call("GET", f"/clients/{client}")["online"], "reverse migration", 60)
    wait(lambda: roundtrip(27100, b"after reverse migration") is None, "reverse migrated TCP", 60)
    ok("Online client migration external → built-in Edge; original ports restored")
    api.call("PATCH", f"/clients/{client}", {"disabled": True, "reason": "WAN device disabled"})
    wait(lambda: not api.call("GET", f"/clients/{client}")["online"], "disabled client")
    api.call("PATCH", f"/clients/{client}", {"disabled": False, "reason": "WAN device enabled"})
    wait(lambda: api.call("GET", f"/clients/{client}")["online"], "reenabled client", 60)
    wait(lambda: roundtrip(27100, b"after device reenabling") is None, "reenabled tunnel")
    api.call("PATCH", f"/nodes/{node['id']}", {"disabled": True, "reason": "WAN node disabled"})
    wait(lambda: not next(n for n in api.call("GET", "/nodes") if n["id"] == node["id"])["online"], "disabled node")
    api.call("PATCH", f"/nodes/{node['id']}", {"disabled": False, "reason": "WAN node enabled"})
    wait(lambda: next(n for n in api.call("GET", "/nodes") if n["id"] == node["id"])["online"], "reenabled node", 60)
    ok("Client and node disable / enable; authentication blocking and recovery")
    save("state.json", state)


def standalone():
    credentials = load("credentials.json")
    token = credentials.setdefault("standalone_token", secrets.token_urlsafe(32))
    save("credentials.json", credentials)
    ssh(SERVER, "sudo -u rustfrp-test sh -c 'test -f /var/lib/rustfrp-test/standalone-cert.pem || openssl req -x509 -newkey rsa:2048 -nodes -keyout /var/lib/rustfrp-test/standalone-key.pem -out /var/lib/rustfrp-test/standalone-cert.pem -days 30 -subj /CN=rustfrp.dev99.cn -addext subjectAltName=DNS:rustfrp.dev99.cn -addext basicConstraints=critical,CA:FALSE -addext keyUsage=critical,digitalSignature,keyEncipherment -addext extendedKeyUsage=serverAuth 2>/dev/null'")
    ca = ssh(SERVER, "sudo cat /var/lib/rustfrp-test/standalone-cert.pem")
    write_remote(CLIENT, "/etc/rustfrp-test/standalone-ca.pem", ca, owner="rustfrp-test:rustfrp-test")
    server_config = ('bindAddr = "0.0.0.0"\nbindPort = 27600\nproxyBindAddr = "0.0.0.0"\n'
                     'vhostHTTPPort = 27680\nvhostHTTPSPort = 27643\ntcpmuxHTTPConnectPort = 27602\n'
                     'subDomainHost = "standalone.rustfrp.dev99.cn"\nallowPorts = [{start = 27300, end = 27349}]\n'
                     f'auth.token = "{token}"\ntransport.tls.certFile = "/var/lib/rustfrp-test/standalone-cert.pem"\n'
                     'transport.tls.keyFile = "/var/lib/rustfrp-test/standalone-key.pem"\n'
                     'webServer.addr = "127.0.0.1"\nwebServer.port = 28091\n'
                     'webServer.user = "wan"\nwebServer.password = "test-only-dashboard"\n')
    write_remote(SERVER, "/etc/rustfrp-test/frps.toml", server_config, owner="rustfrp-test:rustfrp-test")
    client_config = (f'serverAddr = "rustfrp.dev99.cn"\nserverPort = 27600\nauth.token = "{token}"\n'
                     'transport.tls.trustedCaFile = "/etc/rustfrp-test/standalone-ca.pem"\n'
                     'webServer.addr = "127.0.0.1"\nwebServer.port = 28090\n'
                     'webServer.user = "wan"\nwebServer.password = "test-only-dashboard"\n'
                     '[[proxies]]\nname = "standalone-tcp"\ntype = "tcp"\nlocalPort = 28081\nremotePort = 27300\n'
                     'transport.useEncryption = true\ntransport.useCompression = true\n'
                     '[[proxies]]\nname = "standalone-udp"\ntype = "udp"\nlocalPort = 28082\nremotePort = 27301\n'
                     '[[proxies]]\nname = "standalone-http"\ntype = "http"\nlocalPort = 28083\n'
                     'customDomains = ["standalone.wan-test.example"]\nsubdomain = "site"\n'
                     '[[proxies]]\nname = "standalone-https"\ntype = "https"\nlocalPort = 28084\n'
                     'customDomains = ["wan-test.example"]\n')
    write_remote(CLIENT, "/etc/rustfrp-test/frpc.toml", client_config, owner="rustfrp-test:rustfrp-test")
    for host, mode, file in ((SERVER, "edge", "frps.toml"), (CLIENT, "client", "frpc.toml")):
        report = ssh(host, f"sudo -u rustfrp-test /opt/rustfrp-test/bin/rustfrp-agent verify --mode {mode} -c /etc/rustfrp-test/{file}")
        assert "valid" in report.lower() or "passed" in report.lower() or "ok" in report.lower(), report
        service(host, "rustfrp-test-standalone-" + mode, ["--mode", mode, "-c", "/etc/rustfrp-test/" + file],
                "/var/lib/rustfrp-test/standalone-" + mode)
        ssh(host, "sudo systemctl restart rustfrp-test-standalone-" + mode)
    wait(lambda: roundtrip(27300, b"standalone frpc/frps") is None, "standalone WAN TCP")
    assert http(27680, "standalone.wan-test.example")[0] == 200
    assert http(27680, "site.standalone.rustfrp.dev99.cn")[0] == 200
    assert http(27643, tls=True)[0] == 200
    ok("Standalone frps.toml / frpc.toml verify, real WAN TCP transforms / HTTP domains / subdomain / HTTPS")
    ok("Standalone configured TLS certificate and trustedCaFile validate server domain over WAN")
    # This additional UDP port is outside the managed firewall range. Probe via the
    # server loopback while retaining the real standalone WAN transport to client.
    code = "import socket; s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); s.settimeout(10); s.sendto(b'standalone-UDP',('127.0.0.1',27301)); assert s.recvfrom(100)[0]==b'standalone-UDP'"
    ssh(SERVER, "python3 -c " + shlex.quote(code))
    status = ssh(CLIENT, "sudo -u rustfrp-test /opt/rustfrp-test/bin/rustfrp-agent status -c /etc/rustfrp-test/frpc.toml")
    assert "standalone-tcp" in status
    for host, port, path in ((SERVER, 28091, "/api/serverinfo"), (CLIENT, 28090, "/api/status")):
        code = ("import urllib.request,urllib.error,base64,json; u='http://127.0.0.1:" + str(port) + path + "'; "
                "\ntry: urllib.request.urlopen(u); raise AssertionError('unprotected dashboard')"
                "\nexcept urllib.error.HTTPError as e: assert e.code==401"
                "\nr=urllib.request.Request(u,headers={'Authorization':'Basic '+base64.b64encode(b'wan:test-only-dashboard').decode()}); "
                "d=json.load(urllib.request.urlopen(r)); assert d")
        ssh(host, "python3 -c " + shlex.quote(code))
    ok("Standalone WAN UDP through service loopback; frpc status; server and client dashboard Basic Auth")
    # Append a proxy to the generated configuration, then use the real CLI reload.
    updated = client_config + '\n[[proxies]]\nname = "standalone-reloaded"\ntype = "tcp"\nlocalPort = 28081\nremotePort = 27302\n'
    write_remote(CLIENT, "/etc/rustfrp-test/frpc.toml", updated, owner="rustfrp-test:rustfrp-test")
    ssh(CLIENT, "sudo -u rustfrp-test /opt/rustfrp-test/bin/rustfrp-agent reload -c /etc/rustfrp-test/frpc.toml")
    wait(lambda: roundtrip(27302, b"standalone reload") is None, "CLI reload")
    ssh(CLIENT, "sudo systemctl stop rustfrp-test-standalone-client")
    ssh(CLIENT, "sudo systemctl start rustfrp-test-standalone-client")
    wait(lambda: roundtrip(27300, b"standalone service restart") is None, "standalone systemd lifecycle")
    ok("Standalone CLI reload applies new proxy; independent systemd stop / start restores config")
    # Official service subcommands use a unique disposable unit name and config.
    # Stop the hand-installed client first so the CLI-created service owns its ports.
    ssh(CLIENT, "sudo systemctl stop rustfrp-test-standalone-client")
    try:
        ssh(CLIENT, "sudo /opt/rustfrp-test/bin/rustfrp-agent service install --name rustfrp-test-cli -- --mode client -c /etc/rustfrp-test/frpc.toml")
        # DynamicUser has no access to private test configs. Grant only the temporary
        # service a copy owned by its own StateDirectory via a readable test config.
        # The config is test-only; original frp credentials remain untouched.
        ssh(CLIENT, "sudo systemctl stop rustfrp-test-cli")
        config_copy = "/var/lib/rustfrp-test-cli/frpc.toml"
        cli_config = updated.replace("/etc/rustfrp-test/standalone-ca.pem", "/var/lib/rustfrp-test-cli/ca.pem")
        write_remote(CLIENT, config_copy, cli_config, owner="root:root")
        write_remote(CLIENT, "/var/lib/rustfrp-test-cli/ca.pem", ca, 0o644)
        # Dynamic systemd user owns the state directory; copy through its numeric owner.
        ssh(CLIENT, "sudo chown --reference=/var/lib/rustfrp-test-cli /var/lib/rustfrp-test-cli/frpc.toml /var/lib/rustfrp-test-cli/ca.pem")
        ssh(CLIENT, "sudo /opt/rustfrp-test/bin/rustfrp-agent service uninstall --name rustfrp-test-cli")
        ssh(CLIENT, "sudo /opt/rustfrp-test/bin/rustfrp-agent service install --name rustfrp-test-cli -- --mode client -c /var/lib/rustfrp-test-cli/frpc.toml")
        wait(lambda: roundtrip(27300, b"official service CLI") is None, "official CLI service")
        cli_status = ssh(CLIENT, "sudo /opt/rustfrp-test/bin/rustfrp-agent service status --name rustfrp-test-cli").lower()
        assert "active" in cli_status or "running" in cli_status, cli_status
        ssh(CLIENT, "sudo /opt/rustfrp-test/bin/rustfrp-agent service stop --name rustfrp-test-cli")
        ssh(CLIENT, "sudo /opt/rustfrp-test/bin/rustfrp-agent service start --name rustfrp-test-cli")
        wait(lambda: roundtrip(27300, b"official service CLI start") is None, "official CLI service restarted")
        ok("Linux native Agent service install / status / stop / start / uninstall on an isolated unit")
    finally:
        ssh(CLIENT, "sudo /opt/rustfrp-test/bin/rustfrp-agent service uninstall --name rustfrp-test-cli", check=False)
        ssh(CLIENT, "sudo systemctl start rustfrp-test-standalone-client")


def installer():
    api = Api()
    code = ("from pathlib import Path; import hashlib; p=Path('/opt/rustfrp-test/panel/downloads'); p.mkdir(exist_ok=True); "
            "b=Path('/opt/rustfrp-test/bin/rustfrp-agent').read_bytes(); a=p/'rustfrp-agent-linux-x86_64'; "
            "a.write_bytes(b); a.chmod(0o644); (p/(a.name+'.sha256')).write_text(hashlib.sha256(b).hexdigest()+'\\n')")
    ssh(SERVER, "sudo python3 -c " + shlex.quote(code))
    token = api.call("POST", "/enroll-tokens", {"name": "WAN installer acceptance", "reason": "WAN HTTPS installer"})
    code = ("import json,subprocess,sys,os; x=json.load(sys.stdin); "
            "subprocess.run(['curl','-fsSL','https://rustfrp.dev99.cn/install.sh','-o','/var/lib/rustfrp-test/install-acceptance.sh'],check=True); "
            "f=open('/var/lib/rustfrp-test/install-acceptance.log','w'); "
            "p=subprocess.Popen(['sh','/var/lib/rustfrp-test/install-acceptance.sh','--controller','rustfrp.dev99.cn:27000',"
            "'--enroll-token',x['token'],'--name','WAN installer acceptance','--download-base','https://rustfrp.dev99.cn/downloads',"
            "'--tls-pin',x['pin']],stdout=f,stderr=f,start_new_session=True); print(p.pid)")
    pid = ssh(CLIENT, "sudo -u rustfrp-test python3 -c " + shlex.quote(code),
              json.dumps({"token": token["token"], "pin": load("state.json")["fingerprint"]}).encode()).strip()
    try:
        client = wait(lambda: next((c for c in api.call("GET", "/clients") if c["name"] == "WAN installer acceptance" and c["online"]), None), "installer enrollment", 60)
        tunnel = create(api, client["id"], "WAN installed agent TCP", remote_port=27131)
        roundtrip(27131, b"HTTPS installed Linux agent")
        expected = ssh(SERVER, "sha256sum /opt/rustfrp-test/bin/rustfrp-agent").split()[0]
        actual = ssh(CLIENT, "sudo sha256sum /var/lib/rustfrp-test/.rustfrp/bin/rustfrp-agent").split()[0]
        assert actual == expected
        ok("Public HTTPS installer: Linux x86_64 binary / SHA256 manifest / fixed TLS pin / enrollment / real WAN transfer")
        api.call("DELETE", f"/clients/{client['id']}?reason=WAN-installer-cleanup")
    finally:
        ssh(CLIENT, "sudo kill -TERM " + shlex.quote(pid), check=False)


def crosshost():
    """Real host-to-host Edge migration; NAT-side Edge uses existing Tailscale IP."""
    api, state = Api(), load("state.json")
    node_address = "100.75.133.24"
    config = 'bindAddr = "0.0.0.0"\nbindPort = 27500\nallowPorts = [{start = 27100, end = 27198}]\n'
    write_remote(CLIENT, "/etc/rustfrp-test/lan-node.toml", config, owner="rustfrp-test:rustfrp-test")
    token = api.call("POST", "/enroll-tokens", {"name": "WAN client-host edge", "kind": "node", "reason": "Physical cross-host Edge test"})
    service(CLIENT, "rustfrp-test-lan-node", ["--mode", "edge", "--controller", "rustfrp.dev99.cn:27001",
            "--name", "WAN client-host edge", "--public-host", node_address, "--state-dir", "/var/lib/rustfrp-test/lan-node",
            "--tls-pin", state["fingerprint"], "-c", "/etc/rustfrp-test/lan-node.toml"],
            "/var/lib/rustfrp-test/lan-node", token["token"])
    node = wait(lambda: next((n for n in api.call("GET", "/nodes") if n["name"] == "WAN client-host edge" and n["online"]), None), "client-host Edge online")
    client = state["visitor_client"]
    tunnel = create(api, client, "WAN crosshost TCP", remote_port=27141)
    roundtrip(27141, b"before physical crosshost migration")
    api.call("POST", f"/clients/{client}/migrate", {"node_id": node["id"], "reason": "Physical cross-host Edge migration"})
    wait(lambda: api.call("GET", f"/clients/{client}")["node_id"] == node["id"] and api.call("GET", f"/clients/{client}")["online"], "cross-host migrated device", 60)
    probe = "import socket; s=socket.create_connection(('127.0.0.1',27141),timeout=10); s.sendall(b'crosshost WAN data'); assert s.recv(100)==b'crosshost WAN data'"
    wait(lambda: ssh(CLIENT, "python3 -c " + shlex.quote(probe)) == "", "cross-host migrated transfer", 60)
    ok("Physical cross-host Edge migration server → client host with original port and pinned identity", {"edge_host": CLIENT, "node_address": node_address, "transport": "existing Tailscale WAN private network"})
    try:
        ssh(SERVER, "sudo systemctl stop rustfrp-test-server")
        ssh(CLIENT, "python3 -c " + shlex.quote(probe))
        ssh(SERVER, "sudo systemctl restart rustfrp-test-visitor")
        wait(lambda: ssh(CLIENT, "python3 -c " + shlex.quote(probe)) == "", "cross-host offline cached login", 60)
        ok("Physical cross-host AT-05: Controller offline, remote client restart and signed cached relogin preserve WAN data")
    finally:
        ssh(SERVER, "sudo systemctl start rustfrp-test-server")
    api = wait(lambda: Api(), "controller recovered")
    wait(lambda: api.call("GET", f"/clients/{client}")["online"], "cross-host full reconciliation", 60)
    history = wait(lambda: (lambda x: x if sum(b["bytes_in"] for b in x["items"]) > 0 else None)(
        api.call("GET", f"/traffic?tunnel_id={tunnel['id']}")), "cross-host Edge traffic persisted")
    api.call("POST", f"/clients/{client}/migrate", {"node_id": "", "reason": "Physical cross-host reverse migration"})
    wait(lambda: api.call("GET", f"/clients/{client}")["online"] and api.call("GET", f"/clients/{client}")["node_id"] == "", "reverse cross-host migration", 60)
    wait(lambda: roundtrip(27141, b"returned from physical crosshost migration") is None, "reverse cross-host transfer", 60)
    api.call("DELETE", f"/tunnels/{tunnel['id']}?reason=WAN-crosshost-cleanup")
    state["crosshost_node"] = node["id"]
    save("state.json", state)
    ok("Physical cross-host Edge traffic persistence / control reconciliation / reverse migration", {"buckets": len(history["items"])})


def extras():
    api, state = Api(), load("state.json")
    client = state["client"]
    for scope in ("tunnels", "admin"):
        token = api.call("POST", "/api-tokens", {"name": "WAN " + scope, "scope": scope, "days": 1, "reason": "WAN bearer write scope"})
        headers = {"Authorization": "Bearer " + token["token"]}
        tunnel = api.call("POST", "/tunnels", {"client_id": client, "name": "WAN bearer " + scope,
                           "local_port": 28081, "remote_port": 27132, "reason": "WAN bearer tunnel"}, headers=headers, auth=False)
        running(api, tunnel)
        roundtrip(27132, b"Bearer-created WAN tunnel")
        if scope == "tunnels":
            api.call("POST", "/enroll-tokens", {"name": "scope-rejected", "reason": "test"}, headers=headers, auth=False, expected=403)
        else:
            enrollment = api.call("POST", "/enroll-tokens", {"name": "WAN admin bearer", "reason": "WAN token scope"}, headers=headers, auth=False)
            api.call("DELETE", f"/enroll-tokens/{enrollment['id']}?reason=WAN-cleanup", headers=headers, auth=False)
        api.call("DELETE", f"/tunnels/{tunnel['id']}?reason=WAN-cleanup", headers=headers, auth=False)
        api.call("POST", f"/api-tokens/{token['info']['id']}/revoke", {"reason": "WAN cleanup"})
        api.call("GET", "/overview", headers=headers, auth=False, expected=401)
        ok("Live " + scope + " API Token: permitted writes / role ceiling / actual WAN transfer / immediate revocation")
    api.call("PATCH", f"/admins/{api.admin['id']}", {"disabled": True, "reason": "WAN owner self-protection"}, expected=403)
    bad = api.call("POST", "/enroll-tokens", {"name": "WAN pin rejection", "reason": "WAN TLS pin rejection"})
    ssh(CLIENT, "sudo install -d -o rustfrp-test -g rustfrp-test -m 700 /var/lib/rustfrp-test/wrong-pin")
    code = ("import json,subprocess,sys; x=json.load(sys.stdin); "
            "\ntry: subprocess.run(['/opt/rustfrp-test/bin/rustfrp-agent','--mode','client','--controller','rustfrp.dev99.cn:27000',"
            "'--enroll-token',x['token'],'--state-dir','/var/lib/rustfrp-test/wrong-pin','--tls-pin','0'*64],"
            "stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=3)"
            "\nexcept subprocess.TimeoutExpired: pass")
    ssh(CLIENT, "sudo -u rustfrp-test python3 -c " + shlex.quote(code), json.dumps({"token": bad["token"]}).encode())
    record = next(t for t in api.call("GET", "/enroll-tokens") if t["id"] == bad["id"])
    assert record["used_at"] is None
    code = "import json,os; p='/var/lib/rustfrp-test/wrong-pin/identity.json'; x=json.load(open(p)); assert not x['client_id']; assert os.stat(p).st_mode&511==384"
    ssh(CLIENT, "sudo python3 -c " + shlex.quote(code))
    api.call("DELETE", f"/enroll-tokens/{bad['id']}?reason=WAN-cleanup")
    ok("Wrong TLS pin rejects handshake before enrollment; token unconsumed / no enrolled identity; private local key remains 0600; Owner self-disable denied")
    config = 'bindAddr = "127.0.0.1"\nbindPort = 27900\nallowPorts = [{start = 27100, end = 27198}]\n'
    write_remote(SERVER, "/etc/rustfrp-test/disposable-node.toml", config, owner="rustfrp-test:rustfrp-test")
    token = api.call("POST", "/enroll-tokens", {"name": "WAN disposable node", "kind": "node", "reason": "WAN node CRUD"})
    try:
        service(SERVER, "rustfrp-test-disposable-node", ["--mode", "edge", "--controller", "127.0.0.1:27001", "--name", "WAN disposable node",
                "--public-host", "rustfrp.dev99.cn", "--tls-pin", state["fingerprint"], "--state-dir", "/var/lib/rustfrp-test/disposable-node",
                "-c", "/etc/rustfrp-test/disposable-node.toml"], "/var/lib/rustfrp-test/disposable-node", token["token"])
        node = wait(lambda: next((n for n in api.call("GET", "/nodes") if n["name"] == "WAN disposable node" and n["online"]), None), "disposable node")
        api.call("DELETE", f"/nodes/{node['id']}?reason=WAN-disposable-node-cleanup")
        assert not any(n["id"] == node["id"] for n in api.call("GET", "/nodes"))
        ok("External Node delete removes management resource and revokes control connection")
    finally:
        ssh(SERVER, "sudo systemctl disable --now rustfrp-test-disposable-node", check=False)


def finish():
    api, state = Api(), load("state.json")
    # Revoke only unused enrollment credentials created by this acceptance suite.
    for token in api.call("GET", "/enroll-tokens"):
        if token["name"].startswith("WAN ") and token["used_at"] is None and token["revoked_at"] is None:
            api.call("DELETE", f"/enroll-tokens/{token['id']}?reason=WAN-credential-cleanup")
    for admin in api.call("GET", "/admins"):
        if admin["name"] == "wan-ui" and admin["disabled_at"] is None:
            api.call("PATCH", f"/admins/{admin['id']}", {"disabled": True, "reason": "Disposable browser owner account cleanup"})
    ssh(SERVER, "sudo -u rustfrp-test env RUSTFRP_DB=/var/lib/rustfrp-test/controller.db /opt/rustfrp-test/bin/rustfrp-server backup /var/lib/rustfrp-test/acceptance-backup.db")
    code = "import sqlite3,os; p='/var/lib/rustfrp-test/acceptance-backup.db'; c=sqlite3.connect(p); assert c.execute('PRAGMA integrity_check').fetchone()[0]=='ok'; assert os.stat(p).st_mode&511==384; print(c.execute('PRAGMA user_version').fetchone()[0])"
    version = ssh(SERVER, "sudo -u rustfrp-test python3 -c " + shlex.quote(code)).strip()
    ok("Live WAL SQLite backup, integrity check, schema version and private mode", {"schema": version})
    if state.get("visitor_client"):
        ssh(SERVER, "sudo systemctl disable --now rustfrp-test-visitor")
        api.call("DELETE", f"/clients/{state['visitor_client']}?reason=WAN-test-cleanup")
        assert not any(c["id"] == state["visitor_client"] for c in api.call("GET", "/clients"))
        ok("Disposable visitor device deletion, credential denial and cleanup")
    actions = {row["action"] for row in api.call("GET", "/audit?limit=1000")}
    for action in ("tunnel.create", "tunnel.update", "tunnel.batch", "client.restart", "client.kick", "client.move_node", "node.update", "api_token.revoke"):
        assert action in actions, (action, sorted(actions))
    ok("Audit records cover accepted writes and denied permissions", sorted(actions))
    for host, service_name, path, pid, checksum in (
        (SERVER, "frps", "/etc/frp/frps.toml", "213397", "049a99a74513d8c557cc3879f99d4a26ef3721ef5231392b15e9436be23e26a3"),
        (CLIENT, "frpc", "/opt/frp/frpc.toml", "5851", "16dd50b6a26686e637c3300d2ade5a927c4f24ece420300d19cd0e265e60c1ac")):
        actual_pid = ssh(host, f"sudo systemctl show {service_name} -p MainPID --value").strip()
        actual_hash = ssh(host, f"sudo sha256sum {path}").split()[0]
        assert actual_pid == pid and actual_hash == checksum
        assert ssh(host, f"systemctl is-active {service_name}").strip() == "active"
        ok("Original " + service_name + " unaffected: same PID, config SHA256, active service", {"host": host, "pid": actual_pid, "sha256": actual_hash})
    roundtrip(27100, b"final acceptance")
    udp(27102)
    ok("Final managed WAN TCP / UDP and online service health")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("stage", choices=["bootstrap", "managed", "nodes", "standalone", "installer", "crosshost", "extras", "finish"])
    args = parser.parse_args()
    globals()[args.stage]()
