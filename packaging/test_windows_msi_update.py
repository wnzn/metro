"""Tests for the in-app MSI update test fixtures and contracts
(packaging/windows-msi/update_fixture.py, test-msi-update.ps1,
msi-update-scenarios.ps1) and for the constants the desktop updater shares
with build_msi.py. Nothing here installs anything; the native run is
test-msi-update.ps1 on a Windows CI runner.

MSI builds from update_fixture payloads run when METRO_MSI_TOOLS points at a
fetched msitools tree (`build_msi.py fetch-tools`).
"""

import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
import urllib.error
import urllib.request

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
MSI_DIR = HERE / "windows-msi"
sys.path.insert(0, str(MSI_DIR))
import build_msi as b  # noqa: E402
import update_fixture as uf  # noqa: E402

TOOLS = os.environ.get("METRO_MSI_TOOLS")
RUST_MSI = ROOT / "crates/connector/src/desktop/update/msi.rs"
PWSH = os.environ.get("METRO_PWSH") or shutil.which("pwsh")


def sha(data):
    return hashlib.sha256(data).hexdigest()


class Contracts(unittest.TestCase):
    def test_upgrade_code_and_registry_key_match_the_release_identity(self):
        src = RUST_MSI.read_text(encoding="utf-8")
        code = re.search(r'pub const UPGRADE_CODE: &str = "([^"]+)";', src).group(1)
        key = re.search(r'pub const REG_KEY: &str = r"([^"]+)";', src).group(1)
        self.assertEqual(code, b.IDENTITIES["release"]["upgrade_code"])
        self.assertEqual(key, b.IDENTITIES["release"]["reg_key"])
        ps = (MSI_DIR / "test-msi-update.ps1").read_text(encoding="utf-8")
        self.assertIn(f"$UpgradeCode = '{code}'", ps)

    def test_product_version_mapping_matches_rust(self):
        src = RUST_MSI.read_text(encoding="utf-8")
        for semver, pv in re.findall(r'assert_eq!\(pv\("([0-9.]+)"\), "([0-9.]+)"\);', src):
            self.assertEqual(b.windows_version(semver), (pv, "release"), semver)
        self.assertGreaterEqual(len(re.findall(r'assert_eq!\(pv\(', src)), 3)

    def test_the_msi_remembers_what_the_helper_passes_back(self):
        # The helper passes INSTALLDIR and LOCALRUNNER/STARTMENUSHORTCUT from
        # these remembered values; the MSI must keep writing them.
        src = (MSI_DIR / "build_msi.py").read_text(encoding="utf-8")
        for name in ("InstallDir", "Version", "LocalRunner", "StartMenuShortcut"):
            self.assertIn(f'("{name}", ', src)
        for prop in ("INSTALLDIR", "LOCALRUNNER", "STARTMENUSHORTCUT"):
            self.assertIn(prop, b.PUBLIC_PROPS)
        rust = RUST_MSI.read_text(encoding="utf-8")
        for name in ("InstallDir", "Version", "LocalRunner", "StartMenuShortcut"):
            self.assertIn(f'"{name}"', rust)
        # A major upgrade: the old product is removed inside the new transaction.
        self.assertIn('<RemoveExistingProducts After=\\"InstallInitialize\\" />', src)


class Payload(unittest.TestCase):
    def test_payload_is_a_valid_build_msi_payload(self):
        with tempfile.TemporaryDirectory() as t:
            t = Path(t)
            (t / "launcher.exe").write_bytes(b"MZ launcher 1.0.0")
            (t / "ui.exe").write_bytes(b"MZ stand-in ui")
            top = uf.make_payload("1.0.0", t / "launcher.exe", t / "ui.exe", t / "out")
            self.assertEqual(top.name, "Metro-1.0.0-windows-x64")
            manifest, entries, _ = b.validate_payload(top, "1.0.0")
            self.assertEqual(manifest["local_runner"], "installer-marker")
            self.assertEqual({e[0] for e in entries}, {"Metro.exe", "metro-server.exe", "ui/Metro.exe", "RELEASE.json"})
            self.assertEqual((top / "Metro.exe").read_bytes(), b"MZ launcher 1.0.0")
            with self.assertRaises(uf.Fail):
                uf.make_payload("1.0.0", t / "launcher.exe", t / "ui.exe", t / "out")
            with self.assertRaises(uf.Fail):
                uf.make_payload("1.0", t / "launcher.exe", t / "ui.exe", t / "out2")

    @unittest.skipUnless(TOOLS, "METRO_MSI_TOOLS not set")
    def test_builds_preview_msis_for_both_versions(self):
        with tempfile.TemporaryDirectory() as t:
            t = Path(t)
            (t / "ui.exe").write_bytes(b"MZ stand-in ui")
            for v in ("1.0.0", "1.0.1"):
                (t / f"l{v}.exe").write_bytes(f"MZ launcher {v}".encode())
                top = uf.make_payload(v, t / f"l{v}.exe", t / "ui.exe", t / f"p{v}")
                out = t / f"Metro-{v}-windows-x64.unsigned-preview.msi"
                b.build(b.Tools(TOOLS), top, v, out, preview=True, epoch=315532800)
                self.assertTrue(out.is_file())
                rel = uf.make_release(v, out, t / "release")
                self.assertTrue((rel / out.name).is_file())


class Release(unittest.TestCase):
    def files(self, name="Metro-1.0.1-windows-x64.unsigned-preview.msi"):
        return uf.release_files("1.0.1", name, b"the msi")

    def test_manifest_lists_every_file_and_is_test_signed(self):
        f = self.files()
        sums = {}
        for line in f["SHA256SUMS"].decode().splitlines():
            digest, name = line.split("  ", 1)
            sums[name] = digest
        self.assertEqual(set(sums), set(f) - {"SHA256SUMS", "SHA256SUMS.sigstore.json",
                                              "Metro-1.0.1-linux-x64.tar.gz.sigstore.json",
                                              "Metro-runner-1.0.1-linux-x64.tar.gz.sigstore.json"})
        for name, digest in sums.items():
            self.assertEqual(digest, sha(f[name]), name)
        bundle = json.loads(f["SHA256SUMS.sigstore.json"])
        self.assertEqual(bundle, {"metroTestBundle": 1, "sha256": sha(f["SHA256SUMS"]),
                                  "commit": uf.TEST_COMMIT, "signedAt": uf.SIGNED_AT})
        self.assertEqual(f["Metro-1.0.1-windows-x64.unsigned-preview.msi"], b"the msi")
        for inst in uf.installers():
            self.assertIn(inst, f)

    def test_installers_come_from_the_rust_manifest(self):
        self.assertEqual(uf.installers(), ["install-metro-linux.sh", "install-metro-runner-linux.sh",
                                           "install-metro-windows.ps1", "metro-runner-container.ps1",
                                           "metro-runner-container.sh"])

    def test_msi_name(self):
        self.files("Metro-1.0.1-windows-x64.msi")
        with self.assertRaises(uf.Fail):
            self.files("Metro-1.0.0-windows-x64.msi")
        with self.assertRaises(uf.Fail):
            self.files("metro.msi")

    def test_server_serves_releases_on_loopback_only(self):
        with tempfile.TemporaryDirectory() as t:
            t = Path(t)
            (t / "m.msi").write_bytes(b"x")
            msi = t / "Metro-1.0.1-windows-x64.unsigned-preview.msi"
            shutil.copyfile(t / "m.msi", msi)
            uf.make_release("1.0.1", msi, t / "rel")
            server = uf.make_server(t / "rel", "1.0.1")
            self.assertEqual(server.server_address[0], "127.0.0.1")
            threading.Thread(target=server.serve_forever, daemon=True).start()
            try:
                base = f"http://127.0.0.1:{server.server_address[1]}/wnzn/metro/releases"
                got = urllib.request.urlopen(f"{base}/latest/download/SHA256SUMS").read()
                self.assertEqual(got, (t / "rel/v1.0.1/SHA256SUMS").read_bytes())
                got = urllib.request.urlopen(f"{base}/download/v1.0.1/{msi.name}").read()
                self.assertEqual(got, b"x")
                for bad in ("download/v1.0.1/..%2Fx", "download/v1.0.1/.hidden", "download/v1.0.2/SHA256SUMS",
                            "latest/download/a/b"):
                    with self.assertRaises(urllib.error.HTTPError):
                        urllib.request.urlopen(f"{base}/{bad}")
            finally:
                server.shutdown()
                server.server_close()


class Scripts(unittest.TestCase):
    def test_scenarios_cover_the_spec(self):
        s = (MSI_DIR / "msi-update-scenarios.ps1").read_text(encoding="utf-8")
        for name in ("'update'", "'failure-retry'", "'helper-killed'", "'helper-msiexec-killed'"):
            self.assertIn(name, s)
        o = (MSI_DIR / "test-msi-update.ps1").read_text(encoding="utf-8")
        self.assertIn("-Scenarios update,failure-retry -OwnEnvironment", o)
        self.assertIn("Invoke-WithClientMsiPolicy", o)
        self.assertIn("Get-PolicyFixtureRefusal", o)
        # The dismisser clicks only a lone OK on msiexec's message boxes.
        self.assertIn('buttons == 1', s)
        self.assertIn('"msiexec"', s)

    @unittest.skipUnless(PWSH, "PowerShell 7 (pwsh) not available")
    def test_scripts_parse(self):
        for f in ("test-msi-update.ps1", "msi-update-scenarios.ps1"):
            path = MSI_DIR / f
            cmd = ("$e=$null; $null=[System.Management.Automation.Language.Parser]::ParseFile("
                   f"'{path}',[ref]$null,[ref]$e); if ($e) {{ $e | % {{ $_.ToString() }}; exit 1 }}")
            p = subprocess.run([PWSH, "-NoProfile", "-NonInteractive", "-Command", cmd],
                               capture_output=True, text=True, timeout=120)
            self.assertEqual(p.returncode, 0, f + p.stdout + p.stderr)

    @unittest.skipUnless(PWSH, "PowerShell 7 (pwsh) not available")
    def test_no_parameter_is_shadowed_by_a_script_variable(self):
        # PowerShell names are case-insensitive (see test-msi-harness.ps1).
        for f in ("test-msi-update.ps1", "msi-update-scenarios.ps1"):
            text = (MSI_DIR / f).read_text(encoding="utf-8")
            params = re.search(r"(?s)^param\((.*?)\n\)", text, re.M).group(1)
            names = {n.lower() for n in re.findall(r"\$(\w+)", params)}
            body = text[text.index(params) + len(params):]
            assigned = {n.lower() for n in re.findall(r"^\s*\$(\w+)\s*=", body, re.M)}
            self.assertEqual(names & assigned - {"scenarios"}, set(), f)


if __name__ == "__main__":
    unittest.main()
