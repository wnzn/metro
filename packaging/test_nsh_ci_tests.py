"""Test desktop CI scheduling and failure propagation without installing NSH."""

from contextlib import redirect_stdout
import io
import json
import os
from pathlib import Path
import select
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import nsh_ci_tests as policy


FAKE_NSH = r'''
import json
import os
from pathlib import Path
import sys
import time
import xml.etree.ElementTree as ET

if sys.argv[1:] == ["--version"]:
    print(os.environ.get("FAKE_VERSION", "nsh 0.6.1 (fake)"))
    sys.exit(0)
audit = Path(os.environ["FAKE_AUDIT"])
if sys.argv[1:] == ["test"]:
    (audit / "unfiltered").write_text(str(Path.cwd()))
    sys.exit(int(os.environ.get("FAKE_PLAIN_STATUS", "0")))
_, action, path, option, output = sys.argv
assert action == "test" and option == "--junit"
file = Path(path).relative_to(Path.cwd()).as_posix()
output = Path(output)
dirs = ("HOME", "TMPDIR", "XDG_CACHE_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME",
        "XDG_RUNTIME_DIR", "XDG_STATE_HOME")
state = {key: os.environ[key] for key in dirs}
assert all(Path(path).is_dir() for path in state.values())
work = Path.cwd() / ".nsh" / "test"
work.mkdir(parents=True, exist_ok=True)
assert (Path.cwd() / "target" / "fixture").read_text() == "fixture"
(audit / file.replace("/", "_")).write_text(json.dumps(
    state | {"app": str(Path.cwd()), "work": str(work), "report": str(output)}))
mode = Path(path).read_text()
if mode == "timeout":
    child = os.fork()
    if child == 0:
        (audit / "timeout-child").write_text(str(os.getpid()))
    time.sleep(15)
    sys.exit(0)
if mode == "missing":
    sys.exit(0)
if mode == "malformed":
    output.write_text("<")
    sys.exit(0)
if mode.startswith("empty"):
    root = ET.Element("testsuites", name="nsh test", tests=str(int(mode == "empty-invalid")),
                      failures="0", errors="0", skipped="0")
    ET.ElementTree(root).write(output)
    sys.exit(7 if mode == "empty-nonzero" else 0)
attrs = {"tests": "4" if mode != "bad-count" else "3",
         "failures": str(int(mode in ("failure", "two-failures"))),
         "errors": str(int(mode in ("error", "two-failures"))), "skipped": "2"}
root = ET.Element("testsuites", name="nsh test", **attrs)
suite = ET.SubElement(root, "testsuite", name=file if mode != "wrong-suite" else "Other.test.ts", **attrs)
for name in ("passed", "second", "skip", "todo"):
    label = "third\n::error::not a workflow command ##[error]" if mode == "two-failures" and name == "passed" else name
    case = ET.SubElement(suite, "testcase", name=label, classname=file)
    if name == "second" and mode in ("failure", "error"):
        ET.SubElement(case, mode).text = "test assertion body must not leak"
    if mode == "two-failures" and name in ("passed", "second"):
        ET.SubElement(case, "error" if name == "passed" else "failure").text = "test assertion body must not leak"
    if name == "skip":
        ET.SubElement(case, "skipped")
    if name == "todo":
        ET.SubElement(case, "skipped", message="todo")
ET.ElementTree(root).write(output)
print("raw test output must not leak", file=sys.stderr)
sys.exit(7 if mode == "nonzero" else 0)
'''


@unittest.skipUnless(sys.platform == "linux", "CI shards run only on Linux")
class ShardTests(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name)
        self.app = self.root / "app"
        self.write("App.tsx")
        self.write("nsh.json", "{}")
        self.write("a.test.ts")
        self.write("nested/b.test.tsx")
        self.write("target/fixture", "fixture")
        fake = self.root / "nsh"
        fake.write_text(f"#!{sys.executable}\n" + FAKE_NSH)
        fake.chmod(0o700)
        self.audit = self.root / "audit"
        self.audit.mkdir()
        self.tmp = self.root / "tmp"
        self.tmp.mkdir()
        self.env = os.environ | {
            "PATH": f"{self.root}{os.pathsep}{os.environ['PATH']}", "FAKE_AUDIT": str(self.audit),
            "TMPDIR": str(self.tmp), "RUNNER_TEMP": "", "METRO_NSH_WORKERS": "2", "XDG_DATA_HOME": "/shared",
        }

    def write(self, file, contents=""):
        path = self.app / file
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents)

    def run_helper(self, *args, **overrides):
        return subprocess.run([sys.executable, "-B", policy.__file__, *args],
                              cwd=self.app, env=self.env | overrides, capture_output=True, text=True)

    def test_discovery_matches_nsh_suffixes_and_directory_exclusions(self):
        for name in (".dot.test.ts", "dev/-fresh.test.ts", "dir.test.ts/inner.test.tsx",
                     ".hidden/no.test.ts", "nested/node_modules/no.test.ts", "target/no.test.tsx",
                     ".nsh/test/no.test.ts", "not.test.TS", "not.spec.tsx", "not.test.ts.bak"):
            self.write(name)
        self.assertEqual([file.as_posix() for file in policy.discover(self.app)],
                         [".dot.test.ts", "a.test.ts", "dev/-fresh.test.ts",
                          "dir.test.ts/inner.test.tsx", "nested/b.test.tsx"])

    def test_measured_slowest_first_then_size_and_stable_path_tie_break(self):
        self.write("App.test.tsx", "a")
        self.write("src/features/QuickSearch.test.tsx", "b")
        self.write("large.test.ts", "a" * 300)
        self.assertEqual([file.as_posix() for file in policy.schedule(self.app, policy.discover(self.app))],
                         ["App.test.tsx", "src/features/QuickSearch.test.tsx", "large.test.ts",
                          "a.test.ts", "nested/b.test.tsx"])

    def test_worker_directories_and_reports_are_private(self):
        exported = self.root / "exported"
        runner_temp = self.root / "runner-temp"
        runner_temp.mkdir()
        self.write("dev/fresh.test.ts")
        result = self.run_helper("--report-dir", str(exported), METRO_NSH_WORKERS="3", RUNNER_TEMP=str(runner_temp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("3 files, 3 isolated workers", result.stdout)
        self.assertIn("6 passed, 0 failed, 0 errors, 3 skipped, 3 todo", result.stdout)
        self.assertFalse((self.audit / "unfiltered").exists())
        self.assertFalse((self.app / ".nsh").exists())
        self.assertEqual(list(self.tmp.iterdir()), [])
        self.assertEqual(list(runner_temp.iterdir()), [])
        runs = [json.loads(path.read_text()) for path in self.audit.iterdir()]
        self.assertEqual(len(runs), 3)
        for name in runs[0]:
            self.assertEqual(len({run[name] for run in runs}), 3, name)
        for run in runs:
            self.assertEqual(Path(run["work"]).parent.parent, Path(run["app"]))
            self.assertTrue(Path(run["app"]).is_relative_to(runner_temp))
            self.assertEqual(Path(run["XDG_DATA_HOME"]).parent, Path(run["HOME"]))
        self.assertEqual(sorted(path.name for path in exported.iterdir()), ["000.xml", "001.xml", "002.xml"])

    def test_new_file_added_after_first_run_and_repeatable_count(self):
        self.assertIn("2/2 files", self.run_helper().stdout)
        self.write("dev/new.test.tsx")
        for workers in ("2", "3"):
            result = self.run_helper(METRO_NSH_WORKERS=workers)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("3/3 files", result.stdout)
            self.assertIn("6 passed, 0 failed, 0 errors, 3 skipped, 3 todo", result.stdout)
        self.assertTrue((self.audit / "dev_new.test.tsx").is_file())

    def test_failed_and_unverifiable_shards_never_fall_back(self):
        for mode in ("missing", "malformed", "bad-count", "failure", "error", "nonzero", "wrong-suite",
                     "empty-invalid", "empty-nonzero", "two-failures"):
            with self.subTest(mode=mode):
                self.write("a.test.ts", mode)
                result = self.run_helper()
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn("2/2 files, 1 unsuccessful", result.stdout)
                self.assertIn("'a.test.ts':", result.stderr)
                self.assertIn("NSH exited 7" if mode.endswith("nonzero") else "JUnit", result.stderr)
                if mode in ("failure", "error", "two-failures"):
                    self.assertIn("NSH: 'a.test.ts' › 'second': failure", result.stderr.splitlines())
                if mode == "two-failures":
                    self.assertIn(
                        r"NSH: 'a.test.ts' › 'third\n::error::not a workflow command \x23#[error]': failure",
                        result.stderr.splitlines(),
                    )
                    self.assertTrue(all(line.startswith("NSH: ") for line in result.stderr.splitlines()))
                    self.assertNotIn("##[", result.stderr)
                self.assertNotIn("must not leak", result.stdout + result.stderr)
                self.assertFalse((self.audit / "unfiltered").exists())

    def test_timed_out_file_kills_its_process_group_and_never_falls_back(self):
        self.write("a.test.ts", "timeout")
        # Exercise the real Popen timeout path without waiting ten minutes.
        code = (f"import sys; sys.path.insert(0, {str(Path(policy.__file__).parent)!r}); "
                "import nsh_ci_tests as policy; policy.FILE_TIMEOUT_SECONDS = 1; sys.exit(policy.main())")
        result = subprocess.run([sys.executable, "-B", "-c", code], cwd=self.app,
                                env=self.env, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("'a.test.ts': NSH timed out", result.stderr)
        self.assertIn("2/2 files, 1 unsuccessful; 2 passed", result.stdout)
        self.assertFalse((self.audit / "unfiltered").exists())
        child = int((self.audit / "timeout-child").read_text())
        try:
            fd = os.pidfd_open(child)
        except ProcessLookupError:
            return  # The descendant is already gone.
        try:
            self.assertEqual(select.select([fd], [], [], 2)[0], [fd], "timed-out descendant still running")
        finally:
            os.close(fd)

    def test_empty_but_successful_test_file_matches_plain_nsh(self):
        self.write("a.test.ts", "empty")
        result = self.run_helper()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("2/2 files, 0 unsuccessful", result.stdout)
        self.assertIn("2 passed, 0 failed, 0 errors, 1 skipped, 1 todo", result.stdout)
        self.assertFalse((self.audit / "unfiltered").exists())

    def test_unknown_cli_version_falls_back_and_preserves_plain_failure(self):
        result = self.run_helper(FAKE_VERSION="nsh 0.6.2", FAKE_PLAIN_STATUS="9")
        self.assertEqual(result.returncode, 9)
        self.assertEqual([p.name for p in self.audit.iterdir()], ["unfiltered"])
        self.assertIn("running plain nsh test", result.stdout)
        self.assertEqual((self.audit / "unfiltered").read_text(), str(self.app))

    def test_layout_that_cannot_be_reproduced_falls_back_before_any_shards(self):
        for name in ("nested/App.tsx", "nested/nsh.json", "target/fixture-link"):
            with self.subTest(name=name):
                path = self.app / name
                if name.endswith("link"):
                    path.symlink_to("fixture")
                else:
                    path.touch()
                result = self.run_helper()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("running plain nsh test", result.stdout)
                self.assertEqual([p.name for p in self.audit.iterdir()], ["unfiltered"])
                path.unlink()
                (self.audit / "unfiltered").unlink()

    def test_app_with_rust_crates_falls_back_before_any_shards(self):
        self.write("nsh.json", '{"rust": "."}')
        result = self.run_helper()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Rust tests", result.stdout)
        self.assertEqual([p.name for p in self.audit.iterdir()], ["unfiltered"])

    def test_copy_failure_after_first_worker_falls_back_without_starting_any(self):
        copytree = shutil.copytree

        def copy(source, dest, *args, **kwargs):
            if Path(dest).parent.name == "worker-1":
                raise OSError("disk full")
            return copytree(source, dest, *args, **kwargs)

        with (patch.object(sys, "argv", ["nsh_ci_tests.py"]), patch.dict(os.environ, self.env, clear=True),
              patch.object(Path, "cwd", return_value=self.app), patch.object(shutil, "copytree", side_effect=copy),
              patch.object(tempfile, "tempdir", str(self.tmp)), redirect_stdout(io.StringIO()) as out):
            self.assertEqual(policy.main(), 0)
        self.assertIn("worker preflight failed; running plain nsh test", out.getvalue())
        self.assertEqual([p.name for p in self.audit.iterdir()], ["unfiltered"])
        self.assertEqual(list(self.tmp.iterdir()), [])
