#!/usr/bin/env python3
"""Generate release layouts consumed by the signed native application updater."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[2]


def copy_file(source, destination, executable=False):
    if source.is_symlink() or not source.is_file() or not 0 < source.stat().st_size <= 128 * 1024 * 1024:
        raise ValueError(f"Invalid release artifact: {source}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)
    if executable and os.name != "nt":
        destination.chmod(0o755)


def verify_agent_manifest(resources, agent):
    manifest = json.loads((resources / "agent-manifest.json").read_text())
    with agent.open("rb") as file:
        digest = hashlib.file_digest(file, "sha256").hexdigest()
    if manifest.get("sha256") != digest or manifest.get("bytes") != agent.stat().st_size or manifest.get("file") != agent.name:
        raise ValueError("Bundled Agent does not match the release binary")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--platform", required=True, choices=["linux", "windows"])
    parser.add_argument("--desktop", required=True, type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--dist", type=Path, default=ROOT / "dist")
    args = parser.parse_args()
    version = args.version.removeprefix("v")
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", version):
        raise ValueError("Release version must be a semantic version")
    dist = args.dist.resolve()
    dist.mkdir(parents=True, exist_ok=True)
    extension = ".exe" if args.platform == "windows" else ""
    agent = ROOT / "target/release" / ("rustfrp-agent" + extension)
    server = ROOT / "target/release" / ("rustfrp-server" + extension)
    resources = ROOT / "rustfrp-desktop/resources/bin"
    verify_agent_manifest(resources, agent)
    standalone = dist / (f"rustfrp-agent-{args.platform}-x86_64" + extension)
    copy_file(agent, standalone, True)
    with standalone.open("rb") as file:
        digest = hashlib.file_digest(file, "sha256").hexdigest()
    standalone.with_suffix(standalone.suffix + ".sha256").write_text(digest + "\n")
    with tempfile.TemporaryDirectory(prefix="rustfrp-package-") as temporary:
        stage = Path(temporary)
        if args.platform == "linux":
            layout = stage / "rustfrp-linux-x86_64"
            copy_file(args.desktop.resolve(), layout / "app/rustfrp_app", True)
            for source in [agent, server]:
                copy_file(source, layout / "bin" / source.name, True)
            shutil.copytree(resources, layout / "app/resources/bin")
            archive = dist / "rustfrp-linux-x86_64.tar.gz"
            with tarfile.open(archive, "w:gz") as file:
                file.add(layout, arcname=layout.name)
            deb = stage / "deb"
            shutil.copytree(layout / "app", deb / "usr/lib/rustfrp/app")
            shutil.copytree(layout / "bin", deb / "usr/lib/rustfrp/bin")
            (deb / "usr/bin").mkdir(parents=True)
            for name in ["rustfrp-agent", "rustfrp-server"]:
                (deb / "usr/bin" / name).symlink_to("../lib/rustfrp/bin/" + name)
            icons = deb / "usr/share/icons/hicolor/128x128/apps"
            copy_file(ROOT / "assets/brand/rustfrp-logo-128.png", icons / "rustfrp.png")
            applications = deb / "usr/share/applications"
            applications.mkdir(parents=True)
            (applications / "rustfrp.desktop").write_text("[Desktop Entry]\nType=Application\nName=RustFRP\nExec=/usr/lib/rustfrp/app/rustfrp_app\nIcon=rustfrp\nCategories=Network;Utility;\n")
            (deb / "DEBIAN").mkdir()
            (deb / "DEBIAN/control").write_text(f"Package: rustfrp\nVersion: {version}\nArchitecture: amd64\nMaintainer: RustFRP contributors\nDepends: libwebkit2gtk-4.1-0, libgtk-3-0, libayatana-appindicator3-1, libxdo3\nDescription: Lightweight self-hosted tunneling client and controller\n")
            subprocess.run(["dpkg-deb", "--root-owner-group", "--build", str(deb), str(dist / "rustfrp-linux-x86_64.deb")], check=True)
        else:
            layout = stage / "windows"
            copy_file(args.desktop.resolve(), layout / "rustfrp_app.exe")
            for source in [agent, server]:
                copy_file(source, layout / source.name)
            shutil.copytree(resources, layout / "bin")
            with zipfile.ZipFile(dist / "rustfrp-windows-x86_64.zip", "w", zipfile.ZIP_DEFLATED) as file:
                for path in sorted(layout.rglob("*")):
                    if path.is_file():
                        file.write(path, path.relative_to(layout))
    print(f"Packaged native {args.platform} {version} with verified bundled Agent")


if __name__ == "__main__":
    main()
