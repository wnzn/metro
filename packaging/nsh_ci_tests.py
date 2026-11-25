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
import signal
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
    "src/device/Device.test.tsx": 37,
    "src/features/library/Library.test.tsx": 26,
}
RESULTS = ("passed", "failed", "errors", "skipped", "todo")
FILE_TIMEOUT_SECONDS = 600


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


def check_counts(element, counts):
    expected = {"tests": sum(counts.values()), "failures": counts["failed"],
                "errors": counts["errors"], "skipped": counts["skipped"] + counts["todo"]}
    if any(element.get(key) != str(value) for key, value in expected.items()):
        raise ValueError("inconsistent JUnit test counts")


def suite_counts(suite, file):
    counts = dict.fromkeys(RESULTS, 0)
    failed_cases = []
    if suite.tag != "testsuite" or not file or not len(suite):
        raise ValueError("invalid NSH JUnit suite")
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
            failed_cases.append(case.attrib["name"])
        else:
            raise ValueError("unknown JUnit test result")
    check_counts(suite, counts)
    return counts, failed_cases


def report_counts(report, file=None):
    root = ET.parse(report).getroot()
    if root.tag != "testsuites" or root.get("name") != "nsh test":
        raise ValueError("invalid NSH JUnit report")
    if file is not None and (len(root) > 1 or (len(root) and root[0].get("name") != file)):
        raise ValueError("wrong per-file JUnit suite")
    counts = dict.fromkeys(RESULTS, 0)
    failed_cases = []
    # NSH omits suites with no registered cases. An empty report is only
    # valid with all-zero counts.
    for suite in root:
        source = suite.get("name")
        suite_totals, names = suite_counts(suite, source)
        for key, count in suite_totals.items():
            counts[key] += count
        failed_cases.extend((source, name) for name in names)
    check_counts(root, counts)
    return counts, failed_cases


def run_tests(nsh, report, app, env, *, file=None, export=None):
    counts = dict.fromkeys(RESULTS, 0)
    problems = []
    failed_cases = []
    status = 1
    command = [nsh, "test"]
    if file is not None:
        command.append(str(app / file))
    try:
        with subprocess.Popen([*command, "--junit", str(report)],
                              cwd=app, env=env, stdin=subprocess.DEVNULL,
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                              start_new_session=True) as process:
            try:
                status = process.wait(timeout=FILE_TIMEOUT_SECONDS)
            except subprocess.TimeoutExpired:
                status = 124
                problems.append(f"NSH timed out after {FILE_TIMEOUT_SECONDS} seconds")
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass  # The process group exited as the timeout fired.
            else:
                if status:
                    problems.append(f"NSH exited {status}")
    except OSError:
        problems.append("could not run NSH")
    try:
        counts, failed_cases = report_counts(report, file.as_posix() if file is not None else None)
        if failed_cases:
            problems.append("JUnit recorded failures")
    except (ET.ParseError, LookupError, OSError, ValueError):
        problems.append("missing or invalid JUnit report")
    else:
        if export is not None:
            try:
                shutil.copyfile(report, export / report.name)
            except OSError:
                problems.append("could not export JUnit report")
    # Do not emit raw NSH output or JUnit failure text (screens/assertions).
    return status if status >= 0 else 128 - status, counts, problems, failed_cases


def log_label(value):
    # Repr escapes line breaks. The NSH prefix blocks `::` commands, but the
    # runner also recognizes legacy `##[` commands anywhere on a line.
    # Escape percent-encoded command syntax such as `%0A` as well.
    return repr(str(value)).replace("##[", r"\x23#[").replace("%", r"\x25")


def print_problems(label, problems, failed_cases):
    for problem in problems:
        print(f"NSH: {label}: {problem}", file=sys.stderr)
    for file, name in failed_cases:
        print(f"NSH: {log_label(file)} › {log_label(name)}: failure", file=sys.stderr)


def counts_text(counts):
    return ", ".join(f"{value} {key}" for key, value in counts.items())


def run_shards(nsh, files, contexts, export):
    queue = Queue()
    jobs = list(enumerate(files))
    for job in jobs[len(contexts):]:
        queue.put(job)

    def worker(app, junit, env, job):
        results = []
        while True:
            index, file = job
            _, counts, problems, failed_cases = run_tests(
                nsh, junit / f"{index:03}.xml", app, env, file=file, export=export)
            results.append((file, counts, problems, failed_cases))
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
    for file, counts, problems, failed_cases in sorted(results):
        for key, value in counts.items():
            totals[key] += value
        if problems:
            failures += 1
            print_problems(log_label(file), problems, failed_cases)
    print(f"NSH: {len(results)}/{len(files)} files, {failures} unsuccessful; {counts_text(totals)}")
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
        with tempfile.TemporaryDirectory(prefix="metro-nsh-fallback-") as scratch:
            status, counts, problems, failed_cases = run_tests(nsh or "nsh", Path(scratch) / "unfiltered.xml", app, env)
    except OSError:
        print("NSH: unfiltered: could not prepare JUnit report", file=sys.stderr)
        return 1
    print_problems("unfiltered", problems, failed_cases)
    print(f"NSH: unfiltered; {counts_text(counts)}")
    return status or int(bool(problems))


if __name__ == "__main__":
    raise SystemExit(main())
