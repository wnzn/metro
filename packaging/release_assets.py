#!/usr/bin/env python3
"""The release aggregate: exactly the expected archives, sidecars and installer scripts.

`assemble` takes the per-platform CI outputs (the `dist-*` artifacts) and writes
the release folder with SHA256SUMS:

  - desktop archives: Linux x64 and Windows x64, plus macOS arm64 and x64 in
    `release` mode (tags); the Linux runner archive; each with its .sha256
  - the Windows MSI and its .sha256, and the two PowerShell scripts, exactly as
    windows-distribution.yml produced them (signed on tags; never replaced by
    checkout copies)
  - the shell installers from this checkout (install-metro-macos.sh only in
    release mode, next to the notarized macOS archives)

Every archive is opened and checked against its sidecar and RELEASE.json.
Release mode also requires: the metro-tui terminal client in every desktop
archive (named by RELEASE.json "tui"), both macOS archives notarized, stapled Developer ID
builds per RELEASE.json (the native macOS gate already verified the real
signature and ticket), and an Authenticode signature present on every Windows
executable, the MSI and both .ps1 scripts (validity is checked natively by the
Windows signer). Anything else in the input, such as an ad hoc preview or an
unsigned seed, is refused.

The output is the `release-assets` artifact that sign-release-assets.yml
(release_signatures.py prepare) takes as is: SHA256SUMS lists every file except
the .sha256 sidecars, one "<sha256>  <name>" line each, sorted by name.
"""

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import struct
import tarfile
import zipfile

HERE = Path(__file__).resolve().parent
DESKTOP = ("linux-x64", "windows-x64", "macos-arm64", "macos-x64")
SH_SCRIPTS = ("install-metro-linux.sh", "install-metro-runner-linux.sh", "metro-runner-container.sh")
PS1_SCRIPTS = ("install-metro-windows.ps1", "metro-runner-container.ps1")
MACOS_SCRIPT = "install-metro-macos.sh"
VERSION = re.compile(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?")
MSI_SIGNATURE = "\x05DigitalSignature".encode("utf-16-le")
# The metro-tui terminal client inside each desktop archive (package.py TUI_NAMES).
TUI = {"linux-x64": "metro-tui", "windows-x64": "metro-tui.exe",
       "macos-arm64": "Metro.app/Contents/MacOS/metro-tui", "macos-x64": "Metro.app/Contents/MacOS/metro-tui"}


class AssetError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise AssetError(message)


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def sha256(path):
    with open(path, "rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


class Layout:
    """The file names of one release (or pull-request preview) folder."""

    def __init__(self, version, mode):
        require(VERSION.fullmatch(version), f"bad version {version!r}")
        require(mode in ("release", "smoke"), f"bad mode {mode!r}")
        require(mode == "smoke" or not version.endswith("-ci"), "release mode needs a tag version")
        self.version, self.mode = version, mode
        desktop = DESKTOP if mode == "release" else DESKTOP[:2]
        self.archives = {f"Metro-{version}-{t}{'.zip' if t == 'windows-x64' else '.tar.gz'}": ("desktop", t)
                         for t in desktop}
        self.archives[f"Metro-runner-{version}-linux-x64.tar.gz"] = ("runner", "linux-x64")
        self.msi = f"Metro-{version}-windows-x64.msi"
        self.ps1 = PS1_SCRIPTS
        self.sh = SH_SCRIPTS + ((MACOS_SCRIPT,) if mode == "release" else ())

    def collected(self):
        """What the dist-* artifacts must hold, exactly."""
        with_sidecars = [*self.archives, self.msi]
        return set(with_sidecars) | {n + ".sha256" for n in with_sidecars} | set(self.ps1)

    def summed(self):
        """Files listed in SHA256SUMS."""
        return {*self.archives, self.msi, *self.ps1, *self.sh}

    def folder(self):
        return self.summed() | {n + ".sha256" for n in (*self.archives, self.msi)} | {"SHA256SUMS"}


# --- archives -----------------------------------------------------------------

def members(path):
    """name -> bytes for every member; only regular files."""
    out = {}
    if path.suffix == ".zip":
        with zipfile.ZipFile(path) as z:
            for info in z.infolist():
                require(not info.is_dir() and (info.external_attr >> 16) & 0o170000 in (0, 0o100000),
                        f"{path.name}: {info.filename} is not a regular file")
                out[info.filename] = z.read(info)
    else:
        with tarfile.open(path, "r:gz") as t:
            for info in t:
                require(info.isfile(), f"{path.name}: {info.name} is not a regular file")
                out[info.name] = t.extractfile(info).read()
    return out


def authenticode_present(data):
    """True when a PE file carries a certificate table (an embedded Authenticode signature)."""
    try:
        (pe,) = struct.unpack_from("<I", data, 0x3C)
        if data[:2] != b"MZ" or data[pe:pe + 4] != b"PE\0\0":
            return False
        opt = pe + 24
        (magic,) = struct.unpack_from("<H", data, opt)
        dirs = opt + {0x10B: 96, 0x20B: 112}[magic]
        (count,) = struct.unpack_from("<I", data, dirs - 4)
        if count < 5:
            return False
        offset, size = struct.unpack_from("<II", data, dirs + 4 * 8)
        return size > 8 and offset + size <= len(data)
    except (struct.error, KeyError):
        return False


def ps1_signed(data):
    text = data.decode("utf-8-sig", errors="replace")
    return "\n# SIG # Begin signature block" in text and text.rstrip().endswith("# SIG # End signature block")


def check_archive(path, version, kind, target, release):
    top = path.name.removesuffix(".zip").removesuffix(".tar.gz")
    files = members(path)
    require(all(n.startswith(top + "/") and ".." not in n.split("/") for n in files),
            f"{path.name}: entries outside {top}/")
    try:
        manifest = json.loads(files[f"{top}/RELEASE.json"])
    except KeyError:
        raise AssetError(f"{path.name}: no RELEASE.json") from None
    require((manifest.get("kind"), manifest.get("target"), manifest.get("version")) == (kind, target, version),
            f"{path.name}: RELEASE.json describes {manifest.get('kind')} {manifest.get('target')} "
            f"{manifest.get('version')}")
    listed = manifest.get("files", {})
    require({f"{top}/{n}" for n in listed} | {f"{top}/RELEASE.json"} == set(files),
            f"{path.name}: files differ from RELEASE.json")
    for name, digest in listed.items():
        require(sha256_bytes(files[f"{top}/{name}"]) == digest, f"{path.name}: {name} does not match RELEASE.json")
    if kind == "desktop":
        tui = manifest.get("tui")
        require(tui in (None, TUI[target]) and (tui is None or tui in listed),
                f"{path.name}: RELEASE.json names an unexpected terminal client")
        require(tui or not release, f"{path.name}: a release desktop archive must include {TUI[target]}")
    if target.startswith("macos-"):
        n = manifest.get("notarization") or {}
        require(manifest.get("signing") == "developer-id" and manifest.get("status") == "notarized"
                and n.get("status") == "Accepted" and n.get("stapled") is True
                and re.fullmatch(r"[0-9a-fA-F-]{36}", str(n.get("submission_id", "")))
                and re.fullmatch(r"[A-Z0-9]{10}", str(manifest.get("signing_team", ""))),
                f"{path.name}: not a notarized, stapled Developer ID build; ad hoc previews are never released")
        for ticket in n.get("ticket_files") or [None]:
            require(ticket and f"{top}/{ticket}" in files, f"{path.name}: the stapled ticket is missing")
    elif target == "windows-x64":
        require(manifest.get("signing") != "adhoc", f"{path.name}: unexpected signing status")
        if release:
            code = [n for n in files if n.lower().endswith((".exe", ".dll"))]
            require(code, f"{path.name}: no Windows executables")
            unsigned = [n.split("/", 1)[1] for n in code if not authenticode_present(files[n])]
            require(not unsigned, f"{path.name}: not Authenticode signed: {', '.join(sorted(unsigned))}")
    else:
        require("signing" not in manifest and "status" not in manifest,
                f"{path.name}: unexpected signing status for {target}")
    return manifest


def check_sidecar(path):
    sidecar = path.with_name(path.name + ".sha256")
    require(sidecar.is_file(), f"missing {sidecar.name}")
    require(sidecar.read_text() == f"{sha256(path)}  {path.name}\n", f"{sidecar.name} does not match {path.name}")


def check_windows_extras(folder, layout):
    check_sidecar(folder / layout.msi)
    msi = (folder / layout.msi).read_bytes()
    require(msi[:8] == b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1", f"{layout.msi} is not an MSI (OLE) file")
    if layout.mode == "release":
        require(MSI_SIGNATURE in msi, f"{layout.msi} is not Authenticode signed")
        for script in layout.ps1:
            require(ps1_signed((folder / script).read_bytes()), f"{script} is not Authenticode signed")


def check_contents(folder, layout):
    for name, (kind, target) in layout.archives.items():
        check_sidecar(folder / name)
        check_archive(folder / name, layout.version, kind, target, layout.mode == "release")
    check_windows_extras(folder, layout)


# --- SHA256SUMS -----------------------------------------------------------------

def write_sums(folder, names):
    (folder / "SHA256SUMS").write_text("".join(f"{sha256(folder / n)}  {n}\n" for n in sorted(names)))


def check_sums(folder, names):
    lines = (folder / "SHA256SUMS").read_text().splitlines()
    want = [f"{sha256(folder / n)}  {n}" for n in sorted(names)]
    require(lines == want, "SHA256SUMS does not list exactly the release files with their checksums")


def check_folder(folder, expected):
    have = {p.name for p in folder.iterdir()}
    require(all(p.is_file() and not p.is_symlink() for p in folder.iterdir()), "release folder holds non-files")
    require(have == expected, "release folder differs from the expected set: " + ", ".join(sorted(have ^ expected)))


# --- commands -----------------------------------------------------------------

def assemble(version, mode, collected, out, scripts_dir=HERE):
    layout = Layout(version, mode)
    want, have = layout.collected(), {p.name for p in collected.iterdir()}
    require(not (want - have), "missing release files: " + ", ".join(sorted(want - have)))
    require(not (have - want), "unexpected files (never released): " + ", ".join(sorted(have - want)))
    check_contents(collected, layout)
    out.mkdir(parents=True)
    for name in sorted(want):
        shutil.copyfile(collected / name, out / name)
    for script in layout.sh:
        shutil.copyfile(scripts_dir / script, out / script)
    for path in out.iterdir():
        path.chmod(0o644)
    write_sums(out, layout.summed())
    verify(version, mode, out)


def verify(version, mode, folder):
    layout = Layout(version, mode)
    check_folder(folder, layout.folder())
    check_contents(folder, layout)
    check_sums(folder, layout.summed())
    return layout


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    a = sub.add_parser("assemble")
    a.add_argument("--version", required=True)
    a.add_argument("--mode", required=True, choices=("release", "smoke"))
    a.add_argument("--collected", required=True, type=Path)
    a.add_argument("--out", required=True, type=Path)
    v = sub.add_parser("verify")
    v.add_argument("--version", required=True)
    v.add_argument("--mode", required=True, choices=("release", "smoke"))
    v.add_argument("--release", required=True, type=Path)
    args = parser.parse_args()
    try:
        if args.command == "assemble":
            assemble(args.version, args.mode, args.collected, args.out)
            print((args.out / "SHA256SUMS").read_text(), end="")
        else:
            verify(args.version, args.mode, args.release)
            print(f"release folder for {args.version} ({args.mode}) verified")
    except (OSError, ValueError, KeyError, tarfile.TarError, zipfile.BadZipFile, AssetError) as error:
        parser.exit(1, f"release_assets: {error}\n")


if __name__ == "__main__":
    main()
