#!/usr/bin/env python3
"""Integrated smoke test of the exact Metro server and runner images.

Runs the images as they ship (non-root, read-only root file system, all
capabilities dropped, no-new-privileges, bounded pids/memory/CPU) on a fresh
internal Docker network with no published ports and only throwaway volumes:

  * the server answers /healthz and refuses anonymous and unknown bearers (401);
  * a dedicated runner (Space token) and a registered personal runner
    (metro_rt_v2_ token, attached to two Spaces) start from mode-600 token
    files, claim real Demo jobs and report results; each lists only its own
    Space's files, and the registered runner creates spaces/<sid>/ itself;
  * outsiders get 404; no token appears in any container's configuration or
    log.

Synthetic tokens are generated here and passed to containers on stdin or in
mode-600 files inside volumes, never in argv or the environment, and are never
printed. Everything this run creates carries a unique label and is removed at
the end. The in-network client is packaging/image_smoke_driver.py, run in a
pinned python image.

  python3 -I packaging/image_smoke.py --server-image IMG --runner-image IMG --platform linux/amd64
"""

import argparse
import json
from pathlib import Path
import secrets
import subprocess
import sys
import time
import uuid

DRIVER_IMAGE = ("python:3.13-slim-bookworm@sha256:"
                "a5a7017efc5969e00ae2b91c8eb0631049a569652aa4ffc4501769049ae45c72")
DRIVER = Path(__file__).resolve().with_name("image_smoke_driver.py")
RUNNER_START = ('IFS= read -r METRO_RUNNER_TOKEN < /run/secrets/token || [ -n "$METRO_RUNNER_TOKEN" ]; '
                'export METRO_RUNNER_TOKEN; exec /usr/local/bin/metro-runner')


class Fail(Exception):
    pass


def require(cond, message):
    if not cond:
        raise Fail(message)


class Smoke:
    def __init__(self, args):
        self.args = args
        self.run_id = uuid.uuid4().hex[:10]
        self.prefix = f"metro-imgsmoke-{self.run_id}"
        self.label = f"dev.metro.image-smoke={self.run_id}"
        self.secrets = []
        self.containers, self.volumes, self.network = [], [], None
        self.deadline = time.monotonic() + args.timeout

    # -------------------------------------------------------------- docker

    def redact(self, text):
        for s in self.secrets:
            text = text.replace(s, "[redacted]")
        return text

    def docker(self, *argv, stdin=None, timeout=120, check=True):
        left = self.deadline - time.monotonic()
        require(left > 0, "smoke deadline expired")
        try:
            proc = subprocess.run(["docker", *argv], input=stdin, capture_output=True, text=True,
                                  timeout=min(timeout, left))
        except subprocess.TimeoutExpired:
            raise Fail(f"docker {argv[0]} timed out") from None
        if check and proc.returncode != 0:
            # Only the subcommand is named; stderr is redacted and trimmed.
            raise Fail(f"docker {argv[0]} failed: {self.redact(proc.stderr.strip())[-800:]}")
        return proc

    def hardening(self, pids=64, memory="256m"):
        return ["--label", self.label, "--cap-drop", "ALL", "--security-opt", "no-new-privileges:true",
                "--pids-limit", str(pids), "--memory", memory, "--memory-swap", memory, "--cpus", "1"]

    def volume(self, role):
        name = f"{self.prefix}-{role}"
        self.docker("volume", "create", "--label", self.label, name)
        self.volumes.append(name)
        return name

    # -------------------------------------------------------------- checks

    def image_facts(self, image):
        out = self.docker("image", "inspect", "--format",
                          "{{json .Config.User}} {{.Os}}/{{.Architecture}}", image).stdout.split()
        user, platform = json.loads(out[0]), out[1]
        require(platform == self.args.platform, f"{image}: platform {platform}, expected {self.args.platform}")
        require(user.split(":")[0] not in ("", "0", "root"), f"{image}: runs as root")
        return user

    def run(self):
        a = self.args
        server_user = self.image_facts(a.server_image)
        runner_user = self.image_facts(a.runner_image)
        require(server_user == "10001:10001", f"server image user is {server_user!r}")
        if self.docker("image", "inspect", a.driver_image, check=False).returncode != 0:
            self.docker("pull", "--quiet", a.driver_image, timeout=300)

        users = {"alice": secrets.token_hex(24), "bob": secrets.token_hex(24)}
        self.secrets += list(users.values())

        self.network = f"{self.prefix}-net"
        self.docker("network", "create", "--internal", "--label", self.label, self.network)
        data, users_vol, driver_secrets = (self.volume(r) for r in ("data", "users", "driver-secrets"))
        tok_ded, tok_reg, ws_ded, ws_reg = (self.volume(r) for r in ("tok-ded", "tok-reg", "ws-ded", "ws-reg"))
        offline = ["--rm", "--network", "none", *self.hardening()]

        # Synthetic user tokens, on stdin only. A fresh volume mounted on the
        # image's own /data (server) or /tmp (driver) takes that directory's owner.
        self.docker("run", "-i", *offline, "-v", f"{users_vol}:/data", "--entrypoint", "sh",
                    a.server_image, "-c", "umask 077 && cat > /data/users.json", stdin=json.dumps(users))
        self.docker("run", "-i", *offline, "--user", runner_user, "-v", f"{driver_secrets}:/tmp",
                    "--entrypoint", "sh", a.driver_image, "-c", "umask 077 && cat > /tmp/users.json",
                    stdin=json.dumps(users))
        for vol in (tok_ded, tok_reg):
            self.docker("run", *offline, "-v", f"{vol}:/tmp", "--entrypoint", "true", a.driver_image)

        server = self.start("server", a.server_image, [
            "--network-alias", "server", "-v", f"{data}:/data", "-v", f"{users_vol}:/run/metro:ro",
            "-e", "METRO_DEV_USERS_FILE=/run/metro/users.json",
        ], memory="512m", pids=128)

        driver = ["--rm", "--network", self.network, *self.hardening(), "--read-only", "--user", runner_user,
                  "--tmpfs", "/tmp:rw,noexec,nosuid,nodev,size=16m",
                  "-v", f"{driver_secrets}:/secrets:ro", "-v", f"{tok_ded}:/out/dedicated",
                  "-v", f"{tok_reg}:/out/registered",
                  "--mount", f"type=bind,src={DRIVER},dst=/smoke/driver.py,readonly",
                  "-e", f"SMOKE_TIMEOUT={max(10, int(self.deadline - time.monotonic()) - 5)}",
                  "--entrypoint", "python", a.driver_image, "-I", "-B", "/smoke/driver.py"]
        plan = json.loads(self.driver(driver, "setup").strip().splitlines()[-1])
        dedicated, (reg_marked, reg_empty) = plan["dedicated"], plan["registered"]

        for vol in (tok_ded, tok_reg):
            token = self.docker("run", *offline, "--user", runner_user, "-v", f"{vol}:/t:ro",
                                "--entrypoint", "cat", a.runner_image, "/t/token").stdout.strip()
            require(token, "a runner token file is empty")
            self.secrets.append(token)
        require(self.secrets[-1].startswith("metro_rt_v2_"), "the registered runner token has the wrong form")

        markers = {dedicated: f"dedicated-{uuid.uuid4().hex}.txt",
                   reg_marked: f"registered-{uuid.uuid4().hex}.txt", reg_empty: None}
        self.docker("run", *offline, "-v", f"{ws_ded}:/workspace", "--entrypoint", "sh", a.runner_image,
                    "-c", ': > "/workspace/$1"', "sh", markers[dedicated])
        self.docker("run", *offline, "-v", f"{ws_reg}:/workspace", "--entrypoint", "sh", a.runner_image,
                    "-c", 'mkdir -p "/workspace/spaces/$1" && : > "/workspace/spaces/$1/$2"',
                    "sh", reg_marked, markers[reg_marked])

        # Plain HTTP needs no opt-in (METRO_ALLOW_INSECURE_HTTP is a no-op now).
        runner_env = ["-e", "METRO_SERVER_URL=http://server:8787",
                      "-e", "METRO_WORKSPACE=/workspace", "--entrypoint", "sh"]
        runners = [
            self.start("runner-dedicated", a.runner_image,
                       ["-v", f"{tok_ded}:/run/secrets:ro", "-v", f"{ws_ded}:/workspace", *runner_env],
                       command=["-c", RUNNER_START]),
            self.start("runner-registered", a.runner_image,
                       ["-v", f"{tok_reg}:/run/secrets:ro", "-v", f"{ws_reg}:/workspace", *runner_env],
                       command=["-c", RUNNER_START]),
        ]
        plan["markers"] = markers
        listed = json.loads(self.driver(driver, "demo", json.dumps(plan)).strip().splitlines()[-1])["listed"]
        require(sorted(listed) == sorted(markers), "not every Space's Demo completed")

        for name, want in ((server, "10001"), (runners[0], runner_user.split(":")[0]),
                           (runners[1], runner_user.split(":")[0])):
            state = self.docker("inspect", "--format", "{{.State.Running}} {{.HostConfig.ReadonlyRootfs}} "
                                "{{json .HostConfig.PortBindings}} {{.HostConfig.NetworkMode}}", name).stdout.split()
            require(state == ["true", "true", "{}", self.network], f"{name}: unexpected container state")
            uid = self.docker("exec", name, "id", "-u").stdout.strip()
            require(uid == want and uid != "0", f"{name}: process uid {uid}, expected {want}")

        tree = self.docker("run", *offline, "-v", f"{ws_reg}:/workspace:ro", "--entrypoint", "sh",
                           a.runner_image, "-c",
                           'cd /workspace && find . -mindepth 1 -maxdepth 3 -exec stat -c "%u %F %n" {} + | sort',
                           ).stdout.split("\n")
        tree = [line for line in tree if line]
        uid = runner_user.split(":")[0]
        want = sorted([f"{uid} directory ./spaces", f"{uid} directory ./spaces/{reg_marked}",
                       f"{uid} directory ./spaces/{reg_empty}",
                       f"{uid} regular empty file ./spaces/{reg_marked}/{markers[reg_marked]}"])
        require(tree == want, "the registered runner's volume is not exactly spaces/<sid>/ per attached Space")

        for name in self.containers:
            blob = (self.docker("inspect", name).stdout + self.docker("logs", name).stdout
                    + self.docker("logs", name).stderr)
            require(not any(s in blob for s in self.secrets), f"{name}: a token is visible in inspect or logs")
        return {
            "result": "PASS", "platform": a.platform,
            "server_image": a.server_image, "runner_image": a.runner_image,
            "checks": ["healthz", "401 anonymous/unknown bearer/unknown runner", "404 outsiders",
                       "dedicated runner claim+result", "registered runner claim+result in 2 Spaces",
                       "spaces/<sid> created by runner", "non-root uids", "read-only rootfs",
                       "no published ports", "no token in inspect or logs"],
        }

    def start(self, role, image, opts, command=(), memory="256m", pids=64):
        name = f"{self.prefix}-{role}"
        self.containers.append(name)
        self.docker("run", "-d", "--name", name, "--network", self.network, *self.hardening(pids, memory),
                    "--read-only", "--tmpfs", "/tmp:rw,noexec,nosuid,nodev,size=16m", *opts, image, *command)
        return name

    def driver(self, base, *phase):
        proc = self.docker("run", *base, *phase, check=False, timeout=300)
        if proc.returncode != 0:
            raise Fail(f"driver {phase[0]}: {self.redact(proc.stderr.strip())[-800:]}")
        return proc.stdout

    def diagnostics(self):
        for name in self.containers:
            proc = self.docker("logs", "--tail", "40", name, check=False, timeout=30) if self.time_left() else None
            if proc is not None:
                text = self.redact(proc.stdout + proc.stderr).strip()
                print(f"--- {name} (last lines, tokens redacted)\n{text}", file=sys.stderr)

    def time_left(self):
        return self.deadline - time.monotonic() > 1

    def cleanup(self):
        # Independent of the deadline: always remove what this run created.
        def quiet(*argv):
            subprocess.run(["docker", *argv], capture_output=True, timeout=60)
        if self.containers:
            quiet("rm", "-f", "-v", *self.containers)
        if self.network:
            quiet("network", "rm", self.network)
        if self.volumes:
            quiet("volume", "rm", "-f", *self.volumes)


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--server-image", required=True)
    p.add_argument("--runner-image", required=True)
    p.add_argument("--driver-image", default=DRIVER_IMAGE)
    p.add_argument("--platform", required=True, help="expected platform, e.g. linux/amd64")
    p.add_argument("--timeout", type=float, default=240)
    p.add_argument("--result", help="write the PASS summary JSON here")
    args = p.parse_args(argv)
    smoke = Smoke(args)
    try:
        summary = smoke.run()
    except (Fail, ValueError, KeyError, IndexError) as exc:
        print(f"FAIL: {smoke.redact(str(exc))}", file=sys.stderr)
        try:
            smoke.diagnostics()
        except (Fail, subprocess.SubprocessError, OSError):
            pass
        return 1
    finally:
        smoke.cleanup()
    text = json.dumps(summary, indent=2)
    if args.result:
        Path(args.result).write_text(text + "\n")
    print(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
