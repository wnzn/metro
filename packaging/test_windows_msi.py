"""Tests for the Windows MSI builder (packaging/windows-msi/build_msi.py).

Pure-Python checks (versions, payload validation, ZIP extraction, names) always
run. Tests that compile MSIs need the pinned msitools tree from
`build_msi.py fetch-tools`; point METRO_MSI_TOOLS at it, otherwise they skip.
Nothing here installs anything: native msiexec checks are
windows-msi/test-msi-windows.ps1 (Windows CI only).
"""

import hashlib
import json
import re
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
import zipfile

HERE = Path(__file__).resolve().parent
MSI_DIR = HERE / "windows-msi"
sys.path.insert(0, str(MSI_DIR))
import build_msi as b  # noqa: E402

TOOLS = os.environ.get("METRO_MSI_TOOLS")


def tmpdir(test):
    d = tempfile.mkdtemp(prefix="metro-msi-test-")
    test.addCleanup(shutil.rmtree, d, True)
    return Path(d)


class Versions(unittest.TestCase):
    def pv(self, v):
        return tuple(int(x) for x in b.windows_version(v)[0].split("."))

    def test_order_follows_semver_for_supported_channels(self):
        order = ["0.1.9", "0.2.0-setup-57409bb309ee", "0.2.0-alpha", "0.2.0-alpha.1", "0.2.0-beta.2",
                 "0.2.0-rc.1", "0.2.0-rc.12", "0.2.0", "0.2.1-dev.x", "0.2.1-rc.1", "0.2.1", "0.3.0", "1.0.0"]
        pvs = [self.pv(v) for v in order]
        self.assertEqual(pvs, sorted(pvs))
        self.assertEqual(len(set(pvs)), len(pvs))

    def test_examples(self):
        self.assertEqual(b.windows_version("0.2.0"), ("0.2.99", "release"))
        self.assertEqual(b.windows_version("0.2.0-setup-57409bb309ee"), ("0.2.0", "dev"))
        self.assertEqual(b.windows_version("0.2.0-rc.1"), ("0.2.51", "rc"))
        self.assertEqual(b.windows_version("1.0.0+build.7"), ("1.0.99", "release"))

    def test_unordered_builds_share_a_version_and_build_metadata_is_ignored(self):
        self.assertEqual(self.pv("0.2.0-dev.aaa"), self.pv("0.2.0-dev.bbb"))
        self.assertEqual(self.pv("0.2.0+a"), self.pv("0.2.0+b"))
        self.assertNotEqual(b.product_code("release", "0.2.0-dev.aaa"), b.product_code("release", "0.2.0-dev.bbb"))

    def test_limits(self):
        for bad in ("256.0.0", "0.256.0", "0.0.655", "0.2.0-rc.49", "0.2.0-alpha.20", "v0.2.0", "0.2", "01.2.3",
                    "0.2.0-", ""):
            with self.subTest(bad=bad), self.assertRaises(b.Fail):
                b.windows_version(bad)
        self.assertEqual(b.windows_version("255.255.654")[0], "255.255.65499")

    def test_identity_is_stable(self):
        # Changing these breaks upgrades of installed copies.
        self.assertEqual(b.IDENTITIES["release"]["upgrade_code"], "{29BBE899-7D6A-49A7-B60E-F9644544D683}")
        self.assertEqual(b.product_code("release", "0.2.0"), b.product_code("release", "0.2.0"))
        self.assertNotEqual(b.IDENTITIES["fixture"]["upgrade_code"], b.IDENTITIES["release"]["upgrade_code"])
        self.assertNotEqual(b.IDENTITIES["fixture"]["default_dir"], b.IDENTITIES["release"]["default_dir"])
        self.assertNotEqual(b.IDENTITIES["fixture"]["reg_key"], b.IDENTITIES["release"]["reg_key"])
        # Never the script-managed folder (install-metro-windows.ps1 uses Programs\Metro).
        self.assertNotEqual(b.IDENTITIES["release"]["default_dir"], "Metro")


class Payload(unittest.TestCase):
    def setUp(self):
        self.root = tmpdir(self)
        self.p = b.fake_payload(self.root, "1.0.0")

    def manifest(self):
        return json.loads((self.p / "RELEASE.json").read_text())

    def write_manifest(self, m):
        (self.p / "RELEASE.json").write_text(json.dumps(m))

    def test_valid(self):
        m, entries, digest = b.validate_payload(self.p, "1.0.0", required=("metro-tui.exe",))
        self.assertIn("RELEASE.json", [e[0] for e in entries])
        self.assertEqual(len(digest), 64)

    def test_rejections(self):
        cases = []

        def tamper_hash():
            (self.p / "Metro.exe").write_bytes(b"other")
        cases.append(("does not match", tamper_hash))

        def extra_file():
            (self.p / "extra.dll").write_bytes(b"x")
        cases.append(("does not list", extra_file))

        def marker_in_payload():
            (self.p / "enable-local-runner").write_bytes(b"")
        cases.append(("does not list", marker_in_payload))

        def link():
            (self.p / "Metro.exe").unlink()
            (self.p / "Metro.exe").symlink_to(self.p / "metro-server.exe")
        cases.append(("missing", link))

        def wrong_version():
            m = self.manifest(); m["version"] = "1.0.1"; self.write_manifest(m)
        cases.append(("version", wrong_version))

        def no_marker_support():
            m = self.manifest(); del m["local_runner"]; self.write_manifest(m)
        cases.append(("local_runner", no_marker_support))

        def missing_required():
            m = self.manifest(); del m["files"]["metro-server.exe"]; self.write_manifest(m)
            (self.p / "metro-server.exe").unlink()
        cases.append(("missing metro-server.exe", missing_required))

        for bad in ("../x", "a/../b", "a\\b", "CON.txt", "x:y", "trail.", "/abs"):
            def bad_name(bad=bad):
                m = self.manifest(); m["files"][bad] = "0" * 64; self.write_manifest(m)
            cases.append(("", bad_name))

        def case_collision():
            (self.p / "METRO.exe").write_bytes(b"x")
            m = self.manifest(); m["files"]["METRO.exe"] = hashlib.sha256(b"x").hexdigest(); self.write_manifest(m)
        cases.append(("case", case_collision))

        for want, fn in cases:
            with self.subTest(fn=fn.__name__):
                shutil.rmtree(self.root)
                self.root.mkdir()
                self.p = b.fake_payload(self.root, "1.0.0")
                fn()
                with self.assertRaises(b.Fail) as ctx:
                    b.validate_payload(self.p, "1.0.0")
                self.assertIn(want, str(ctx.exception))

    def test_require_file(self):
        m = self.manifest()
        del m["files"]["metro-tui.exe"]
        self.write_manifest(m)
        (self.p / "metro-tui.exe").unlink()
        b.validate_payload(self.p, "1.0.0")
        with self.assertRaises(b.Fail):
            b.validate_payload(self.p, "1.0.0", required=("metro-tui.exe",))


class Extract(unittest.TestCase):
    def make_zip(self, d, name, members):
        z = d / name
        with zipfile.ZipFile(z, "w") as f:
            for n, data in members.items():
                f.writestr(n, data)
        return z, b.sha256_file(z)

    def test_roundtrip_and_refusals(self):
        d = tmpdir(self)
        src = b.fake_payload(d / "src", "1.0.0")
        members = {f"{src.name}/{p.relative_to(src).as_posix()}": p.read_bytes()
                   for p in src.rglob("*") if p.is_file()}
        z, digest = self.make_zip(d, "Metro-1.0.0-windows-x64.zip", members)
        payload, semver = b.extract_zip(z, digest, d / "out")
        self.assertEqual(semver, "1.0.0")
        b.validate_payload(payload, "1.0.0")
        with self.assertRaises(b.Fail):
            b.extract_zip(z, "0" * 64, d / "out2")
        with self.assertRaises(b.Fail):
            b.extract_zip(z, digest, d / "out")  # not empty
        bads = ("../evil", "Metro-1.0.0-windows-x64/../evil", "other/x", "Metro-1.0.0-windows-x64/a\\b",
                "Metro-1.0.0-windows-x64/c:x", "Metro-1.0.0-windows-x64/dir/")
        for i, bad in enumerate(bads):
            with self.subTest(bad=bad):
                (d / f"z{i}").mkdir()
                z2, dg2 = self.make_zip(d / f"z{i}", "Metro-1.0.0-windows-x64.zip", {bad: b"x"})
                with self.assertRaises(b.Fail):
                    b.extract_zip(z2, dg2, d / f"o{i}")
                self.assertFalse((d / "evil").exists())
        with self.assertRaises(b.Fail):
            b.extract_zip(d / "Metro-1.0.0-linux-x64.tar.gz", digest, d / "o3")


class Sources(unittest.TestCase):
    def test_wxs_files_are_wellformed(self):
        for p in [MSI_DIR / "metro-ui.wxs", *sorted((MSI_DIR / "wixl-ext/ui").glob("*.wxs"))]:
            with self.subTest(p=p.name):
                ET.parse(p)

    def test_generated_product_is_wellformed(self):
        d = tmpdir(self)
        p = b.fake_payload(d, "1.0.0")
        _, entries, _ = b.validate_payload(p, "1.0.0")
        xml = b.product_wxs("release", "1.0.0", "1.0.99", entries, p, d / "marker", False)
        root = ET.fromstring(xml)
        self.assertEqual(len(root.findall(".//{*}CustomAction")), 9)
        self.assertFalse([c for c in root.iter() if c.tag.endswith("CustomAction")
                          and set(c.attrib) - {"Id", "Property", "Value"}], "only property-setting actions")

    def test_short_names(self):
        used = set()
        self.assertEqual(b.msi_name("Metro.exe", "Metro.exe", used), "Metro.exe")
        n = b.msi_name("metro-server.exe", "metro-server.exe", used)
        self.assertRegex(n, r"^[0-9A-F]{8}\.exe\|metro-server\.exe$")
        self.assertEqual(b.msi_name("Metro.exe", "x/Metro.exe", used).split("|")[1], "Metro.exe")  # collision

    def test_windows_installer_com_calls_are_typed(self):
        # Every Windows Installer automation call goes through Invoke-MsiAutomation with a
        # declared signature; PowerShell values reaching IDispatch raised DISP_E_TYPEMISMATCH.
        import re
        helper = (MSI_DIR / "MsiAutomation.ps1").read_text(encoding="utf-8")
        declared = set(re.findall(r"^\s+'(\w+\.\w+)'\s+= @\('(?:Method|Property)'", helper, re.M))
        self.assertIn("Record.StringData", declared)
        users = [MSI_DIR / "MsiHarness.ps1", HERE / "windows-signing" / "MetroWindowsSigning.psm1"]
        for p in users:
            with self.subTest(p=p.name):
                text = p.read_text(encoding="utf-8")
                self.assertNotIn("InvokeMember", text)
                used = set(re.findall(r"Invoke-MsiAutomation \$\w+ '(\w+\.\w+)'", text))
                self.assertTrue(used, "no Windows Installer calls found")
                self.assertLessEqual(used, declared)
                self.assertIsNone(re.search(r"\$(installer|db|view|record|list)\.\w+\(", text, re.I))
        self.assertIn("MsiAutomation.ps1", (HERE / "windows-signing" / "MetroWindowsSigning.psm1").read_text(encoding="utf-8"))
        # The native harness reaches Windows Installer only through MsiHarness.ps1's helpers.
        harness = (MSI_DIR / "test-msi-windows.ps1").read_text(encoding="utf-8")
        self.assertIn(". (Join-Path $PSScriptRoot 'MsiAutomation.ps1')", harness)
        self.assertIn(". (Join-Path $PSScriptRoot 'MsiHarness.ps1')", harness)
        self.assertNotIn("InvokeMember", harness)
        self.assertNotIn("Invoke-MsiAutomation", harness)
        self.assertIsNone(re.search(r"\$(installer|db|view|record|list)\.\w+\(", harness, re.I))
        self.assertNotIn("Get-ArpKey", harness)  # per-user MSI ARP keys live under HKLM; context comes from ProductInfo

    def test_msi_automation_returns_objects_unenumerated(self):
        # A plain `return <InvokeMember result>` enumerated RelatedProducts' StringList:
        # no items -> $null ("Cannot bind argument to parameter 'Object'"), one -> a string.
        helper = (MSI_DIR / "MsiAutomation.ps1").read_text(encoding="utf-8")
        self.assertIn("    return , $result\n}", helper)
        self.assertNotIn("return $target.GetType().InvokeMember", helper)

    @unittest.skipUnless(os.environ.get("METRO_PWSH") or shutil.which("pwsh"), "PowerShell 7 (pwsh) not available")
    def test_msi_automation_with_stand_ins(self):
        pwsh = os.environ.get("METRO_PWSH") or shutil.which("pwsh")
        env = dict(os.environ, POWERSHELL_TELEMETRY_OPTOUT="1")
        p = subprocess.run([pwsh, "-NoProfile", "-NonInteractive", "-File", str(MSI_DIR / "test-msi-automation.ps1")],
                           capture_output=True, text=True, timeout=300, env=env)
        self.assertEqual(p.returncode, 0, p.stdout[-4000:] + p.stderr[-2000:])
        self.assertIn("fails=0", p.stdout)
        self.assertNotIn("FAIL ", p.stdout)
        self.assertEqual(p.stdout.count("PASS "), 25)

    @unittest.skipUnless(os.environ.get("METRO_PWSH") or shutil.which("pwsh"), "PowerShell 7 (pwsh) not available")
    def test_native_harness_helpers_with_stand_ins(self):
        # test-msi-windows.ps1 helpers and its parameter/variable rule, without Windows.
        # With the msitools tree, identity checks read tables of real build_msi.py output.
        pwsh = os.environ.get("METRO_PWSH") or shutil.which("pwsh")
        env = dict(os.environ, POWERSHELL_TELEMETRY_OPTOUT="1")
        env.pop("METRO_MSI_FACTS_JSON", None)
        real = bool(TOOLS)
        if real:
            tools = b.Tools(TOOLS)
            d = tmpdir(self)
            p = b.fake_payload(d / "p", "1.0.0")
            facts = {"version": "1.0.0"}
            for kind, preview in (("release", False), ("preview", True)):
                out = d / kind / b.out_name("1.0.0", preview)
                out.parent.mkdir()
                b.build(tools, p, "1.0.0", out, preview=preview, epoch=315532800)
                facts[kind] = {
                    "file": out.name,
                    "Property": [[r["Property"], r["Value"]] for r in tools.table(out, "Property")],
                    "Shortcut": [[r["Name"], r["Directory_"]] for r in tools.table(out, "Shortcut")],
                }
            (d / "facts.json").write_text(json.dumps(facts))
            env["METRO_MSI_FACTS_JSON"] = str(d / "facts.json")
            # The installed layout of the release package, unpacked by msiextract.
            x = d / "extracted"
            tools.run("msiextract", "-C", x, d / "release" / b.out_name("1.0.0", False))
            env["METRO_MSI_EXTRACTED_TREE"] = str(x / "Programs" / b.IDENTITIES["release"]["default_dir"])
        proc = subprocess.run([pwsh, "-NoProfile", "-NonInteractive", "-File", str(MSI_DIR / "test-msi-harness.ps1")],
                              capture_output=True, text=True, timeout=300, env=env)
        self.assertEqual(proc.returncode, 0, proc.stdout[-4000:] + proc.stderr[-2000:])
        self.assertIn("fails=0", proc.stdout)
        self.assertNotIn("FAIL ", proc.stdout)
        self.assertEqual(proc.stdout.count("PASS "), 86 + (1 if real else 0))
        self.assertIn("exported from packages built by build_msi.py" if real else "written in this test", proc.stdout)
        if real:
            self.assertIn("extracted from a package built by build_msi.py", proc.stdout)

    def test_policy_fixture_only_on_hosted_harness_runs(self):
        # Both CI calls of the native harness run on GitHub-hosted windows-2025 and ask for the
        # client-policy fixture; the harness itself refuses it anywhere else.
        root = HERE.parent / ".github" / "workflows"
        for wf, job in (("windows-distribution.yml", "preview"), ("windows-sign.yml", "verify")):
            with self.subTest(wf=wf):
                text = (root / wf).read_text(encoding="utf-8")
                start = text.index(f"\n  {job}:\n")
                nxt = re.search(r"\n  [a-z][a-z0-9-]*:\n", text[start + 3:])
                body = text[start: start + 3 + nxt.start()] if nxt else text[start:]
                self.assertIn("runs-on: windows-2025", body)
                self.assertIn("test-msi-windows.ps1", body)
                self.assertIn("-StandardUserFixture", body)
                self.assertNotIn("-ClientPolicyFixture", text)
        harness = (MSI_DIR / "test-msi-windows.ps1").read_text(encoding="utf-8")
        code = "\n".join(l for l in harness.splitlines() if not l.lstrip().startswith("#"))
        self.assertIn("Get-PolicyFixtureRefusal", code)
        self.assertIn("Invoke-WithClientMsiPolicy", code)
        self.assertIn("New-LocalUser", code)
        self.assertIn("-Credential $Credential -LoadUserProfile", code)
        self.assertIn("Remove-LocalUser", code)
        self.assertIn("Win32_UserProfile", code)
        for banned in ("AlwaysInstallElevated", "runas.exe", "trustlevel", "Add-LocalGroupMember -SID 'S-1-5-32-544'",
                       "ConvertFrom-SecureString", "ConvertTo-SecureString", "-AsPlainText"):
            self.assertNotIn(banned, code)
        # Helpers return arrays as one object (unary comma); @() around them would nest the array.
        for p in MSI_DIR.glob("*.ps1"):
            text = p.read_text(encoding="utf-8")
            self.assertIsNone(re.search(r"@\((Get-Tree|Get-RelatedProducts|Get-InstalledTreeProblems|Get-StandardUserResultProblems|"
                                        r"Get-PackageIdentityProblems|Get-MsiQueryRows|Get-ParamShadowing)\b", text), p.name)

    def test_installer_is_policy_neutral(self):
        d = tmpdir(self)
        p = b.fake_payload(d, "1.0.0")
        _, entries, _ = b.validate_payload(p, "1.0.0")
        xml = b.product_wxs("release", "1.0.0", "1.0.99", entries, p, d / "marker", False)
        for word in ("Policies", "DisableMsi", "AlwaysInstallElevated", "Root=\"HKLM\" Key=\"SOFTWARE\\Policies"):
            self.assertNotIn(word, xml)

    def test_lock_is_pinned(self):
        lock = json.loads((MSI_DIR / "wixl-lock.json").read_text())
        self.assertRegex(lock["tree_sha256"], r"^[0-9a-f]{64}$")
        for deb in lock["debs"]:
            self.assertRegex(deb["sha256"], r"^[0-9a-f]{64}$")
            self.assertTrue(all(u.startswith("https://") for u in deb["urls"]))


@unittest.skipUnless(TOOLS, "set METRO_MSI_TOOLS to a `build_msi.py fetch-tools` directory")
class Build(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tools = b.Tools(TOOLS)

    def build(self, d, v="1.0.0", **kw):
        p = b.fake_payload(d / ("p-" + v), v)
        out = d / kw.pop("name", f"Metro-{v}-windows-x64.msi")
        return p, out, b.build(self.tools, p, v, out, epoch=315532800, **kw)

    def test_build_inspect_reproducible(self):
        d1, d2 = tmpdir(self), tmpdir(self)
        p, out1, info = self.build(d1)
        _, out2, _ = self.build(d2)
        self.assertEqual(out1.read_bytes(), out2.read_bytes())
        self.assertEqual(info["signing"], "unsigned")
        self.assertTrue(info["payload"]["has_metro_tui"])
        self.assertEqual(info["builder"]["icon_sha256"], b.sha256_file(b.APP_ICON))
        self.assertEqual(info["builder"]["ui_sha256"], b.sha256_file(b.UI_WXS))
        self.assertEqual(self.tools.stream(out1, f"Binary.{b.SETUP_MARK}"), b.APP_ICON.read_bytes())
        self.assertEqual(self.tools.stream(out1, f"Icon.{b.APP_ICON_ID}"), b.APP_ICON.read_bytes())
        b.inspect(self.tools, out1, "1.0.0", payload=p)
        with self.assertRaises(b.Fail):
            b.inspect(self.tools, out1, "1.0.0", payload=p, preview=True)

    def test_names_and_overwrite(self):
        d = tmpdir(self)
        with self.assertRaises(b.Fail):
            self.build(d, name="metro.msi")
        with self.assertRaises(b.Fail):
            self.build(d, name="Metro-1.0.0-windows-x64.msi", preview=True)
        _, out, _ = self.build(d, name="Metro-1.0.0-windows-x64.unsigned-preview.msi", preview=True)
        b.inspect(self.tools, out, "1.0.0", preview=True)
        existing = d / "Metro-1.0.0-windows-x64.msi"
        existing.write_bytes(b"signed already")
        with self.assertRaises(b.Fail):
            self.build(d)
        self.assertEqual(existing.read_bytes(), b"signed already")

    def test_package_code_follows_payload(self):
        d = tmpdir(self)
        p, out, info = self.build(d)
        # Same version, different bytes (e.g. signed EXEs): same ProductCode, new PackageCode.
        (p / "Metro.exe").write_bytes(b"signed")
        m = json.loads((p / "RELEASE.json").read_text())
        m["files"]["Metro.exe"] = hashlib.sha256(b"signed").hexdigest()
        (p / "RELEASE.json").write_text(json.dumps(m))
        out2 = d / "again" / out.name
        out2.parent.mkdir()
        info2 = b.build(self.tools, p, "1.0.0", out2, epoch=315532800)
        self.assertEqual(info["product_code"], info2["product_code"])
        self.assertNotEqual(info["package_code"], info2["package_code"])

    def test_inspect_catches_a_missing_browse_dialog(self):
        d = tmpdir(self)
        p, out, _ = self.build(d)
        self.tools.run("msibuild", out, "-q", "DELETE FROM `Control` WHERE `Dialog_` = 'BrowseDlg' "
                                              "AND `Control` = 'DirectoryList'")
        with self.assertRaises(b.Fail) as ctx:
            b.inspect(self.tools, out, "1.0.0", payload=p)
        self.assertIn("DirectoryList", str(ctx.exception))

    def test_inspect_catches_wrong_arp_icon(self):
        d = tmpdir(self)
        _, out, _ = self.build(d)
        self.tools.run("msibuild", out, "-q", "UPDATE `Property` SET `Value` = 'Wrong.ico' "
                                              "WHERE `Property` = 'ARPPRODUCTICON'")
        with self.assertRaisesRegex(b.Fail, "Add or Remove Programs icon"):
            b.inspect(self.tools, out, "1.0.0")

    def test_inspect_catches_changed_bytes(self):
        d = tmpdir(self)
        p, out, _ = self.build(d)
        (p / "ui/Metro.exe").write_bytes(b"different")
        m = json.loads((p / "RELEASE.json").read_text())
        m["files"]["ui/Metro.exe"] = hashlib.sha256(b"different").hexdigest()
        (p / "RELEASE.json").write_text(json.dumps(m))
        with self.assertRaises(b.Fail):
            b.inspect(self.tools, out, "1.0.0", payload=p)


if __name__ == "__main__":
    unittest.main()
