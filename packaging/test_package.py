import json
import os
from pathlib import Path
import plistlib
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import zipfile

import package


# Icons each production module imports in the fixtures; activity.tsx shares chrome's.
MODULE_ICONS = {
    "chrome.tsx": ["check.svg"],
    "activity.tsx": ["check.svg"],
    "chatControls.tsx": ["send.svg"],
    "library.tsx": ["play.svg"],
}


def renderer_source(source):
    icons = source / "src/ui/icons"
    icons.mkdir(parents=True)
    for module, names in MODULE_ICONS.items():
        (icons.parent / module).write_text(
            "".join(f'import {Path(n).stem} from "./icons/{n}";\n' for n in names))
        for name in names:
            (icons / name).write_text(f"<svg id='{name}'/>")
    package.copy(package.RENDERER_SOURCE / package.INTER_FONT, source / package.INTER_FONT)
    return icons


def bundle_font(source, assets):
    package.copy(source / package.INTER_FONT, assets / package.INTER_FONT)


class PackageTests(unittest.TestCase):
    def test_icon_modules_are_the_production_owners(self):
        self.assertEqual(package.ICON_MODULES, tuple(MODULE_ICONS))
        live = package.RENDERER_SOURCE / "src/ui"
        for module in package.ICON_MODULES:
            self.assertTrue((live / module).is_file(), module)
    def test_fixed_archive_outputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stage = root / "stage"
            stage.mkdir()
            source = stage / "Metro.exe"
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
                        member = result.getinfo("Metro/Metro.exe")
                        self.assertEqual(member.compress_type, zipfile.ZIP_DEFLATED)
                        self.assertEqual(member.external_attr >> 16 & 0o777, 0o755)
                        self.assertEqual(result.read(member), source.read_bytes())
                else:
                    with tarfile.open(a) as result:
                        self.assertEqual(result.getmember("Metro/Metro.exe").mode, 0o755)
                        self.assertEqual(result.extractfile("Metro/Metro.exe").read(), source.read_bytes())
            self.assertEqual(package.sha256(source), expected)

    def test_windows_zip_modes_are_independent_of_host_stat(self):
        executables = ("Metro.exe", "metro-server.exe", "metro-tui.exe", "ui/Metro.exe")
        data = ("RELEASE.json", "START.txt", "licenses/BRAND-MARKS.txt", "licenses/INTER-OFL.txt",
                "ui/nsh/icon.ico", "ui/nsh/assets/src/ui/fonts/InterVariable.ttf")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stage = root / "stage"
            for rel in (*executables, *data):
                path = stage / rel
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(rel.encode())
            original_stat = Path.stat

            def host_stat(exe_mode, data_mode):
                def get_stat(path, *args, **kwargs):
                    result = original_stat(path, *args, **kwargs)
                    if stat.S_ISREG(result.st_mode) and path.is_relative_to(stage):
                        mode = exe_mode if path.relative_to(stage).as_posix() in executables else data_mode
                        return os.stat_result((stat.S_IFREG | mode, *result[1:]))
                    return result
                return get_stat

            linux, windows = root / "linux.zip", root / "windows.zip"
            with patch.object(Path, "stat", host_stat(0o755, 0o644)):
                package.archive(stage, linux, "Metro", True)
            # Mode bits observed in the native Windows seed from run 45191721188.
            with patch.object(Path, "stat", host_stat(0o777, 0o666)):
                package.archive(stage, windows, "Metro", True)
            self.assertEqual(linux.read_bytes(), windows.read_bytes())
            with zipfile.ZipFile(windows) as result:
                self.assertEqual(
                    {member.filename.removeprefix("Metro/"): (member.create_system, member.external_attr)
                     for member in result.infolist()},
                    {rel: (3, (stat.S_IFREG | mode) << 16)
                     for rels, mode in ((executables, 0o755), (data, 0o644)) for rel in rels},
                )
                for rel in (*executables, *data):
                    self.assertEqual(result.read(f"Metro/{rel}"), rel.encode())

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
            icons = renderer_source(source)
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
            for name in ("check.svg", "send.svg", "play.svg"):
                package.copy(icons / name, packaged_icons / name)
            bundle_font(source, resources / "assets")
            self.assertIn(Path("assets/src/ui/icons/check.svg"), package.nsh_resources(ui, source, "0.2.0"))
            # The final packager supplies its own exact renderer sidecar, not an
            # arbitrary icon submitted alongside the stock NSH build.
            upstream_icon = resources / "icon.ico"
            upstream_icon.write_bytes(b"unapproved")
            with self.assertRaisesRegex(ValueError, "expected NSH resources"):
                package.nsh_resources(ui, source, "0.2.0")
            upstream_icon.unlink()
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

    def test_icons_of_every_production_module_and_no_stray_ones(self):
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            source, actual = scratch / "source", scratch / "assets"
            icons = renderer_source(source)
            bundled = actual / "src/ui/icons"
            bundled.mkdir(parents=True)
            for name in ("check.svg", "send.svg"):
                package.copy(icons / name, bundled / name)
            bundle_font(source, actual)
            # library.tsx's own import is required, though chrome.tsx doesn't import it.
            with self.assertRaisesRegex(ValueError, r"missing renderer assets: src/ui/icons/play\.svg"):
                package.nsh_assets(actual, source)
            package.copy(icons / "play.svg", bundled / "play.svg")
            self.assertEqual(set(package.nsh_assets(actual, source)),
                             {Path("src/ui/icons") / n for n in ("check.svg", "send.svg", "play.svg")}
                             | {package.INTER_FONT})

            # A test, fixture or unlisted module never approves an icon, even an exact copy.
            (icons / "stray.svg").write_text("<svg id='stray'/>")
            for module in ("theme.fixture.tsx", "QuickSearch.test.tsx", "extra.tsx"):
                (icons.parent / module).write_text('import stray from "./icons/stray.svg";\n')
            self.assertEqual(len(package.nsh_assets(actual, source)), 4)  # unbundled: fine
            package.copy(icons / "stray.svg", bundled / "stray.svg")
            with self.assertRaisesRegex(ValueError, r"unapproved renderer asset: src/ui/icons/stray\.svg"):
                package.nsh_assets(actual, source)
            (bundled / "stray.svg").unlink()

            # A non-chrome module's icon is still checked byte for byte.
            (bundled / "play.svg").write_text("<svg id='tampered'/>")
            with self.assertRaisesRegex(ValueError, "exact non-executable copy of its source: src/ui/icons/play.svg"):
                package.nsh_assets(actual, source)
            package.copy(icons / "play.svg", bundled / "play.svg")

            # Every listed module must exist and import distinct icons.
            library = icons.parent / "library.tsx"
            library.write_text('import a from "./icons/play.svg";\nimport b from "./icons/play.svg";\n')
            with self.assertRaisesRegex(ValueError, r"src/ui/library\.tsx must import distinct"):
                package.nsh_assets(actual, source)
            library.unlink()
            with self.assertRaisesRegex(ValueError, "not a regular release file"):
                package.nsh_assets(actual, source)

    def test_inter_font_and_license_are_pinned(self):
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            source, actual = scratch / "source", scratch / "assets"
            icons = renderer_source(source)
            for icon in icons.iterdir():
                package.copy(icon, actual / "src/ui/icons" / icon.name)
            bundled = actual / package.INTER_FONT
            with self.assertRaisesRegex(ValueError, r"missing renderer assets: src/ui/fonts/InterVariable\.ttf"):
                package.nsh_assets(actual, source)
            bundle_font(source, actual)
            self.assertEqual(package.nsh_assets(actual, source)[package.INTER_FONT], bundled)
            other = bundled.with_name("other.ttf")
            package.copy(bundled, other)
            with self.assertRaisesRegex(ValueError, "unapproved renderer asset: src/ui/fonts/other.ttf"):
                package.nsh_assets(actual, source)
            other.unlink()
            bundled.write_bytes(b"not Inter")
            with self.assertRaisesRegex(ValueError, "unexpected Inter font digest"):
                package.nsh_assets(actual, source)
            (source / package.INTER_FONT).write_bytes(bundled.read_bytes())
            with self.assertRaisesRegex(ValueError, "unexpected Inter font digest"):
                package.nsh_assets(actual, source)
            package.write_inter_notice(scratch)
            self.assertEqual(package.sha256(scratch / "licenses/INTER-OFL.txt"), package.INTER_OFL_SHA)

    def test_macos_outer_bundle_has_canonical_icon_without_an_inner_icon(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            ui = root / "Metro.app"
            contents = ui / "Contents"
            for part in ("Resources", "MacOS", "_CodeSignature"):
                (contents / part).mkdir(parents=True)
            package.write(contents / "MacOS/Metro", "signed renderer")
            package.write(contents / "_CodeSignature/CodeResources", "inner seal")
            (contents / "Info.plist").write_bytes(plistlib.dumps({
                "CFBundleExecutable": "Metro", "CFBundleIdentifier": package.MACOS_UI_ID,
                "CFBundleShortVersionString": "0.2.0", "LSMinimumSystemVersion": package.MACOS_MIN,
            }))
            stock = root / "runtime/macos/nsh-macos-app"
            stock.parent.mkdir(parents=True)
            license_path = root / "LICENSE-APACHE"
            launcher, server = root / "launcher", root / "server"
            for f in (stock, license_path, launcher, server):
                package.write(f, f.name)
            target = "macos-arm64"
            args = SimpleNamespace(target=target, version="0.2.0", nsh_root=root, ui_dir=ui,
                                   launcher=launcher, server=server, tui=None)
            stage = root / "stage"
            with (patch.object(package.sys, "platform", "darwin"),
                  patch.object(package, "NSH_MACOS_APPS", {target: (package.sha256(stock), 0x0100000C)}),
                  patch.object(package, "NSH_LICENSE_SHA", package.sha256(license_path)),
                  patch.object(package, "nsh_resources", return_value={}),
                  patch.object(package, "brand_notice", return_value=license_path),
                  patch.object(package, "stock_renderer") as stock_renderer,
                  patch.object(package, "macho", return_value=[]),
                  patch.object(package, "codesign") as codesign):
                info = package.macos(args, stage)
            outer = stage / "Metro.app/Contents"
            plist = plistlib.loads((outer / "Info.plist").read_bytes())
            self.assertEqual(plist["CFBundleIconFile"], "Metro.icns")
            icns = (outer / "Resources/Metro.icns").read_bytes()
            self.assertEqual(struct.unpack(">4sI4sI", icns[:16]), (b"icns", len(icns), b"ic08", len(icns) - 8))
            self.assertEqual(icns[16:], package.app_icon("metro-256.png").read_bytes())
            self.assertEqual(struct.unpack(">II", icns[32:40]), (256, 256))
            inner = outer / "Helpers/MetroUI.app/Contents"
            self.assertFalse((inner / "Resources/Metro.icns").exists())
            self.assertEqual(package.sha256(inner / "_CodeSignature/CodeResources"),
                             info["renderer"]["code_resources_sha256"])
            self.assertEqual(package.sha256(inner / "MacOS/Metro"), package.sha256(contents / "MacOS/Metro"))
            for call in codesign.call_args_list:
                self.assertFalse("--sign" in call.args and "MetroUI.app" in call.args[-1])
            stock_renderer.assert_called_once_with(contents / "MacOS/Metro", stock, contents, target)
            self.assertEqual(package.sha256(stage / "licenses/INTER-OFL.txt"), package.INTER_OFL_SHA)

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

    def test_multi_resolution_pe_icon_tree(self):
        images = package.ico_images(package.app_icon("metro.ico"))
        tree = bytearray()

        def put(fmt, *values):
            pos = len(tree)
            tree.extend(struct.pack(fmt, *values))
            return pos

        def directory(*ids):
            start = put("<IIHHHH", 0, 0, 0, 0, 0, len(ids))
            for id in ids:
                put("<II", id, 0)
            return start

        def child(parent, index, target, is_dir=True):
            struct.pack_into("<I", tree, parent + 20 + 8 * index, target | (0x80000000 if is_dir else 0))

        rva = 0x6000

        def language(data):
            directory_pos = directory(0)
            leaf = put("<IIII", rva + len(tree) + 16, len(data), 0, 0)
            child(directory_pos, 0, leaf, is_dir=False)
            tree.extend(data)
            return directory_pos

        group = struct.pack("<3H", 0, 1, len(images))
        root = directory(3, 14)
        icons = directory(*range(1, 1 + len(images)))
        child(root, 0, icons)
        for i, (meta, image) in enumerate(images):
            child(icons, i, language(image))
            group += meta + struct.pack("<H", i + 1)
        groups = directory(1)
        child(root, 1, groups)
        child(groups, 0, language(group))

        def verify():
            package.pe_icon(tree, rva, "Metro.exe", images)

        verify()
        tree[-1] ^= 1  # A dangling RT_ICON ID in the group is not accepted.
        with self.assertRaisesRegex(ValueError, "missing PE icon resource"):
            verify()
        tree[-1] ^= 1
        tree[icons + 16 + 8 * len(images) + 40] ^= 1  # First frame's payload.
        with self.assertRaisesRegex(ValueError, "RT_ICON 1 does not match"):
            verify()

    @unittest.skipUnless(sys.platform.startswith("linux") and shutil.which("gio"),
                         "requires GLib's real desktop launcher")
    def test_linux_application_menu_registration_and_relocation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stage = root / 'Metro space % \'" $ ` \\ 日本'
            stage.mkdir()
            for dest, source in package.linux_icon_assets().items():
                package.copy(source, stage / dest)
            package.copy(Path(package.__file__).with_name("install-desktop.sh"),
                         stage / "install-desktop", executable=True)
            home = root / "home"
            home.mkdir()
            data = root / "data with spaces"
            mark = root / "started"
            env = {**os.environ, "HOME": str(home), "XDG_DATA_HOME": str(data), "TEST_MARK": str(mark)}
            launcher = stage / "Metro"
            launcher.write_text('#!/bin/sh\nprintf "%s\\n" "$0" > "$TEST_MARK"\n')
            launcher.chmod(0o755)
            result = subprocess.run([stage / "install-desktop"], cwd=root, env=env, capture_output=True)
            self.assertEqual(result.returncode, 2)
            self.assertFalse(data.exists())
            self.assertEqual(list(home.iterdir()), [])
            entry = data / f"applications/{package.APP_ID}.desktop"
            theme = data / "icons/hicolor"
            installed_icons = [theme / f"{size}/apps/{package.APP_ID}.{ext}"
                               for size, ext in (("256x256", "png"), ("scalable", "svg"))]
            for cycle in range(2):
                package.verify_desktop_icons(stage, windows=False)
                subprocess.run([stage / "install-desktop", "--install"], cwd=root, env=env, check=True,
                               capture_output=True)
                for icon in installed_icons:
                    self.assertEqual(icon.read_bytes(), (stage / "icons" / icon.name).read_bytes())
                lines = entry.read_text().splitlines()
                self.assertIn(f"Icon={package.APP_ID}", lines)
                self.assertIn(f"StartupWMClass={package.APP_ID}", lines)
                if shutil.which("desktop-file-validate"):
                    subprocess.run(["desktop-file-validate", entry], check=True, capture_output=True)
                launched = subprocess.run(["gio", "launch", entry], cwd=root, env=env, text=True,
                                          capture_output=True, timeout=10)
                self.assertEqual(launched.returncode, 0, launched.stderr)
                self.assertEqual(mark.read_text().strip(), str(stage / "Metro"))
                mark.unlink()
                if cycle == 0:
                    stage = stage.rename(root / "new folder")
            welcome = data / "applications/unrelated.desktop"
            welcome.write_text("unchanged")
            subprocess.run([stage / "install-desktop", "--uninstall"], cwd=root, env=env, check=True,
                           capture_output=True)
            self.assertFalse(entry.exists())
            self.assertTrue(welcome.exists())
            self.assertTrue(all(not icon.exists() for icon in installed_icons))
            self.assertEqual(list(home.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
