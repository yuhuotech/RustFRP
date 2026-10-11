#!/usr/bin/env python3
"""校验 lcov 覆盖率是否达标（行 / 函数 / 分支），低于门槛或低于基线则失败。

用法：check_lcov.py <lcov.info> [--min 90] [--name core] [--baseline tools/cov/baseline.json] [--update-baseline]
分支数据（BRF/BRH）不存在时（如 Dart 的 lcov），分支项显示 n/a 并跳过门槛。
"""
import argparse
import json
import sys
from pathlib import Path


def parse(path):
    tot = {"LF": 0, "LH": 0, "FNF": 0, "FNH": 0, "BRF": 0, "BRH": 0}
    per_file = {}
    cur = None
    for raw in Path(path).read_text().splitlines():
        if raw.startswith("SF:"):
            cur = raw[3:]
            per_file[cur] = {k: 0 for k in tot}
        elif raw.split(":", 1)[0] in tot and cur is not None:
            k, v = raw.split(":", 1)
            tot[k] += int(v)
            per_file[cur][k] += int(v)
    return tot, per_file


def pct(h, f):
    return None if f == 0 else 100.0 * h / f


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("lcov")
    ap.add_argument("--min", type=float, default=90.0)
    ap.add_argument("--name", default="default")
    ap.add_argument("--baseline", default="tools/cov/baseline.json")
    ap.add_argument("--update-baseline", action="store_true")
    ap.add_argument("--list-gaps", action="store_true")
    a = ap.parse_args()

    tot, per_file = parse(a.lcov)
    cur = {
        "lines": pct(tot["LH"], tot["LF"]),
        "functions": pct(tot["FNH"], tot["FNF"]),
        "branches": pct(tot["BRH"], tot["BRF"]),
    }
    print(f"[{a.name}] 行 {fmt(cur['lines'])} ({tot['LH']}/{tot['LF']})  函数 {fmt(cur['functions'])} "
          f"({tot['FNH']}/{tot['FNF']})  分支 {fmt(cur['branches'])} ({tot['BRH']}/{tot['BRF']})")

    if a.list_gaps:
        for f, t in sorted(per_file.items()):
            miss = []
            if t["LF"] > t["LH"]:
                miss.append(f"行 {t['LH']}/{t['LF']}")
            if t["BRF"] > t["BRH"]:
                miss.append(f"分支 {t['BRH']}/{t['BRF']}")
            if miss:
                print("  缺口:", f, ", ".join(miss))

    failed = []
    for k, v in cur.items():
        if v is not None and v < a.min:
            failed.append(f"{k} {v:.2f}% < {a.min}%")

    base_path = Path(a.baseline)
    base = json.loads(base_path.read_text()) if base_path.exists() else {}
    prev = base.get(a.name, {})
    for k, v in cur.items():
        p = prev.get(k)
        if v is not None and p is not None and v + 0.05 < p:
            failed.append(f"{k} 低于基线：{v:.2f}% < {p:.2f}%")

    if a.update_baseline and not failed:
        base[a.name] = {k: (round(v, 2) if v is not None else None) for k, v in cur.items()}
        base_path.parent.mkdir(parents=True, exist_ok=True)
        base_path.write_text(json.dumps(base, indent=2, ensure_ascii=False) + "\n")
        print("基线已更新")

    if failed:
        print("覆盖率未达标：\n  " + "\n  ".join(failed))
        sys.exit(1)


def fmt(v):
    return "n/a" if v is None else f"{v:.2f}%"


if __name__ == "__main__":
    main()
