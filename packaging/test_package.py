import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile

import package


class PackageTests(unittest.TestCase):
    def test_fixed_archive_outputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stage = root / "stage"
            stage.mkdir()
            source = stage / "bin"
            source.write_bytes(b"Metro" * 8192)
            source.chmod(0o755)
            expected = package.sha256(source)
            for strict_windows in (True, False):
                a, b = root / "first", root / "second"
                package.archive(stage, a, "Metro", strict_windows)
                source.touch()  # file mtime must not affect the output.
                package.archive(stage, b, "Metro", strict_windows)
                self.assertEqual(package.sha256(a), package.sha256(b))
                if strict_windows:
                    with zipfile.ZipFile(a) as result:
                        member = result.getinfo("Metro/bin")
                        self.assertEqual(member.compress_type, zipfile.ZIP_DEFLATED)
                        self.assertEqual(member.external_attr >> 16 & 0o777, 0o755)
                        self.assertEqual(result.read(member), source.read_bytes())
                else:
                    with tarfile.open(a) as result:
                        self.assertEqual(result.getmember("Metro/bin").mode, 0o755)
                        self.assertEqual(result.extractfile("Metro/bin").read(), source.read_bytes())
                self.assertEqual(package.sha256(source), expected)

    def test_rejects_invalid_binaries(self):
        with tempfile.TemporaryDirectory() as directory:
            fake = Path(directory) / "fake"
            fake.write_text("not an executable")
            with self.assertRaisesRegex(ValueError, "expected PE"):
                package.pe(fake)
            with self.assertRaisesRegex(ValueError, "expected Linux x64 ELF"):
                package.elf(fake, (2, 39))

    def test_desktop_requires_server(self):
        result = subprocess.run(
            [sys.executable, str(Path(package.__file__)), "desktop", "--target", "windows-x64",
             "--launcher", "launcher", "--ui-dir", "ui", "--nsh-root", "nsh",
             "--version", "0.2.0", "--notices", "notices"],
            text=True, capture_output=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("required: --server", result.stderr)

    def test_wrong_ui_version_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            ui = Path(directory) / "ui"
            (ui / "nsh").mkdir(parents=True)
            (ui / "nsh/bundle.nshc").write_bytes(b"bundle")
            (ui / "nsh/capabilities.json").write_text("{}")
            (ui / "nsh/assets").mkdir()
            (ui / "nsh/app.json").write_text(json.dumps({
                "name": "Metro", "id": "dev.metro.desktop", "version": "0.1.0",
            }))
            result = subprocess.run(
                [sys.executable, str(Path(package.__file__)), "desktop", "--target", "windows-x64",
                 "--launcher", "launcher", "--server", "server", "--ui-dir", str(ui),
                 "--nsh-root", "nsh", "--version", "0.2.0", "--notices", "notices",
                 "--out-dir", str(Path(directory) / "out")],
                text=True, capture_output=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("manifest name, ID or version", result.stderr)
            self.assertEqual(list((Path(directory) / "out").iterdir()), [])

    def test_nsh_resources_validate_caps_and_nested_icons(self):
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            source, ui = scratch / "source", scratch / "ui"
            icons = source / "src/ui/icons"
            icons.mkdir(parents=True)
            (icons.parent / "chrome.tsx").write_text('import check from "./icons/check.svg";\n')
            (icons / "check.svg").write_text("<svg/>")
            declared = {"network": {"hosts": ["*"]}, "process": {"env": sorted(package.BRIDGE_ENV)}}
            (source / "nsh.json").write_text(json.dumps({"capabilities": declared}))
            resources = ui / "nsh"
            packaged_icons = resources / "assets/src/ui/icons"
            packaged_icons.mkdir(parents=True)
            (resources / "app.json").write_text(json.dumps({
                "name": "Metro", "id": "dev.metro.desktop", "version": "0.2.0", "schemes": [],
            }))
            (resources / "bundle.nshc").write_bytes(b"bundle")
            capabilities = resources / "capabilities.json"
            correct = {
                "network": {"http": [], **declared["network"]},
                "process": {"execute": [], "exit": False, **declared["process"]},
            }
            capabilities.write_text(json.dumps(correct))
            asset = packaged_icons / "check.svg"
            with self.assertRaisesRegex(ValueError, "missing renderer assets"):
                package.nsh_resources(ui, source, "0.2.0")
            package.copy(icons / "check.svg", asset)
            self.assertIn(Path("assets/src/ui/icons/check.svg"), package.nsh_resources(ui, source, "0.2.0"))
            (packaged_icons / ".env").write_text("unapproved")
            with self.assertRaisesRegex(ValueError, "unapproved renderer asset"):
                package.nsh_resources(ui, source, "0.2.0")
            (packaged_icons / ".env").unlink()
            capabilities.write_text(json.dumps({"network": correct["network"]}))
            with self.assertRaisesRegex(ValueError, "packaged capabilities differ"):
                package.nsh_resources(ui, source, "0.2.0")
            capabilities.write_text(json.dumps(correct))
            asset.write_text("tampered")
            with self.assertRaisesRegex(ValueError, "exact non-executable copy"):
                package.nsh_resources(ui, source, "0.2.0")
            asset.unlink()
            try:
                asset.symlink_to(icons / "check.svg")
            except OSError:
                return  # native Windows test users may lack permission to create symlinks
            with self.assertRaisesRegex(ValueError, "not a regular release file"):
                package.nsh_resources(ui, source, "0.2.0")

    def test_brand_notice_must_cover_every_shipped_mark(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)
            icons = source / "src/ui/icons"
            icons.mkdir(parents=True)
            notice = icons / "LICENSE-brand-marks.txt"
            notice.write_text("@lobehub/icons-static-svg 1.105.0 (MIT)\n"
                              "  brand-example.svg <- upstream.svg\n"
                              "MIT License\nCopyright (c) 2023 LobeHub\n")
            resources = {Path("assets/src/ui/icons/brand-example.svg")}
            self.assertEqual(package.brand_notice(source, resources), notice)
            resources.add(Path("assets/src/ui/icons/brand-unlisted.svg"))
            with self.assertRaisesRegex(ValueError, "attribution list"):
                package.brand_notice(source, resources)


if __name__ == "__main__":
    unittest.main()
