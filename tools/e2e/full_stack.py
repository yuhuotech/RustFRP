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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin-dir", default="target/release")
    a = ap.parse_args()
    server_bin = os.path.join(a.bin_dir, "rustfrp-server")
    agent_bin = os.path.join(a.bin_dir, "rustfrp-agent")
    tmp = tempfile.mkdtemp(prefix="rfrp-e2e-")
    http_port, edge_port, echo_port = free_port(), free_port(), free_port()
    env = dict(
        os.environ,
        RUSTFRP_DB=f"{tmp}/ctl.db",
        RUSTFRP_HTTP=f"127.0.0.1:{http_port}",
        RUSTFRP_EDGE=f"127.0.0.1:{edge_port}",
        RUSTFRP_PROXY_BIND="127.0.0.1",
        RUSTFRP_PUBLIC_HOST="127.0.0.1",
        RUSTFRP_ADMIN_PASSWORD="a-long-enough-password",
    )
    procs = []
    echo = echo_server(echo_port)
    try:
        subprocess.run([server_bin, "admin", "create", "root"], env=env, check=True, stdout=subprocess.DEVNULL)
        procs.append(subprocess.Popen([server_bin, "run"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        api = Api(f"http://127.0.0.1:{http_port}")
        wait(lambda: not port_closed(http_port), "controller HTTP")

        me = api.call("POST", "/auth/login", {"name": "root", "password": env["RUSTFRP_ADMIN_PASSWORD"]}, login=True)
        api.csrf = me["csrf"]
        print("登录 OK，角色", me["admin"]["role"])

        tok = api.call("POST", "/enroll-tokens", {"name": "e2e-box", "ttl_minutes": 5, "reason": "e2e"})
        assert tok["token"].startswith("rfrp_") and "--enroll-token" in tok["commands"]["direct"]
        print("接入令牌 OK:", tok["commands"]["direct"].split(" --enroll-token")[0])

        procs.append(subprocess.Popen(
            [agent_bin, "--mode", "client", "--controller", f"127.0.0.1:{edge_port}", "--enroll-token", tok["token"], "--name", "e2e-box", "--state-dir", f"{tmp}/agent"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        client = wait(lambda: next((c for c in api.call("GET", "/clients") if c["online"]), None), "agent online")
        assert client["name"] == "e2e-box", client
        print("设备上线 OK:", client["id"], client["os"], client["arch"])
        ident = json.load(open(f"{tmp}/agent/identity.json"))
        assert ident["client_id"] == client["id"] and oct(os.stat(f"{tmp}/agent/identity.json").st_mode & 0o777) == "0o600"
        print("身份文件已保存（权限 600）")

        t = api.call("POST", "/tunnels", {"client_id": client["id"], "name": "echo", "local_port": echo_port, "reason": "e2e"})
        t = wait(lambda: (lambda x: x if x["state"] == "running" else None)(api.call("GET", f"/tunnels/{t['id']}")), "tunnel running")
        port = t["status_port"]
        payload = os.urandom(300_000)
        assert tcp_roundtrip(port, payload) == payload
        print(f"隧道 OK: 公网端口 {port} 回显 {len(payload)} 字节一致")

        api.call("PATCH", f"/tunnels/{t['id']}", {"enabled": False, "reason": "e2e"})
        wait(lambda: port_closed(port), "tunnel port closed")
        wait(lambda: api.call("GET", f"/tunnels/{t['id']}")["state"] == "stopped", "state stopped")
        print("停用隧道 OK: 端口已关闭")

        api.call("POST", f"/clients/{client['id']}/kick", {"reason": "e2e"})
        time.sleep(0.5)
        wait(lambda: api.call("GET", f"/clients/{client['id']}")["online"], "agent reconnect")
        print("强制下线后自动重连 OK")

        api.call("PATCH", f"/tunnels/{t['id']}", {"enabled": True, "reason": "e2e"})
        t2 = wait(lambda: (lambda x: x if x["state"] == "running" else None)(api.call("GET", f"/tunnels/{t['id']}")), "tunnel running again")
        assert tcp_roundtrip(t2["status_port"], b"after reconnect") == b"after reconnect"
        print("重连后隧道恢复 OK")

        api.call("DELETE", f"/clients/{client['id']}?reason=e2e")
        wait(lambda: port_closed(t2["status_port"]), "port closed after delete")
        assert api.call("GET", "/clients") == []
        actions = {e["action"] for e in api.call("GET", "/audit")}
        for need in ["auth.login", "enroll_token.create", "client.enroll", "tunnel.create", "tunnel.update", "client.kick", "client.delete"]:
            assert need in actions, (need, actions)
        print("删除设备 OK；审计完整:", sorted(actions))
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
