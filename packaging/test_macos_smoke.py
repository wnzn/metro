"""The macOS install smoke in release.yml (desktop-macos) and smoke-macos-launch.sh.

Runs the real snippets on any host: the folder inventory (extracted from the
workflow step), the step's failure gate, and the launch script's preflight.
Launching Metro itself needs a Mac; that part runs only on the native runners.
"""

import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

try:
    import yaml
except ImportError:  # pragma: no cover
    yaml = None

ROOT = Path(__file__).resolve().parent.parent
LAUNCH = ROOT / "packaging/smoke-macos-launch.sh"
STEP = "Install into a path with spaces, then a clean start and shutdown"


def install_step():
    data = yaml.safe_load((ROOT / ".github/workflows/release.yml").read_text())
    steps = data["jobs"]["desktop-macos"]["steps"]
    return next(s for s in steps if s.get("name") == STEP)["run"]


@unittest.skipUnless(yaml, "PyYAML is not installed")
class InstallStepTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.run_text = install_step()
        # Everything up to the end of the inventory() function: shell options,
        # the gate trap and the Python inventory, exactly as the step runs them.
        end = cls.run_text.index("\n}\n") + 3
        cls.prelude = cls.run_text[:end]

    def sh(self, script, cwd):
        return subprocess.run(["bash", "--noprofile", "--norc", "-c", self.prelude + script],
                              cwd=cwd, capture_output=True, text=True,
                              env={"PATH": os.defpath + ":/usr/local/bin", "LC_ALL": "C"})

    def folder(self, root, marker=b"", mode=0o644, extra=None, app_link=False):
        d = root / "Metro Desktop"
        d.mkdir()
        if app_link:
            (root / "real.app").mkdir()
            (d / "Metro.app").symlink_to(root / "real.app")
        else:
            (d / "Metro.app").mkdir()
        if marker is not None:
            (d / "enable-local-runner").write_bytes(marker)
            (d / "enable-local-runner").chmod(mode)
        if extra:
            (d / extra).write_text("x")
        return d

    def test_inventory_is_exact_and_locale_independent(self):
        want = 'inventory "$1" Metro.app=dir enable-local-runner=empty-file-644'
        cases = {
            "installed": ({}, True, None),
            "extra .DS_Store": ({"extra": ".DS_Store"}, False, "('.DS_Store', 'file-"),
            "marker missing": ({"marker": None}, False, "found [('Metro.app', 'dir')]"),
            "marker not empty": ({"marker": b"1"}, False, "'file-644'"),
            "marker mode 600": ({"mode": 0o600}, False, "'empty-file-600'"),
            "app is a symlink": ({"app_link": True}, False, "('Metro.app', 'symlink')"),
        }
        for name, (kwargs, ok, detail) in cases.items():
            with self.subTest(name), tempfile.TemporaryDirectory() as d:
                folder = self.folder(Path(d), **kwargs)
                for locale in ("C", "en_US.UTF-8"):
                    proc = subprocess.run(
                        ["bash", "--noprofile", "--norc", "-c", self.prelude + want, "_", str(folder)],
                        capture_output=True, text=True,
                        env={"PATH": os.defpath + ":/usr/local/bin", "LC_ALL": locale, "LANG": locale,
                             **({"LOCPATH": os.environ["LOCPATH"]} if "LOCPATH" in os.environ else {})})
                    self.assertEqual(proc.returncode == 0, ok, (locale, proc.stderr))
                    if not ok:
                        self.assertIn(detail, proc.stderr)
                        self.assertIn('gate "setup" failed', proc.stderr)

    def test_after_update_without_local_runner_only_the_app_remains(self):
        self.assertIn('inventory "$dir" Metro.app=dir\n', self.run_text)
        with tempfile.TemporaryDirectory() as d:
            folder = self.folder(Path(d), marker=None)
            proc = self.sh(f'inventory "{folder}" Metro.app=dir', d)
            self.assertEqual(proc.returncode, 0, proc.stderr)

    def test_a_failing_check_names_its_gate_without_printing_arguments(self):
        with tempfile.TemporaryDirectory() as d:
            proc = self.sh('gate="outer bundle identifier dev.metro.desktop"\n'
                           'test "$(printf wrong)" = dev.metro.desktop\necho unreachable', d)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('::error title=macOS install smoke::gate "outer bundle identifier dev.metro.desktop" failed',
                      proc.stderr)
        self.assertNotIn("unreachable", proc.stdout)
        self.assertNotIn("wrong", proc.stderr)

    def test_every_check_has_a_gate_and_order_is_kept(self):
        text = self.run_text
        self.assertTrue(text.startswith("set -Eeuo pipefail\n"))
        code = "\n".join(line for line in text.splitlines() if not line.lstrip().startswith("#"))
        self.assertNotRegex(code, r"\|\s*sort\b")
        self.assertNotIn("set -x", text)
        order = ['gate="install (path with spaces)"', "install --archive",
                 'gate="install folder holds exactly', "inventory \"$dir\" Metro.app=dir enable-local-runner",
                 "codesign --verify --deep --strict \"$app\"", "MetroUI.app\"\n", "metro-tui\"\n",
                 "metro-tui\" --help", 'grep -q "Metro conversations"',
                 '= dev.metro.desktop\n', '= dev.metro.desktop.ui\n', "smoke-macos-launch.sh",
                 "--no-local-runner", 'inventory "$dir" Metro.app=dir\n', " uninstall", 'test ! -e "$dir"',
                 'test -d "$METRO_DESKTOP_HOME"', 'echo "macOS install smoke passed"']
        positions = [text.find(o) for o in order]
        self.assertTrue(all(p >= 0 for p in positions) and positions == sorted(positions),
                        list(zip(order, positions, strict=True)))
        # Each command after the prelude sits under a gate set just before it.
        body = text[text.index('gate="install (path with spaces)"'):]
        checks = [line for line in body.splitlines()
                  if re.match(r"\s*(HOME=|inventory |codesign |\"\$app|grep |test |sh packaging/)", line)]
        self.assertGreaterEqual(len(checks), 14)
        self.assertEqual(text.count("gate="), 13)


@unittest.skipUnless(shutil.which("shasum"), "shasum is not installed")
class LaunchScriptTests(unittest.TestCase):
    def app(self, root, renderer=b"renderer", launcher=b"launcher", server=b"server"):
        app = root / "Metro Desktop/Metro.app"
        for rel, data in (("Contents/MacOS/Metro", launcher), ("Contents/MacOS/metro-server", server),
                          ("Contents/Helpers/MetroUI.app/Contents/MacOS/Metro", renderer)):
            (app / rel).parent.mkdir(parents=True, exist_ok=True)
            (app / rel).write_bytes(data)
        return app

    def run_script(self, app, manifest_text, root):
        manifest = root / "RELEASE.json"
        manifest.write_text(manifest_text)
        return subprocess.run(["sh", str(LAUNCH), str(app), str(manifest), str(root / "out"), "shot"],
                              capture_output=True, text=True,
                              env={"PATH": os.environ["PATH"], "METRO_DESKTOP_HOME": str(root / "data")})

    def test_preflight_failures_are_named(self):
        import hashlib
        renderer = hashlib.sha256(b"renderer").hexdigest()
        cases = {
            "no renderer key": (dict(), '{"files": {}}', "preflight: RELEASE.json has no renderer"),
            "UI is not the packaged renderer": (dict(), '{"renderer": {"packaged_signed_sha256": "%s"}}' % ("0" * 64),
                                                "preflight: the UI is not the packaged renderer"),
            "launcher is the renderer": (dict(launcher=b"renderer"),
                                         '{"renderer": {"packaged_signed_sha256": "%s"}}' % renderer,
                                         "preflight: the launcher is the renderer"),
            "launcher is the server": (dict(server=b"launcher"),
                                       '{"renderer": {"packaged_signed_sha256": "%s"}}' % renderer,
                                       "preflight: the launcher is the server"),
        }
        for name, (kwargs, manifest, message) in cases.items():
            with self.subTest(name), tempfile.TemporaryDirectory() as d:
                proc = self.run_script(self.app(Path(d), **kwargs), manifest, Path(d))
                self.assertEqual(proc.returncode, 1, proc.stderr)
                self.assertIn("macOS launch smoke failed: " + message, proc.stderr)

    def test_every_exit_is_a_named_failure(self):
        text = LAUNCH.read_text()
        self.assertEqual(re.findall(r"exit 1\b", text), ["exit 1"])  # only inside fail()
        self.assertNotRegex(text, r"\|\s*sort\b")
        self.assertIn('kill -TERM "$pid" || fail', text)
        for line in text.splitlines():
            stripped = line.strip()
            if stripped.startswith("test ") or (stripped.startswith("[ ") and "||" not in stripped
                                                and not stripped.endswith("&& break")):
                self.fail(f"silent check: {stripped}")
        subprocess.run(["sh", "-n", str(LAUNCH)], check=True)


if __name__ == "__main__":
    unittest.main()
