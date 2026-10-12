#!/usr/bin/env python3
"""全栈冒烟：真实 rustfrp-server（含内置 Edge）+ 真实 rustfrp-agent（受管模式）+ 管理 REST API。

流程：创建管理员 → 登录 → 生成接入令牌 → agent 接入 → 建隧道 → 经公网端口收发数据 →
停用隧道（端口关闭）→ 强制下线（agent 自动重连）→ 删除设备。
用法：python3 tools/e2e/full_stack.py [--bin-dir target/release]
"""
import argparse
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


class Api:
    def __init__(self, base):
        self.base, self.cookie, self.csrf = base, None, None

    def call(self, method, path, body=None, login=False):
        req = urllib.request.Request(self.base + "/api/v1" + path, method=method)
        req.add_header("Accept", "application/json")
        if body is not None:
            req.add_header("Content-Type", "application/json")
            req.data = json.dumps(body).encode()
        if self.cookie:
            req.add_header("Cookie", self.cookie)
        if method != "GET" and self.csrf:
            req.add_header("X-CSRF-Token", self.csrf)
        if login:
            req.add_header("X-Rustfrp-Admin", "1")
        try:
            with urllib.request.urlopen(req, timeout=10) as r:
                sc = r.headers.get("Set-Cookie")
                if sc:
                    self.cookie = sc.split(";")[0]
                return json.loads(r.read() or b"null")
        except urllib.error.HTTPError as e:
            raise RuntimeError(f"{method} {path} -> {e.code}: {e.read().decode()}")


def wait(cond, what, timeout=15):
    end = time.time() + timeout
    while time.time() < end:
        v = cond()
        if v:
            return v
        time.sleep(0.1)
    raise RuntimeError(f"timeout waiting for {what}")


def echo_server(port):
    srv = socket.socket()
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", port))
    srv.listen(64)

    def handle(c):
        try:
            while True:
                d = c.recv(65536)
                if not d:
                    return
                c.sendall(d)
        except OSError:
            pass
        finally:
            c.close()

    def loop():
        while True:
            try:
                c, _ = srv.accept()
            except OSError:
                return
            threading.Thread(target=handle, args=(c,), daemon=True).start()

    threading.Thread(target=loop, daemon=True).start()
    return srv


def tcp_roundtrip(port, payload):
    with socket.create_connection(("127.0.0.1", port), timeout=5) as s:
        s.sendall(payload)
        got = b""
        while len(got) < len(payload):
            chunk = s.recv(65536)
            if not chunk:
                break
            got += chunk
        return got


def port_closed(port):
    try:
        socket.create_connection(("127.0.0.1", port), timeout=0.5).close()
        return False
    except OSError:
        return True


def local_request(path, command, token=None):
    with open(path) as file:
        discovery = json.load(file)
    with socket.create_connection(("127.0.0.1", discovery["port"]), timeout=5) as stream:
        stream.sendall((json.dumps({"token": token or discovery["token"], "command": command}) + "\n").encode())
        with stream.makefile("rb") as response:
            return json.loads(response.readline(2048))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin-dir", default="target/release")
    a = ap.parse_args()
    server_bin = os.path.join(a.bin_dir, "rustfrp-server")
    agent_bin = os.path.join(a.bin_dir, "rustfrp-agent")
    tmp = tempfile.mkdtemp(prefix="rfrp-e2e-")
    http_port, edge_port, echo_port, vhost_port = free_port(), free_port(), free_port(), free_port()
    env = dict(
        os.environ,
        RUSTFRP_DB=f"{tmp}/ctl.db",
        RUSTFRP_HTTP=f"127.0.0.1:{http_port}",
        RUSTFRP_EDGE=f"127.0.0.1:{edge_port}",
        RUSTFRP_PROXY_BIND="127.0.0.1",
        RUSTFRP_PUBLIC_HOST="127.0.0.1",
        RUSTFRP_VHOST_HTTP=str(vhost_port),
        RUSTFRP_SUBDOMAIN_HOST="t.example.com",
        RUSTFRP_ADMIN_PASSWORD="a-long-enough-password",
    )
    procs = []
    echo = echo_server(echo_port)
    try:
        subprocess.run([server_bin, "admin", "create", "root"], env=env, check=True, stdout=subprocess.DEVNULL)
        procs.append(subprocess.Popen([server_bin, "run"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        api = Api(f"http://127.0.0.1:{http_port}")
        wait(lambda: not port_closed(http_port), "controller HTTP")
        for path, marker in (("install.sh", b"#!/bin/sh"), ("install.ps1", b"function Install-RustFrp")):
            with urllib.request.urlopen(f"{api.base}/{path}", timeout=5) as response:
                assert response.status == 200
                assert response.headers.get_content_type() == "text/plain"
                assert response.read().startswith(marker)
        print("未登录安装脚本入口 OK")

        me = api.call("POST", "/auth/login", {"name": "root", "password": env["RUSTFRP_ADMIN_PASSWORD"]}, login=True)
        api.csrf = me["csrf"]
        print("登录 OK，角色", me["admin"]["role"])

        tok = api.call("POST", "/enroll-tokens", {"name": "e2e-box", "ttl_minutes": 5, "reason": "e2e"})
        assert tok["token"].startswith("rfrp_") and "--enroll-token" in tok["commands"]["direct"]
        print("接入令牌 OK:", tok["commands"]["direct"].split(" --enroll-token")[0])

        fingerprint=api.call("GET","/node")["tls_fingerprint"]
        assert len(fingerprint)==64
        assert "--tls-pin" in tok["commands"]["direct"]
        procs.append(subprocess.Popen(
            [agent_bin, "--mode", "client", "--controller", f"127.0.0.1:{edge_port}", "--enroll-token", tok["token"], "--name", "e2e-box", "--state-dir", f"{tmp}/agent", "--local-control", f"{tmp}/agent/control.json", "--tls-pin", fingerprint],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        client = wait(lambda: next((c for c in api.call("GET", "/clients") if c["online"]), None), "agent online")
        assert client["name"] == "e2e-box", client
        control_path = f"{tmp}/agent/control.json"
        status = wait(lambda: (lambda s: s if s["connected"] else None)(local_request(control_path, "status")), "local connected")
        assert status["version"] == 1 and status["pid"] == procs[-1].pid
        assert local_request(control_path, "stop", "wrong-token") == {"error": "unauthorized"}
        assert local_request(control_path, "status")["connected"]
        assert oct(os.stat(control_path).st_mode & 0o777) == "0o600"
        print("本机控制鉴权 / 实际连接状态 / 私有发现文件 OK")
        print("设备上线 OK:", client["id"], client["os"], client["arch"])
        ident = json.load(open(f"{tmp}/agent/identity.json"))
        assert ident["client_id"] == client["id"] and oct(os.stat(f"{tmp}/agent/identity.json").st_mode & 0o777) == "0o600"
        print("身份文件已保存（权限 600）")

        t = api.call("POST", "/tunnels", {"client_id": client["id"], "name": "echo", "local_port": echo_port, "reason": "e2e"})
        t = wait(lambda: (lambda x: x if x["state"] == "running" else None)(api.call("GET", f"/tunnels/{t['id']}")), "tunnel running")
        port = t["status_port"]
        payload = os.urandom(300_000)
        assert tcp_roundtrip(port, payload) == payload
        wait(lambda: local_request(control_path, "status")["proxies"] == 1, "local config count")
        print(f"隧道 OK: 公网端口 {port} 回显 {len(payload)} 字节一致")
        traffic = wait(lambda: (lambda x: x if sum(b["bytes_in"] for b in x["items"]) == len(payload) else None)(api.call("GET", f"/traffic?tunnel_id={t['id']}")), "traffic persisted")
        assert sum(b["bytes_out"] for b in traffic["items"]) == len(payload)
        assert api.call("GET", "/overview")["traffic"]["bytes_in"] == len(payload)
        local_traffic = local_request(control_path, "status")["traffic"]
        assert local_traffic["bytes_in"] == len(payload) and local_traffic["bytes_out"] == len(payload)
        logs = local_request(control_path, "logs")
        assert 0 < len(logs["items"]) <= 128
        assert all(len(line.encode()) <= 512 for line in logs["items"])
        assert any("applied config" in line for line in logs["items"])
        local_tunnels = local_request(control_path, "tunnels")
        assert len(local_tunnels["items"]) == 1
        assert local_tunnels["items"][0]["id"] == t["id"]
        assert local_tunnels["items"][0]["local_port"] == echo_port
        automation = api.call("POST", "/api-tokens", {"name":"e2e", "scope":"read", "days":1, "reason":"e2e"})
        req = urllib.request.Request(api.base + "/api/v1/overview", headers={"Authorization":"Bearer " + automation["token"]})
        with urllib.request.urlopen(req) as response:
            assert json.loads(response.read())["traffic"]["bytes_in"] == len(payload)
        api.call("POST", f"/api-tokens/{automation['info']['id']}/revoke", {"reason":"e2e"})
        try:
            urllib.request.urlopen(req)
            raise AssertionError("revoked automation token accepted")
        except urllib.error.HTTPError as error:
            assert error.code == 401
        controller_logs=api.call("GET","/logs")
        assert 0 < len(controller_logs["items"]) <= 128
        assert any("management on" in line for line in controller_logs["items"])
        print("API Token 真实调用 / 撤销立即失效 / Controller 有界日志 OK")
        print("实时流量 / 历史查询 / 本机统计 / 有界日志 / 已应用隧道列表 OK")

        api.call("PATCH", f"/tunnels/{t['id']}", {"name":"echo-renamed", "reason":"e2e rename"})
        assert api.call("POST", "/tunnels/batch", {"ids":[t["id"]], "enabled":False, "reason":"e2e batch"})["updated"] == 1
        wait(lambda: port_closed(port), "tunnel port closed")
        wait(lambda: api.call("GET", f"/tunnels/{t['id']}")["state"] == "stopped", "state stopped")
        print("停用隧道 OK: 端口已关闭")

        api.call("POST", f"/clients/{client['id']}/kick", {"reason": "e2e"})
        time.sleep(0.5)
        wait(lambda: api.call("GET", f"/clients/{client['id']}")["online"], "agent reconnect")
        print("强制下线后自动重连 OK")

        api.call("POST", "/tunnels/batch", {"ids":[t["id"]], "enabled":True, "reason":"e2e batch"})
        t2 = wait(lambda: (lambda x: x if x["state"] == "running" else None)(api.call("GET", f"/tunnels/{t['id']}")), "tunnel running again")
        assert tcp_roundtrip(t2["status_port"], b"after reconnect") == b"after reconnect"
        print("重连后隧道恢复 OK")

        # UDP 与 HTTP（按域名 / 子域名路由）
        udp_srv = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        udp_srv.bind(("127.0.0.1", 0))
        udp_port = udp_srv.getsockname()[1]

        def udp_loop():
            while True:
                try:
                    d, a_ = udp_srv.recvfrom(4096)
                    udp_srv.sendto(b"udp:" + d, a_)
                except OSError:
                    return

        threading.Thread(target=udp_loop, daemon=True).start()
        u = api.call("POST", "/tunnels", {"client_id": client["id"], "name": "dns", "kind": "udp", "local_port": udp_port, "reason": "e2e"})
        u = wait(lambda: (lambda x: x if x["state"] == "running" else None)(api.call("GET", f"/tunnels/{u['id']}")), "udp tunnel running")
        c = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        c.settimeout(5)
        c.sendto(b"ping", ("127.0.0.1", u["status_port"]))
        assert c.recvfrom(100)[0] == b"udp:ping"
        print("UDP 隧道 OK")

        http_srv = socket.socket()
        http_srv.bind(("127.0.0.1", 0))
        http_srv.listen(8)
        http_port_local = http_srv.getsockname()[1]

        def http_loop():
            while True:
                try:
                    cs, _ = http_srv.accept()
                except OSError:
                    return
                data = cs.recv(4096).decode(errors="replace")
                host = next((l for l in data.split("\r\n") if l.lower().startswith("host:")), "")
                body = f"site[{host}]".encode()
                cs.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: %d\r\nConnection: close\r\n\r\n" % len(body) + body)
                cs.close()

        threading.Thread(target=http_loop, daemon=True).start()
        w = api.call("POST", "/tunnels", {"client_id": client["id"], "name": "site", "kind": "http", "local_port": http_port_local, "custom_domains": ["www.example.com"], "subdomain": "blog", "host_header_rewrite": "internal.lan", "reason": "e2e"})
        wait(lambda: api.call("GET", f"/tunnels/{w['id']}")["state"] == "running", "http tunnel running")
        for host in ["www.example.com", "blog.t.example.com"]:
            with socket.create_connection(("127.0.0.1", vhost_port), timeout=5) as hs:
                hs.sendall(f"GET / HTTP/1.1\r\nHost: {host}\r\n\r\n".encode())
                resp = b""
                while True:
                    chunk = hs.recv(4096)
                    if not chunk:
                        break
                    resp += chunk
            assert b"site[Host: internal.lan]" in resp, resp
        with socket.create_connection(("127.0.0.1", vhost_port), timeout=5) as hs:
            hs.sendall(b"GET / HTTP/1.1\r\nHost: nobody.example.com\r\n\r\n")
            assert hs.recv(100).startswith(b"HTTP/1.1 404")
        print("HTTP 域名 / 子域名路由与 Host 改写 OK")

        # Online snapshot of the running WAL database: no stop and no WAL file copy.
        snapshot = f"{tmp}/snapshot.db"
        subprocess.run([server_bin, "backup", snapshot], env=env, check=True, timeout=30, stdout=subprocess.DEVNULL)
        import sqlite3
        with sqlite3.connect(snapshot) as saved:
            assert saved.execute("PRAGMA integrity_check").fetchone()[0] == "ok"
            assert saved.execute("SELECT count(*) FROM clients WHERE deleted_at IS NULL").fetchone()[0] == 1
            assert saved.execute("SELECT count(*) FROM tunnels WHERE deleted_at IS NULL").fetchone()[0] >= 3
            assert saved.execute("PRAGMA user_version").fetchone()[0] == 19
        assert oct(os.stat(snapshot).st_mode & 0o777) == "0o600"
        print("在线 WAL 快照 / 私有权限 / 数据完整性 OK")

        api.call("DELETE", f"/clients/{client['id']}?reason=e2e")
        wait(lambda: port_closed(t2["status_port"]), "port closed after delete")
        assert api.call("GET", "/clients") == []
        actions = {e["action"] for e in api.call("GET", "/audit")}
        for need in ["auth.login", "enroll_token.create", "client.enroll", "tunnel.create", "tunnel.update", "tunnel.batch", "api_token.create", "api_token.revoke", "client.kick", "client.delete"]:
            assert need in actions, (need, actions)
        print("删除设备 OK；审计完整:", sorted(actions))
        assert local_request(control_path, "stop") == {"ok": True}
        assert procs[-1].wait(timeout=5) == 0
        print("本机控制停止 agent OK")
        print("全栈冒烟通过")
    finally:
        for p in procs:
            p.terminate()
        for p in procs:
            try:
                p.wait(timeout=5)
            except subprocess.TimeoutExpired:
                p.kill()
        echo.close()


if __name__ == "__main__":
    try:
        main()
    except Exception as e:
        print("失败:", e, file=sys.stderr)
        sys.exit(1)
