"""Updates end to end: real metro-server and metro-runner, a local fake release
server, the dev test verifier.

The binaries must be e2e test builds (never a release build):

    CARGO_TARGET_DIR=target-e2e cargo build -p metro-server -p metro-runner \\
        --features metro-server/e2e-test-release,metro-runner/e2e-test-release
    python3 tests/e2e/updates.py --bin-dir target-e2e/debug --timeout 120

With METRO_TEST_RELEASE_BASE pointing at the loopback fixture server, such a
build counts as a release, fetches releases only from there and verifies them
with the test verifier (crypto is covered by crates/release's tests on the
real 0.4.0 bundle). A runner in a managed install takes its version from its
versions/<v> folder, so one binary plays 1.0.0, 1.0.1 and 1.0.2.

Covers: the server's verified check (latest, available) → the runner reports
its version and self-update → owner-only update request → the runner fetches,
verifies, start-checks, switches and execs 1.0.1 → the server sees it
updated → a 1.0.2 that crashes on start is rolled back after 3 failed starts
and reported as failed (start_failed).
"""

import gzip
import hashlib
from contextlib import ExitStack
from functools import partial
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
import os
from pathlib import Path
import secrets
import shutil
import signal
import subprocess
import tarfile
import threading
import time

from smoke import (
    HOST, ROOT, clean_env, main, remaining, request, require, server_port, start_child, stop_child,
    temp_state, terminate, wait_for_server,
)

RUNNERS = "/v1/runners"
COMMIT = "0123456789abcdef0123456789abcdef01234567"
OLD, GOOD, BAD = "1.0.0", "1.0.1", "1.0.2"
INSTALLERS = ("install-metro-linux.sh", "install-metro-runner-linux.sh", "install-metro-windows.ps1",
              "metro-runner-container.ps1", "metro-runner-container.sh")


def sha(data):
    return hashlib.sha256(data).hexdigest()


def test_bundle(data):
    """The dev TestVerifier's bundle format (metro_release::testing)."""
    return json.dumps({"metroTestBundle": 1, "sha256": sha(data), "commit": COMMIT,
                       "signedAt": 1800000000}).encode()


def runner_archive(version, binary):
    run = b'#!/bin/sh\nexec "$(dirname "$0")/metro-runner" "$@"\n'
    payload = {"metro-runner": (binary, 0o755), "run-runner": (run, 0o755)}
    release = json.dumps({"kind": "runner", "target": "linux-x64", "version": version,
                          "files": {n: sha(d) for n, (d, _) in payload.items()}}, indent=2).encode()
    top = f"Metro-runner-{version}-linux-x64"
    out = io.BytesIO()
    with gzip.GzipFile(fileobj=out, mode="wb", mtime=0) as gz:
        with tarfile.open(fileobj=gz, mode="w|", format=tarfile.USTAR_FORMAT) as tar:
            for name, (data, mode) in {"RELEASE.json": (release, 0o644), **payload}.items():
                info = tarfile.TarInfo(f"{top}/{name}")
                info.size, info.mode = len(data), mode
                tar.addfile(info, io.BytesIO(data))
    return out.getvalue()


def release_files(version, binary):
    """A complete release file set as SHA256SUMS lists it (placeholders for
    what the runner never downloads)."""
    files = {}
    binaries = [f"Metro-{version}-linux-x64.tar.gz", f"Metro-{version}-windows-x64.zip",
                f"Metro-{version}-windows-x64.unsigned-preview.msi", f"Metro-runner-{version}-linux-x64.tar.gz"]
    for name in binaries + list(INSTALLERS):
        files[name] = f"placeholder {name}\n".encode()
    files[binaries[3]] = runner_archive(version, binary)
    files["install-metro-runner-linux.sh"] = (ROOT / "packaging/install-metro-runner-linux.sh").read_bytes()
    for name in binaries:
        files[f"{name}.sha256"] = f"{sha(files[name])}  {name}\n".encode()
    d = lambda c: c * 64  # noqa: E731
    files["IMAGES.txt"] = (
        f"# Metro container images v{version}\n# Source commit: {COMMIT}\n\n"
        f"ghcr.io/wnzn/metro-runner:v{version}@sha256:{d('a')}  linux/amd64,linux/arm64\n"
        f"ghcr.io/wnzn/metro-runner@sha256:{d('b')}  linux/amd64\n"
        f"ghcr.io/wnzn/metro-runner@sha256:{d('c')}  linux/arm64\n\n"
        f"ghcr.io/wnzn/metro-server:v{version}@sha256:{d('d')}  linux/amd64,linux/arm64\n"
        f"ghcr.io/wnzn/metro-server@sha256:{d('e')}  linux/amd64\n"
        f"ghcr.io/wnzn/metro-server@sha256:{d('f')}  linux/arm64\n").encode()
    files["SHA256SUMS"] = "".join(f"{sha(files[n])}  {n}\n" for n in sorted(files)).encode()
    files["SHA256SUMS.sigstore.json"] = test_bundle(files["SHA256SUMS"])
    files[f"{binaries[3]}.sigstore.json"] = test_bundle(files[binaries[3]])
    return files


class ReleaseServer(ThreadingHTTPServer):
    """GitHub's release download URLs, for any number of versions."""

    def __init__(self, releases, latest):
        super().__init__((HOST, 0), ReleaseHandler)
        self.releases, self.latest, self.log = releases, latest, []


class ReleaseHandler(BaseHTTPRequestHandler):
    def do_GET(self):  # noqa: N802
        self.server.log.append(self.path)
        prefix = "/wnzn/metro/releases/"
        rest = self.path.removeprefix(prefix) if self.path.startswith(prefix) else None
        body = None
        if rest and rest.startswith("latest/download/"):
            body = self.server.releases[self.server.latest].get(rest.removeprefix("latest/download/"))
        elif rest and rest.startswith("download/v"):
            version, _, name = rest.removeprefix("download/v").partition("/")
            body = self.server.releases.get(version, {}).get(name)
        self.send_response(200 if body is not None else 404)
        body = body if body is not None else b"not found"
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


class NoModel:
    error = None


class Supervisor:
    """Restarts `<root>/current/metro-runner` when it exits, like systemd's
    Restart=on-failure; counts the restarts."""

    def __init__(self, root, env):
        self.root, self.env, self.restarts, self.stopped = root, env, 0, False
        self.child = None
        self.lock = threading.Lock()
        self.thread = threading.Thread(target=self.loop, daemon=True)

    def start_one(self):
        # Child diagnostics are not kept (they could include a token).
        return subprocess.Popen([str(self.root / "current" / "metro-runner")], cwd=self.root, env=self.env,
                                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def loop(self):
        while not self.stopped:
            with self.lock:
                if self.stopped:
                    return
                self.child = self.start_one()
            self.child.wait()
            if self.stopped:
                return
            self.restarts += 1
            time.sleep(0.3)

    def stop(self):
        with self.lock:
            self.stopped = True
            if self.child is not None:
                stop_child(self.child)
        self.thread.join(5)


def wait_until(deadline, what, probe):
    last = None
    while True:
        last = probe()
        if last:
            return last
        require(remaining(deadline) > 0.2, f"timed out waiting for {what}")
        time.sleep(0.2)


def updates(server, runner, timeout):
    alice, bob = secrets.token_hex(24), secrets.token_hex(24)
    binary = runner.read_bytes()
    releases = {GOOD: release_files(GOOD, binary), BAD: release_files(BAD, binary)}
    with temp_state() as directory, ExitStack() as stack:
        state = Path(directory)
        deadline = time.monotonic() + timeout
        fake = ReleaseServer(releases, GOOD)
        thread = threading.Thread(target=fake.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
        thread.start()
        stack.callback(fake.server_close)
        stack.callback(fake.shutdown)
        base = f"http://{HOST}:{fake.server_port}"

        env = clean_env(directory)
        port = server_port()
        children = [start_child(stack, "server", server, state, env | {
            "METRO_BIND": f"{HOST}:{port}",
            "METRO_DB": str(state / "metro.sqlite"),
            "METRO_DEV_USERS": json.dumps({"alice": alice, "bob": bob}),
            "METRO_ORG_ADMINS": json.dumps(["alice"]),
            "METRO_TEST_RELEASE_BASE": base,
            "METRO_TEST_RELEASE_CHECK_SECS": "1",
        })]
        wait_for_server(port, children, NoModel(), deadline)
        call = partial(request, port, deadline)

        # 1. The server's verified check: latest and available.
        def server_view():
            v = call(alice, "GET", "/v1/server/update", read_json=True)
            require(v["channel"] == "release", "these binaries are not e2e test builds "
                    "(build with --features metro-server/e2e-test-release,metro-runner/e2e-test-release)")
            return v if (v.get("latest") or {}).get("version") == GOOD else None
        view = wait_until(deadline, "the server's release check", server_view)
        require(view["available"] is True and view["result"] == "ok", "server check did not find the update")
        call(bob, "GET", "/v1/server/update", expect=(404,), label="non-admin server notice")

        # 2. A managed runner install of 1.0.0 (the scripts' layout) with self-update on.
        created = call(alice, "POST", RUNNERS, body={"name": "updating runner", "visibility": "personal"},
                       read_json=True)
        rid, token = created["runner"]["id"], created["token"]
        root = state / "metro-runner"
        (root / "versions" / OLD).mkdir(parents=True)
        for path in (root, root / "versions", root / "versions" / OLD):
            path.chmod(0o700)
        installed_runner = root / "versions" / OLD / "metro-runner"
        shutil.copy2(runner, installed_runner)
        installed_runner.chmod(0o755)
        (root / ".metro-runner-install").write_text("metro-update-lock-v2\n")
        shutil.copy2(ROOT / "packaging/install-metro-runner-linux.sh", root / "install-metro-runner-linux.sh")
        os.symlink(f"versions/{OLD}", root / "current")
        workspace = state / "workspace"
        workspace.mkdir()
        runner_env = env | {
            "METRO_RUNNER_TOKEN": token,
            "METRO_SERVER_URL": f"http://{HOST}:{port}",
            "METRO_WORKSPACE": str(workspace),
            "METRO_POLL_INTERVAL_MS": "200",
            "METRO_RUNNER_SELF_UPDATE": "1",
            "METRO_TEST_RELEASE_BASE": base,
            "METRO_TEST_CRASH_VERSION": BAD,
        }
        def runner_view():
            for r in call(alice, "GET", RUNNERS, read_json=True)["runners"]:
                if r["id"] == rid:
                    return r
            raise RuntimeError("runner disappeared")

        # A real runner with rollback due and a missing previous executable
        # must persist intent and exit before sending even its first claim.
        pending = root / ".update-pending.json"
        for raw, mode in [(b"{bad journal", 0o600), (b"unprotected journal", 0o666)]:
            pending.write_bytes(raw)
            pending.chmod(mode)
            failed = subprocess.run([root / "current" / "metro-runner"], env=runner_env,
                                    capture_output=True, text=True, timeout=remaining(deadline))
            require(failed.returncode == 75 and "unreadable or malformed" in failed.stderr,
                    "runner did not fail closed on a damaged pending record")
            require(pending.read_bytes() == raw, "runner modified a damaged pending record")
            require(runner_view()["presence"]["seen_at"] is None, "the failed runner sent a claim")
            pending.unlink()
        pending.write_text(json.dumps({"version": 1, "from": "0.9.0", "to": OLD,
                                       "attempts": 3, "rolling_back": False}))
        pending.chmod(0o600)
        failed = subprocess.run([root / "current" / "metro-runner"], env=runner_env,
                                capture_output=True, text=True, timeout=remaining(deadline))
        require(failed.returncode == 75 and "exiting before claiming work" in failed.stderr,
                f"runner did not exit on rollback preparation failure: {failed.returncode}")
        require(json.loads(pending.read_text())["rolling_back"], "rollback intent wasn't saved")
        require(runner_view()["presence"]["seen_at"] is None, "the failed runner sent a claim")
        require(os.readlink(root / "current") == f"versions/{OLD}", "failed rollback moved current")
        pending.unlink()

        supervisor = Supervisor(root, runner_env)
        supervisor.thread.start()
        stack.callback(supervisor.stop)

        r = wait_until(deadline, "the runner's presence", lambda: (v := runner_view())["presence"]["version"] and v)
        require(r["presence"]["version"] == OLD and r["presence"]["install"] == "managed"
                and r["presence"]["self_update"] is True, f"unexpected presence {r['presence']}")
        require(r["update"]["available"] is True and r["update"]["action"] == "self_update"
                and r["update"]["latest"] == GOOD, f"unexpected update view {r['update']}")

        # 3. Only its owner may ask, and only for the verified latest.
        path = f"{RUNNERS}/{rid}/update"
        call(bob, "POST", path, body={"version": GOOD}, expect=(404,), label="another person's runner")
        refused = call(alice, "POST", path, body={"version": BAD}, expect=(409,), read_json=True)
        require(refused["code"] == "not_latest", "requesting a non-latest version was not refused")
        before = len(fake.log)
        call(alice, "POST", path, body={"version": GOOD}, expect=(202,))

        # 4. The runner fetches, verifies, start-checks, switches and execs 1.0.1.
        r = wait_until(deadline, "the runner to update", lambda: (v := runner_view())["presence"]["version"] == GOOD and v)
        r = wait_until(deadline, "the update to settle",
                       lambda: (v := runner_view())["update"]["last_result"] and v)
        require(r["update"]["last_result"]["state"] == "updated" and r["update"]["pending"] is None,
                f"unexpected result {r['update']}")
        require(os.readlink(root / "current") == f"versions/{GOOD}", "current does not name the new version")
        wait_until(deadline, "the update to be confirmed", lambda: not (root / ".update-pending.json").exists())
        require(sorted(p.name for p in (root / "versions").iterdir()) == [OLD, GOOD], "unexpected versions kept")
        require((root / "install-metro-runner-linux.sh").read_bytes() == releases[GOOD]["install-metro-runner-linux.sh"],
                "install script not replaced by the verified one")
        runner_requests = [p for p in fake.log[before:] if f"/download/v{GOOD}/" in p]
        for name in ("SHA256SUMS", "SHA256SUMS.sigstore.json", f"Metro-runner-{GOOD}-linux-x64.tar.gz",
                     f"Metro-runner-{GOOD}-linux-x64.tar.gz.sigstore.json", "install-metro-runner-linux.sh"):
            require(any(p.endswith("/" + name) for p in runner_requests), f"runner did not fetch {name}")
        require(supervisor.restarts == 0, "the runner restarted instead of exec'ing in place")

        # 5. A 1.0.2 that crashes on start is rolled back after 3 failed starts.
        fake.latest = BAD
        wait_until(deadline, "the server to see 1.0.2",
                   lambda: (call(alice, "GET", "/v1/server/update", read_json=True).get("latest") or {}).get("version") == BAD)
        wait_until(deadline, "the runner to be offered 1.0.2", lambda: runner_view()["update"]["available"])
        call(alice, "POST", path, body={"version": BAD}, expect=(202,))
        r = wait_until(deadline, "the failed update to be reported",
                       lambda: (v := runner_view())["update"]["last_result"]["target"] == BAD and v)
        result = r["update"]["last_result"]
        require(result["state"] == "failed" and result["code"] == "start_failed", f"unexpected result {result}")
        require(r["presence"]["version"] == GOOD, "the runner is not back on 1.0.1")
        require(os.readlink(root / "current") == f"versions/{GOOD}", "current was not switched back")
        require(supervisor.restarts >= 3, f"expected 3 failed starts, saw {supervisor.restarts} restarts")
        restarts = supervisor.restarts
        time.sleep(1.5)
        require(supervisor.restarts == restarts, "the rolled-back runner keeps restarting")
        require(not (root / ".update-pending.json").exists(), "a pending update was left behind")
        for child, label in children:
            require(child.poll() is None, f"{label} exited")
    print(f"PASS: verified check and notice; owner-only request; damaged pending journals and failed rollback preparation sent no claim; "
          f"runner self-updated {OLD} → {GOOD} in place; "
          f"crashing {BAD} rolled back after {restarts} restarts and reported start_failed.")


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, terminate)
    raise SystemExit(main(run=updates, description=__doc__))
