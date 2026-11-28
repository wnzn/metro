"""In-app updates of a Windows script install, natively on Windows.

Needs e2e test builds (never a release build) of the launcher and the UI
stand-in:

    cargo build --locked --release --target x86_64-pc-windows-msvc -p metro-desktop \\
        --features e2e-test-release --bin metro-desktop --bin metro-ui-stub
    python -I packaging/windows_update_e2e.py \\
        --desktop target/x86_64-pc-windows-msvc/release/metro-desktop.exe \\
        --ui-stub target/x86_64-pc-windows-msvc/release/metro-ui-stub.exe --log-dir logs

With METRO_TEST_RELEASE_BASE pointing at the loopback fixture server below,
such a launcher counts as a release build, takes its version from its
versions\\<v> folder (one binary plays 1.0.0, 1.0.1 and 1.0.2) and verifies
releases with the test verifier. The stand-in UI drives Settings > About &
updates through the launcher's bridge.

It changes the real per-user Start menu shortcut and Installed-apps entry
(and removes them again), so it refuses to run when either already exists.
Meant for a disposable CI machine.

Covers, on the real Windows PowerShell 5.1 installer and real processes:
1. an install made by the published 0.4.0 script is not updated in-app
   (manual_migration_required) until the current script's Update migrates it
   (entry point, shortcut, marker);
2. while the script holds the installer lock, the app's download refuses
   (installer_busy); while a trial runs, the script refuses;
3. update 1.0.0 -> 1.0.1 through the entry point: trial, commit, the lock
   handoff (a third start is secondary), Installed-apps version, and the
   outcome at the next start;
4. 1.0.1 -> a 1.0.2 that exits before its first render: rollback, database
   restored byte for byte, 1.0.1 runs again, nothing of 1.0.2 is left;
5. a stale version's start forwards to the current one; Status and Uninstall.
"""

import argparse
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import zipfile

HERE = Path(__file__).resolve().parent
INSTALLER = HERE / "install-metro-windows.ps1"
PUBLISHED_040 = HERE / "test-fixtures" / "install-metro-windows-0.4.0.ps1"
COMMIT = "0123456789abcdef0123456789abcdef01234567"
INSTALLERS = ("install-metro-linux.sh", "install-metro-runner-linux.sh", "install-metro-windows.ps1",
              "metro-runner-container.ps1", "metro-runner-container.sh")
UNINSTALL_KEY = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\dev.metro.desktop"
A, B, BAD = "1.0.0", "1.0.1", "1.0.2"


class Failed(Exception):
    pass


def require(ok, what):
    if not ok:
        raise Failed(what)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def test_bundle(data):
    """The dev TestVerifier's bundle format (metro_release::testing)."""
    return json.dumps({"metroTestBundle": 1, "sha256": sha(data), "commit": COMMIT,
                       "signedAt": 1800000000}).encode()


def desktop_zip(version, desktop, stub):
    payload = {"Metro.exe": desktop, "metro-server.exe": stub, "ui/Metro.exe": stub}
    release = json.dumps({"kind": "desktop", "target": "windows-x64", "version": version,
                          "files": {n: sha(d) for n, d in sorted(payload.items())}}, indent=2).encode()
    top = f"Metro-{version}-windows-x64"
    out = Path(tempfile.mkdtemp(prefix="metro zip ")) / f"{top}.zip"
    with zipfile.ZipFile(out, "w") as z:
        for name, data in {"RELEASE.json": release, **payload}.items():
            # Like package.py's Windows archives: regular files, deflated.
            info = zipfile.ZipInfo(f"{top}/{name}", (2026, 1, 1, 0, 0, 0))
            info.create_system, info.compress_type = 0, zipfile.ZIP_DEFLATED
            z.writestr(info, data)
    data = out.read_bytes()
    (out.parent / f"{out.name}.sha256").write_text(f"{sha(data)}  {out.name}\n")
    return out, data


def release_files(version, zip_bytes):
    """A complete release file set as SHA256SUMS lists it (placeholders for
    what the desktop never downloads), like metro_release::testing."""
    binaries = [f"Metro-{version}-linux-x64.tar.gz", f"Metro-{version}-windows-x64.zip",
                f"Metro-{version}-windows-x64.unsigned-preview.msi", f"Metro-runner-{version}-linux-x64.tar.gz"]
    files = {name: f"placeholder {name}\n".encode() for name in binaries + list(INSTALLERS)}
    files[binaries[1]] = zip_bytes
    for name in binaries:
        files[f"{name}.sha256"] = f"{sha(files[name])}  {name}\n".encode()
    d = lambda c: c * 64  # noqa: E731
    files["IMAGES.txt"] = (
        f"# Metro container images v{version}\n# Source commit: {COMMIT}\n\n"
        f"ghcr.io/wnzn/metro-runner:v{version}@sha256:{d('a')}  linux/amd64,linux/arm64\n"
        f"ghcr.io/wnzn/metro-runner@sha256:{d('b')}  linux/amd64\n"
        f"ghcr.io/wnzn/metro-runner@sha256:{d('c')}  linux/arm64\n\n"
        f"ghcr.io/wnzn/metro-server:v{version}@sha256:{d('d')}  linux/amd64,linux/arm64\n"
        f"ghcr.io/wnzn/metro-server@sha256:{d('e')}  linux/amd64\n"
        f"ghcr.io/wnzn/metro-server@sha256:{d('f')}  linux/arm64\n").encode()
    files["SHA256SUMS"] = "".join(f"{sha(files[n])}  {n}\n" for n in sorted(files)).encode()
    files["SHA256SUMS.sigstore.json"] = test_bundle(files["SHA256SUMS"])
    for name in (binaries[0], binaries[3]):
        files[f"{name}.sigstore.json"] = test_bundle(files[name])
    return files


class ReleaseServer(ThreadingHTTPServer):
    def __init__(self):
        super().__init__(("127.0.0.1", 0), ReleaseHandler)
        self.releases, self.latest = {}, None


class ReleaseHandler(BaseHTTPRequestHandler):
    def do_GET(self):  # noqa: N802
        prefix = "/wnzn/metro/releases/"
        rest = self.path.removeprefix(prefix) if self.path.startswith(prefix) else ""
        body = None
        if rest.startswith("latest/download/") and self.server.latest:
            body = self.server.releases[self.server.latest].get(rest.removeprefix("latest/download/"))
        elif rest.startswith("download/v"):
            version, _, name = rest.removeprefix("download/v").partition("/")
            body = self.server.releases.get(version, {}).get(name)
        self.send_response(200 if body is not None else 404)
        body = body if body is not None else b"not found"
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


class Run:
    def __init__(self, args, tmp):
        self.timeout = args.timeout
        self.logs = Path(args.log_dir).resolve()
        self.logs.mkdir(parents=True, exist_ok=True)
        self.root = tmp / "Programs" / "Metro"
        self.home = tmp / "Desktop data"
        self.stub_log = self.logs / "ui-stub.jsonl"
        self.stop = tmp / "stop"
        self.hold = tmp / "hold"
        self.server = ReleaseServer()
        threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True).start()
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("METRO_")}
        self.env.update({
            "METRO_INSTALL_DIR": str(self.root),
            "METRO_DESKTOP_HOME": str(self.home),
            "METRO_TEST_RELEASE_BASE": f"http://127.0.0.1:{self.server.server_port}",
            "METRO_UI_STUB_LOG": str(self.stub_log),
            "METRO_UI_STUB_STOP": str(self.stop),
            "METRO_UI_STUB_HOLD_FILE": str(self.hold),
        })

    # ---- helpers

    def say(self, msg):
        print(f"windows_update_e2e: {msg}", flush=True)

    def ps(self, script, *args, ok=True):
        r = subprocess.run(["powershell.exe", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass",
                            "-File", str(script), *map(str, args)], env=self.env, capture_output=True, text=True,
                           timeout=300)
        out = r.stdout + r.stderr
        with (self.logs / "installer.log").open("a", encoding="utf-8") as f:
            f.write(f"== {Path(script).name} {' '.join(map(str, args))} -> {r.returncode}\n{out}\n")
        if ok:
            require(r.returncode == 0, f"installer {args} failed: {out}")
        return r.returncode, out

    def events(self):
        if not self.stub_log.exists():
            return []
        return [json.loads(line) for line in self.stub_log.read_text(encoding="utf-8").splitlines() if line.strip()]

    def wait_event(self, what, pred, since=0):
        deadline = time.monotonic() + self.timeout
        while time.monotonic() < deadline:
            for e in self.events()[since:]:
                if pred(e):
                    return e
            time.sleep(0.25)
        raise Failed(f"timed out waiting for {what}; events: {self.events()[since:]}")

    def wait_for(self, what, probe):
        deadline = time.monotonic() + self.timeout
        while time.monotonic() < deadline:
            value = probe()
            if value:
                return value
            time.sleep(0.25)
        raise Failed(f"timed out waiting for {what}")

    def running(self):
        """Processes started from the install root (launchers, UI stand-ins)."""
        q = ("$p = [IO.Path]::GetFullPath($env:METRO_INSTALL_DIR).TrimEnd('\\') + '\\'; "
             "@(Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -and "
             "$_.ExecutablePath.StartsWith($p, [StringComparison]::OrdinalIgnoreCase) }).Count")
        r = subprocess.run(["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", q], env=self.env,
                           capture_output=True, text=True, timeout=60)
        return int(r.stdout.strip() or "0")

    def start(self, exe):
        """Start like the Start menu does, and let the entry point exit."""
        p = subprocess.Popen([str(exe)], cwd=str(Path(exe).parent), env=self.env, stdin=subprocess.DEVNULL,
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        return p

    def stop_all(self):
        self.stop.write_text("stop")
        self.wait_for("every Metro process to exit", lambda: self.running() == 0)
        self.stop.unlink()

    def pointer(self):
        return (self.root / "current.txt").read_text().strip()

    def journal(self):
        path = self.root / ".update.json"
        return json.loads(path.read_text()) if path.exists() else None

    def registry(self):
        import winreg
        try:
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER, UNINSTALL_KEY) as k:
                return {n: winreg.QueryValueEx(k, n)[0] for n in ("DisplayVersion", "DisplayIcon", "InstallLocation")}
        except FileNotFoundError:
            return None

    def shortcut(self):
        lnk = Path(os.environ["APPDATA"]) / "Microsoft/Windows/Start Menu/Programs/Metro.lnk"
        if not lnk.exists():
            return None
        q = f"(New-Object -ComObject WScript.Shell).CreateShortcut('{lnk}').TargetPath"
        r = subprocess.run(["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", q],
                           capture_output=True, text=True, timeout=60)
        return r.stdout.strip()

    def serve(self, version, zip_bytes):
        self.server.releases[version] = release_files(version, zip_bytes)
        self.server.latest = version

    # ---- scenarios

    def main(self, desktop, stub):
        require(self.registry() is None and self.shortcut() is None,
                "a Metro Installed-apps entry or Start menu shortcut already exists here; use a disposable machine")
        zips = {v: desktop_zip(v, desktop, stub) for v in (A, B, BAD)}
        try:
            self.migration(zips)
            self.updates(zips)
        finally:
            if self.stop.parent.exists():
                self.stop.write_text("stop")
            if self.root.exists():
                self.ps(INSTALLER, "-Action", "Uninstall", ok=False)

    def migration(self, zips):
        self.say("an install by the published 0.4.0 script")
        self.ps(PUBLISHED_040, "-Action", "Install", "-Archive", zips[A][0])
        old_exe = self.root / "versions" / A / "Metro.exe"
        require(self.shortcut() == str(old_exe), f"0.4.0 shortcut: {self.shortcut()}")
        self.serve(B, zips[B][1])
        self.start(old_exe)
        e = self.wait_event("1.0.0 to start", lambda e: e.get("event") == "start" and e["version"] == A)
        s = e["state"]
        require(s["current"]["install"] == "windows-script" and s["can_update_in_app"] is False
                and s.get("reason") == "manual_migration_required", f"unmigrated state: {s}")
        self.stop_all()

        self.say("the current script migrates it")
        _, out = self.ps(INSTALLER, "-Action", "Update", "-Archive", zips[A][0])
        require("In-app updates are on for this install" in out, out)
        require((self.root / ".metro-desktop-install").read_bytes() == b"metro-update-lock-v2\n", "marker")
        entry = self.root / "Metro.exe"
        require(entry.is_file(), "no entry point")
        require(self.shortcut() == str(entry), f"shortcut after migration: {self.shortcut()}")
        reg = self.registry()
        require(reg and reg["DisplayVersion"] == A and reg["DisplayIcon"] == str(entry), f"registry: {reg}")
        _, out = self.ps(INSTALLER, "-Action", "Status")
        require("In-app updates: on" in out, out)

    def updates(self, zips):
        entry = self.root / "Metro.exe"

        self.say("the script holds the installer lock: the app's download refuses")
        holder_ps = Path(self.stop.parent) / "hold-lock.ps1"
        holder_ps.write_text(HOLD_LOCK, encoding="utf-8")
        holder = subprocess.Popen(["powershell.exe", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass",
                                   "-File", str(holder_ps), str(INSTALLER), str(self.root)], env=self.env,
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        try:
            require(holder.stdout.readline().strip() == "locked", "the lock holder did not lock")
            self.env["METRO_UI_STUB_DRIVE_FROM"] = A
            mark = len(self.events())
            self.start(entry)
            e = self.wait_event("the refused download", lambda e: e.get("event") == "drive_failed", mark)
            require(e["state"].get("failure", {}).get("code") == "installer_busy", f"busy: {e}")
            require(self.pointer() == A and not (self.root / "versions" / B).exists(), "something changed")
        finally:
            holder.stdin.write("\n")
            holder.stdin.close()
            holder.wait(60)
        self.stop_all()

        self.say("1.0.0 -> 1.0.1 through the entry point")
        self.env["METRO_UI_STUB_HOLD_VERSION"] = B
        mark = len(self.events())
        p = self.start(entry)
        require(p.wait(60) == 0, "the entry point did not exit")
        e = self.wait_event("1.0.0's start", lambda e: e.get("event") == "start", mark)
        require(e["version"] == A and e["state"]["can_update_in_app"] is True, f"A: {e}")
        self.wait_event("the verified stage", lambda e: e.get("event") == "ready" and e["version"] == B, mark)
        self.wait_event("1.0.1's trial", lambda e: e.get("event") == "holding" and e["version"] == B, mark)
        require(self.journal()["phase"] == "trial" and self.pointer() == A, f"journal: {self.journal()}")
        code, out = self.ps(INSTALLER, "-Action", "Update", "-Archive", zips[A][0], ok=False)
        require(code != 0 and "another install is running" in out, f"script during the trial: {out}")
        _, out = self.ps(INSTALLER, "-Action", "Status")
        require("Update journal phase: trial" in out, out)
        self.hold.write_text("go")
        e = self.wait_event("1.0.1's first render", lambda e: e.get("event") == "start" and e["version"] == B, mark)
        require(e["trial"] is True, f"B: {e}")
        self.wait_for("the commit", lambda: self.pointer() == B and (self.journal() or {}).get("phase") == "committed")
        # 1.0.1's launcher and its UI remain; the old launcher handed over and exited.
        self.wait_for("the old launcher to exit", lambda: self.running() == 2)
        reg = self.registry()
        require(reg["DisplayVersion"] == B and reg["DisplayIcon"] == str(entry), f"registry after commit: {reg}")
        mark = len(self.events())
        self.start(entry)
        e = self.wait_event("a third start", lambda e: e.get("event") == "start", mark)
        require("current" not in e["state"] and e["state"].get("error", {}).get("code") == "update_unavailable",
                f"a third start became primary next to the committed trial: {e}")
        self.stop_all()
        self.hold.unlink()
        del self.env["METRO_UI_STUB_HOLD_VERSION"]

        mark = len(self.events())
        self.start(entry)
        e = self.wait_event("1.0.1's next start", lambda e: e.get("event") == "start", mark)
        last = e["state"].get("last_update") or {}
        require(e["version"] == B and e["trial"] is False and last.get("outcome") == "updated"
                and last.get("from") == A, f"after commit: {e}")
        self.wait_for("journal cleanup", lambda: self.journal() is None)
        self.stop_all()

        self.say("a stale version's start forwards to the current one")
        mark = len(self.events())
        self.start(self.root / "versions" / A / "Metro.exe")
        e = self.wait_event("the forwarded start", lambda e: e.get("event") == "start", mark)
        require(e["version"] == B, f"stale start: {e}")
        self.stop_all()

        self.say("1.0.1 -> a broken 1.0.2: rollback")
        db = self.home / "local" / "db" / "metro.sqlite3"
        db.parent.mkdir(parents=True, exist_ok=True)
        db.write_bytes(b"the database before 1.0.2")
        self.serve(BAD, zips[BAD][1])
        self.env.update({"METRO_UI_STUB_DRIVE_FROM": B, "METRO_UI_STUB_FAIL_VERSION": BAD})
        mark = len(self.events())
        self.start(entry)
        self.wait_event("1.0.2 to fail", lambda e: e.get("event") == "fail" and e["version"] == BAD, mark)
        e = self.wait_event("1.0.1 to run again",
                            lambda e: e.get("event") == "start" and (e["state"].get("last_update") or {}).get("to") == BAD,
                            mark)
        last = e["state"]["last_update"]
        require(e["version"] == B and last["outcome"] == "rolled_back" and "exited with 3" in last.get("detail", ""),
                f"after rollback: {e}")
        require(self.pointer() == B, "pointer moved")
        require(db.read_bytes() == b"the database before 1.0.2", "database not restored")
        self.wait_for("1.0.2's files to go", lambda: not (self.root / "versions" / BAD).exists())
        require(self.registry()["DisplayVersion"] == B, "registry changed by the rollback")
        self.stop_all()

        self.say("Status and Uninstall")
        _, out = self.ps(INSTALLER, "-Action", "Status")
        require(f"Installed: Metro {B}" in out and "In-app updates: on" in out, out)
        self.ps(INSTALLER, "-Action", "Uninstall")
        require(not self.root.exists() and self.registry() is None and self.shortcut() is None, "uninstall left things")
        require(db.read_bytes() == b"the database before 1.0.2", "uninstall touched Metro data")


# Holds the installer's own lock (its functions, unmodified) until stdin closes.
HOLD_LOCK = r"""
param($Installer, $Root)
$ErrorActionPreference = 'Stop'
$Prog = 'install-metro-windows'
$Quiet = $true
$tokens = $null
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile($Installer, [ref]$tokens, [ref]$errors)
if ($errors) { throw 'could not parse installer' }
foreach ($def in $ast.EndBlock.Statements) {
    if ($def -is [System.Management.Automation.Language.FunctionDefinitionAst]) { . ([scriptblock]::Create($def.Extent.Text)) }
}
$lock = Enter-InstallLock
[Console]::Out.WriteLine('locked')
[Console]::Out.Flush()
[void][Console]::In.ReadLine()
try { Exit-LegacyLock } finally { $lock.Dispose() }
"""


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--desktop", required=True, help="e2e build of metro-desktop.exe")
    ap.add_argument("--ui-stub", required=True, help="e2e build of metro-ui-stub.exe")
    ap.add_argument("--log-dir", default="windows-update-e2e-logs")
    ap.add_argument("--timeout", type=float, default=120, help="seconds per wait")
    args = ap.parse_args()
    if sys.platform != "win32":
        sys.exit("windows_update_e2e: native Windows only")
    desktop, stub = Path(args.desktop).read_bytes(), Path(args.ui_stub).read_bytes()
    # Long names: Windows may give TEMP in 8.3 form, the launcher sees its own
    # canonical path.
    tmp = Path(tempfile.mkdtemp(prefix="metro update e2e ")).resolve()
    try:
        Run(args, tmp).main(desktop, stub)
    except Failed as e:
        sys.exit(f"windows_update_e2e: FAILED: {e}")
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
    print("windows_update_e2e: all scenarios passed")


if __name__ == "__main__":
    main()
