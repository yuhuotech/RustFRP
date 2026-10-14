#!/usr/bin/env python3
"""Create an offline, labelled performance fixture only in an isolated target database."""
import argparse
import json
import os
from pathlib import Path
import sqlite3
import time

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", required=True, type=Path)
    parser.add_argument("--database", required=True, type=Path)
    parser.add_argument("--clients", type=int, default=50)
    parser.add_argument("--tunnels", type=int, default=500)
    options = parser.parse_args()
    destination = options.database.absolute()
    assert destination.is_relative_to(ROOT / "target"), "Fixtures must remain in target/"
    assert not destination.exists() and not destination.is_symlink(), "Do not overwrite a database"
    assert 1 <= options.clients <= 100 and options.clients <= options.tunnels <= 5000
    destination.parent.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(f"file:{options.source.resolve()}?mode=ro", uri=True) as source, sqlite3.connect(destination) as db:
        source.backup(db)
        db.execute("PRAGMA foreign_keys=ON")
        now = int(time.time() * 1000)
        for index in range(options.clients):
            db.execute("INSERT INTO clients(id,name,pubkey,os,arch,version,created_at,last_seen_at,tags) VALUES(?,?,?,?,?,?,?,?,?)", (
                f"ui-perf-client-{index}", f"性能样本 {index:03} 中文 🦀", f"{index:064x}", "linux", "x86_64", "0.1.0", now, now - 60000, "性能,中文"))
        for index in range(options.tunnels):
            db.execute("INSERT INTO tunnels(id,client_id,name,kind,local_ip,local_port,remote_port,enabled,created_at,updated_at,status_state) VALUES(?,?,?,?,?,?,?,?,?,?,?)", (
                f"ui-perf-tunnel-{index}", f"ui-perf-client-{index % options.clients}", f"性能隧道 {index:04} 中文名称", "tcp", "127.0.0.1", 8080, 0, 1, now, now, "pending"))
        minute = now // 60000 * 60000
        for index in range(7 * 24 * 60):
            db.execute("INSERT INTO traffic_minutes(tunnel_id,minute,bytes_in,bytes_out,connections_total,connections_peak,samples,gap) VALUES(?,?,?,?,?,?,?,?)", (
                "ui-perf-tunnel-0", minute - index * 60000, 1000 + index, 2000 + index, 3, 2, 1, int(index % 73 == 0)))
        db.commit()
        assert db.execute("SELECT count(*) FROM clients WHERE id LIKE 'ui-perf-client-%'").fetchone()[0] == options.clients
        assert db.execute("SELECT count(*) FROM tunnels WHERE id LIKE 'ui-perf-tunnel-%'").fetchone()[0] == options.tunnels
    if os.name != "nt":
        destination.chmod(0o600)
    print(json.dumps({"database": str(destination), "clients": options.clients, "tunnels": options.tunnels, "traffic_minutes": 7 * 24 * 60, "offline_fixture": True}, indent=2))


if __name__ == "__main__":
    main()
