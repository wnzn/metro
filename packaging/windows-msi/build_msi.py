#!/usr/bin/env python3
"""Metro's per-user Windows MSI: Metro-<version>-windows-x64.msi.

Builds an *unsigned* Windows Installer package from an already verified
desktop payload tree (the extracted Metro-<version>-windows-x64/ folder of the
portable ZIP, whose RELEASE.json lists every file and its SHA-256). Nothing is
downloaded or compiled for the user; the MSI only copies those files. Signing
is a separate, later step: sign the EXEs first (and update RELEASE.json the way
the ZIP needs it anyway), build the MSI from that tree, then sign the MSI.

Subcommands (all paths are arguments; run with `python3 -I`):

  fetch-tools --dest DIR
      Download the pinned msitools 0.106 .debs (wixl-lock.json), check each
      SHA-256, unpack them into DIR (new or empty) and check the unpacked tree.
  extract --zip Metro-<v>-windows-x64.zip (--sha256 HEX | --checksum-file F) --dest DIR
      Check the ZIP against its checksum, unpack it safely into DIR (new or
      empty), validate the payload, and print the payload directory.
  build --tools DIR --payload DIR --version SEMVER --out FILE [--receipt FILE]
        [--require-file REL]... [--source-date-epoch N] [--preview]
      Validate the payload and write the unsigned MSI to FILE (which must not
      exist). FILE must be named Metro-<SEMVER>-windows-x64.msi, or
      Metro-<SEMVER>-windows-x64.unsigned-preview.msi together with --preview.
      The same inputs always give the same bytes.
  inspect --tools DIR --msi FILE --version SEMVER [--payload DIR] [--preview]
      Static checks of a built MSI (tables, wizard with Browse, upgrade rules,
      per-user flags, and with --payload the exact file bytes).
  fixtures --tools DIR --dest DIR
      Build small test MSIs (a separate "Metro MSI test fixture" product with
      its own UpgradeCode, folder, registry key and shortcut) for the native
      Windows checks in test-msi-windows.ps1. Never released.

This script never installs anything and never signs anything.
"""

import argparse
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
import uuid
import zipfile
from xml.sax.saxutils import escape, quoteattr

HERE = Path(__file__).resolve().parent
LOCK = HERE / "wixl-lock.json"
UI_WXS = HERE / "metro-ui.wxs"
# ProductName suffix of an unsigned (--preview) build; MsiHarness.ps1 checks the same name.
UNSIGNED_SUFFIX = " (unsigned)"
EXT_DIR = HERE / "wixl-ext"
APP_ICON = HERE.parents[1] / "apps/desktop/assets/metro.ico"
SETUP_MARK = "MetroSetupMark"
APP_ICON_ID = "MetroApp.ico"
TOOLS_STAMP = "METRO-MSI-TOOLS.json"

# Fixed forever for the released product: changing it breaks upgrades.
NAMESPACE = uuid.UUID("18478927-d898-45b0-9203-a5cf7dec62d7")
IDENTITIES = {
    "release": {
        "product_name": "Metro",
        "manufacturer": "Metro",
        "upgrade_code": "{29BBE899-7D6A-49A7-B60E-F9644544D683}",
        "default_dir": "Metro Desktop",
        "reg_key": r"Software\Metro\Desktop MSI",
        "shortcut": "Metro",
        "legacy_key": r"Software\Microsoft\Windows\CurrentVersion\Uninstall\dev.metro.desktop",
        "data_dir": r"[LocalAppDataFolder]Metro\Desktop",
    },
    # Test-only product for test-msi-windows.ps1; never shares anything with "release".
    "fixture": {
        "product_name": "Metro MSI test fixture",
        "manufacturer": "Metro",
        "upgrade_code": "{D8AC2BC3-7A7C-4AA3-896B-CA3ED48E8B50}",
        "default_dir": "Metro MSI Fixture",
        "reg_key": r"Software\Metro\Desktop MSI Fixture",
        "shortcut": "Metro MSI Fixture",
        "legacy_key": r"Software\Microsoft\Windows\CurrentVersion\Uninstall\dev.metro.desktop.msi-fixture",
        "data_dir": r"[LocalAppDataFolder]Metro MSI Fixture Data",
    },
}
MARKER = "enable-local-runner"
REQUIRED = ("Metro.exe", "metro-server.exe", "ui/Metro.exe")
SEMVER = re.compile(
    r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*))?"
    r"(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?")
HEX64 = re.compile(r"[0-9a-f]{64}")
WIN_BAD = re.compile(r'[<>:"/\\|?*\x00-\x1f]')
WIN_RESERVED = {"CON", "PRN", "AUX", "NUL", *(f"COM{i}" for i in range(1, 10)), *(f"LPT{i}" for i in range(1, 10))}

# Tab order per dialog (first entry is the dialog's first control, see metro-ui.wxs).
TAB_ORDER = {
    "WelcomeDlg": ["Next", "Cancel", "Back"],
    "InstallDirDlg": ["Folder", "ChangeFolder", "LocalRunner", "StartMenu", "Next", "Back", "Cancel"],
    "BrowseDlg": ["PathEdit", "OK", "Cancel", "DirectoryCombo", "Up", "NewFolder", "DirectoryList"],
    "VerifyReadyDlg": ["Install", "Repair", "Remove", "Back", "Cancel"],
    "MaintenanceDlg": ["RepairButton", "RemoveButton", "Cancel", "Back", "Next"],
    "ProgressDlg": ["Cancel", "Back", "Next"],
    "ExitDialog": ["Finish", "Back", "Cancel"],
    "UserExit": ["Finish", "Back", "Cancel"],
    "FatalError": ["Finish", "Back", "Cancel"],
    "CancelDlg": ["No", "Yes"],
    "FilesInUse": ["Retry", "Ignore", "Exit"],
    "OutOfDiskDlg": ["OK"],
}
REQUIRED_DIALOGS = set(TAB_ORDER) | {"ErrorDlg"}
PUBLIC_PROPS = ["INSTALLDIR", "LOCALRUNNER", "STARTMENUSHORTCUT", "LRBOX", "SCBOX", "CHOICESDONE",
                "METRO_REPLACE_SAME_VERSION"]


class Fail(Exception):
    pass


def require(cond, msg):
    if not cond:
        raise Fail(msg)


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


# ---------------------------------------------------------------- versions

def windows_version(semver):
    """Map SemVer to a Windows Installer ProductVersion (3 fields, compared by
    FindRelatedProducts). Field 3 = PATCH*100 + rank, where rank is 99 for a
    release, 10+N for alpha.N, 30+N for beta.N, 50+N for rc.N (alpha/beta/rc
    without a number count as N=0) and 0 for any other prerelease (local or
    dev builds such as 0.2.0-setup-<sha>). So every prerelease of x.y.z sorts
    below x.y.z, and x.y.z below every x.y.(z+1) build. Build metadata (+...)
    is ignored, like SemVer precedence. Builds that map to the same
    ProductVersion are never replaced silently (see METRO_REPLACE_SAME_VERSION).
    Returns (product_version, kind) where kind is release|alpha|beta|rc|dev."""
    m = SEMVER.fullmatch(semver or "")
    require(m, f"not a SemVer 2.0 version: {semver!r}")
    major, minor, patch = (int(m.group(i)) for i in (1, 2, 3))
    pre = m.group(4)
    require(major <= 255 and minor <= 255, f"{semver}: Windows Installer needs MAJOR and MINOR <= 255")
    require(patch <= 654, f"{semver}: PATCH must be <= 654 for the Windows Installer version mapping")
    if pre is None:
        rank, kind = 99, "release"
    else:
        c = re.fullmatch(r"(alpha|beta|rc)(?:\.(0|[1-9]\d*))?", pre)
        if c:
            kind = c.group(1)
            n = int(c.group(2) or 0)
            base, limit = {"alpha": (10, 19), "beta": (30, 19), "rc": (50, 48)}[kind]
            require(n <= limit, f"{semver}: {kind}.N supports N <= {limit} in the Windows Installer mapping")
            rank = base + n
        else:
            rank, kind = 0, "dev"
    return f"{major}.{minor}.{patch * 100 + rank}", kind


def product_code(identity, semver):
    return "{%s}" % str(uuid.uuid5(NAMESPACE, f"{identity}:product:{semver}")).upper()


def package_code(identity, semver, payload_digest, preview, sources):
    key = f"{identity}:package:{semver}:{payload_digest}:{'preview' if preview else 'plain'}"
    key += ":" + json.dumps(sources, sort_keys=True)
    return "{%s}" % str(uuid.uuid5(NAMESPACE, key)).upper()


def stable_id(prefix, text):
    return prefix + hashlib.sha256(text.encode()).hexdigest()[:20].upper()


def component_guid(identity, key):
    return "{%s}" % str(uuid.uuid5(NAMESPACE, f"{identity}:component:{key}")).upper()


# ---------------------------------------------------------------- payload

def check_rel(rel):
    p = PurePosixPath(rel)
    require(rel and not rel.startswith("/") and "\\" not in rel and str(p) == rel,
            f"RELEASE.json lists an invalid path: {rel!r}")
    for part in p.parts:
        require(part not in (".", "..") and not WIN_BAD.search(part) and not part.endswith((" ", "."))
                and part.split(".")[0].upper() not in WIN_RESERVED,
                f"RELEASE.json lists a name Windows can't use: {rel!r}")
    require(len(rel) <= 180, f"path too long for a predictable Windows install: {rel!r}")


def validate_payload(payload, semver, required=()):
    """The payload tree must match its RELEASE.json exactly (as the portable ZIP
    and install-metro-windows.ps1 require). Returns (manifest, [(rel, sha, size)], digest)."""
    payload = Path(payload)
    require(payload.is_dir() and not payload.is_symlink(), f"payload is not a directory: {payload}")
    mpath = payload / "RELEASE.json"
    require(mpath.is_file() and not mpath.is_symlink(), "payload has no RELEASE.json")
    try:
        manifest = json.loads(mpath.read_text(encoding="utf-8"))
    except ValueError as e:
        raise Fail(f"RELEASE.json is not valid JSON: {e}")
    require(manifest.get("kind") == "desktop" and manifest.get("target") == "windows-x64",
            "RELEASE.json does not describe a windows-x64 desktop package")
    require(manifest.get("version") == semver,
            f"RELEASE.json version {manifest.get('version')!r} is not {semver!r}")
    require(manifest.get("local_runner") == "installer-marker",
            "RELEASE.json has no local_runner=installer-marker: this build predates the local runner choice")
    files = manifest.get("files")
    require(isinstance(files, dict) and files, "RELEASE.json has no file list")
    entries, seen_ci = [], {}
    for rel, digest in sorted(files.items()):
        require(isinstance(rel, str) and isinstance(digest, str) and HEX64.fullmatch(digest),
                f"RELEASE.json lists an invalid entry: {rel!r}")
        check_rel(rel)
        require(rel != "RELEASE.json" and rel.lower() != MARKER, f"RELEASE.json must not list {rel}")
        low = rel.lower()
        require(low not in seen_ci, f"two files differ only in case: {seen_ci.get(low)} and {rel}")
        seen_ci[low] = rel
        path = payload / rel
        st = os.lstat(path) if os.path.lexists(path) else None
        require(st is not None and stat.S_ISREG(st.st_mode), f"payload is missing {rel}")
        require(sha256_file(path) == digest, f"payload file does not match RELEASE.json: {rel}")
        entries.append((rel, digest, st.st_size))
    # Every directory component must be a real directory (no links anywhere).
    for rel, _, _ in entries:
        for parent in PurePosixPath(rel).parents:
            if str(parent) != ".":
                require(not (payload / str(parent)).is_symlink(), f"payload contains a link: {parent}")
    want = set(files) | {"RELEASE.json"}
    dirs_needed = {str(p) for rel in want for p in PurePosixPath(rel).parents if str(p) != "."}
    for root, dnames, fnames in os.walk(payload, followlinks=False):
        for name in dnames + fnames:
            full = Path(root) / name
            rel = full.relative_to(payload).as_posix()
            require(not full.is_symlink(), f"payload contains a link: {rel}")
            if full.is_dir():
                require(rel in dirs_needed, f"payload has a directory RELEASE.json does not need: {rel}")
            else:
                require(rel in want, f"payload has a file RELEASE.json does not list: {rel}")
    for rel in tuple(REQUIRED) + tuple(required):
        require(rel in files, f"payload is missing {rel}")
    rel_sha = sha256_file(mpath)
    entries.append(("RELEASE.json", rel_sha, mpath.stat().st_size))
    digest = hashlib.sha256("".join(f"{r}\0{s}\n" for r, s, _ in sorted(entries)).encode()).hexdigest()
    return manifest, sorted(entries), digest


def extract_zip(zip_path, expected, dest):
    zip_path, dest = Path(zip_path), Path(dest)
    m = re.fullmatch(r"Metro-(.+)-windows-x64\.zip", zip_path.name)
    require(m, f"expected Metro-<version>-windows-x64.zip, got {zip_path.name}")
    semver, top = m.group(1), zip_path.name[:-4]
    windows_version(semver)
    require(HEX64.fullmatch(expected or ""), "expected SHA-256 must be 64 lowercase hex digits")
    actual = sha256_file(zip_path)
    require(actual == expected, f"checksum mismatch for {zip_path.name}: expected {expected}, got {actual}")
    dest.mkdir(parents=True, exist_ok=True)
    require(not any(dest.iterdir()), f"{dest} is not empty")
    with zipfile.ZipFile(zip_path) as z:
        names = z.namelist()
        require(len(names) == len(set(names)), "archive has duplicate entries")
        for info in z.infolist():
            n = info.filename
            parts = n.split("/")
            require(n.startswith(top + "/") and not n.endswith("/") and "\\" not in n and ":" not in n
                    and ".." not in parts and "." not in parts and "" not in parts,
                    f"unexpected archive entry: {n}")
            mode = (info.external_attr >> 16) & 0o170000
            require(mode in (0, stat.S_IFREG), f"archive entry is not a regular file: {n}")
        for info in z.infolist():
            out = dest / info.filename
            out.parent.mkdir(parents=True, exist_ok=True)
            with z.open(info) as src, open(out, "xb") as dst:
                shutil.copyfileobj(src, dst)
    payload = dest / top
    validate_payload(payload, semver)
    return payload, semver


# ---------------------------------------------------------------- tools

def read_lock():
    return json.loads(LOCK.read_text())


def tree_digest(root):
    root = Path(root)
    lines = []
    for p in sorted(root.rglob("*")):
        rel = p.relative_to(root).as_posix()
        if rel == TOOLS_STAMP:
            continue
        if p.is_symlink():
            lines.append(f"L {rel} {os.readlink(p)}")
        elif p.is_file():
            lines.append(f"F {rel} {sha256_file(p)}")
        elif p.is_dir():
            lines.append(f"D {rel}")
    return hashlib.sha256("\n".join(lines).encode()).hexdigest()


def _deb_data(deb_bytes):
    require(deb_bytes[:8] == b"!<arch>\n", "not a .deb archive")
    off = 8
    while off + 60 <= len(deb_bytes):
        hdr = deb_bytes[off:off + 60]
        name = hdr[:16].decode().strip().rstrip("/")
        size = int(hdr[48:58].decode().strip())
        body = deb_bytes[off + 60: off + 60 + size]
        if name.startswith("data.tar"):
            require(name in ("data.tar.xz", "data.tar.gz"), f"unsupported .deb payload {name}")
            return body, name
        off += 60 + size + (size & 1)
    raise Fail("no data.tar in .deb")


def fetch_tools(dest):
    lock = read_lock()
    dest = Path(dest)
    dest.mkdir(parents=True, exist_ok=True)
    require(not any(dest.iterdir()), f"{dest} is not empty")
    for deb in lock["debs"]:
        data = None
        for url in deb["urls"]:
            try:
                req = urllib.request.Request(url, headers={"User-Agent": "metro-msi-build"})
                with urllib.request.urlopen(req, timeout=60) as r:
                    data = r.read(64 << 20)
                break
            except OSError as e:
                print(f"fetch-tools: {url}: {e}", file=sys.stderr)
        require(data is not None, f"could not download {deb['name']}")
        got = hashlib.sha256(data).hexdigest()
        require(got == deb["sha256"], f"{deb['name']}: SHA-256 {got} is not the pinned {deb['sha256']}")
        body, name = _deb_data(data)
        with tarfile.open(fileobj=io.BytesIO(body), mode="r:xz" if name.endswith("xz") else "r:gz") as t:
            members = [m for m in t.getmembers()
                       if m.name.lstrip("./").startswith(("usr/bin/", "usr/lib/"))]
            t.extractall(dest, members=members, filter="data")
    got = tree_digest(dest)
    (dest / TOOLS_STAMP).write_text(json.dumps({"tool_version": lock["tool_version"], "tree_sha256": got,
                                                "debs": {d["name"]: d["sha256"] for d in lock["debs"]}},
                                               indent=2) + "\n")
    if lock["tree_sha256"] != got:
        raise Fail(f"unpacked tool tree {got} is not the pinned tree_sha256 {lock['tree_sha256']}")
    return got


class Tools:
    def __init__(self, root):
        self.root = Path(root).resolve()
        lock = read_lock()
        require((self.root / "usr/bin/wixl").is_file(), f"no wixl under {self.root} (run fetch-tools)")
        got = tree_digest(self.root)
        require(got == lock["tree_sha256"], f"tool tree {self.root} ({got}) is not the pinned one "
                                            f"({lock['tree_sha256']}); run fetch-tools into a new directory")
        self.tree_sha256 = got
        self.debs = {d["name"]: d["sha256"] for d in lock["debs"]}
        self.env = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8", "TZ": "UTC",
                    "LD_LIBRARY_PATH": str(self.root / "usr/lib/x86_64-linux-gnu"), "HOME": "/nonexistent"}
        out = self.run("wixl", "--version").stdout.strip()
        require(out == lock["tool_version"], f"wixl reports {out!r}, expected {lock['tool_version']}")
        self.version = out

    def run(self, tool, *args, cwd=None, check=True):
        p = subprocess.run([str(self.root / "usr/bin" / tool), *map(str, args)], cwd=cwd, env=self.env,
                           capture_output=True, text=True, timeout=600)
        noisy = re.search(r"CRITICAL|WARNING|ERROR|error:", p.stderr)
        if check and (p.returncode != 0 or noisy):
            raise Fail(f"{tool} failed ({p.returncode}): {p.stderr.strip()[-2000:]}")
        return p

    def table(self, msi, name):
        if name in ("Binary", "Icon"):
            # IDT export writes object columns as side files; never touch the
            # caller's working directory when inspecting an installer.
            with tempfile.TemporaryDirectory(prefix="metro-msi-table-") as export:
                p = self.run("msiinfo", "export", Path(msi).resolve(), name, cwd=export, check=False)
        else:
            p = self.run("msiinfo", "export", msi, name, check=False)
        if p.returncode != 0:
            return None
        lines = p.stdout.split("\r\n") if "\r\n" in p.stdout else p.stdout.split("\n")
        cols = lines[0].split("\t")
        rows = []
        for line in lines[3:]:
            if line:
                vals = line.split("\t")
                rows.append(dict(zip(cols, vals + [""] * (len(cols) - len(vals)))))
        return rows

    def tables(self, msi):
        return set(self.run("msiinfo", "tables", msi).stdout.split())

    def stream(self, msi, name):
        p = subprocess.run([str(self.root / "usr/bin/msiinfo"), "extract", str(msi), name],
                           env=self.env, capture_output=True, timeout=600)
        return p.stdout if p.returncode == 0 else None

    def suminfo(self, msi):
        out = self.run("msiinfo", "suminfo", msi).stdout
        return dict(line.split(": ", 1) for line in out.splitlines() if ": " in line)


# ---------------------------------------------------------------- WXS

SHORT_OK = re.compile(r"[A-Za-z0-9_^$~!#%&@{}()'`-]{1,8}(\.[A-Za-z0-9_^$~!#%&@{}()'`-]{1,3})?")


def msi_name(long_name, rel_key, used):
    """`short|long` for names that aren't valid 8.3 names; unique per directory."""
    if SHORT_OK.fullmatch(long_name) and long_name.upper() not in used:
        used.add(long_name.upper())
        return long_name
    ext = "".join(c for c in PurePosixPath(long_name).suffix[1:] if c.isalnum())[:3]
    n = 0
    while True:
        h = hashlib.sha256(f"{rel_key}\0{n}".encode()).hexdigest()[:8].upper()
        short = f"{h}.{ext}" if ext else h
        if short.upper() not in used:
            used.add(short.upper())
            return f"{short}|{long_name}"
        n += 1


def a(v):
    return quoteattr(str(v))


def product_wxs(identity, semver, pv, entries, files_root, marker_path, preview):
    idn = IDENTITIES[identity]
    reg = idn["reg_key"]
    # Directory tree: "" is INSTALLDIR.
    dirs = {""}
    for rel, _, _ in entries:
        for p in PurePosixPath(rel).parents:
            dirs.add("" if str(p) == "." else str(p))
    children = {d: [] for d in dirs}
    for d in sorted(dirs):
        if d:
            parent = str(PurePosixPath(d).parent)
            children["" if parent == "." else parent].append(d)
    files_in = {d: [] for d in dirs}
    for rel, digest, size in entries:
        parent = str(PurePosixPath(rel).parent)
        files_in["" if parent == "." else parent].append(rel)

    def dir_id(d):
        return "INSTALLDIR" if d == "" else stable_id("D_", d)

    used_names = {d: set() for d in dirs}
    out = []
    w = out.append

    def emit_dir(d, indent):
        sp = " " * indent
        comp = "C_ROOT" if d == "" else stable_id("C_", d)
        w(f"{sp}<Component Id={a(comp)} Guid={a(component_guid(identity, 'dir:' + d))}>")
        w(f"{sp}  <RegistryValue Root=\"HKCU\" Key={a(reg + chr(92) + 'Components')} Name={a(comp)} "
          f"Type=\"integer\" Value=\"1\" KeyPath=\"yes\" />")
        w(f"{sp}  <RemoveFolder Id={a(stable_id('RF_', d))} On=\"uninstall\" />")
        for rel in sorted(files_in[d]):
            name = PurePosixPath(rel).name
            w(f"{sp}  <File Id={a(stable_id('F_', rel))} Name={a(msi_name(name, rel, used_names[d]))} "
              f"Source={a(files_root / rel)} />")
        w(f"{sp}</Component>")
        if d == "":
            # Marker: present only while the local runner choice is on.
            w(f"{sp}<Component Id=\"C_MARKER\" Guid={a(component_guid(identity, 'marker'))}>")
            w(f"{sp}  <Condition>LRBOX</Condition>")
            w(f"{sp}  <RegistryValue Root=\"HKCU\" Key={a(reg + chr(92) + 'Components')} Name=\"C_MARKER\" "
              f"Type=\"integer\" Value=\"1\" KeyPath=\"yes\" />")
            w(f"{sp}  <File Id=\"F_MARKER\" Name={a(msi_name(MARKER, MARKER, used_names[d]))} "
              f"Source={a(marker_path)} />")
            w(f"{sp}</Component>")
            w(f"{sp}<Component Id=\"C_PREFS\" Guid={a(component_guid(identity, 'prefs'))}>")
            for name, value, key in (("InstallDir", "[INSTALLDIR]", True), ("Version", semver, False),
                                     ("LocalRunner", "[LRSTATE]", False),
                                     ("StartMenuShortcut", "[SCSTATE]", False)):
                kp = ' KeyPath="yes"' if key else ""
                w(f"{sp}  <RegistryValue Root=\"HKCU\" Key={a(reg)} Name={a(name)} Type=\"string\" "
                  f"Value={a(value)}{kp} />")
            w(f"{sp}</Component>")
        for c in sorted(children[d]):
            name = PurePosixPath(c).name
            w(f"{sp}<Directory Id={a(dir_id(c))} Name={a(msi_name(name, 'dir:' + c, used_names[d]))}>")
            emit_dir(c, indent + 2)
            w(f"{sp}</Directory>")

    emit_dir("", 12)
    tree = "\n".join(out)
    comps = ["C_ROOT", "C_MARKER", "C_PREFS", "C_SHORTCUT"] + [stable_id("C_", d) for d in sorted(dirs) if d]
    comp_refs = "\n".join(f"      <ComponentRef Id={a(c)} />" for c in comps)

    root_names = set()
    default_dir = msi_name(idn["default_dir"], "root:" + idn["default_dir"], root_names)
    data_dir = idn["data_dir"]
    # Unsigned builds (pull request previews and, while packaging/windows-release-signing.txt
    # says unsigned, official releases): wording that is true for both.
    pname = idn["product_name"] + (UNSIGNED_SUFFIX if preview else "")
    comments = (f"{idn['product_name']} {semver}" +
                (". Unsigned build: not Authenticode signed; check it against its SHA-256 checksum." if preview else ""))
    legacy = idn["legacy_key"]

    def ca(id_, prop, value):
        return f"    <CustomAction Id={a(id_)} Property={a(prop)} Value={a(value)} />"

    cas = "\n".join([
        ca("SetInstallDirFromPrev", "INSTALLDIR", "[PREVINSTALLDIR]"),
        ca("LrDefault", "LRBOX", "1"), ca("LrOn", "LRBOX", "1"),
        ca("ScDefault", "SCBOX", "1"), ca("ScOn", "SCBOX", "1"),
        ca("ChoicesDone", "CHOICESDONE", "1"),
        ca("LrState", "LRSTATE", "1"), ca("ScState", "SCSTATE", "1"),
        ca("SetArpInstallLocation", "ARPINSTALLLOCATION", "[INSTALLDIR]"),
    ])
    # Decisions: an explicit LOCALRUNNER/STARTMENUSHORTCUT (1 or 0) wins, else
    # the previous install's choice, else on. With the full UI the client
    # decides once (CHOICESDONE) and the user's checkbox state is passed on.
    decide = [
        ("SetInstallDirFromPrev", 51, "PREVINSTALLDIR AND NOT INSTALLDIR"),
        ("LrDefault", 52, 'NOT CHOICESDONE AND NOT LOCALRUNNER AND PREVLOCALRUNNER <> "0"'),
        ("LrOn", 53, 'NOT CHOICESDONE AND LOCALRUNNER = "1"'),
        ("ScDefault", 54, 'NOT CHOICESDONE AND NOT STARTMENUSHORTCUT AND PREVSTARTMENU <> "0"'),
        ("ScOn", 55, 'NOT CHOICESDONE AND STARTMENUSHORTCUT = "1"'),
    ]
    ui_seq = "\n".join(f"      <Custom Action={a(x)} Sequence=\"{n}\" Condition={a(c)} />" for x, n, c in decide)
    ui_seq += "\n      <Custom Action=\"ChoicesDone\" Sequence=\"56\" Condition=\"NOT CHOICESDONE\" />"
    ex_seq = "\n".join(f"      <Custom Action={a(x)} Sequence=\"{n}\" Condition={a(c)} />" for x, n, c in decide)
    ex_seq += ("\n      <Custom Action=\"LrState\" Sequence=\"57\" Condition=\"LRBOX\" />"
               "\n      <Custom Action=\"ScState\" Sequence=\"58\" Condition=\"SCBOX\" />"
               "\n      <Custom Action=\"SetArpInstallLocation\" After=\"CostFinalize\" />"
               "\n      <RemoveExistingProducts After=\"InstallInitialize\" />")

    conds = [
        ("VersionNT64", "Metro for Windows needs 64-bit Windows 10 version 1809 or newer, or Windows 11."),
        ("NOT ALLUSERS", "This Metro installer is per-user only. Run it without ALLUSERS."),
        ("Installed OR NOT LEGACYSCRIPTDIR",
         "Metro is already installed for you by install-metro-windows.ps1 in [LEGACYSCRIPTDIR]. "
         "This installer doesn't replace or share that copy. Uninstall it first (Settings > Apps > "
         "Installed apps > Metro > Uninstall; your Metro data is kept), then run this installer again."),
        ("Installed OR NOT NEWERFOUND",
         "A newer Metro ([PREVSEMVER]) is already installed, so nothing was changed. To go back to "
         "[MetroSemver], uninstall Metro first (your Metro data is kept), then run this installer."),
        ('Installed OR NOT SAMEVERSIONFOUND OR METRO_REPLACE_SAME_VERSION = "1"',
         "A different Metro build ([PREVSEMVER]) with the same Windows Installer version "
         "([ProductVersion]) is installed. Windows can't tell which build is newer, so nothing was "
         "changed. Uninstall it first, or run this installer with METRO_REPLACE_SAME_VERSION=1 "
         "(msiexec /i <this file> METRO_REPLACE_SAME_VERSION=1) to replace it."),
        ('NOT LOCALRUNNER OR LOCALRUNNER = "0" OR LOCALRUNNER = "1"', "LOCALRUNNER must be 1 or 0."),
        ('NOT STARTMENUSHORTCUT OR STARTMENUSHORTCUT = "0" OR STARTMENUSHORTCUT = "1"',
         "STARTMENUSHORTCUT must be 1 or 0."),
        ('NOT METRO_REPLACE_SAME_VERSION OR METRO_REPLACE_SAME_VERSION = "1"',
         "METRO_REPLACE_SAME_VERSION must be 1 when set."),
    ]
    cond_xml = "\n".join(f"    <Condition Message={a(m)}>{escape(c)}</Condition>" for c, m in conds)

    def search(prop, root, key, name, win64=False):
        w64 = ' Win64="yes"' if win64 else ""
        return (f"    <Property Id={a(prop)}><RegistrySearch Id={a('S_' + prop)} Root={a(root)} "
                f"Key={a(key)} Name={a(name)} Type=\"raw\"{w64} /></Property>")

    searches = "\n".join([
        search("PREVINSTALLDIR", "HKCU", reg, "InstallDir"),
        search("PREVSEMVER", "HKCU", reg, "Version"),
        search("PREVLOCALRUNNER", "HKCU", reg, "LocalRunner"),
        search("PREVSTARTMENU", "HKCU", reg, "StartMenuShortcut"),
        search("LEGACYSCRIPTDIR", "HKCU", legacy, "InstallLocation"),
        search("VCREDIST", "HKLM", r"SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64", "Installed", True),
    ])
    props = "\n".join(f"    <Property Id={a(k)} Value={a(v)} />" for k, v in [
        ("MetroSemver", semver), ("MetroDataDir", data_dir), ("MetroPreview", "1" if preview else "0"),
        ("LRSTATE", "0"), ("SCSTATE", "0"), ("ARPNOMODIFY", "1"), ("ARPCOMMENTS", comments),
        ("ARPPRODUCTICON", APP_ICON_ID),
        ("ARPURLINFOABOUT", "https://github.com/wnzn/metro"), ("ARPHELPLINK", "https://github.com/wnzn/metro"),
    ])
    uc = idn["upgrade_code"]
    return f"""<?xml version="1.0" encoding="utf-8"?>
<!-- Generated by packaging/windows-msi/build_msi.py; do not edit. -->
<Wix xmlns="http://schemas.microsoft.com/wix/2006/wi">
  <Product Id={a(product_code(identity, semver))} Name={a(pname)} Language="1033" Version={a(pv)}
           Manufacturer={a(idn['manufacturer'])} UpgradeCode={a(uc)}>
    <Package InstallerVersion="500" Compressed="yes" InstallScope="perUser" Description={a(pname)}
             Comments={a(comments)} Manufacturer={a(idn['manufacturer'])} />
    <Media Id="1" Cabinet="metro.cab" EmbedCab="yes" />
    <Icon Id={a(APP_ICON_ID)} SourceFile={a(APP_ICON)} />
    <Binary Id={a(SETUP_MARK)} SourceFile={a(APP_ICON)} />
    <Upgrade Id={a(uc)}>
      <UpgradeVersion Property="OLDERFOUND" Minimum="0.0.0" IncludeMinimum="yes" Maximum={a(pv)} IncludeMaximum="no" />
      <UpgradeVersion Property="SAMEVERSIONFOUND" Minimum={a(pv)} IncludeMinimum="yes" Maximum={a(pv)} IncludeMaximum="yes" />
      <UpgradeVersion Property="NEWERFOUND" Minimum={a(pv)} IncludeMinimum="no" OnlyDetect="yes" />
    </Upgrade>
{searches}
{props}
{cond_xml}
{cas}
    <InstallUISequence>
{ui_seq}
    </InstallUISequence>
    <InstallExecuteSequence>
{ex_seq}
    </InstallExecuteSequence>
    <Directory Id="TARGETDIR" Name="SourceDir">
      <Directory Id="LocalAppDataFolder">
        <Directory Id="ProgramsDir" Name="Programs">
          <Directory Id="INSTALLDIR" Name={a(default_dir)}>
{tree}
          </Directory>
        </Directory>
      </Directory>
      <Directory Id="ProgramMenuFolder">
        <Component Id="C_SHORTCUT" Guid={a(component_guid(identity, 'shortcut'))}>
          <Condition>SCBOX</Condition>
          <RegistryValue Root="HKCU" Key={a(reg + chr(92) + 'Components')} Name="C_SHORTCUT" Type="integer" Value="1" KeyPath="yes" />
          <Shortcut Id="S_START" Name={a(msi_name(idn['shortcut'] + '.lnk', 'shortcut', set()))} Target="[INSTALLDIR]Metro.exe"
                    WorkingDirectory="INSTALLDIR" Description={a(idn['product_name'])} Icon={a(APP_ICON_ID)} IconIndex="0" />
        </Component>
      </Directory>
    </Directory>
    <Feature Id="Main" Title={a(idn['product_name'])} Level="1">
{comp_refs}
    </Feature>
    <UIRef Id="MetroUI" />
  </Product>
</Wix>
"""


# ---------------------------------------------------------------- summary patch

FREE = 0xFFFFFFFA


def patch_summary(path, new_uuid, filetime):
    """Rewrite the PackageCode (PID 9) and both timestamps (PIDs 12, 13) of the
    \\x05SummaryInformation stream in place, so a build is reproducible."""
    data = bytearray(Path(path).read_bytes())
    require(data[:8] == bytes.fromhex("D0CF11E0A1B11AE1"), "not an OLE compound file")
    u16 = lambda o: struct.unpack_from("<H", data, o)[0]
    u32 = lambda o: struct.unpack_from("<I", data, o)[0]
    ss, mss = 1 << u16(0x1E), 1 << u16(0x20)
    nfat, dir_start, cutoff = u32(0x2C), u32(0x30), u32(0x38)
    minifat_start, difat_next, ndifat = u32(0x3C), u32(0x44), u32(0x48)
    difat = [u32(0x4C + 4 * i) for i in range(109)]
    for _ in range(ndifat):
        off = 512 + difat_next * ss if ss == 512 else (difat_next + 1) * ss
        difat += [u32(off + 4 * i) for i in range(ss // 4 - 1)]
        difat_next = u32(off + ss - 4)
    sec_off = lambda s: (s + 1) * ss
    fat = []
    for s in [x for x in difat if x < FREE][:nfat]:
        fat += struct.unpack_from(f"<{ss // 4}I", data, sec_off(s))

    def chain(start, table):
        out, s = [], start
        while s < FREE:
            require(len(out) <= len(table), "corrupt sector chain")
            out.append(s)
            s = table[s]
        return out

    dir_bytes_off = [sec_off(s) for s in chain(dir_start, fat)]
    entries = []
    for base in dir_bytes_off:
        for i in range(ss // 128):
            o = base + i * 128
            nlen = u16(o + 0x40)
            name = data[o:o + max(nlen - 2, 0)].decode("utf-16-le", "replace")
            entries.append((name, data[o + 0x42], u32(o + 0x74), struct.unpack_from("<Q", data, o + 0x78)[0]))
    root = entries[0]
    found = [e for e in entries if e[0] == "\x05SummaryInformation" and e[1] == 2]
    require(len(found) == 1, "no SummaryInformation stream")
    _, _, start, size = found[0]
    if size < cutoff:
        minifat = []
        for s in chain(minifat_start, fat):
            minifat += struct.unpack_from(f"<{ss // 4}I", data, sec_off(s))
        mini_secs = chain(root[2], fat)
        offsets = []
        for m in chain(start, minifat):
            pos = m * mss
            offsets.append(sec_off(mini_secs[pos // ss]) + pos % ss)
        unit = mss
    else:
        offsets, unit = [sec_off(s) for s in chain(start, fat)], ss

    def at(i):
        return offsets[i // unit] + i % unit

    stream = bytes(data[at(i)] for i in range(size))
    require(stream[:2] == b"\xfe\xff", "bad property set")
    sect = struct.unpack_from("<I", stream, 44)[0]
    count = struct.unpack_from("<I", stream, sect + 4)[0]
    props = {}
    for i in range(count):
        pid, off = struct.unpack_from("<II", stream, sect + 8 + 8 * i)
        props[pid] = sect + off
    patches = []
    for pid in (12, 13):
        require(pid in props and struct.unpack_from("<I", stream, props[pid])[0] == 64, "no FILETIME")
        patches.append((props[pid] + 4, struct.pack("<Q", filetime)))
    require(9 in props and struct.unpack_from("<I", stream, props[9])[0] == 30, "no PackageCode")
    n = struct.unpack_from("<I", stream, props[9] + 4)[0]
    new = new_uuid.encode("ascii") + b"\0"
    require(n == len(new) and re.fullmatch(rb"\{[0-9A-F-]{36}\}\0", stream[props[9] + 8: props[9] + 8 + n]),
            "unexpected PackageCode layout")
    patches.append((props[9] + 8, new))
    for off, b in patches:
        for i, byte in enumerate(b):
            data[at(off + i)] = byte
    Path(path).write_bytes(bytes(data))


# ---------------------------------------------------------------- build

def out_name(semver, preview):
    return f"Metro-{semver}-windows-x64" + (".unsigned-preview.msi" if preview else ".msi")


REMOTE_CONTROLS = (("BrowseDlg", "DirectoryCombo"), ("OutOfDiskDlg", "VolumeList"))


def post_process(tools, msi):
    sql = []
    # wixl ignores Remote="yes": list mapped network drives too (msidbControlAttributesRemoteVolume).
    attrs = {(r["Dialog_"], r["Control"]): int(r["Attributes"]) for r in tools.table(msi, "Control")}
    for key in REMOTE_CONTROLS:
        sql.append(f"UPDATE `Control` SET `Attributes` = {attrs[key] | 0x40000} "
                   f"WHERE `Dialog_` = '{key[0]}' AND `Control` = '{key[1]}'")
    for dlg, order in TAB_ORDER.items():
        for i, ctl in enumerate(order):
            nxt = order[(i + 1) % len(order)] if len(order) > 1 else ctl
            sql.append(f"UPDATE `Control` SET `Control_Next` = '{nxt}' "
                       f"WHERE `Dialog_` = '{dlg}' AND `Control` = '{ctl}'")
        sql.append(f"UPDATE `Dialog` SET `Control_First` = '{order[0]}' WHERE `Dialog` = '{dlg}'")
    # Transitive: a repair or reinstall re-evaluates the marker and shortcut choices.
    for comp in ("C_MARKER", "C_SHORTCUT"):
        sql.append(f"UPDATE `Component` SET `Attributes` = 324 WHERE `Component` = '{comp}'")
    sql.append("UPDATE `Property` SET `Value` = '" + ";".join(
        ["NEWERFOUND", "OLDERFOUND", "SAMEVERSIONFOUND"] + PUBLIC_PROPS) +
        "' WHERE `Property` = 'SecureCustomProperties'")
    tools.run("msibuild", msi, "-q", *sql)


def build(tools, payload, semver, out, identity="release", preview=False, required=(), epoch=None,
          receipt=None, extra_receipt=None):
    out = Path(out)
    require(out.name == out_name(semver, preview) or identity != "release",
            f"output must be named {out_name(semver, preview)}")
    require(not out.exists(), f"{out} already exists")
    pv, kind = windows_version(semver)
    manifest, entries, digest = validate_payload(payload, semver, required)
    sources = {"script_sha256": sha256_file(Path(__file__)), "ui_sha256": sha256_file(UI_WXS),
               "icon_sha256": sha256_file(APP_ICON),
               "ext_sha256": {p.name: sha256_file(p) for p in sorted((EXT_DIR / "ui").glob("*.wxs"))}}
    if epoch is None:
        epoch = int(os.environ.get("SOURCE_DATE_EPOCH", "315532800"))
    require(315532800 <= epoch < 4102444800, "SOURCE_DATE_EPOCH must be between 1980 and 2100")
    with tempfile.TemporaryDirectory(prefix="metro-msi-", dir=out.parent) as work:
        work = Path(work)
        files_root = work / "files"
        for rel, _, _ in entries:
            dst = files_root / rel
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(Path(payload) / rel, dst)
            require(sha256_file(dst) == dict((r, s) for r, s, _ in entries)[rel], f"copy changed {rel}")
        marker = work / "marker" / MARKER
        marker.parent.mkdir()
        marker.write_bytes(b"")
        for p in [*files_root.rglob("*"), marker]:
            if p.is_file():
                os.utime(p, (epoch, epoch))
        wxs = work / "product.wxs"
        wxs.write_text(product_wxs(identity, semver, pv, entries, files_root, marker, preview))
        raw = work / "raw.msi"
        tools.run("wixl", "-a", "x64", "--ext", "ui", "--extdir", EXT_DIR, "-o", raw, wxs, UI_WXS, cwd=work)
        post_process(tools, raw)
        pc = package_code(identity, semver, digest, preview, sources)
        patch_summary(raw, pc, (epoch + 11644473600) * 10 ** 7)
        inspect(tools, raw, semver, payload=payload, preview=preview, identity=identity, entries=entries,
                expect_package_code=pc)
        tmp = out.with_name(out.name + ".partial")
        shutil.copyfile(raw, tmp)
        os.replace(tmp, out)
    info = {
        "artifact": out.name, "sha256": sha256_file(out), "size": out.stat().st_size,
        "signing": "unsigned", "preview": preview, "identity": identity,
        "semver": semver, "product_version": pv, "version_kind": kind,
        "product_code": product_code(identity, semver), "package_code": pc,
        "upgrade_code": IDENTITIES[identity]["upgrade_code"],
        "payload": {"release_json_sha256": dict((r, s) for r, s, _ in entries)["RELEASE.json"],
                    "tree_sha256": digest, "files": len(entries),
                    "has_metro_tui": "metro-tui.exe" in manifest["files"]},
        "source_date_epoch": epoch,
        "tools": {"wixl": tools.version, "tree_sha256": tools.tree_sha256, "debs": tools.debs},
        "builder": sources,
    }
    if extra_receipt:
        info.update(extra_receipt)
    if receipt:
        Path(receipt).write_text(json.dumps(info, indent=2, sort_keys=True) + "\n")
    return info


# ---------------------------------------------------------------- inspect

def inspect(tools, msi, semver, payload=None, preview=False, identity="release", entries=None,
            expect_package_code=None):
    """Static validation of a built MSI. Raises Fail with every problem found."""
    idn = IDENTITIES[identity]
    pv, _ = windows_version(semver)
    problems = []

    def check(cond, msg):
        if not cond:
            problems.append(msg)

    T = {name: tools.table(msi, name) or [] for name in (
        "Property", "Directory", "Component", "File", "Dialog", "Control", "ControlEvent", "ControlCondition",
        "CheckBox", "EventMapping", "InstallUISequence", "InstallExecuteSequence", "Upgrade", "LaunchCondition",
        "CustomAction", "AppSearch", "RegLocator", "Registry", "RemoveFile", "Shortcut", "Binary", "Icon", "TextStyle",
        "Media")}
    prop = {r["Property"]: r["Value"] for r in T["Property"]}
    si = tools.suminfo(msi)
    check(si.get("Template") == "x64;1033", f"template is {si.get('Template')}")
    check(si.get("Source", "").startswith("10 "), "word count must be 10 (compressed, long names, no elevation)")
    check(si.get("Version", "").startswith("500 "), "needs Windows Installer 5.0")
    if expect_package_code:
        check(si.get("Revision number (UUID)") == expect_package_code, "PackageCode is not the deterministic one")
    check(prop.get("ProductCode") == product_code(identity, semver), "ProductCode mismatch")
    check(prop.get("ProductVersion") == pv, f"ProductVersion {prop.get('ProductVersion')} != {pv}")
    check(prop.get("UpgradeCode") == idn["upgrade_code"], "UpgradeCode mismatch")
    check(prop.get("MetroSemver") == semver, "MetroSemver mismatch")
    check(prop.get("MetroPreview") == ("1" if preview else "0"), "preview flag mismatch")
    check(prop.get("ProductName", "").endswith(UNSIGNED_SUFFIX) == preview, "preview name mismatch")
    for p in ("ALLUSERS", "MSIINSTALLPERUSER", "LOCALRUNNER", "STARTMENUSHORTCUT", "LRBOX", "SCBOX", "INSTALLDIR"):
        check(p not in prop, f"Property table must not preset {p}")
    check(prop.get("WIXUI_INSTALLDIR") == "INSTALLDIR", "WIXUI_INSTALLDIR must name INSTALLDIR")
    check(prop.get("ErrorDialog") == "ErrorDlg" and prop.get("ARPNOMODIFY") == "1", "ErrorDialog/ARPNOMODIFY")
    check(prop.get("ARPPRODUCTICON") == APP_ICON_ID, "Add or Remove Programs icon")
    secure = set(prop.get("SecureCustomProperties", "").split(";"))
    check(set(PUBLIC_PROPS) | {"OLDERFOUND", "NEWERFOUND", "SAMEVERSIONFOUND"} <= secure,
          "SecureCustomProperties incomplete")
    d = {r["Directory"]: r for r in T["Directory"]}
    check(d.get("INSTALLDIR", {}).get("Directory_Parent") == "ProgramsDir"
          and d.get("ProgramsDir", {}).get("Directory_Parent") == "LocalAppDataFolder"
          and d["INSTALLDIR"]["DefaultDir"].split("|")[-1] == idn["default_dir"],
          "default folder must be [LocalAppDataFolder]Programs\\" + idn["default_dir"])
    # Wizard and Browse.
    dialogs = {r["Dialog"]: r for r in T["Dialog"]}
    check(REQUIRED_DIALOGS <= set(dialogs), f"missing dialogs: {sorted(REQUIRED_DIALOGS - set(dialogs))}")
    ctl = {(r["Dialog_"], r["Control"]): r for r in T["Control"]}
    styles = {r["TextStyle"]: r for r in T["TextStyle"]}
    check(prop.get("DefaultUIFont") == "MetroNormal" and all(
        styles.get(s, {}).get("FaceName") == "Segoe UI" and not styles[s]["Color"]
        for s in ("MetroNormal", "MetroTitle", "MetroBigger", "MetroWarning", "MetroBrand")),
        "setup text must use Segoe UI with the system foreground")
    for dlg in ("WelcomeDlg", "ExitDialog"):
        mark = ctl.get((dlg, "BrandIcon"), {})
        wordmark = ctl.get((dlg, "BrandName"), {})
        # MSI Icon attributes: fixed size (0x100000) and 32×32 (0x400000).
        check(mark.get("Type") == "Icon" and mark.get("Text") == SETUP_MARK
              and int(mark.get("Attributes") or 0) & 0x700000 == 0x500000, f"{dlg} 32px setup mark")
        check(wordmark.get("Type") == "Text" and wordmark.get("Text") == r"{\MetroBrand}metro",
              f"{dlg} lowercase native wordmark")
    ev = [(r["Dialog_"], r["Control_"], r["Event"], r["Argument"], r["Condition"]) for r in T["ControlEvent"]]
    for c, typ in (("DirectoryCombo", "DirectoryCombo"), ("DirectoryList", "DirectoryList"), ("PathEdit", "PathEdit")):
        r = ctl.get(("BrowseDlg", c), {})
        check(r.get("Type") == typ and r.get("Property") == "_BrowseProperty" and int(r.get("Attributes") or 0) & 8,
              f"BrowseDlg {c} must be an indirect {typ} on _BrowseProperty")
    check(int(ctl.get(("BrowseDlg", "DirectoryCombo"), {}).get("Attributes") or 0) & 0x60000 == 0x60000,
          "BrowseDlg DirectoryCombo must list fixed and network drives")
    check(("BrowseDlg", "OK", "SetTargetPath", "[_BrowseProperty]", "1") in ev, "BrowseDlg OK must SetTargetPath")
    check(("BrowseDlg", "Up", "DirectoryListUp", "0", "1") in ev and
          ("BrowseDlg", "NewFolder", "DirectoryListNew", "0", "1") in ev, "BrowseDlg Up/New folder")
    check(("BrowseDlg", "Cancel", "Reset", "0", "1") in ev, "BrowseDlg Cancel must Reset")
    r = ctl.get(("InstallDirDlg", "Folder"), {})
    check(r.get("Type") == "PathEdit" and r.get("Property") == "WIXUI_INSTALLDIR" and int(r.get("Attributes") or 0) & 8,
          "InstallDirDlg must show the folder as an editable indirect PathEdit")
    check(("InstallDirDlg", "ChangeFolder", "[_BrowseProperty]", "[WIXUI_INSTALLDIR]", "1") in ev and
          ("InstallDirDlg", "ChangeFolder", "SpawnDialog", "BrowseDlg", "1") in ev, "Browse... must open BrowseDlg")
    check(("InstallDirDlg", "Next", "SetTargetPath", "[WIXUI_INSTALLDIR]", "1") in ev, "Next must SetTargetPath")
    check(("WelcomeDlg", "Next", "NewDialog", "InstallDirDlg", "1") in ev, "Welcome must lead to InstallDirDlg")
    cb = {r["Property"]: r["Value"] for r in T["CheckBox"]}
    check(ctl.get(("InstallDirDlg", "LocalRunner"), {}).get("Property") == "LRBOX" and cb.get("LRBOX") == "1",
          "local runner checkbox")
    check(ctl.get(("InstallDirDlg", "StartMenu"), {}).get("Property") == "SCBOX" and cb.get("SCBOX") == "1",
          "Start menu checkbox")
    for dlg, order in TAB_ORDER.items():
        if dlg not in dialogs:
            continue
        check(dialogs[dlg]["Control_First"] == order[0], f"{dlg} first control")
        for i, c in enumerate(order):
            check(ctl.get((dlg, c), {}).get("Control_Next") == order[(i + 1) % len(order)] if len(order) > 1 else True,
                  f"{dlg} tab order at {c}")
    ui = {r["Action"]: r for r in T["InstallUISequence"]}
    seq = lambda t, k: int(t.get(k, {}).get("Sequence") or -999)
    check(seq(ui, "ExitDialog") == -1 and seq(ui, "UserExit") == -2 and seq(ui, "FatalError") == -3, "end dialogs")
    check(seq(ui, "CostFinalize") < seq(ui, "WelcomeDlg") < seq(ui, "ProgressDlg") < seq(ui, "ExecuteAction"),
          "WelcomeDlg must show after CostFinalize, before ProgressDlg/ExecuteAction")
    check(ui.get("WelcomeDlg", {}).get("Condition") == "NOT Installed", "WelcomeDlg condition")
    check(seq(ui, "MaintenanceDlg") > seq(ui, "CostFinalize"), "MaintenanceDlg sequence")
    ex = {r["Action"]: r for r in T["InstallExecuteSequence"]}
    for t in (ui, ex):
        check(seq(t, "FindRelatedProducts") < seq(t, "AppSearch") < seq(t, "SetInstallDirFromPrev")
              < seq(t, "LrDefault") < seq(t, "LaunchConditions") < seq(t, "CostFinalize"),
              "searches and choices must run before LaunchConditions and CostFinalize")
    check(seq(ex, "InstallInitialize") < seq(ex, "RemoveExistingProducts") < seq(ex, "ProcessComponents"),
          "RemoveExistingProducts must run right after InstallInitialize (old version removed first)")
    check(seq(ex, "LrState") < seq(ex, "WriteRegistryValues") and "ChoicesDone" not in ex, "choice state")
    check(seq(ex, "SetArpInstallLocation") > seq(ex, "CostFinalize"), "ARPINSTALLLOCATION after CostFinalize")
    up = {r["ActionProperty"]: r for r in T["Upgrade"]}
    check(set(up) == {"OLDERFOUND", "SAMEVERSIONFOUND", "NEWERFOUND"}, f"Upgrade rows {sorted(up)}")
    if set(up) == {"OLDERFOUND", "SAMEVERSIONFOUND", "NEWERFOUND"}:
        check(up["OLDERFOUND"]["VersionMax"] == pv and int(up["OLDERFOUND"]["Attributes"]) == 256, "OLDERFOUND row")
        check(up["SAMEVERSIONFOUND"]["VersionMin"] == pv == up["SAMEVERSIONFOUND"]["VersionMax"]
              and int(up["SAMEVERSIONFOUND"]["Attributes"]) == 768, "SAMEVERSIONFOUND row")
        check(up["NEWERFOUND"]["VersionMin"] == pv and int(up["NEWERFOUND"]["Attributes"]) == 2, "NEWERFOUND row")
        check(all(r["UpgradeCode"] == idn["upgrade_code"] for r in up.values()), "Upgrade code rows")
    lc = {r["Condition"] for r in T["LaunchCondition"]}
    for c in ("VersionNT64", "NOT ALLUSERS", "Installed OR NOT LEGACYSCRIPTDIR", "Installed OR NOT NEWERFOUND",
              'Installed OR NOT SAMEVERSIONFOUND OR METRO_REPLACE_SAME_VERSION = "1"'):
        check(c in lc, f"missing launch condition {c}")
    check(all(r["Type"] == "51" for r in T["CustomAction"]), "custom actions may only set properties (type 51)")
    check([r["Name"] for r in T["Binary"]] == [SETUP_MARK], "only the setup mark may be embedded in Binary")
    check([r["Name"] for r in T["Icon"]] == [APP_ICON_ID], "expected exactly the Metro app icon")
    icon_bytes = APP_ICON.read_bytes()
    check(tools.stream(msi, f"Binary.{SETUP_MARK}") == icon_bytes, "setup mark differs from Metro's source ICO")
    check(tools.stream(msi, f"Icon.{APP_ICON_ID}") == icon_bytes, "app icon differs from Metro's source ICO")
    rl = {r["Signature_"]: r for r in T["RegLocator"]}
    check(rl.get("S_LEGACYSCRIPTDIR", {}).get("Key") == idn["legacy_key"], "legacy script detection")
    comps = {r["Component"]: r for r in T["Component"]}
    check(comps.get("C_MARKER", {}).get("Condition") == "LRBOX" and comps["C_MARKER"]["Attributes"] == "324",
          "marker component must be conditional on LRBOX and transitive")
    check(comps.get("C_SHORTCUT", {}).get("Condition") == "SCBOX" and comps["C_SHORTCUT"]["Attributes"] == "324",
          "shortcut component must be conditional on SCBOX and transitive")
    check(all(int(c["Attributes"]) & 4 and int(c["Attributes"]) & 256 for c in comps.values()),
          "every component is 64-bit with an HKCU registry key path")
    reg = [r for r in T["Registry"]]
    check(all(r["Root"] == "1" for r in reg), "registry writes are HKCU only")
    sc = T["Shortcut"]
    check(len(sc) == 1 and sc[0]["Directory_"] == "ProgramMenuFolder" and sc[0]["Target"] == "[INSTALLDIR]Metro.exe"
          and sc[0]["Name"].split("|")[-1] == idn["shortcut"] + ".lnk"
          and sc[0]["Icon_"] == APP_ICON_ID and sc[0]["IconIndex"] == "0", "Start menu shortcut/icon")
    # Files: exactly payload + marker, under the right directories.
    dirpath = {}

    def path_of(did):
        if did == "INSTALLDIR":
            return ""
        if did not in dirpath:
            r = d[did]
            parent = path_of(r["Directory_Parent"])
            name = r["DefaultDir"].split("|")[-1]
            dirpath[did] = f"{parent}/{name}" if parent else name
        return dirpath[did]

    files = {}
    for r in T["File"]:
        comp = comps.get(r["Component_"], {})
        rel = path_of(comp.get("Directory_", "INSTALLDIR"))
        name = r["FileName"].split("|")[-1]
        files[f"{rel}/{name}" if rel else name] = int(r["FileSize"])
    rf = {r["DirProperty"] for r in T["RemoveFile"]}
    check(rf == {r["Directory_"] for r in comps.values() if r["Component"] not in ("C_SHORTCUT", "C_MARKER", "C_PREFS")},
          "every installed folder must have a RemoveFolder entry (empty folders only)")
    if entries is None and payload is not None:
        _, entries, _ = validate_payload(payload, semver)
    if entries is not None:
        want = {rel: size for rel, _, size in entries}
        want[MARKER] = 0
        check(files == want, f"MSI files differ from payload: {sorted(set(files) ^ set(want))[:10]}")
        if payload is not None and not problems:
            with tempfile.TemporaryDirectory(prefix="metro-msi-x-") as x:
                tools.run("msiextract", "-C", x, msi)
                base = Path(x) / "Programs" / idn["default_dir"]
                got = {p.relative_to(base).as_posix(): p for p in base.rglob("*") if p.is_file()}
                sums = dict((r, s) for r, s, _ in entries)
                sums[MARKER] = hashlib.sha256(b"").hexdigest()
                check(set(got) == set(sums), f"extracted files differ: {sorted(set(got) ^ set(sums))[:10]}")
                for rel, digest in sums.items():
                    check(rel in got and sha256_file(got[rel]) == digest, f"MSI bytes for {rel} differ from the payload")
    if problems:
        raise Fail("MSI validation failed:\n  - " + "\n  - ".join(problems))
    return True


# ---------------------------------------------------------------- fixtures

def fake_payload(root, semver):
    top = Path(root) / f"Metro-{semver}-windows-x64"
    files = {
        "Metro.exe": f"fixture launcher {semver}\n".encode(),
        "metro-server.exe": f"fixture server {semver}\n".encode(),
        "metro-tui.exe": f"fixture tui {semver}\n".encode(),
        "ui/Metro.exe": f"fixture ui {semver}\n".encode(),
        "ui/nsh/app.json": json.dumps({"version": semver}).encode(),
        "licenses/THIRD-PARTY NOTICES.txt": b"fixture notices with a space in the name\n",
    }
    for rel, data in files.items():
        p = top / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data)
    manifest = {"kind": "desktop", "target": "windows-x64", "version": semver,
                "local_runner": "installer-marker",
                "files": {rel: hashlib.sha256(data).hexdigest() for rel, data in sorted(files.items())}}
    (top / "RELEASE.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    return top


FIXTURES = ("1.0.0", "1.1.0", "1.2.0-dev.aaa", "1.2.0-dev.bbb")


def build_fixtures(tools, dest):
    dest = Path(dest)
    dest.mkdir(parents=True, exist_ok=True)
    require(not any(dest.iterdir()), f"{dest} is not empty")
    index = {}
    with tempfile.TemporaryDirectory(prefix="metro-msi-fx-", dir=dest.parent) as work:
        for v in FIXTURES:
            pay = fake_payload(Path(work) / v, v)
            out = dest / f"Metro-msi-fixture-{v}.msi"
            info = build(tools, pay, v, out, identity="fixture", epoch=315532800)
            index[v] = {"file": out.name, "sha256": info["sha256"], "product_code": info["product_code"],
                        "product_version": info["product_version"],
                        "files": json.loads((pay / "RELEASE.json").read_text())["files"]}
    (dest / "fixtures.json").write_text(json.dumps({
        "upgrade_code": IDENTITIES["fixture"]["upgrade_code"], "default_dir": IDENTITIES["fixture"]["default_dir"],
        "reg_key": IDENTITIES["fixture"]["reg_key"], "shortcut": IDENTITIES["fixture"]["shortcut"] + ".lnk",
        "legacy_key": IDENTITIES["fixture"]["legacy_key"], "versions": index}, indent=2) + "\n")
    return index


# ---------------------------------------------------------------- CLI

def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("fetch-tools")
    p.add_argument("--dest", required=True)
    p = sub.add_parser("extract")
    p.add_argument("--zip", required=True)
    g = p.add_mutually_exclusive_group(required=True)
    g.add_argument("--sha256")
    g.add_argument("--checksum-file")
    p.add_argument("--dest", required=True)
    p = sub.add_parser("build")
    p.add_argument("--tools", required=True)
    p.add_argument("--payload", required=True)
    p.add_argument("--version", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--receipt")
    p.add_argument("--require-file", action="append", default=[])
    p.add_argument("--source-date-epoch", type=int)
    p.add_argument("--preview", action="store_true")
    p = sub.add_parser("inspect")
    p.add_argument("--tools", required=True)
    p.add_argument("--msi", required=True)
    p.add_argument("--version", required=True)
    p.add_argument("--payload")
    p.add_argument("--preview", action="store_true")
    p = sub.add_parser("fixtures")
    p.add_argument("--tools", required=True)
    p.add_argument("--dest", required=True)
    p = sub.add_parser("version")
    p.add_argument("semver")
    args = ap.parse_args(argv)
    try:
        if args.cmd == "fetch-tools":
            print(fetch_tools(args.dest))
        elif args.cmd == "extract":
            expected = args.sha256
            if args.checksum_file:
                name = Path(args.zip).name
                found = [ln.split()[0].lower() for ln in Path(args.checksum_file).read_text().splitlines()
                         if len(ln.split()) == 2 and ln.split()[1].lstrip("*") == name]
                require(len(found) == 1, f"{args.checksum_file} does not list exactly one SHA-256 for {name}")
                expected = found[0]
            payload, _ = extract_zip(args.zip, expected.lower(), args.dest)
            print(payload)
        elif args.cmd == "build":
            info = build(Tools(args.tools), args.payload, args.version, args.out, preview=args.preview,
                         required=tuple(args.require_file), epoch=args.source_date_epoch, receipt=args.receipt)
            print(json.dumps(info, indent=2, sort_keys=True))
        elif args.cmd == "inspect":
            inspect(Tools(args.tools), args.msi, args.version, payload=args.payload, preview=args.preview)
            print(f"{args.msi}: OK")
        elif args.cmd == "fixtures":
            print(json.dumps(build_fixtures(Tools(args.tools), args.dest), indent=2))
        elif args.cmd == "version":
            pv, kind = windows_version(args.semver)
            print(f"{pv} {kind}")
    except Fail as e:
        print(f"build_msi: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
