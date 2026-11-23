"""Contract tests for macos_release.py with a stateful stand-in for Apple's tools.

The fake models what matters for the release contract: a Mach-O's signature
(appended trailer), a bundle's seal over its nested code (signing nested code
after its parent breaks the parent, as with real codesign), the stapled ticket
file, and Gatekeeper's verdict. Nothing here signs or notarizes for real; the
native macos-sign job on a tag is where that happens.
"""

import base64
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest

import macos_release as mr
import package
import release_assets

HERE = Path(__file__).resolve().parent
TEAM = "TEAM123456"
IDENTITY = f"Developer ID Application: Example Org ({TEAM})"
SHA1 = "A" * 40
P12 = os.urandom(600)
P8 = "-----BEGIN PRIVATE KEY-----\nMIGTAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBHkwdwIBAQQgfake\n-----END PRIVATE KEY-----"
ENV = {
    "MACOS_SIGNING_P12_BASE64": base64.b64encode(P12).decode(),
    "MACOS_SIGNING_P12_PASSWORD": "p12-pass-WORD-9f3a",
    "APPLE_NOTARY_KEY_ID": "ABCDE12345",
    "APPLE_NOTARY_ISSUER_ID": "69a6de7e-1111-47e3-e053-5b8c7c11a4d1",
    "APPLE_NOTARY_KEY_P8": P8,
}
SUBMISSION = "2efe2717-52ef-43a5-96dc-0797e4ca1041"
MAGIC = b"\xcf\xfa\xed\xfe"
TRAILER = b"\n#FAKESIG#"
LOGIN = "/Users/runner/Library/Keychains/login.keychain-db"


def read_sig(path):
    data = Path(path).read_bytes()
    if TRAILER not in data:
        return None
    return json.loads(data.split(TRAILER, 1)[1])


def write_sig(path, sig):
    data = Path(path).read_bytes().split(TRAILER, 1)[0]
    Path(path).write_bytes(data + TRAILER + json.dumps(sig).encode())


class FakeApple:
    """Stands in for mr.Runner: security, codesign, ditto, xcrun (notarytool, stapler), spctl.
    `tar` runs for real, as the installer would."""

    def __init__(self, identities=1, notary_status="Accepted", import_fails=False, ticket=True):
        self.calls = []
        self.secrets = []
        self.clock = 0
        self.search = [LOGIN]
        self.keychains = set()
        self.identities = identities
        self.notary_status = notary_status
        self.import_fails = import_fails
        self.ticket = ticket
        self.secret_files_seen = []

    # mr.Runner interface
    redact = mr.Runner.redact
    clean = mr.Runner.clean

    def __call__(self, argv, check=True, cwd=None):
        argv = [str(a) for a in argv]
        self.calls.append(argv)
        if argv[0] == "tar":
            result = subprocess.run(argv, capture_output=True, text=True)
        else:
            rc, out, err = getattr(self, "do_" + Path(argv[0]).name)(argv[1:])
            result = subprocess.CompletedProcess(argv, rc, out, err)
        if check and result.returncode != 0:
            raise mr.ReleaseError(f"{argv[0]} failed (exit {result.returncode}): " + self.clean(result.stderr))
        return result

    # --- security
    def do_security(self, a):
        cmd = a[0]
        if cmd == "list-keychains":
            if "-s" in a:
                self.search = a[a.index("-s") + 1:]
                return 0, "", ""
            return 0, "".join(f'    "{k}"\n' for k in self.search), ""
        if cmd == "create-keychain":
            self.keychains.add(a[-1])
            return 0, "", ""
        if cmd == "import":
            p12 = Path(a[1])
            self.secret_files_seen.append((p12, oct(p12.stat().st_mode & 0o777), p12.read_bytes() == P12))
            if self.import_fails:
                return 1, "", f"security: SecKeychainItemImport: bad passphrase {a[a.index('-P') + 1]}"
            return 0, "1 identity imported.\n", ""
        if cmd == "find-identity":
            lines = [f'  {i + 1}) {SHA1[:-1]}{i} "{IDENTITY}"' for i in range(self.identities)]
            return 0, "\n".join(lines) + f"\n     {self.identities} valid identities found\n", ""
        if cmd == "delete-keychain":
            self.keychains.discard(a[-1])
            return 0, "", ""
        return 0, "", ""  # set-keychain-settings, unlock-keychain, set-key-partition-list

    # --- codesign
    def bundle_exe(self, path):
        return path / "Contents/MacOS/Metro"

    def sig_of(self, path):
        if path.suffix == ".app":
            seal = path / "Contents/_CodeSignature/CodeResources"
            return json.loads(seal.read_text()) if seal.is_file() else None
        return read_sig(path)

    def nested(self, path):
        return [p for p in (path / "Contents/Helpers/MetroUI.app", path / "Contents/MacOS/metro-server",
                            path / "Contents/MacOS/metro-tui") if p.exists()] if path.name == "Metro.app" else []

    def valid(self, path, deep=False):
        sig = self.sig_of(path)
        if not sig:
            return False
        if path.suffix == ".app":
            exe = read_sig(self.bundle_exe(path))
            if not exe or exe["time"] != sig["time"]:
                return False
            for n in self.nested(path):
                ns = self.sig_of(n)
                if not ns or ns["time"] != sig["sealed"].get(n.name) or (deep and not self.valid(n, deep)):
                    return False
        return True

    def do_codesign(self, a):
        path = Path(a[-1])
        if "--remove-signature" in a:
            if read_sig(path) is None:
                return 1, "", f"{path}: code object is not signed at all"
            path.write_bytes(path.read_bytes().split(TRAILER, 1)[0])
            return 0, "", ""
        if "--verify" in a:
            return (0, "", "") if self.valid(path, "--deep" in a) else (1, "", f"{path}: invalid signature")
        if a[:1] == ["-d"] and "--entitlements" in a:
            sig = self.sig_of(path) or {}
            ents = sig.get("ents") or {}
            return 0, plistlib.dumps(ents).decode() if ents else "", ""
        if a[:1] == ["-d"]:
            sig = self.sig_of(path)
            if not sig:
                return 1, "", f"{path}: code object is not signed at all"
            lines = [f"Executable={path}", f"Identifier={sig['id']}"]
            if sig["kind"] == "adhoc":
                lines += ["CodeDirectory v=20400 size=1 flags=0x2(adhoc) hashes=1", "Signature=adhoc",
                          "TeamIdentifier=not set"]
            else:
                flags = "0x10000(runtime)" if sig["runtime"] else "0x0(none)"
                lines += [f"CodeDirectory v=20500 size=1 flags={flags} hashes=1", f"Authority={IDENTITY}",
                          "Authority=Developer ID Certification Authority", "Authority=Apple Root CA"]
                lines.append("Timestamp=Nov 23, 2026 at 10:00:00" if sig["timestamp"] else "Signed Time=now")
                lines.append(f"TeamIdentifier={TEAM}")
            return 0, "", "\n".join(lines) + "\n"
        assert "--force" in a and "--sign" in a, a
        # The .p12 is deleted right after import, before any code is signed.
        assert not any(p.exists() for p, _, _ in self.secret_files_seen), "the .p12 outlived its import"
        self.clock += 1
        ents = plistlib.loads(Path(a[a.index("--entitlements") + 1]).read_bytes()) if "--entitlements" in a else {}
        sig = {"kind": "devid", "id": a[a.index("--identifier") + 1], "time": self.clock,
               "runtime": a[a.index("--options") + 1] == "runtime" if "--options" in a else False,
               "timestamp": "--timestamp" in a, "ents": ents}
        if path.suffix == ".app":
            sig["sealed"] = {n.name: (self.sig_of(n) or {}).get("time") for n in self.nested(path)}
            write_sig(self.bundle_exe(path), sig)
            seal = path / "Contents/_CodeSignature/CodeResources"
            seal.parent.mkdir(exist_ok=True)
            seal.write_text(json.dumps(sig))
        else:
            write_sig(path, sig)
        return 0, "", f"{path}: signed Mach-O thin\n"

    # --- packaging and notarization
    def do_ditto(self, a):
        Path(a[-1]).write_bytes(b"PK zip of " + a[-2].encode())
        return 0, "", ""

    def do_xcrun(self, a):
        tool, cmd = a[0], a[1]
        if tool == "notarytool":
            key = Path(a[a.index("--key") + 1])
            self.secret_files_seen.append((key, oct(key.stat().st_mode & 0o777), key.read_text().strip() == P8))
            if cmd == "submit":
                return 0, json.dumps({"id": SUBMISSION, "status": self.notary_status, "message": "done"}), ""
            return 0, json.dumps({"issues": [{"message": "The binary is not signed."}]}), ""
        app = Path(a[-1])
        ticket = app / "Contents/CodeResources"
        if cmd == "staple":
            # The App Store Connect key is deleted as soon as notarytool is done.
            assert not any(p.exists() for p, _, _ in self.secret_files_seen), "the .p8 outlived notarytool"
            if self.ticket:
                ticket.write_bytes(b"notarization ticket for " + str(self.sig_of(app)["time"]).encode())
            return 0, "The staple and validate action worked!\n", ""
        if cmd == "validate":
            return (0, "The validate action worked!\n", "") if ticket.is_file() else (65, "", "does not have a ticket")
        raise AssertionError(a)

    def do_spctl(self, a):
        app = Path(a[-1])
        if self.valid(app, True) and (app / "Contents/CodeResources").is_file():
            return 0, "", f"{app}: accepted\nsource=Notarized Developer ID\norigin={IDENTITY}\n"
        return 3, "", f"{app}: rejected\n"

    # --- helpers for assertions
    def signs(self):
        return [c for c in self.calls if c[0] == "codesign" and "--sign" in c]


def adhoc(path, ident, ents=None):
    write_sig(path, {"kind": "adhoc", "id": ident, "time": 0, "ents": ents or {}})


def make_adhoc_archive(folder, version="0.3.0", target="macos-arm64", ents=None, extra_exec=False, manifest_fix=None,
                       tui=False):
    """An archive shaped like package.py's ad hoc macOS output."""
    with tempfile.TemporaryDirectory() as work:
        stage = Path(work) / "stage"
        app = stage / "Metro.app/Contents"
        ui = app / "Helpers/MetroUI.app/Contents"
        for rel, data in {
            app / "Info.plist": b"<plist outer/>", app / "MacOS/Metro": MAGIC + b"launcher",
            app / "MacOS/metro-server": MAGIC + b"server", app / "Resources/metro.icns": b"icns",
            ui / "Info.plist": b"<plist ui/>", ui / "MacOS/Metro": MAGIC + b"renderer",
            ui / "Resources/nsh/app.json": b"{}", stage / "START.txt": b"preview\n",
            stage / "licenses/NSH-LICENSE-APACHE.txt": b"license",
        }.items():
            rel.parent.mkdir(parents=True, exist_ok=True)
            rel.write_bytes(data)
        for exe in (app / "MacOS/Metro", app / "MacOS/metro-server", ui / "MacOS/Metro"):
            exe.chmod(0o755)
        if tui:
            (app / "MacOS/metro-tui").write_bytes(MAGIC + b"terminal client")
            (app / "MacOS/metro-tui").chmod(0o755)
            adhoc(app / "MacOS/metro-tui", "metro-tui")
        if extra_exec:
            (app / "MacOS/helper.sh").write_bytes(b"#!/bin/sh\n")
            (app / "MacOS/helper.sh").chmod(0o755)
        adhoc(ui / "MacOS/Metro", "dev.metro.desktop.ui", ents)
        (ui / "_CodeSignature").mkdir()
        (ui / "_CodeSignature/CodeResources").write_text(json.dumps(
            {"kind": "adhoc", "id": "dev.metro.desktop.ui", "time": 0, "ents": ents or {}, "sealed": {}}))
        adhoc(app / "MacOS/metro-server", "metro-server")
        adhoc(app / "MacOS/Metro", "dev.metro.desktop")
        (app / "_CodeSignature").mkdir()
        (app / "_CodeSignature/CodeResources").write_text(json.dumps(
            {"kind": "adhoc", "id": "dev.metro.desktop", "time": 0, "ents": {},
             "sealed": {"MetroUI.app": 0, "metro-server": 0}}))
        manifest = {"kind": "desktop", "target": target, "version": version, "signing": "adhoc",
                    **({"tui": "Metro.app/Contents/MacOS/metro-tui"} if tui else {}),
                    "status": "preview-not-verified", "local_runner": "installer-marker",
                    "renderer": {"stock_unsigned_sha256": "0" * 64, "packaged_signed_sha256": "1" * 64,
                                 "code_resources_sha256": "2" * 64},
                    "files": {p.relative_to(stage).as_posix(): package.sha256(p) for p in package.files(stage)}}
        if manifest_fix:
            manifest_fix(manifest)
        (stage / "RELEASE.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
        name = f"Metro-{version}-{target}.tar.gz"
        package.archive(stage, folder / name, name.removesuffix(".tar.gz"), False)
        (folder / (name + ".sha256")).write_text(f"{package.sha256(folder / name)}  {name}\n")
        return folder / name


class MacosReleaseTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.adhoc_dir = self.root / "adhoc"
        self.adhoc_dir.mkdir()
        self.out = self.root / "dist"
        os.environ["RUNNER_TEMP"] = str(self.root)
        self.addCleanup(os.environ.pop, "RUNNER_TEMP", None)

    def tearDown(self):
        self.tmp.cleanup()

    def sign(self, fake, archive=None, env=ENV):
        return mr.sign(fake, archive or make_adhoc_archive(self.adhoc_dir), self.out, dict(env))

    def assert_no_secret(self, text):
        for name, value in ENV.items():
            self.assertNotIn(value.strip()[:40], text, name)
        self.assertNotIn("-----BEGIN PRIVATE KEY", text)

    def assert_clean_host(self, fake):
        self.assertEqual(fake.search, [LOGIN], "the keychain search list was not restored")
        self.assertEqual(fake.keychains, set(), "the throwaway keychain was not deleted")
        self.assertEqual([p.name for p in self.root.iterdir() if p.name.startswith(".metro-")], [],
                         "temporary signing files were left behind")
        for path, mode, matched in fake.secret_files_seen:
            self.assertEqual(mode, "0o600")
            self.assertTrue(matched)
            self.assertFalse(path.exists())

    def test_signs_inside_out_notarizes_staples_and_rearchives(self):
        fake = FakeApple()
        final = self.sign(fake)
        self.assertEqual(final, self.out / "Metro-0.3.0-macos-arm64.tar.gz")
        self.assertEqual(sorted(p.name for p in self.out.iterdir()),
                         ["Metro-0.3.0-macos-arm64.tar.gz", "Metro-0.3.0-macos-arm64.tar.gz.sha256"])
        self.assertEqual((self.out / (final.name + ".sha256")).read_text(),
                         f"{package.sha256(final)}  {final.name}\n")

        self.assertEqual([c[-1].split("/Metro.app", 1)[1] or "/" for c in fake.signs()],
                         ["/Contents/Helpers/MetroUI.app", "/Contents/MacOS/metro-server", "/"])
        for c in fake.signs():
            self.assertIn("--options", c)
            self.assertEqual(c[c.index("--options") + 1], "runtime")
            self.assertIn("--timestamp", c)
            self.assertNotIn("--deep", c)
            self.assertNotIn("--timestamp=none", c)
            self.assertEqual(c[c.index("--sign") + 1], SHA1[:-1] + "0")
            self.assertEqual(c[c.index("--keychain") + 1].rsplit("/", 1)[-1], "metro-signing.keychain-db")
        self.assertEqual([c[c.index("--identifier") + 1] for c in fake.signs()],
                         ["dev.metro.desktop.ui", "metro-server", "dev.metro.desktop"])
        order = [c[:3] for c in fake.calls if c[0] in ("xcrun", "ditto")]
        self.assertEqual([o[:2] for o in order],
                         [["ditto", "-c"], ["xcrun", "notarytool"], ["xcrun", "stapler"], ["xcrun", "stapler"],
                          ["xcrun", "stapler"]])
        submit = next(c for c in fake.calls if c[1:3] == ["notarytool", "submit"])
        self.assertIn("--wait", submit)
        self.assert_clean_host(fake)

        with tarfile.open(final) as t:
            self.assertTrue(all(m.isfile() for m in t))
            names = t.getnames()
            manifest = json.loads(t.extractfile("Metro-0.3.0-macos-arm64/RELEASE.json").read())
            start = t.extractfile("Metro-0.3.0-macos-arm64/START.txt").read().decode()
        self.assertIn("Metro-0.3.0-macos-arm64/Metro.app/Contents/CodeResources", names)  # the stapled ticket
        self.assertEqual((manifest["signing"], manifest["status"], manifest["signing_team"]),
                         ("developer-id", "notarized", TEAM))
        self.assertEqual(manifest["notarization"], {"service": "notarytool", "submission_id": SUBMISSION,
                                                    "status": "Accepted", "stapled": True,
                                                    "ticket_files": ["Metro.app/Contents/CodeResources"]})
        self.assertTrue(manifest["hardened_runtime"])
        self.assertEqual(manifest["local_runner"], "installer-marker")
        self.assertNotIn("preview", json.dumps(manifest))
        self.assertNotIn("NOT YET VERIFIED", start)
        self.assertIn("notarized by Apple", start)
        # The aggregate gate accepts this output only once it carries the terminal client.
        with self.assertRaisesRegex(release_assets.AssetError, "metro-tui"):
            release_assets.check_archive(final, "0.3.0", "desktop", "macos-arm64", True)
        release_assets.check_archive(final, "0.3.0", "desktop", "macos-arm64", False)
        # And the installer's awk parser reads its manifest layout.
        self.assertIn('\n  "signing": "developer-id",\n', json.dumps(manifest, indent=2, sort_keys=True))

    def test_fresh_extraction_is_verified_and_a_lost_ticket_fails(self):
        fake = FakeApple()
        final = self.sign(fake)
        self.assertEqual(mr.verify_archive(fake, final, TEAM)["signing_team"], TEAM)
        tars = [c for c in fake.calls if c[0] == "tar"]
        self.assertEqual(len(tars), 2)  # once while signing, once now
        spctl = [c for c in fake.calls if c[0] == "spctl"]
        self.assertEqual(spctl[-1][:4], ["spctl", "--assess", "--type", "execute"])
        # A copy whose ticket was lost (e.g. a format that drops it) is refused.
        broken = self.root / "broken"
        broken.mkdir()
        top = final.name.removesuffix(".tar.gz")
        with tarfile.open(final) as t:
            t.extractall(broken, filter="data")
        (broken / top / "Metro.app/Contents/CodeResources").unlink()
        manifest = json.loads((broken / top / "RELEASE.json").read_text())
        del manifest["files"]["Metro.app/Contents/CodeResources"]
        (broken / top / "RELEASE.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
        repacked = self.root / "re" / final.name
        repacked.parent.mkdir()
        package.archive(broken / top, repacked, top, False)
        (repacked.parent / (final.name + ".sha256")).write_text(f"{package.sha256(repacked)}  {final.name}\n")
        with self.assertRaisesRegex(mr.ReleaseError, "ticket"):
            mr.verify_archive(fake, repacked, TEAM)

    def test_missing_credentials_are_named_never_shown(self):
        env = dict(ENV, APPLE_NOTARY_KEY_P8="", MACOS_SIGNING_P12_PASSWORD=" ")
        self.assertEqual(mr.missing_credentials(env), ["MACOS_SIGNING_P12_PASSWORD", "APPLE_NOTARY_KEY_P8"])
        with self.assertRaises(mr.ReleaseError) as e:
            self.sign(FakeApple(), env=env)
        self.assertIn("APPLE_NOTARY_KEY_P8", str(e.exception))
        self.assert_no_secret(str(e.exception))
        self.assertFalse(self.out.exists())
        r = subprocess.run([sys.executable, "-I", str(HERE / "macos_release.py"), "check-credentials"],
                           env={"PATH": os.environ["PATH"], "APPLE_NOTARY_KEY_ID": ENV["APPLE_NOTARY_KEY_ID"]},
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 1)
        self.assertIn("MACOS_SIGNING_P12_BASE64, MACOS_SIGNING_P12_PASSWORD, APPLE_NOTARY_ISSUER_ID, "
                      "APPLE_NOTARY_KEY_P8", r.stderr)
        self.assertNotIn(ENV["APPLE_NOTARY_KEY_ID"], r.stdout + r.stderr)
        r = subprocess.run([sys.executable, "-I", str(HERE / "macos_release.py"), "check-credentials"],
                           env={"PATH": os.environ["PATH"], **ENV}, capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assert_no_secret(r.stdout + r.stderr)

    def test_malformed_credentials_name_the_variable_only(self):
        for name, value, message in (("MACOS_SIGNING_P12_BASE64", "@@not base64@@", "not valid base64"),
                                     ("APPLE_NOTARY_KEY_ID", "short", "10-character"),
                                     ("APPLE_NOTARY_ISSUER_ID", "nope", "issuer UUID"),
                                     ("APPLE_NOTARY_KEY_P8", "not a pem", "PEM")):
            with self.assertRaisesRegex(mr.ReleaseError, message) as e:
                mr.Credentials(dict(ENV, **{name: value}))
            self.assertIn(name, str(e.exception))
            self.assertNotIn(value, str(e.exception))

    def test_tool_failures_never_show_secrets(self):
        fake = FakeApple(import_fails=True)
        with self.assertRaises(mr.ReleaseError) as e:
            self.sign(fake)
        self.assertIn("[redacted]", str(e.exception))
        self.assert_no_secret(str(e.exception))
        self.assert_clean_host(fake)
        self.assertFalse(self.out.exists() and any(self.out.iterdir()))
        # The real runner names the tool, not its arguments.
        run = mr.Runner()
        run.redact("s3cret-value")
        with self.assertRaises(mr.ReleaseError) as e:
            run([sys.executable, "-c", "import sys; print('s3cret-value', file=sys.stderr); sys.exit(3)",
                 "s3cret-value"])
        self.assertNotIn("s3cret-value", str(e.exception))
        self.assertIn("exit 3", str(e.exception))

    def test_rejected_notarization_writes_nothing(self):
        fake = FakeApple(notary_status="Invalid")
        with self.assertRaisesRegex(mr.ReleaseError, "status 'Invalid'"):
            self.sign(fake)
        self.assertTrue(any(c[1:3] == ["notarytool", "log"] for c in fake.calls))
        self.assertFalse(any(c[1:3] == ["stapler", "staple"] for c in fake.calls))
        self.assertEqual(list(self.out.iterdir()) if self.out.exists() else [], [])
        self.assert_clean_host(fake)

    def test_no_ticket_after_stapling_fails(self):
        with self.assertRaises(mr.ReleaseError):
            self.sign(FakeApple(ticket=False))
        self.assertEqual(list(self.out.iterdir()) if self.out.exists() else [], [])

    def test_identity_must_be_exactly_one_from_the_expected_team(self):
        with self.assertRaisesRegex(mr.ReleaseError, "exactly one"):
            self.sign(FakeApple(identities=2))
        with self.assertRaisesRegex(mr.ReleaseError, "not from team"):
            self.sign(FakeApple(), env=dict(ENV, MACOS_SIGNING_TEAM_ID="OTHER12345"))
        self.sign(FakeApple(), env=dict(ENV, MACOS_SIGNING_TEAM_ID=TEAM))

    def test_entitlements_are_preserved_never_invented(self):
        keep = {"com.apple.security.cs.allow-jit": True}
        fake = FakeApple()
        final = self.sign(fake, make_adhoc_archive(self.adhoc_dir, ents=keep))
        ui_sign, server_sign, app_sign = fake.signs()
        self.assertIn("--entitlements", ui_sign)
        self.assertNotIn("--entitlements", server_sign)
        self.assertNotIn("--entitlements", app_sign)
        with tarfile.open(final) as t:
            m = json.loads(t.extractfile("Metro-0.3.0-macos-arm64/RELEASE.json").read())
        self.assertEqual(m["entitlements"], {"Metro.app/Contents/Helpers/MetroUI.app": sorted(keep),
                                             "Metro.app/Contents/MacOS/metro-server": [], "Metro.app": []})

    def test_get_task_allow_is_refused_not_dropped(self):
        shutil.rmtree(self.adhoc_dir)
        self.adhoc_dir.mkdir()
        fake = FakeApple()
        with self.assertRaisesRegex(mr.ReleaseError, "get-task-allow"):
            self.sign(fake, make_adhoc_archive(self.adhoc_dir, ents={"com.apple.security.get-task-allow": True}))
        self.assertEqual(fake.signs(), [])
        self.assert_clean_host(fake)

    def test_inputs_are_checked_before_any_credential_is_used(self):
        cases = {
            "unknown code": lambda: make_adhoc_archive(self.adhoc_dir, extra_exec=True),
            "already notarized": lambda: make_adhoc_archive(
                self.adhoc_dir, manifest_fix=lambda m: m.update(signing="developer-id")),
            "wrong target": lambda: make_adhoc_archive(
                self.adhoc_dir, manifest_fix=lambda m: m.update(target="macos-x64")),
        }
        for label, build in cases.items():
            with self.subTest(label):
                shutil.rmtree(self.adhoc_dir)
                self.adhoc_dir.mkdir()
                fake = FakeApple()
                with self.assertRaises(mr.ReleaseError):
                    self.sign(fake, build())
                self.assertEqual(fake.signs(), [])
                self.assertFalse(any(c[1:2] == ["notarytool"] for c in fake.calls))
        shutil.rmtree(self.adhoc_dir)
        self.adhoc_dir.mkdir()
        a = make_adhoc_archive(self.adhoc_dir)
        (self.adhoc_dir / (a.name + ".sha256")).write_text(f"{'0' * 64}  {a.name}\n")
        with self.assertRaisesRegex(mr.ReleaseError, "checksum mismatch"):
            self.sign(FakeApple(), a)
        (self.adhoc_dir / (a.name + ".sha256")).write_text(f"{package.sha256(a)}  {a.name}\n")
        with self.assertRaisesRegex(mr.ReleaseError, "must differ"):
            mr.sign(FakeApple(), a, self.adhoc_dir, dict(ENV))

    def test_terminal_client_is_signed_like_the_server(self):
        fake = FakeApple()
        final = self.sign(fake, make_adhoc_archive(self.adhoc_dir, tui=True))
        self.assertEqual([c[-1].split("/Metro.app", 1)[1] or "/" for c in fake.signs()],
                         ["/Contents/Helpers/MetroUI.app", "/Contents/MacOS/metro-server",
                          "/Contents/MacOS/metro-tui", "/"])
        tui_sign = fake.signs()[2]
        self.assertEqual(tui_sign[tui_sign.index("--options") + 1], "runtime")
        self.assertIn("--timestamp", tui_sign)
        self.assertEqual(tui_sign[tui_sign.index("--identifier") + 1], "metro-tui")
        manifest = mr.verify_archive(fake, final, TEAM)
        self.assertEqual(manifest["tui"], "Metro.app/Contents/MacOS/metro-tui")
        self.assertTrue(any(c[0] == "codesign" and c[-1].endswith("/Contents/MacOS/metro-tui") and "-d" in c
                            for c in fake.calls[-12:]), "the extracted terminal client was not checked")
        release_assets.check_archive(final, "0.3.0", "desktop", "macos-arm64", True)

    def test_terminal_client_must_match_the_manifest(self):
        undeclared = lambda m: m.pop("tui")  # noqa: E731
        for label, kw in (("binary without manifest entry", dict(tui=True, manifest_fix=undeclared)),
                          ("manifest entry without binary", dict(manifest_fix=lambda m: m.update(
                              tui="Metro.app/Contents/MacOS/metro-tui")))):
            with self.subTest(label):
                shutil.rmtree(self.adhoc_dir)
                self.adhoc_dir.mkdir()
                fake = FakeApple()
                with self.assertRaises(mr.ReleaseError):
                    self.sign(fake, make_adhoc_archive(self.adhoc_dir, **kw))
                self.assertEqual(fake.signs(), [])

    def test_existing_output_is_never_overwritten(self):
        self.sign(FakeApple())
        with self.assertRaisesRegex(mr.ReleaseError, "already exists"):
            self.sign(FakeApple())

    def test_signing_nested_code_after_the_parent_is_caught(self):
        fake = FakeApple()
        a = make_adhoc_archive(self.adhoc_dir)
        original = mr.sign_app

        def wrong_order(run, app, keychain, identity, work, manifest):
            result = original(run, app, keychain, identity, work, manifest)
            run(["codesign", "--force", "--sign", identity["sha1"], "--options", "runtime", "--timestamp",
                 "--identifier", "metro-server", app / mr.SERVER])
            return result
        mr.sign_app = wrong_order
        try:
            with self.assertRaises(mr.ReleaseError):
                mr.sign(fake, a, self.out, dict(ENV))
        finally:
            mr.sign_app = original
        self.assertEqual(list(self.out.iterdir()) if self.out.exists() else [], [])

    @unittest.skipIf(sys.platform == "darwin", "checks the refusal on other hosts")
    def test_sign_needs_a_mac(self):
        r = subprocess.run([sys.executable, "-I", str(HERE / "macos_release.py"), "sign", "--archive", "x.tar.gz",
                            "--out-dir", str(self.out)], env={"PATH": os.environ["PATH"], **ENV},
                           capture_output=True, text=True)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("run on macOS", r.stderr)
        self.assert_no_secret(r.stdout + r.stderr)


if __name__ == "__main__":
    unittest.main()
