#!/usr/bin/env python3
"""Stream retained monitor samples into a bounded resource/stability summary."""
import argparse
import collections
import datetime as dt
import json
from pathlib import Path
import re
import time


def summarize(directory, hours=24):
    cutoff = time.time() - hours * 3600
    stats, alert_counts = {}, collections.Counter()
    first = last = None
    count = invalid = gaps = 0
    for file in sorted(directory.glob("samples-*.jsonl*")):
        if not re.fullmatch(r"samples-\d{4}-\d{2}-\d{2}\.jsonl(?:\.1)?", file.name):
            continue
        with file.open() as source:
            for line in source:
                try:
                    row = json.loads(line)
                except json.JSONDecodeError:
                    invalid += 1
                    continue
                timestamp = row.get("timestamp", 0)
                if timestamp < cutoff:
                    continue
                count += 1
                first = timestamp if first is None else min(first, timestamp)
                last = timestamp if last is None else max(last, timestamp)
                interval = row.get("interval_seconds")
                gaps += int(interval is not None and interval > 120)
                alert_counts.update(row.get("alerts", []))
                for unit, item in row.get("services", {}).items():
                    current = stats.setdefault(unit, {
                        "samples": 0, "unavailable_samples": 0, "sampling_errors": 0,
                        "first_timestamp": None, "last_timestamp": None,
                        "rss_min_mib": None, "rss_max_mib": None,
                        "threads_min": None, "threads_max": None, "fds_max": None,
                        "cpu_max_percent": None, "cpu_weighted_sum": 0, "cpu_window_seconds": 0,
                        "restart_events": 0,
                    })
                    current["samples"] += 1
                    current["unavailable_samples"] += int(item.get("systemd", {}).get("ActiveState") != "active")
                    current["sampling_errors"] += int(bool(item.get("sampling_error")))
                    current["restart_events"] += int("process_restarted" in item.get("alerts", []))
                    p = item.get("process") or {}
                    if not p:
                        continue
                    rss = p.get("rss_bytes")
                    if rss is not None:
                        rss /= 1024 * 1024
                        current["rss_min_mib"] = rss if current["rss_min_mib"] is None else min(current["rss_min_mib"], rss)
                        current["rss_max_mib"] = rss if current["rss_max_mib"] is None else max(current["rss_max_mib"], rss)
                    for metric, suffix in (("threads", "min"), ("threads", "max"), ("fds", "max")):
                        key = metric + "_" + suffix
                        value = p.get(metric)
                        if value is not None:
                            op = min if suffix == "min" else max
                            current[key] = value if current[key] is None else op(current[key], value)
                    cpu = p.get("cpu_percent_one_core")
                    if cpu is not None:
                        current["cpu_max_percent"] = cpu if current["cpu_max_percent"] is None else max(current["cpu_max_percent"], cpu)
                        if interval and 0 < interval <= 120:
                            current["cpu_weighted_sum"] += cpu * interval
                            current["cpu_window_seconds"] += interval
                    if current["first_timestamp"] is None or timestamp < current["first_timestamp"]:
                        current["first_timestamp"] = timestamp
                        current["first_rss_mib"] = rss
                    if current["last_timestamp"] is None or timestamp > current["last_timestamp"]:
                        current["last_timestamp"] = timestamp
                        current["last_rss_mib"] = rss
                        current["last_pid"] = p["pid"]
                        current["last_uptime_seconds"] = p.get("uptime_seconds")
    for current in stats.values():
        total = current.pop("cpu_weighted_sum")
        window = current["cpu_window_seconds"]
        current["cpu_mean_percent_one_core"] = round(total / window, 4) if window else None
    return {"requested_hours": hours, "samples": count,
            "observed_seconds": round(last - first, 3) if count else 0,
            "first_utc": dt.datetime.fromtimestamp(first, dt.timezone.utc).isoformat() if count else None,
            "last_utc": dt.datetime.fromtimestamp(last, dt.timezone.utc).isoformat() if count else None,
            "invalid_lines": invalid, "sample_gaps_over_120s": gaps,
            "alert_sample_counts": dict(alert_counts), "services": stats}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, default=Path("/var/lib/rustfrp-test-monitor"))
    parser.add_argument("--hours", type=float, default=24)
    args = parser.parse_args()
    if not 0 < args.hours <= 168:
        parser.error("hours must be within (0, 168]")
    print(json.dumps(summarize(args.directory, args.hours), ensure_ascii=False, indent=2))
