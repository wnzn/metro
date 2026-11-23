"""Contract tests for release_assets.py: the release aggregate, image evidence and signed set."""

import ast
import hashlib
import io
import json
from pathlib import Path
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import package
import release_assets as ra

sys.path.insert(0, str(Path(__file__).resolve().parent / "release-signatures"))
sys.path.insert(0, str(Path(__file__).resolve().parent / "windows-signing"))
import release_signatures  # noqa: E402
import winsign  # noqa: E402  (the Windows lane's own verifier: manifest keys, script signature format)

HERE = Path(__file__).resolve().parent
V = "0.3.0"
OWNER = "wnzn"  # release_signatures.py pins the wnzn/metro repository
REV = "a" * 40
# One PowerShell signature block around a tiny, well-formed DER object (winsign.parse_exact accepts it).
PS1_SIG = b"# SIG # Begin signature block\r\n# MAMCAQE=\r\n# SIG # End signature block\r\n"


def sha(data):
    return hashlib.sha256(data).hexdigest()


def pe(signed):
    """A minimal PE32+ image; with `signed`, a certificate table (Authenticode) at its end."""
    data = bytearray(0x200)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, 0x80)
    data[0x80:0x84] = b"PE\0\0"
    opt = 0x80 + 24
    struct.pack_into("<H", data, opt, 0x20B)
    struct.pack_into("<I", data, opt + 108, 16)  # NumberOfRvaAndSizes
    if signed:
        struct.pack_into("<II", data, opt + 112 + 4 * 8, len(data), 64)
        data += b"\0" * 64
    return bytes(data)


def write_archive(folder, top, files, kind, target, fields=None):
    manifest = {"kind": kind, "target": target, "version": V, **(fields or {}),
                "files": {n: sha(d) for n, d in files.items()}}
    members = {**files, "RELEASE.json": json.dumps(manifest, indent=2, sort_keys=True).encode() + b"\n"}
    if top.endswith("windows-x64"):
        path = folder / (top + ".zip")
        with zipfile.ZipFile(path, "w") as z:
            for name, data in sorted(members.items()):
                info = zipfile.ZipInfo(f"{top}/{name}")
                info.external_attr = (stat.S_IFREG | 0o644) << 16
                z.writestr(info, data)
    else:
        path = folder / (top + ".tar.gz")
        with tarfile.open(path, "w:gz", format=tarfile.USTAR_FORMAT) as t:
            for name, data in sorted(members.items()):
                info = tarfile.TarInfo(f"{top}/{name}")
                info.size, info.mode = len(data), 0o755
                t.addfile(info, io.BytesIO(data))
    sidecar(path)
    return path


def sidecar(path):
    path.with_name(path.name + ".sha256").write_text(f"{sha(path.read_bytes())}  {path.name}\n")


def mac_fields(**over):
    fields = {"signing": "developer-id", "status": "notarized", "signing_team": "TEAM123456",
              "notarization": {"service": "notarytool", "submission_id": "2efe2717-52ef-43a5-96dc-0797e4ca1041",
                               "status": "Accepted", "stapled": True,
                               "ticket_files": ["Metro.app/Contents/CodeResources"]}}
    fields.update(over)
    return fields


def windows_fields(files, mode):
    """RELEASE.json beyond kind/target/version/files, in winsign.py's exact schema."""
    exes = [n for n in winsign.SIGNED_EXES if n in files]
    fields = {"nsh": package.NSH_VERSION, "connector": "linked", "local_runner": package.LOCAL_RUNNER,
              "dependencies": {n: {"imports": ["kernel32.dll"]} for n in exes}}
    if mode:
        fields["signing"] = {"mode": mode, "seed_sha256": "0" * 64, "stock_renderer_unsigned_sha256": "1" * 64,
                             "files": {}}
    return fields


def collected(folder, mode="release", windows_signed=True, mac=None, msi_signed=True, ps1_signed=True, tui=True,
              win_mode=None):
    """What the dist-* artifacts of one run hold, named and shaped as the real producers write them."""
    folder.mkdir(parents=True, exist_ok=True)
    linux = {"Metro": b"\x7fELF launcher", "START.txt": b"s", **({"metro-tui": b"\x7fELF tui"} if tui else {})}
    write_archive(folder, f"Metro-{V}-linux-x64", linux, "desktop", "linux-x64")
    write_archive(folder, f"Metro-runner-{V}-linux-x64", {"metro-runner": b"\x7fELF runner"}, "runner", "linux-x64")
    win = {"Metro.exe": pe(windows_signed), "metro-server.exe": pe(windows_signed),
           "ui/Metro.exe": pe(windows_signed), "START.txt": b"s",
           **({"metro-tui.exe": pe(windows_signed)} if tui else {})}
    signing_mode = win_mode or ("authenticode" if mode == "release" else "unsigned-preview")
    write_archive(folder, f"Metro-{V}-windows-x64", win, "desktop", "windows-x64", windows_fields(win, signing_mode))
    msi = folder / ra.Layout(V, mode).msi
    msi.write_bytes(b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1" + b"\0" * 64
                    + (ra.MSI_SIGNATURE if msi_signed else b"") + b"\0" * 32)
    sidecar(msi)
    for script in ra.PS1_SCRIPTS:
        source = (HERE / script).read_bytes()  # preview: unchanged source; tag: source + one signature block
        (folder / script).write_bytes(source + (PS1_SIG if ps1_signed and mode == "release" else b""))
    if mode == "release":
        for target in ra.MACOS:  # none while macOS builds are off; tests patch ra.MACOS
            write_mac(folder, target, mac or mac_fields())
    return folder


def write_mac(folder, target, fields):
    write_archive(folder, f"Metro-{V}-{target}",
                  {"Metro.app/Contents/MacOS/Metro": b"\xcf\xfa\xed\xfe",
                   "Metro.app/Contents/MacOS/metro-tui": b"\xcf\xfa\xed\xfe tui",
                   "Metro.app/Contents/CodeResources": b"ticket"},
                  "desktop", target, fields)


def image_evidence(folder, version=V, revision=REV):
    """IMAGES.txt exactly as image_release.py's evidence command writes it."""
    results = []
    folder.mkdir(parents=True)
    for i, (name, platforms) in enumerate((("metro-server", ("linux/amd64", "linux/arm64")),
                                           ("metro-runner", ("linux/amd64", "linux/arm64")),
                                           ("metro-embeddinggemma2", ("linux/amd64",)))):
        result = {"repository": f"ghcr.io/{OWNER}/{name}", "tag": f"v{version}",
                  "index_digest": "sha256:" + f"{i + 1}" * 64,
                  "platforms": {p: {"manifest_digest": "sha256:" + f"{i + 3}{j}" * 32,
                                    "config_digest": "sha256:" + f"{j}{i + 5}" * 32}
                                for j, p in enumerate(platforms)}}
        (folder / f"{name}.json").write_text(json.dumps(result))
        results.append(str(folder / f"{name}.json"))
    out = folder / "evidence"
    r = subprocess.run([sys.executable, "-I", str(HERE / "image_release.py"), "evidence", "--version", version,
                        "--revision", revision, "--out", str(out),
                        "--signer-identity", f"https://github.com/wnzn/metro/.github/workflows/release-images.yml"
                                             f"@refs/tags/v{version}",
                        "--signer-issuer", "https://token.actions.githubusercontent.com", *results],
                       capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    return out


class AssembleTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.out = self.root / "release"

    def tearDown(self):
        self.tmp.cleanup()

    def assemble(self, mode="release", **kw):
        return ra.assemble(V, mode, collected(self.root / "collected", mode, **kw), self.out)

    def test_release_holds_exactly_the_published_set(self):
        self.assemble()
        names = sorted(p.name for p in self.out.iterdir())
        self.assertEqual(ra.MACOS, ())  # macOS builds are off for now: Windows and Linux only
        archives = [f"Metro-{V}-linux-x64.tar.gz",
                    f"Metro-{V}-windows-x64.msi", f"Metro-{V}-windows-x64.zip", f"Metro-runner-{V}-linux-x64.tar.gz"]
        scripts = ["install-metro-linux.sh", "install-metro-runner-linux.sh",
                   "install-metro-windows.ps1", "metro-runner-container.ps1", "metro-runner-container.sh"]
        self.assertEqual(names, sorted(archives + [a + ".sha256" for a in archives] + scripts + ["SHA256SUMS"]))
        sums = (self.out / "SHA256SUMS").read_text().splitlines()
        self.assertEqual([line.split("  ")[1] for line in sums], sorted(archives + scripts))
        for line in sums:
            digest, name = line.split("  ")
            self.assertEqual(sha((self.out / name).read_bytes()), digest)
        # The signed .ps1 scripts are the Windows signer's bytes, never checkout copies, and the
        # Windows lane's own parser accepts them as source + one signature block.
        for script in ra.PS1_SCRIPTS:
            signed, source = (self.out / script).read_bytes(), (HERE / script).read_bytes()
            self.assertNotEqual(signed, source)
            info = winsign.script_unsigned_info(source, script)
            self.assertEqual(winsign.script_split(signed, info, script)[0], source)
        self.assertEqual((self.out / "install-metro-linux.sh").read_bytes(),
                         (HERE / "install-metro-linux.sh").read_bytes())

    def test_smoke_has_no_macos_and_allows_unsigned_previews(self):
        self.assemble("smoke", windows_signed=False, msi_signed=False, ps1_signed=False, tui=False)
        names = {p.name for p in self.out.iterdir()}
        self.assertFalse(any("macos" in n for n in names))
        # windows-distribution.yml's preview name; the release name never appears on a preview.
        self.assertIn(f"Metro-{V}-windows-x64.unsigned-preview.msi", names)
        self.assertNotIn(f"Metro-{V}-windows-x64.msi", names)
        for script in ra.PS1_SCRIPTS:
            self.assertEqual((self.out / script).read_bytes(), (HERE / script).read_bytes())

    def test_names_and_schema_match_the_real_windows_producers(self):
        workflow = (HERE.parent / ".github/workflows/windows-distribution.yml").read_text()
        self.assertIn('Metro-$VERSION-windows-x64.unsigned-preview.msi', workflow)
        self.assertIn("name: dist-desktop-windows-x64", workflow)
        sign = (HERE.parent / ".github/workflows/windows-sign.yml").read_text()
        self.assertIn("name: dist-desktop-windows-x64", sign)
        self.assertEqual(set(ra.PS1_SCRIPTS), set(winsign.SCRIPTS))
        self.assertEqual(ra.TUI["windows-x64"], winsign.TUI_EXE)
        for mode, official in (("release", True), ("smoke", False)):
            src = collected(self.root / f"real-{mode}", mode)
            name = f"Metro-{V}-windows-x64.zip"
            with zipfile.ZipFile(src / name) as z:
                manifest = json.loads(z.read(f"Metro-{V}-windows-x64/RELEASE.json"))
            winsign.check_manifest(manifest, V, True, official)
            self.assertEqual({"mode": "authenticode" if official else "unsigned-preview"}["mode"],
                             manifest["signing"]["mode"])
        # package.py writes exactly winsign.MANIFEST_KEYS for Windows (no extra key such as "tui").
        tree = ast.parse((HERE / "package.py").read_text())
        desktop = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == "desktop")
        returned = next(n.value for n in ast.walk(desktop) if isinstance(n, ast.Return))
        keys = {k.value for k in returned.keys if isinstance(k, ast.Constant)}
        self.assertEqual(keys | {"version", "target", "kind", "files"}, winsign.MANIFEST_KEYS)

    def test_ps1_must_be_this_checkouts_script(self):
        src = collected(self.root / "collected", "smoke")
        (src / "metro-runner-container.ps1").write_bytes(b"Write-Output 'other'\r\n")
        with self.assertRaisesRegex(ra.AssetError, "differs from this checkout"):
            ra.assemble(V, "smoke", src, self.out)
        shutil.rmtree(src)
        src = collected(self.root / "collected")
        (src / "install-metro-windows.ps1").write_bytes(b"# changed\r\n" + PS1_SIG)
        with self.assertRaisesRegex(ra.AssetError, "plus one Authenticode block"):
            ra.assemble(V, "release", src, self.out)

    def test_release_refuses_what_must_never_ship(self):
        cases = {  # the macOS cases are in test_macos_release_checks_still_hold_when_macs_return
            "unsigned Windows seed": dict(windows_signed=False),
            "unsigned MSI": dict(msi_signed=False),
            "unsigned ps1": dict(ps1_signed=False),
            "preview-mode Windows ZIP": dict(win_mode="unsigned-preview"),
            "no terminal client": dict(tui=False),
        }
        for label, kw in cases.items():
            with self.subTest(label):
                shutil.rmtree(self.root / "collected", ignore_errors=True)
                shutil.rmtree(self.out, ignore_errors=True)
                with self.assertRaises(ra.AssetError):
                    self.assemble(**kw)
                self.assertFalse(self.out.exists())

    def test_missing_and_extra_inputs_are_refused(self):
        src = collected(self.root / "collected")
        (src / f"Metro-{V}-linux-x64.tar.gz").unlink()
        with self.assertRaisesRegex(ra.AssetError, "missing release files: Metro-0.3.0-linux-x64.tar.gz"):
            ra.assemble(V, "release", src, self.out)
        shutil.rmtree(src)
        src = collected(self.root / "collected")
        (src / "preview-not-verified-adhoc-Metro-0.3.0-macos-arm64.tar.gz").write_bytes(b"x")
        with self.assertRaisesRegex(ra.AssetError, "unexpected files"):
            ra.assemble(V, "release", src, self.out)
        shutil.rmtree(src)
        src = collected(self.root / "collected", "smoke")
        with self.assertRaisesRegex(ra.AssetError, "missing release files"):
            ra.assemble(V, "release", src, self.out)  # a preview set is never a release
        shutil.rmtree(src)
        # macOS builds are off: even a notarized-looking Mac archive is never part of a release.
        for target in ("macos-arm64", "macos-x64"):
            src = collected(self.root / "collected")
            write_mac(src, target, mac_fields())
            with self.subTest(target), self.assertRaisesRegex(ra.AssetError, f"unexpected files.*{target}"):
                ra.assemble(V, "release", src, self.out)
            shutil.rmtree(src)

    def test_macos_release_checks_still_hold_when_macs_return(self):
        """The macOS gate is kept for later; exercise it with the targets it would have."""
        with patch.object(ra, "MACOS", ("macos-arm64", "macos-x64")):
            self.assemble()
            names = {p.name for p in self.out.iterdir()}
            self.assertTrue({f"Metro-{V}-macos-arm64.tar.gz", f"Metro-{V}-macos-x64.tar.gz",
                             "install-metro-macos.sh"} <= names)
            src = collected(self.root / "smoke-collected", "smoke")
            with self.assertRaisesRegex(ra.AssetError, "missing release files"):
                ra.assemble(V, "release", src, self.root / "out2")  # a tag never ships without macOS
            for label, mac in {
                "ad hoc macOS": mac_fields(signing="adhoc", status="preview-not-verified"),
                "not stapled": mac_fields(notarization={**mac_fields()["notarization"], "stapled": False}),
                "ticket missing": mac_fields(notarization={**mac_fields()["notarization"],
                                                           "ticket_files": ["Metro.app/Contents/Nope"]}),
            }.items():
                with self.subTest(label):
                    out = self.root / f"out-{label}"
                    with self.assertRaises(ra.AssetError):
                        ra.assemble(V, "release", collected(self.root / f"c-{label}", mac=mac), out)
                    self.assertFalse(out.exists())

    def test_tampering_is_refused(self):
        src = collected(self.root / "collected")
        zip_path = src / f"Metro-{V}-windows-x64.zip"
        zip_path.write_bytes(zip_path.read_bytes() + b"!")
        with self.assertRaisesRegex(ra.AssetError, "does not match"):
            ra.assemble(V, "release", src, self.out)
        shutil.rmtree(src)
        src = collected(self.root / "collected")
        write_archive(src, f"Metro-{V}-linux-x64", {"Metro": b"x", "metro-tui": b"t"}, "desktop", "linux-x64",
                      {"signing": "adhoc", "tui": "metro-tui"})
        with self.assertRaisesRegex(ra.AssetError, "unexpected signing"):
            ra.assemble(V, "release", src, self.out)

    def test_versions(self):
        with self.assertRaisesRegex(ra.AssetError, "tag version"):
            ra.Layout("0.3.0-ci", "release")
        ra.Layout("0.3.0-ci", "smoke")
        ra.Layout("0.3.0-rc.1", "release")

    def test_terminal_client_names_agree_across_packaging(self):
        import macos_release
        import package
        self.assertEqual(ra.TUI, {"linux-x64": package.TUI_NAMES["linux"], "windows-x64": package.TUI_NAMES["windows"],
                                  "macos-arm64": "Metro.app/" + package.TUI_NAMES["macos"],
                                  "macos-x64": "Metro.app/" + package.TUI_NAMES["macos"]})
        self.assertEqual(package.TUI_NAMES["macos"], macos_release.TUI)
        r = subprocess.run([sys.executable, "-I", str(HERE / "package.py"), "desktop", "--help"],
                           capture_output=True, text=True)
        self.assertIn("--tui", r.stdout)

    def test_authenticode_detection(self):
        self.assertTrue(ra.authenticode_present(pe(True)))
        self.assertFalse(ra.authenticode_present(pe(False)))
        self.assertFalse(ra.authenticode_present(b"MZ" + b"\0" * 10))
        self.assertTrue(ra.ps1_signed(b"x\r\n" + PS1_SIG, b"x\r\n"))
        self.assertTrue(ra.ps1_signed(b"x" + b"\r\n" + PS1_SIG, b"x"))
        self.assertFalse(ra.ps1_signed(b"x\r\n" + PS1_SIG + PS1_SIG, b"x\r\n"))
        self.assertFalse(ra.ps1_signed(b"y\r\n" + PS1_SIG, b"x\r\n"))
        self.assertFalse(ra.ps1_signed(b"x\r\n# SIG # Begin signature block\r\n", b"x\r\n"))
        self.assertTrue(winsign.SIG_BLOCK.match(PS1_SIG))


class SignerContractTests(unittest.TestCase):
    """The assembled folder is exactly what sign-release-assets.yml (release_signatures.py) stages."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.rel = self.root / "release"
        ra.assemble(V, "release", collected(self.root / "collected"), self.rel)

    def tearDown(self):
        self.tmp.cleanup()

    def test_release_signatures_prepare_accepts_the_aggregate(self):
        staged = self.root / "staged"
        release_signatures.prepare(self.rel, image_evidence(self.root / "img"), staged, V, REV)
        release_signatures.check_directory(staged, V, REV, signed=False)
        self.assertEqual({p.name for p in staged.iterdir()}, {p.name for p in self.rel.iterdir()} | {"IMAGES.txt"})
        for name in ra.PS1_SCRIPTS:  # the signed scripts reach the signer unchanged
            self.assertEqual((staged / name).read_bytes(), (self.rel / name).read_bytes())

    def test_verify_cli_and_tamper(self):
        r = subprocess.run([sys.executable, "-I", str(HERE / "release_assets.py"), "verify", "--version", V,
                            "--mode", "release", "--release", str(self.rel)], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        script = self.rel / "install-metro-linux.sh"
        script.write_text(script.read_text() + "# changed\n")
        with self.assertRaisesRegex(ra.AssetError, "SHA256SUMS"):
            ra.verify(V, "release", self.rel)
        (self.rel / "notes.txt").write_text("x")
        with self.assertRaisesRegex(ra.AssetError, "differs"):
            ra.verify(V, "release", self.rel)

    def test_cli_failures_exit_nonzero(self):
        r = subprocess.run([sys.executable, "-I", str(HERE / "release_assets.py"), "assemble", "--version", V,
                            "--mode", "release", "--collected", str(self.root / "nope"), "--out",
                            str(self.root / "o")], capture_output=True, text=True)
        self.assertEqual(r.returncode, 1)
        self.assertIn("release_assets:", r.stderr)


if __name__ == "__main__":
    unittest.main()
