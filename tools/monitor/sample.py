#!/usr/bin/env python3
"""Bounded Linux/systemd snapshots; no resident collector or secret logging."""
import argparse
import datetime as dt
import fcntl
import json
import os
from pathlib import Path
import re
import resource
import socket
import subprocess
import time
import urllib.request

PROPERTIES = (
    "Id", "ActiveState", "SubState", "MainPID", "NRestarts", "Result",
    "ExecMainStartTimestamp", "MemoryCurrent", "MemoryPeak", "CPUUsageNSec",
    "TasksCurrent", "LimitNOFILE", "ControlGroup",
)
MAX_FILE_BYTES = 4 * 1024 * 1024
MAX_SERVICES = 16


def command(args, allow_no_matches=False):
    result = subprocess.run(args, text=True, timeout=15, capture_output=True)
    # journalctl --grep exits 1 with empty output when there are no matching logs.
    if allow_no_matches and result.returncode == 1 and not result.stdout.strip() and not result.stderr.strip():
        return ""
    result.check_returncode()
    return result.stdout


def fields(text):
    return dict(line.split(":", 1) for line in text.splitlines() if ":" in line)


def number(value):
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


def kb(value):
    return int(value.split()[0]) * 1024


def process_stat(text, ticks):
    # The comm field may contain spaces and closing parentheses.
    tail = text[text.rindex(")") + 2:].split()
    return {"state": tail[0], "cpu_seconds": (int(tail[11]) + int(tail[12])) / ticks,
            "start_ticks": int(tail[19])}


def cpu_percent(current, previous, boot_id, interval):
    if not previous or interval <= 0 or previous.get("boot_id") != boot_id:
        return None
    old = previous.get("process") or {}
    if (current.get("pid"), current.get("start_ticks")) != (old.get("pid"), old.get("start_ticks")):
        return None
    delta = current["cpu_seconds"] - old["cpu_seconds"]
    return round(delta / interval * 100, 4) if delta >= 0 else None


def proc_sample(pid, proc=Path("/proc")):
    path = proc / str(pid)
    ticks = os.sysconf("SC_CLK_TCK")
    first = process_stat((path / "stat").read_text(), ticks)
    status = fields((path / "status").read_text())
    result = {"pid": pid, **first}
    for key, target in (("VmRSS", "rss_bytes"), ("VmHWM", "rss_peak_bytes"),
                        ("VmSize", "virtual_bytes"), ("VmSwap", "swap_bytes"),
                        ("RssAnon", "anon_bytes"), ("RssFile", "file_bytes")):
        result[target] = kb(status[key]) if key in status else None
    result["threads"] = number(status.get("Threads", "").strip())
    result["fds"] = sum(1 for _ in (path / "fd").iterdir())
    result["sockets"] = sum(1 for p in (path / "fd").iterdir()
                            if os.readlink(p).startswith("socket:"))
    try:
        rollup = fields((path / "smaps_rollup").read_text())
        result["status_rss_bytes"] = result["rss_bytes"]
        result["rss_bytes"] = kb(rollup["Rss"])
        result["pss_bytes"] = kb(rollup["Pss"])
        result["private_bytes"] = sum(kb(rollup.get(k, "0 kB"))
                                      for k in ("Private_Clean", "Private_Dirty", "Private_Hugetlb"))
        result["rss_source"] = "smaps_rollup"
    except (OSError, KeyError):
        result["rss_source"] = "status"
    io = fields((path / "io").read_text())
    result["read_bytes"] = number(io.get("read_bytes", "").strip())
    result["write_bytes"] = number(io.get("write_bytes", "").strip())
    result["uptime_seconds"] = round(float((proc / "uptime").read_text().split()[0])
                                      - first["start_ticks"] / ticks, 3)
    if first["start_ticks"] != process_stat((path / "stat").read_text(), ticks)["start_ticks"]:
        raise RuntimeError("process changed during sampling")
    return result


def systemd_sample(unit):
    args = ["systemctl", "show", unit, "--no-pager"]
    for key in PROPERTIES:
        args.extend(["-p", key])
    values = dict(line.split("=", 1) for line in command(args).splitlines() if "=" in line)
    result = {k: values.get(k) for k in PROPERTIES}
    for key in ("MainPID", "NRestarts", "MemoryCurrent", "MemoryPeak", "CPUUsageNSec", "TasksCurrent", "LimitNOFILE"):
        result[key] = number(result[key])
    return result


def journal_sample(unit, since, until):
    # No message text is saved: credentials in third-party logs cannot leak into the monitor.
    raw = command(["journalctl", "-u", unit, "--since", "@" + str(since),
                   "--until", "@" + str(until), "-p", "warning", "-n", "201",
                   "-o", "json", "--output-fields=PRIORITY,__REALTIME_TIMESTAMP", "--no-pager"])
    events = [json.loads(line) for line in raw.splitlines() if line.startswith("{")]
    # RustFRP writes [W]/[E] to stderr; journald may assign both priority 6.
    app_raw = command(["journalctl", "-u", unit, "--since", "@" + str(since),
                       "--until", "@" + str(until), "--grep=^\\[(W|E)\\]|panicked|panic|fatal|^error:",
                       "-n", "201", "-o", "json",
                       "--output-fields=PRIORITY,MESSAGE,__CURSOR", "--no-pager"], allow_no_matches=True)
    known = {e.get("__CURSOR") for e in events if e.get("__CURSOR")}
    app_events = [json.loads(line) for line in app_raw.splitlines() if line.startswith("{")]
    extra = [e for e in app_events if e.get("__CURSOR") not in known]
    return {"warning_or_higher": len(events), "error_or_higher": sum(
        int(e.get("PRIORITY", 4)) <= 3 for e in events),
        "application_warning_or_error": len(extra),
        "application_errors": sum(bool(re.search(r"^\[E\]|panicked|panic|fatal|^error:", str(e.get("MESSAGE", "")))) for e in extra),
        "capped": len(events) >= 201 or len(app_events) >= 201}


def evaluate(item, previous, limits):
    conditions = []
    process = item.get("process") or {}
    if item.get("systemd", {}).get("ActiveState") != "active" or not process:
        conditions.append(("service_unavailable", 1))
    if item.get("sampling_error"):
        conditions.append(("sampling_error", 1))
    if previous and previous.get("boot_id") == item.get("boot_id"):
        old = previous.get("process") or {}
        if old and process and (old.get("pid"), old.get("start_ticks")) != (process.get("pid"), process.get("start_ticks")):
            conditions.append(("process_restarted", 1))
        before = previous.get("systemd", {}).get("NRestarts")
        after = item.get("systemd", {}).get("NRestarts")
        if before is not None and after is not None and after > before:
            conditions.append(("automatic_restart", 1))
    for key, metric, threshold in (("rss_budget", "rss_bytes", limits["rss_mib"] * 1024 * 1024),
                                   ("cpu_high", "cpu_percent_one_core", limits.get("cpu_percent", 10)),
                                   ("threads_high", "threads", limits.get("threads", 16))):
        if process.get(metric) is not None and process[metric] > threshold:
            conditions.append((key, 5))
    fd_limit = item.get("systemd", {}).get("LimitNOFILE")
    if fd_limit and process.get("fds", 0) >= fd_limit * 0.8:
        conditions.append(("fd_pressure", 3))
    if process.get("swap_bytes", 0):
        conditions.append(("process_swap", 3))
    if item.get("journal", {}).get("error_or_higher", 0) or item.get("journal", {}).get("application_errors", 0):
        conditions.append(("journal_error", 1))
    streaks = {key: (previous or {}).get("streaks", {}).get(key, 0) + 1 for key, _ in conditions}
    alerts = [key for key, required in conditions if streaks[key] >= required]
    return streaks, alerts


def business_metrics(config):
    if not config.get("metrics_url"):
        return None
    token = None
    for line in Path(config["metrics_env"]).read_text().splitlines():
        if line.startswith("RUSTFRP_METRICS_TOKEN="):
            token = line.split("=", 1)[1].strip().strip("\"'")
    if not token:
        raise RuntimeError("metrics credential unavailable")
    request = urllib.request.Request(config["metrics_url"], headers={"Authorization": "Bearer " + token})
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(request, timeout=5) as response:
        text = response.read(128 * 1024).decode()
    allowed = {"rustfrp_clients", "rustfrp_clients_online", "rustfrp_nodes", "rustfrp_nodes_online",
               "rustfrp_tunnels", "rustfrp_tunnels_running", "rustfrp_builtin_connections"}
    return {parts[0]: float(parts[1]) for line in text.splitlines()
            if len(parts := line.split()) == 2 and parts[0] in allowed}


def echo_probe(config):
    if not config.get("echo_address"):
        return None
    payload = b"rustfrp-monitor-" + os.urandom(16)
    with socket.create_connection(tuple(config["echo_address"]), timeout=5) as connection:
        connection.settimeout(5)
        connection.sendall(payload)
        data = b""
        while len(data) < len(payload):
            chunk = connection.recv(len(payload) - len(data))
            if not chunk:
                raise RuntimeError("incomplete echo")
            data += chunk
    return data == payload


def atomic_json(path, value):
    temp = path.with_suffix(".tmp")
    temp.write_text(json.dumps(value, ensure_ascii=False, separators=(",", ":")))
    temp.replace(path)


def append_sample(directory, sample, today):
    for path in directory.glob("samples-*.jsonl*"):
        match = re.fullmatch(r"samples-(\d{4}-\d{2}-\d{2})\.jsonl(?:\.1)?", path.name)
        if match and (today - dt.date.fromisoformat(match[1])).days >= 7:
            path.unlink()
    path = directory / ("samples-" + today.isoformat() + ".jsonl")
    data = json.dumps(sample, ensure_ascii=False, separators=(",", ":")) + "\n"
    if len(data.encode()) > MAX_FILE_BYTES:
        raise RuntimeError("sample exceeds storage bound")
    if path.exists() and path.stat().st_size + len(data.encode()) > MAX_FILE_BYTES:
        path.replace(path.with_suffix(".jsonl.1"))
    with path.open("a") as output:
        output.write(data)


def collect(config, previous):
    now = time.time()
    mono = time.monotonic()
    boot_id = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
    interval = mono - previous.get("monotonic", mono)
    same_boot = previous.get("boot_id") == boot_id
    since = max(now - 120, previous.get("timestamp", now - 60)) if same_boot else now - 60
    result = {"timestamp": now, "utc": dt.datetime.fromtimestamp(now, dt.timezone.utc).isoformat(),
              "monotonic": mono, "boot_id": boot_id, "hostname": socket.gethostname(),
              "interval_seconds": round(interval, 3) if same_boot else None, "services": {}}
    for unit, limits in config["services"].items():
        item = {"boot_id": boot_id}
        old = previous.get("services", {}).get(unit)
        try:
            item["systemd"] = systemd_sample(unit)
            pid = item["systemd"]["MainPID"]
            if pid:
                item["process"] = proc_sample(pid)
                item["process"]["cpu_percent_one_core"] = cpu_percent(item["process"], old, boot_id, interval)
            item["journal"] = journal_sample(unit, since, now)
        except (OSError, ValueError, RuntimeError, subprocess.SubprocessError):
            item["sampling_error"] = True
        item["streaks"], item["alerts"] = evaluate(item, old, limits)
        result["services"][unit] = item
    result["business"] = {}
    for name, operation in (("metrics", business_metrics), ("tcp_echo", echo_probe)):
        try:
            result["business"][name] = operation(config)
        except (OSError, ValueError, RuntimeError):
            result["business"][name + "_error"] = True
    mem = fields(Path("/proc/meminfo").read_text())
    result["host"] = {"load": os.getloadavg(), "cpus": os.cpu_count(),
                      "memory_available_bytes": kb(mem["MemAvailable"]),
                      "swap_used_bytes": kb(mem["SwapTotal"]) - kb(mem["SwapFree"])}
    alerts = [unit + ":" + reason for unit, item in result["services"].items() for reason in item["alerts"]]
    for name, value in result["business"].items():
        if name.endswith("_error") or (name == "tcp_echo" and value is False):
            alerts.append("business:" + name)
    metrics = result["business"].get("metrics") or {}
    for name, minimum in config.get("minimum_metrics", {}).items():
        if name not in metrics or metrics[name] < minimum:
            alerts.append("business:" + name + "_below_expected")
    result["alerts"] = alerts
    result["collector"] = {"duration_seconds": round(time.monotonic() - mono, 3),
                           "peak_rss_bytes": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * 1024}
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True, type=Path)
    args = parser.parse_args()
    os.umask(0o077)
    config = json.loads(args.config.read_text())
    units = config.get("services", {})
    if not units or len(units) > MAX_SERVICES or any(not re.fullmatch(r"rustfrp-test-[a-z-]+\.service", u) for u in units):
        raise ValueError("invalid service allowlist")
    for unit_limits in units.values():
        if unit_limits.get("rss_mib", 0) <= 0:
            raise ValueError("positive RSS budget required")
    directory = Path(config["state_dir"])
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (directory / "collector.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        state = directory / "latest.json"
        previous = json.loads(state.read_text()) if state.exists() else {}
        sample = collect(config, previous)
        append_sample(directory, sample, dt.datetime.now(dt.timezone.utc).date())
        atomic_json(state, sample)
        if sample["alerts"] != previous.get("alerts", []):
            print(json.dumps({"utc": sample["utc"], "alerts": sample["alerts"],
                              "event": "alert_change"}, ensure_ascii=False))
        elif not previous:
            print("RustFRP periodic monitor initialized")


if __name__ == "__main__":
    main()
