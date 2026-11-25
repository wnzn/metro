"""Run Metro's NSH 0.6.1 headless tests in isolated app copies on Linux."""

import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack
import json
import os
from pathlib import Path
from queue import Empty, Queue
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET


# Release CI 45560122825, rounded seconds. These are scheduling hints, never a
# test inventory. Unknown files are ordered by size after the measured slow ones.
SLOW_FILES = {
    "App.test.tsx": 54,
    "src/features/QuickSearch.test.tsx": 42,
    "src/features/Monitoring.test.tsx": 39,
    "src/device/Device.test.tsx": 37,
    "src/features/library/Library.test.tsx": 26,
}
RESULTS = ("passed", "failed", "errors", "skipped", "todo")


class CannotShard(Exception):
    """Only used in preflight, before any test subprocess starts."""


def entries(directory, ignored=()):
    for path in directory.iterdir():
        if path.name in ignored:
            continue
        # NSH follows directory symlinks. Copying or following one could
        # change discovery, share writes, or loop. Let NSH handle that layout.
        mode = path.lstat().st_mode
        if not (stat.S_ISDIR(mode) or stat.S_ISREG(mode)):
            raise CannotShard("app contains links or special files")
        try:
            path.name.encode("utf-8")
        except UnicodeEncodeError:
            raise CannotShard("app contains non-UTF-8 paths") from None
        if not path.name.isprintable():
            raise CannotShard("app contains paths NSH cannot report faithfully")
        yield path, stat.S_ISDIR(mode)


def discover(app):
    # NSH v0.6.1 crates/nsh-cli/src/test.rs: find_tests, is_test_file,
    # app_entry. In particular, a dotted *file* can be a test, and dev/ is
    # searched. A nested app is only ambiguous if it contains a test.
    found = []
    pending = [(app, False)]
    while pending:
        directory, nested_app = pending.pop()
        children = list(entries(directory, ignored=(".nsh",) if directory == app else ()))
        nested_app |= directory != app and any(
            not is_dir and (path.name == "nsh.json" or path.name.lower() == "app.tsx")
            for path, is_dir in children
        )
        for path, is_dir in children:
            if is_dir:
                if not path.name.startswith(".") and path.name not in {"node_modules", "target"}:
                    pending.append((path, nested_app))
            elif path.name.endswith((".test.ts", ".test.tsx")):
                if nested_app:
                    raise CannotShard("individual files would select a nested NSH app")
                found.append(path.relative_to(app))
    return sorted(found)


def schedule(app, files):
    return sorted(files, key=lambda file: (-SLOW_FILES.get(file.as_posix(), 0),
                                          -(app / file).stat().st_size, file))


def prepare(app, scratch, nsh, env):
    if sys.platform != "linux" or not nsh:
        raise CannotShard("isolated workers require Linux and NSH on PATH")
    version = subprocess.run([nsh, "--version"], cwd=app, env=env, check=True,
                             capture_output=True, timeout=10)
    if not re.fullmatch(rb"nsh 0\.6\.1(?: \([^)]+\))?\s*", version.stdout):
        raise CannotShard("discovery and reports are only verified for NSH 0.6.1")
    workers = int(env.get("METRO_NSH_WORKERS", "2"))
    if not 2 <= workers <= 3:
        raise CannotShard("set METRO_NSH_WORKERS to 2 or 3 to shard")
    cache = app / ".nsh"
    if cache.is_symlink() or (cache.exists() and not cache.is_dir()):
        raise CannotShard("app cache is not a regular directory")
    if not (app / "App.tsx").is_file() or sum(
        path.name.lower() == "app.tsx" and not is_dir
        for path, is_dir in entries(app, ignored=(".nsh",))
    ) != 1:
        raise CannotShard("expected one app entry")
    config = json.loads((app / "nsh.json").read_text(encoding="utf-8"))
    if not isinstance(config, dict) or {"rust", "root"} & config.keys():
        raise CannotShard("the directory run may include additional Rust tests")
    files = discover(app)
    if len(files) < 2:
        raise CannotShard("fewer than two files can be sharded")
    if scratch.resolve().is_relative_to(app):
        raise CannotShard("worker scratch space must be outside the app")

    def ignore(directory, _names):
        # Copy source and fixtures, even directories excluded from discovery.
        # Validate all copied entries; only the root .nsh cache is disposable.
        ignored = (".nsh",) if Path(directory) == app else ()
        list(entries(Path(directory), ignored))
        return ignored

    contexts = []
    for index in range(min(workers, len(files))):
        worker = scratch / f"worker-{index}"
        copied = worker / "app"
        worker.mkdir(mode=0o700)
        shutil.copytree(app, copied, ignore=ignore)
        if discover(copied) != files:
            raise CannotShard("worker discovery differs from the original")
        # NSH's work dir is app/.nsh/test. The cloned app makes that private
        # too. Keep the CLI's installed tooling (PATH, NSH_TSC) available.
        home, tmp, junit = [worker / name for name in ("home", "tmp", "junit")]
        for path in (home, tmp, junit):
            path.mkdir(mode=0o700)
        worker_env = env | {"HOME": str(home), "TMPDIR": str(tmp), "TMP": str(tmp), "TEMP": str(tmp)}
        for var in ("XDG_CACHE_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME",
                    "XDG_RUNTIME_DIR", "XDG_STATE_HOME"):
            path = home / var.lower()
            path.mkdir(mode=0o700)
            worker_env[var] = str(path)
        contexts.append((copied, junit, worker_env))
    return schedule(app, files), contexts


def report_counts(report, file):
    root = ET.parse(report).getroot()
    counts = dict.fromkeys(RESULTS, 0)
    if root.tag != "testsuites" or root.get("name") != "nsh test" or len(root) > 1:
        raise ValueError("expected at most one NSH JUnit suite")
    if not len(root):
        # NSH runs files with no registered test cases, but omits their suite
        # entirely. This is only success with all-zero counts and exit 0.
        if any(root.get(key) != "0" for key in ("tests", "failures", "errors", "skipped")):
            raise ValueError("inconsistent empty JUnit report")
        return counts
    suite = root[0]
    if suite.tag != "testsuite" or suite.get("name") != file or not len(suite):
        raise ValueError("missing or wrong per-file JUnit suite")
    for case in suite:
        if case.tag != "testcase" or case.get("classname") != file or "name" not in case.attrib:
            raise ValueError("unexpected JUnit test case")
        if not len(case):
            counts["passed"] += 1
        elif len(case) != 1:
            raise ValueError("unexpected JUnit test result")
        elif case[0].tag == "skipped":
            counts["todo" if case[0].get("message") == "todo" else "skipped"] += 1
        elif case[0].tag in ("failure", "error"):
            counts["failed" if case[0].tag == "failure" else "errors"] += 1
        else:
            raise ValueError("unknown JUnit test result")
    expected = {"tests": sum(counts.values()), "failures": counts["failed"],
                "errors": counts["errors"], "skipped": counts["skipped"] + counts["todo"]}
    if any(element.get(key) != str(value) for element in (root, suite) for key, value in expected.items()):
        raise ValueError("inconsistent JUnit test counts")
    return counts


def run_file(nsh, file, report, app, env, export):
    counts = dict.fromkeys(RESULTS, 0)
    problems = []
    try:
        result = subprocess.run([nsh, "test", str(app / file), "--junit", str(report)],
                                cwd=app, env=env, stdin=subprocess.DEVNULL,
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if result.returncode:
            problems.append(f"NSH exited {result.returncode}")
    except OSError:
        problems.append("could not start NSH")
    try:
        counts = report_counts(report, file.as_posix())
        if counts["failed"] or counts["errors"]:
            problems.append("JUnit recorded failures")
        if export is not None:
            shutil.copyfile(report, export / report.name)
    except (ET.ParseError, OSError, ValueError):
        problems.append("missing, invalid or unwritable JUnit report")
    # Do not emit raw NSH output or JUnit failure text (screens/assertions).
    return file, counts, problems


def run_shards(nsh, files, contexts, export):
    queue = Queue()
    jobs = list(enumerate(files))
    for job in jobs[len(contexts):]:
        queue.put(job)

    def worker(app, junit, env, job):
        results = []
        while True:
            index, file = job
            results.append(run_file(nsh, file, junit / f"{index:03}.xml", app, env, export))
            try:
                job = queue.get_nowait()
            except Empty:
                return results

    totals = dict.fromkeys(RESULTS, 0)
    failures = 0
    print(f"NSH: {len(files)} files, {len(contexts)} isolated workers", flush=True)
    with ThreadPoolExecutor(max_workers=len(contexts)) as pool:
        futures = [pool.submit(worker, *ctx, job) for ctx, job in zip(contexts, jobs)]
        results = [result for future in futures for result in future.result()]
    for file, counts, problems in sorted(results):
        for key, value in counts.items():
            totals[key] += value
        if problems:
            failures += 1
            # repr escapes control characters; the prefix is not a CI command.
            print(f"NSH: {str(file)!r}: {', '.join(problems)}", file=sys.stderr)
    print(f"NSH: {len(results)}/{len(files)} files, {failures} unsuccessful; " +
          ", ".join(f"{value} {key}" for key, value in totals.items()))
    return int(bool(failures) or len(results) != len(files))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report-dir", type=Path, help="new directory for local parity reports")
    export = parser.parse_args().report_dir
    app = Path.cwd()
    nsh = shutil.which("nsh")
    if nsh is not None:
        nsh = os.path.abspath(nsh)
    env = dict(os.environ)
    with ExitStack() as stack:
        try:
            # Actions' scratch lives on the runner volume; self-hosted /tmp
            # is a small tmpfs and NSH writes substantial .nsh/test data.
            scratch = Path(stack.enter_context(tempfile.TemporaryDirectory(
                prefix="metro-nsh-", dir=env.get("RUNNER_TEMP") or None)))
            files, contexts = prepare(app, scratch, nsh, env)
        except (CannotShard, OSError, ValueError, subprocess.SubprocessError) as error:
            reason = str(error) if isinstance(error, CannotShard) else "worker preflight failed"
        else:
            # No fallback from here on: a shard/infrastructure failure fails CI.
            if export is not None:
                if export.resolve().is_relative_to(app):
                    parser.error("--report-dir must be outside the app")
                export.mkdir(mode=0o700)
            return run_shards(nsh, files, contexts, export)
    print(f"NSH: {reason}; running plain nsh test", flush=True)
    try:
        result = subprocess.run([nsh or "nsh", "test"], cwd=app, env=env).returncode
    except OSError:
        print("NSH: could not start plain nsh test", file=sys.stderr)
        return 1
    return result if result >= 0 else 128 - result


if __name__ == "__main__":
    raise SystemExit(main())
