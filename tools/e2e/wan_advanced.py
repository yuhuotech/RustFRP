#!/usr/bin/env python3
"""Additional real-WAN plugin, routing, health, limits and bandwidth assertions."""
import base64
from concurrent.futures import ThreadPoolExecutor
import json
import os
import shlex
import socket
import ssl
import time

import wan


def exact(stream, count):
    out = bytearray()
    while len(out) < count:
        part = stream.recv(count - len(out))
        assert part, "unexpected EOF"
        out.extend(part)
    return bytes(out)


def main():
    api, state = wan.Api(), wan.load("state.json")
    client = state["client"]
    tunnels = []

    def create(name, port, local=28081, **fields):
        tunnel = wan.create(api, client, "WAN advanced " + name, local, remote_port=port, **fields)
        tunnels.append(tunnel)
        return tunnel

    try:
        for version, port in ((1, 27110), (2, 27111)):
            create("PROXY v" + str(version), port, 28085, proxy_protocol=version)
            wan.roundtrip(port, b"proxy-header-consumed")
            data = json.loads(wan.ssh(wan.CLIENT, f"sudo cat /var/lib/rustfrp-test/proxy-v{version}.json"))["head"]
            assert data.startswith("PROXY TCP4 ") if version == 1 else data.startswith("0d0a0d0a000d0a515549540a2111000c")
            wan.ok("WAN PROXY protocol v" + str(version) + ": original peer header delivered before payload")
        for server_side, port in ((False, 27112), (True, 27113)):
            create("bandwidth " + str(server_side), port, bandwidth_bps=65536, bandwidth_server=server_side)
            start = time.monotonic()
            wan.roundtrip(port, b"rate" * 49152)
            elapsed = time.monotonic() - start
            assert 1.5 < elapsed < 20, elapsed
            wan.ok("WAN shared bandwidth limit on " + ("Edge" if server_side else "client"), {"bytes_per_direction": 196608, "seconds": round(elapsed, 2)})
        limit = create("connection limit", 27114, max_connections=1)
        with socket.create_connection((wan.SERVER, 27114), timeout=10) as held:
            held.settimeout(10)
            held.sendall(b"first")
            assert exact(held, 5) == b"first"
            with socket.create_connection((wan.SERVER, 27114), timeout=10) as second:
                second.settimeout(3)
                second.sendall(b"second")
                try:
                    assert second.recv(100) == b""
                except ConnectionResetError:
                    pass
        wan.wait(lambda: wan.roundtrip(27114, b"limit released") is None, "released connection budget")
        wan.ok("WAN per-tunnel connection quota rejects second connection and releases after close")
        # Authenticated SOCKS avoids publishing an anonymous proxy on the test host.
        create("socks5", 27115, plugin={"kind": "socks5", "username": "wan", "password": "test-only"})
        with socket.create_connection((wan.SERVER, 27115), timeout=10) as stream:
            stream.settimeout(10)
            stream.sendall(bytes([5, 1, 2]))
            assert exact(stream, 2) == bytes([5, 2])
            stream.sendall(b"\x01\x03wan\x09test-only")
            assert exact(stream, 2) == b"\x01\x00"
            stream.sendall(bytes([5, 1, 0, 1, 127, 0, 0, 1]) + (28081).to_bytes(2, "big"))
            assert exact(stream, 10)[1] == 0
            stream.sendall(b"SOCKS over WAN")
            assert exact(stream, 14) == b"SOCKS over WAN"
        wan.ok("WAN socks5 plugin with RFC1929 authentication and backend CONNECT")
        create("http proxy", 27116, plugin={"kind": "http_proxy", "http_user": "wan", "http_password": "test-only"})
        basic = "Basic " + base64.b64encode(b"wan:test-only").decode()
        with socket.create_connection((wan.SERVER, 27116), timeout=10) as stream:
            stream.settimeout(10)
            stream.sendall(("CONNECT 127.0.0.1:28081 HTTP/1.1\r\nHost: 127.0.0.1:28081\r\nProxy-Authorization: " + basic + "\r\n\r\n").encode())
            head = bytearray()
            while not head.endswith(b"\r\n\r\n"):
                head.extend(exact(stream, 1))
            assert b"200" in head
            stream.sendall(b"HTTP proxy WAN")
            assert exact(stream, 14) == b"HTTP proxy WAN"
        wan.ok("WAN http_proxy plugin with proxy authentication and CONNECT")
        wan.write_remote(wan.CLIENT, "/var/lib/rustfrp-test/static/index.html", "RustFRP WAN static asset", owner="rustfrp-test:rustfrp-test")
        create("static file", 27117, plugin={"kind": "static_file", "local_path": "/var/lib/rustfrp-test/static",
                                            "http_user": "wan", "http_password": "test-only"})
        assert wan.http(27117)[0] == 401
        code, _, body = wan.http(27117, headers={"Authorization": basic})
        assert code == 200 and body == b"RustFRP WAN static asset"
        assert wan.http(27117, path="/../backend-key.pem", headers={"Authorization": basic})[0] in (400, 403, 404)
        wan.ok("WAN static_file plugin: Basic Auth / exact file content / traversal rejection")
        create("unix socket", 27118, plugin={"kind": "unix_domain_socket", "unix_path": "/var/lib/rustfrp-test/backend.sock"})
        wan.roundtrip(27118, b"Unix socket across WAN")
        wan.ok("WAN unix_domain_socket plugin")
        for kind, port, backend in (("http2https", 27119, 28084), ("https2http", 27120, 28083), ("https2https", 27121, 28084)):
            create(kind, port, plugin={"kind": kind, "local_addr": f"127.0.0.1:{backend}",
                                       "host_header_rewrite": "plugin.internal", "enable_http2": False,
                                       "crt_path": "/var/lib/rustfrp-test/backend-cert.pem", "key_path": "/var/lib/rustfrp-test/backend-key.pem"})
            code, _, body = wan.http(port, tls=kind.startswith("https"))
            assert code == 200 and json.loads(body)["headers"]["Host"] == "plugin.internal"
            wan.ok("WAN " + kind + " plugin: HTTP/1.1 / TLS / Host rewrite")
        create("tls2raw", 27122, plugin={"kind": "tls2raw", "local_addr": "127.0.0.1:28081",
                                        "crt_path": "/var/lib/rustfrp-test/backend-cert.pem", "key_path": "/var/lib/rustfrp-test/backend-key.pem"})
        context = ssl.create_default_context(cafile=str(wan.ROOT / "backend-cert.pem"))
        with context.wrap_socket(socket.create_connection((wan.SERVER, 27122), timeout=10), server_hostname="wan-test.example") as stream:
            stream.settimeout(10)
            stream.sendall(b"TLS terminated raw backend")
            assert exact(stream, 26) == b"TLS terminated raw backend"
        wan.ok("WAN tls2raw plugin with validated inbound certificate")
        # Two proxies share a domain and group, request headers identify the member.
        for member in ("A", "B"):
            create("LB " + member, 0, 28083, kind="http", custom_domains=["lb.wan-test.example"], lb_group="wan-lb",
                   lb_group_key="test-only-lb", request_headers={"X-Wan-Member": member})
        members = {json.loads(wan.http(27080, "lb.wan-test.example")[2])["headers"]["X-Wan-Member"] for _ in range(8)}
        assert members == {"A", "B"}, members
        wan.ok("WAN HTTP load balancing rotates both healthy group members")
        # Different Basic usernames route to different proxies on the same host.
        for user in ("alice", "bob"):
            create("route user " + user, 0, 28083, kind="http", custom_domains=["user.wan-test.example"],
                   route_by_http_user=user, request_headers={"X-Routed-User": user})
            auth = "Basic " + base64.b64encode((user + ":anything").encode()).decode()
            body = wan.http(27080, "user.wan-test.example", headers={"Authorization": auth})[2]
            assert json.loads(body)["headers"]["X-Routed-User"] == user
        wan.ok("WAN routeByHTTPUser selects separate upstreams on the same domain")
        health = create("health", 27123, 28083, health_check={"kind": "http", "timeout_secs": 1,
                        "max_failed": 1, "interval_secs": 1, "path": "/health"})
        wan.wait(lambda: wan.http(27123)[0] == 200, "initial healthy service")
        wan.write_remote(wan.CLIENT, "/var/lib/rustfrp-test/unhealthy", "test-only", owner="rustfrp-test:rustfrp-test")
        wan.wait(lambda: api.call("GET", f"/tunnels/{health['id']}")["state"] != "running", "HTTP health failure")
        wan.ssh(wan.CLIENT, "sudo rm /var/lib/rustfrp-test/unhealthy")
        wan.running(api, health)
        assert wan.http(27123)[0] == 200
        wan.ok("WAN HTTP health check withdraws failed backend and restores recovered backend")
        # Typical concurrent load, exact binary payload in every connection.
        def transfer(index):
            payload = bytes([index]) * 4096
            wan.roundtrip(27100, payload)
        with ThreadPoolExecutor(max_workers=40) as pool:
            list(pool.map(transfer, range(40)))
        wan.ok("WAN 40 simultaneous TCP clients with exact binary assertions")
    finally:
        wan.ssh(wan.CLIENT, "sudo rm -f /var/lib/rustfrp-test/unhealthy", check=False)
        for tunnel in reversed(tunnels):
            api.call("DELETE", f"/tunnels/{tunnel['id']}?reason=WAN-advanced-cleanup")


if __name__ == "__main__":
    main()
