#!/usr/bin/env python3
"""Fixtures for the native in-app MSI update test (test-msi-update.ps1).

Test builds only. Nothing here is released or signed for real.

  payload  --version V --launcher Metro.exe --ui msi_update_ui.exe --dest DIR
           A payload tree for build_msi.py (`--preview`): the launcher (an
           e2e test build, `metro-desktop` with `--features e2e-test-release`,
           whose workspace version is V), the stand-in UI as ui/Metro.exe and
           as metro-server.exe (the update test never opens the local
           workspace), and RELEASE.json. Prints the payload directory.
  release  --version V --msi FILE --dest DIR
           A fixture release v<V> that the desktop's test verifier accepts:
           every file SHA256SUMS must list (placeholders except the MSI),
           SHA256SUMS and its test bundle (metro_release::testing format).
  serve    --root DIR --latest V --port-file FILE
           GitHub's release download URLs on 127.0.0.1 (any free port,
           written to FILE) for every <DIR>/v<X>/ release, until stopped.
           Point METRO_TEST_RELEASE_BASE at http://127.0.0.1:<port>.

Exit status 0 on success, 1 with an `update_fixture:` message.
"""

import argparse
import hashlib
import http.server
import json
import re
import shutil
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
TEST_COMMIT = "0123456789abcdef0123456789abcdef01234567"
SIGNED_AT = 1800000000
VERSION = re.compile(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)")


class Fail(Exception):
    pass


def sha(data):
    return hashlib.sha256(data).hexdigest()


def test_bundle(data):
    """metro_release::testing::test_bundle (accepted only by TestVerifier)."""
    return json.dumps({"metroTestBundle": 1, "sha256": sha(data), "commit": TEST_COMMIT,
                       "signedAt": SIGNED_AT}, separators=(",", ":")).encode()


def installers():
    """metro_release::manifest::INSTALLERS, read from the source."""
    text = (ROOT / "crates/release/src/manifest.rs").read_text(encoding="utf-8")
    m = re.search(r"pub const INSTALLERS: \[&str; \d+\] = \[(.*?)\];", text, re.S)
    if not m:
        raise Fail("INSTALLERS not found in crates/release/src/manifest.rs")
    return re.findall(r'"([^"]+)"', m.group(1))


def images(version):
    """metro_release::testing::fixture_images."""
    d = lambda c: c * 64  # noqa: E731
    return (f"# Metro container images v{version}\n# Source commit: {TEST_COMMIT}\n\n"
            f"ghcr.io/wnzn/metro-runner:v{version}@sha256:{d('a')}  linux/amd64,linux/arm64\n"
            f"ghcr.io/wnzn/metro-runner@sha256:{d('b')}  linux/amd64\n"
            f"ghcr.io/wnzn/metro-runner@sha256:{d('c')}  linux/arm64\n\n"
            f"ghcr.io/wnzn/metro-server:v{version}@sha256:{d('d')}  linux/amd64,linux/arm64\n"
            f"ghcr.io/wnzn/metro-server@sha256:{d('e')}  linux/amd64\n"
            f"ghcr.io/wnzn/metro-server@sha256:{d('f')}  linux/arm64\n")


def check_version(version):
    if not VERSION.fullmatch(version or ""):
        raise Fail(f"not X.Y.Z: {version!r}")


def make_payload(version, launcher, ui, dest):
    check_version(version)
    launcher, ui, dest = Path(launcher), Path(ui), Path(dest)
    for f in (launcher, ui):
        if not f.is_file() or f.is_symlink():
            raise Fail(f"not a file: {f}")
    top = dest / f"Metro-{version}-windows-x64"
    if top.exists():
        raise Fail(f"{top} already exists")
    (top / "ui").mkdir(parents=True)
    shutil.copyfile(launcher, top / "Metro.exe")
    shutil.copyfile(ui, top / "ui" / "Metro.exe")
    shutil.copyfile(ui, top / "metro-server.exe")
    files = {rel: sha((top / rel).read_bytes()) for rel in ("Metro.exe", "metro-server.exe", "ui/Metro.exe")}
    manifest = {"kind": "desktop", "target": "windows-x64", "version": version, "commit": TEST_COMMIT,
                "local_runner": "installer-marker", "files": files}
    (top / "RELEASE.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return top


def release_files(version, msi_name, msi_bytes):
    check_version(version)
    allowed = (f"Metro-{version}-windows-x64.msi", f"Metro-{version}-windows-x64.unsigned-preview.msi")
    if msi_name not in allowed:
        raise Fail(f"the MSI must be named {allowed[1]} (or {allowed[0]}), not {msi_name}")
    binaries = [f"Metro-{version}-linux-x64.tar.gz", f"Metro-{version}-windows-x64.zip", msi_name,
                f"Metro-runner-{version}-linux-x64.tar.gz"]
    files = {}
    for name in binaries + installers():
        files[name] = f"placeholder {name}\n".encode()
    files[msi_name] = msi_bytes
    for name in binaries:
        files[f"{name}.sha256"] = f"{sha(files[name])}  {name}\n".encode()
    files["IMAGES.txt"] = images(version).encode()
    files["SHA256SUMS"] = "".join(f"{sha(files[n])}  {n}\n" for n in sorted(files)).encode()
    files["SHA256SUMS.sigstore.json"] = test_bundle(files["SHA256SUMS"])
    for name in (binaries[0], binaries[3]):
        files[f"{name}.sigstore.json"] = test_bundle(files[name])
    return files


def make_release(version, msi, dest):
    msi = Path(msi)
    if not msi.is_file():
        raise Fail(f"not a file: {msi}")
    out = Path(dest) / f"v{version}"
    if out.exists():
        raise Fail(f"{out} already exists")
    out.mkdir(parents=True)
    for name, data in release_files(version, msi.name, msi.read_bytes()).items():
        (out / name).write_bytes(data)
    return out


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):  # noqa: N802
        prefix = "/wnzn/metro/releases/"
        rest = self.path[len(prefix):] if self.path.startswith(prefix) else ""
        version = name = None
        if rest.startswith("latest/download/"):
            version, name = self.server.latest, rest[len("latest/download/"):]
        elif rest.startswith("download/v"):
            version, _, name = rest[len("download/v"):].partition("/")
        body = None
        if version and name and VERSION.fullmatch(version) and re.fullmatch(r"[A-Za-z0-9._-]{1,160}", name) \
                and not name.startswith("."):
            path = self.server.root / f"v{version}" / name
            if path.is_file():
                body = path.read_bytes()
        self.send_response(200 if body is not None else 404)
        body = body if body is not None else b"not found"
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


def make_server(root, latest):
    check_version(latest)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.root, server.latest = Path(root), latest
    return server


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("payload")
    p.add_argument("--version", required=True)
    p.add_argument("--launcher", required=True)
    p.add_argument("--ui", required=True)
    p.add_argument("--dest", required=True)
    p = sub.add_parser("release")
    p.add_argument("--version", required=True)
    p.add_argument("--msi", required=True)
    p.add_argument("--dest", required=True)
    p = sub.add_parser("serve")
    p.add_argument("--root", required=True)
    p.add_argument("--latest", required=True)
    p.add_argument("--port-file", required=True)
    a = ap.parse_args(argv)
    try:
        if a.cmd == "payload":
            print(make_payload(a.version, a.launcher, a.ui, a.dest))
        elif a.cmd == "release":
            print(make_release(a.version, a.msi, a.dest))
        else:
            server = make_server(a.root, a.latest)
            tmp = Path(a.port_file + ".tmp")
            tmp.write_text(str(server.server_address[1]), encoding="ascii")
            tmp.replace(a.port_file)
            server.serve_forever()
    except Fail as e:
        print(f"update_fixture: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
