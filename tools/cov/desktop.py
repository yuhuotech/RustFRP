#!/usr/bin/env python3
"""Collect desktop unit + real system WebView/Agent coverage and gate each crate."""
import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
DESKTOP = ROOT / "rustfrp-desktop"


def run(command, env, cwd=ROOT, capture=False):
    return subprocess.run(command, cwd=cwd, env=env, text=True, check=True, capture_output=capture)


def split(report, source, output):
    """Keep every record belonging to a crate's source directory, without metric edits."""
    prefix = str(source.resolve()).replace("\\", "/").rstrip("/") + "/"
    records = []
    for record in report.read_text().split("end_of_record"):
        lines = record.strip().splitlines()
        filenames = [line[3:].replace("\\", "/") for line in lines if line.startswith("SF:")]
        if filenames and all(filename.startswith(prefix) for filename in filenames):
            records.append("\n".join(lines) + "\nend_of_record\n")
    if not records:
        raise RuntimeError(f"No coverage records for {source}")
    output.write_text("".join(records))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report-only", action="store_true", help="gate profiles from an already completed native run")
    parser.add_argument("--target-dir", type=Path, default=DESKTOP / "target/llvm-cov-target")
    args = parser.parse_args()
    target = args.target_dir.resolve()
    env = os.environ.copy()
    env.update({"RUSTC_BOOTSTRAP": "1", "CARGO_TARGET_DIR": str(target), "CARGO_LLVM_COV_TARGET_DIR": str(target)})
    output = ROOT / "target/cov"
    output.mkdir(parents=True, exist_ok=True)
    if not args.report_only:
        # The dedicated target may contain code maps from older source layouts.
        # Remove binaries as well as profiles so split lcov records describe the
        # current crate files, especially after modules move between adapters.
        run(["cargo", "clean"], env, DESKTOP)
        # Test library targets only. A zero-test binary harness links the app
        # library again and would add uncovered duplicate monomorphizations;
        # the real executable is exercised separately by native WebView E2E.
        run(["cargo", "llvm-cov", "test", "--branch", "--workspace", "--lib", "--all-features", "--no-report"], env, DESKTOP)
        native_env = run(["cargo", "llvm-cov", "show-env", "--sh", "--branch"], env, DESKTOP, True).stdout
        for line in native_env.splitlines():
            if line.startswith("export "):
                key, value = line.removeprefix("export ").split("=", 1)
                env[key] = shlex.split(value.rstrip(";"))[0]
        run(["cargo", "build", "-p", "rustfrp-desktop-support", "--example", "local_probe"], env, DESKTOP)
        cli = ["node", str(ROOT / "rustfrp-ui/node_modules/@tauri-apps/cli/tauri.js")]
        bundle = ["--bundles", "app"] if sys.platform == "darwin" else ["--no-bundle"]
        run(cli + ["build", "--debug", *bundle, "--features", "native-e2e", "--config", "tauri.e2e.conf.json"], env, DESKTOP)
        if sys.platform == "darwin":
            executable = target / "debug/bundle/macos/RustFRP E2E.app/Contents/MacOS/rustfrp-desktop"
        else:
            staging = target / "native-e2e/app"
            staging.mkdir(parents=True, exist_ok=True)
            filename = "rustfrp-desktop.exe" if os.name == "nt" else "rustfrp-desktop"
            executable = staging / filename
            shutil.copy2(target / "debug" / filename, executable)
            resources = staging if os.name == "nt" else staging / "resources"
            shutil.copytree(DESKTOP / "resources/bin", resources / "bin", dirs_exist_ok=True)
        env["RUSTFRP_DESKTOP_PROBE"] = str(target / "debug/examples" / ("local_probe.exe" if os.name == "nt" else "local_probe"))
        env["RUSTFRP_DESKTOP_SHELL"] = str(executable)
        # The normal release server/Agent remain outside the instrumented desktop build.
        ordinary = os.environ.copy()
        ordinary.pop("CARGO_TARGET_DIR", None)
        run(["cargo", "build", "--release", "-p", "rustfrp-server", "-p", "rustfrp-agent", "--features", "rustfrp-server/embedded-panel"], ordinary)
        for command in [[sys.executable, "tools/e2e/native_local.py"], [sys.executable, "tools/e2e/native_window.py", str(executable)]]:
            run([sys.executable, "tools/e2e/run_ui.py", "--", *command], env)
    combined = output / "desktop-combined.lcov"
    run([
        "cargo", "llvm-cov", "report", "--branch", "--workspace",
        "--ignore-filename-regex", r"(src/main\.rs|src/native_calls\.rs|src/native_e2e\.rs)$",
        "--lcov", "--output-path", str(combined),
    ], env, DESKTOP)
    reports = []
    for name, source, filename in [
        ("rustfrp-desktop", DESKTOP / "src", "desktop.lcov"),
        ("rustfrp-desktop-support", DESKTOP / "crates/support/src", "desktop-support.lcov"),
    ]:
        path = output / filename
        split(combined, source, path)
        reports.append((name, path))
    failed = False
    for name, path in reports:
        result = subprocess.run([sys.executable, "tools/cov/check_lcov.py", str(path), "--name", name, "--require-all", "--list-gaps"], cwd=ROOT, env=env)
        failed = failed or result.returncode != 0
    (output / "desktop-run.json").write_text(json.dumps({"target": str(target), "platform": sys.platform, "report_only": args.report_only, "native_report": "target/ui/native-window-e2e.json"}, indent=2) + "\n")
    if failed:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
