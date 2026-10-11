#!/usr/bin/env python3
"""内存基准：启动 edge + client，建立若干隧道与并发连接，读取各进程常驻内存（RSS）。

用法：python3 tools/bench/mem.py [--bin target/release/rustfrp-agent] [--tunnels 10] [--conns 100] [--max-edge-mb 10] [--max-client-mb 10]
超过上限则退出码为 1（CI 内存回归）。仅依赖 Python 标准库；RSS 通过 `ps` 读取（macOS / Linux 通用）。
"""
import argparse
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


def rss_mb(pid):
    out = subprocess.check_output(["ps", "-o", "rss=", "-p", str(pid)]).decode().strip()
    return int(out) / 1024.0


def echo_server(port):
    srv = socket.socket()
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", port))
    srv.listen(512)

    def handle(c):
        try:
            while True:
                d = c.recv(65536)
                if not d:
                    break
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


def wait_port(port, timeout=10):
    end = time.time() + timeout
    while time.time() < end:
        try:
            socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
            return True
        except OSError:
            time.sleep(0.05)
    return False


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="target/release/rustfrp-agent")
    ap.add_argument("--tunnels", type=int, default=10)
    ap.add_argument("--conns", type=int, default=100)
    ap.add_argument("--max-edge-mb", type=float, default=None)
    ap.add_argument("--max-client-mb", type=float, default=None)
    a = ap.parse_args()

    tmp = tempfile.mkdtemp(prefix="rfrp-bench-")
    bind = free_port()
    echo_port = free_port()
    remote_base = free_port()
    echo = echo_server(echo_port)

    with open(f"{tmp}/frps.toml", "w") as f:
        f.write(f'bindAddr = "127.0.0.1"\nbindPort = {bind}\nproxyBindAddr = "127.0.0.1"\nauth.token = "bench"\nlog.level = "error"\n')
    ports = [free_port() for _ in range(a.tunnels)]
    with open(f"{tmp}/frpc.toml", "w") as f:
        f.write(f'serverAddr = "127.0.0.1"\nserverPort = {bind}\nauth.token = "bench"\nlog.level = "error"\n')
        for i, p in enumerate(ports):
            f.write(f'\n[[proxies]]\nname = "t{i}"\ntype = "tcp"\nlocalPort = {echo_port}\nremotePort = {p}\n')

    edge = subprocess.Popen([a.bin, "--mode", "edge", "-c", f"{tmp}/frps.toml"], stderr=subprocess.DEVNULL, stdout=subprocess.DEVNULL)
    if not wait_port(bind):
        print("edge did not start", file=sys.stderr)
        sys.exit(2)
    client = subprocess.Popen([a.bin, "--mode", "client", "-c", f"{tmp}/frpc.toml"], stderr=subprocess.DEVNULL, stdout=subprocess.DEVNULL)
    if not wait_port(ports[0], 10) if ports else False:
        print("tunnel did not come up", file=sys.stderr)
        sys.exit(2)
    time.sleep(1.0)

    results = {}
    results["idle (edge, client)"] = (rss_mb(edge.pid), rss_mb(client.pid))

    conns = []
    for i in range(a.conns):
        c = socket.create_connection(("127.0.0.1", ports[i % len(ports)]))
        c.sendall(b"x")
        c.recv(1)
        conns.append(c)
    time.sleep(1.0)
    results[f"{a.conns} idle connections"] = (rss_mb(edge.pid), rss_mb(client.pid))

    payload = b"y" * 65536
    for c in conns[:20]:
        for _ in range(8):
            c.sendall(payload)
        got = 0
        while got < 8 * len(payload):
            got += len(c.recv(65536))
    results["after 20 x 512KiB echo"] = (rss_mb(edge.pid), rss_mb(client.pid))

    for c in conns:
        c.close()
    time.sleep(1.0)
    results["after close"] = (rss_mb(edge.pid), rss_mb(client.pid))

    print(f"tunnels={a.tunnels} conns={a.conns} bin={a.bin} ({os.path.getsize(a.bin) / 1024 / 1024:.2f} MiB)")
    print(f"{'phase':28} {'edge MB':>9} {'client MB':>10}")
    for k, (e, c) in results.items():
        print(f"{k:28} {e:9.2f} {c:10.2f}")

    edge.terminate()
    client.terminate()
    edge.wait()
    client.wait()
    echo.close()

    peak_e = max(v[0] for v in results.values())
    peak_c = max(v[1] for v in results.values())
    bad = []
    if a.max_edge_mb is not None and peak_e > a.max_edge_mb:
        bad.append(f"edge peak {peak_e:.2f} MB > {a.max_edge_mb} MB")
    if a.max_client_mb is not None and peak_c > a.max_client_mb:
        bad.append(f"client peak {peak_c:.2f} MB > {a.max_client_mb} MB")
    if bad:
        print("内存超出预算：\n  " + "\n  ".join(bad))
        sys.exit(1)


if __name__ == "__main__":
    main()
