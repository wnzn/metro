"""Build and probe the real runner Compose stack on a disposable hosted VM.

The only Compose overrides are resource names. This is not an enrollment test:
neither the host helper nor the image's registration command is invoked.
"""

import base64
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import tempfile
import threading
import urllib.request
import uuid


ROOT = Path(__file__).resolve().parent
DOCKER = ("docker", "--context", "default")
FAKE_HEADER = "preflight:not-a-credential"
PROXY = "http://egress:3128"
PUBLIC_HOST = "github.com"
PUBLIC_GIT = f"https://{PUBLIC_HOST}/wnzn/metro.git/info/refs?service=git-upload-pack"
SENSITIVE = re.compile(r"(?:TOKEN|SECRET|PASSWORD|CREDENTIAL|AUTHORIZATION|PRIVATE_KEY)", re.I)


def require(condition, problem):
    if not condition:
        raise RuntimeError(problem)


def run(*argv, capture=False, stdin=None):
    return subprocess.run(argv, check=True, text=True, stdout=subprocess.PIPE if capture else None,
                          timeout=900, input=stdin).stdout


def read_json(*argv):
    return json.loads(run(*argv, capture=True))


def inspect(*names):
    return read_json(*DOCKER, "inspect", *names)


def check_mounts(container, service, project):
    host = container["HostConfig"]
    tmpfs = {"/tmp", "/home/runner"} if service == "runner" else {"/tmp"}
    allowed = {"/state", "/runner"} if service == "runner" else set()
    require(set(host.get("Tmpfs") or {}) <= tmpfs and not host.get("VolumesFrom"),
            f"{service} has unexpected tmpfs or inherited mounts")
    mounts = {}
    for mount in container["Mounts"] or []:
        target = mount["Destination"]
        require(target not in mounts, f"{service} has overlapping mount definitions")
        mounts[target] = mount
        # Legacy HostConfig.Tmpfs entries need not appear here.
        require((mount["Type"] == "tmpfs" and target in tmpfs)
                or (mount["Type"] == "volume" and target in allowed),
                f"{service} has an unexpected effective mount; host paths and Docker sockets are forbidden")
    volumes = {target: mount for target, mount in mounts.items() if mount["Type"] == "volume"}
    require(set(volumes) == allowed, f"{service} does not have the expected Docker volumes")

    # Compose 2.38.2 sends named local volumes as NAME:TARGET:rw in
    # HostConfig.Binds. Allow only an exact match to an effective volume.
    # Newer Compose may use the typed HostConfig.Mounts API instead.
    bind_entries = {f'{m["Name"]}:{target}:rw' for target, m in volumes.items()}
    require(set(host.get("Binds") or []) <= bind_entries, f"{service} has an unexpected legacy bind entry")
    for mount in host.get("Mounts") or []:
        target = mount["Target"]
        kind = mount["Type"]
        source = mount.get("Source", "")
        require((kind == "tmpfs" and target in tmpfs and not source)
                or (kind == "volume" and target in volumes
                    and (source == volumes[target]["Name"] or (target == "/runner" and not source))),
                f"{service} has an unexpected structured mount")
    if service != "runner":
        return

    state_name = f"{project}-state"
    require(volumes["/state"]["Name"] == state_name, "runner is not using the test state volume")
    require(re.fullmatch(r"[a-f0-9]{64}", volumes["/runner"]["Name"]),
            "runner scratch storage is not an anonymous Docker volume")
    for target, mount in volumes.items():
        require(mount["Driver"] == "local" and mount["RW"] is True, f"{target} must be a writable local volume")
        # Even Driver=local can bind a host directory via driver_opts
        # (type=none,o=bind,device=/host/path). Inspect the actual volume.
        volume, = read_json(*DOCKER, "volume", "inspect", mount["Name"])
        require(volume["Name"] == mount["Name"] and volume["Mountpoint"] == mount["Source"]
                and volume["Driver"] == "local" and volume["Scope"] == "local" and not volume["Options"],
                f"{target} is not a plain Docker-managed local volume; driver options and host backing are forbidden")
        if target == "/state":
            labels = volume["Labels"] or {}
            require(labels.get("com.docker.compose.project") == project
                    and labels.get("com.docker.compose.volume") == "state",
                    "runner state volume does not belong to this test project")


def check_containers(containers, project):
    for service, uid in (("runner", "1001"), ("egress", "65532")):
        container = containers[service]
        cfg, host = container["Config"], container["HostConfig"]
        require(cfg["User"] == f"{uid}:{uid}", f"{service} must run non-root")
        require(host["ReadonlyRootfs"] and not host["Privileged"], f"{service} has excess host privileges")
        require("ALL" in host["CapDrop"] and "no-new-privileges:true" in host["SecurityOpt"],
                f"{service} has lost containment options")
        net = container["NetworkSettings"]
        # Engine reports exposed but unpublished ports as e.g. {"3128/tcp": null}.
        require(not host["PortBindings"] and not host["PublishAllPorts"] and not any((net.get("Ports") or {}).values()),
                f"{service} must not publish ports")
        require(not host.get("CapAdd") and not host.get("Devices") and not host.get("DeviceRequests")
                and host.get("PidMode") != "host" and host.get("IpcMode") != "host",
                f"{service} must not access host devices or share host namespaces")
        require(not any(SENSITIVE.search(var.partition("=")[0]) for var in cfg["Env"] or []),
                f"{service} has a credential-like environment variable")
        check_mounts(container, service, project)
        networks = {f"{project}-jobs"} | ({f"{project}-out"} if service == "egress" else set())
        require(set(net["Networks"] or {}) == networks and host["NetworkMode"] in networks,
                f"{service} is attached to an unexpected network")


class HostFixture(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(204 if self.path == "/preflight" else 404)
        self.end_headers()

    def log_message(self, *_args):
        pass


def network_checks(compose, project):
    def exec_runner(*argv, **kw):
        return run(*compose, "exec", "-T", "runner", *argv, **kw)

    # -q as the first curl option ignores a curlrc. Empty --noproxy forces the
    # tunnel even if a caller accidentally changes NO_PROXY.
    curl = ("curl", "-q", "--fail", "--silent", "--show-error", "--connect-timeout", "8", "--max-time", "20")
    got = exec_runner(*curl, "--noproxy", "", "--proxy", PROXY, "--retry", "2", PUBLIC_GIT, capture=True)
    require(got.startswith("001e# service=git-upload-pack\n"), "proxy did not return GitHub's public Git advertisement")

    encoded = base64.b64encode(FAKE_HEADER.encode()).decode()
    denied = exec_runner("python3", "-I", "-c", """
import socket
import sys
with socket.create_connection(("egress", 3128), 5) as conn:
    conn.settimeout(5)
    conn.sendall(sys.stdin.buffer.read())
    print(conn.makefile("rb").readline(256).decode().strip())
""", stdin=("CONNECT example.invalid:443 HTTP/1.1\r\nHost: example.invalid:443\r\n"
            f"Proxy-Authorization: Basic {encoded}\r\n\r\n"), capture=True)
    require(re.match(r"^HTTP/1\.[01] 403(?: |$)", denied), "unauthorized CONNECT was not denied")
    logs = run(*compose, "logs", "--no-color", capture=True)
    require(encoded not in logs and FAKE_HEADER not in logs, "container logs included the synthetic header")

    # Resolve a known internet destination on the host. Numeric direct TCP
    # probes in the runner bypass DNS and all proxy variables.
    public_ip = socket.getaddrinfo(PUBLIC_HOST, 443, socket.AF_INET, socket.SOCK_STREAM)[0][4][0]
    with socket.create_connection((public_ip, 443), 8):
        pass  # positive control for the exact external socket
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as route:
        route.connect((public_ip, 443))  # select this VM's source address without sending a packet
        host_ip = route.getsockname()[0]

    fixture = f"{project}-lan"
    with ThreadingHTTPServer((host_ip, 0), HostFixture) as host_fixture:
        thread = threading.Thread(target=host_fixture.serve_forever, daemon=True)
        thread.start()
        host_port = host_fixture.server_address[1]
        try:
            opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
            with opener.open(f"http://{host_ip}:{host_port}/preflight", timeout=5) as response:
                require(response.status == 204, "host fixture did not answer")

            # A known listener on the other bridge is a LAN-like positive
            # fixture. Never probe unrelated IP addresses or host services.
            run(*DOCKER, "run", "--detach", "--rm", "--name", fixture, "--network", f"{project}-out",
                "--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges:true",
                "--entrypoint", "python3", f"{project}-runner:ci", "-I", "-u", "-c", """
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(204 if self.path == "/preflight" else 404)
        self.end_headers()
ThreadingHTTPServer(("0.0.0.0", 4080), Handler).serve_forever()
""", capture=True)
            lan_ip = inspect(fixture)[0]["NetworkSettings"]["Networks"][f"{project}-out"]["IPAddress"]
            require(bool(lan_ip), "LAN fixture has no bridge IP")
            run(*DOCKER, "exec", fixture, *curl, "--noproxy", "*", "--retry", "3", "--retry-connrefused",
                f"http://{lan_ip}:4080/preflight", capture=True)
            exec_runner("python3", "-I", "-c", """
import socket
import sys
for name, ip, port in zip(sys.argv[1::3], sys.argv[2::3], sys.argv[3::3], strict=True):
    try:
        with socket.create_connection((ip, int(port)), 3):
            raise SystemExit(f"unexpected direct reachability: {name}")
    except OSError:
        print(f"Direct TCP to {name} is blocked")
""", "external GitHub", public_ip, "443", "host fixture", host_ip, str(host_port),
                "LAN fixture", lan_ip, "4080")
        finally:
            subprocess.run((*DOCKER, "rm", "-f", fixture), check=False, timeout=30,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            host_fixture.shutdown()
            thread.join()
    print("Forced proxied GitHub HTTPS worked; unauthorized CONNECT and log checks passed", flush=True)


NSH_FIXTURE = r"""
set -euo pipefail
test "$(id -u)" = 1001
test ! -S /var/run/docker.sock
test ! -S /run/docker.sock
test -x /opt/actions-runner/bin/Runner.Listener
git --version
printf 'Pinned NSH CLI: '
nsh --version | grep -Ex 'nsh 0\.6\.1 \([0-9a-f]+\)'
test "$NSH_TSC" = /opt/typescript/lib/tsc
test -x "$NSH_TSC"
printf 'Pinned offline TypeScript: '
"$NSH_TSC" --version | grep -Fx 'Version 7.0.2'
dir="$(mktemp -d)"
cd "$dir"
cat > App.tsx <<'TSX'
import { mount, Text } from "nsh";
mount(() => <Text testID="marker">Runner preflight</Text>);
TSX
cat > smoke.test.tsx <<'TSX'
import { expect, open, test } from "nsh/test";
test("runs the bundled fixture headlessly", async () => {
  const app = await open("./App.tsx", { platform: "linux" });
  await app.waitFor("id:marker");
  expect(app.shows("id:marker")).toBe(true);
});
TSX
nsh build --release App.tsx
test -n "$(find .nsh/out -type f -print -quit)"
nsh test smoke.test.tsx
"""


def exercise(compose, project):
    def exec_runner(*argv, **kw):
        return run(*compose, "exec", "-T", "runner", *argv, **kw)

    def containers():
        ids = run(*compose, "ps", "--all", "--quiet", capture=True).split()
        return {c["Config"]["Labels"]["com.docker.compose.service"]: c for c in inspect(*ids)}

    def unregistered(expected_file=""):
        status = exec_runner("/opt/metro-runner/entrypoint.sh", "status", capture=True)
        require(status.strip() == "metro-ci-runner: not registered", "runner is not unregistered")
        identity = exec_runner("/opt/metro-runner/entrypoint.sh", "status", "--identity", capture=True)
        require(not identity.strip(), "unregistered runner reported an identity")
        state = exec_runner("find", "/state", "-mindepth", "1", "-maxdepth", "1", "-printf", "%f\n", capture=True)
        require(state.strip() == expected_file, "enrollment volume contains unexpected files")
        listener = exec_runner("find", "/runner", "-mindepth", "1", "-maxdepth", "1", "-print", capture=True)
        require(not listener.strip(), "unregistered runner initialized Actions listener files")

    run(*compose, "build")
    # Exercise creation without starting either service, then ordinary up.
    run(*compose, "create", "--no-build")
    created = containers()
    require(set(created) == {"runner", "egress"} and all(c["State"]["Status"] == "created" for c in created.values()),
            "compose create unexpectedly started services")
    run(*compose, "up", "-d", "--no-build", "--wait", "--wait-timeout", "90")
    active = containers()
    check_containers(active, project)
    require(active["egress"]["State"]["Health"]["Status"] == "healthy"
            and active["runner"]["State"]["Running"], "stack is not healthy")
    unregistered()
    jobs = read_json(*DOCKER, "network", "inspect", f"{project}-jobs")[0]
    require(jobs["Driver"] == "bridge" and jobs["Internal"] and not jobs["EnableIPv6"]
            and (jobs["Options"] or {}).get("com.docker.network.bridge.gateway_mode_ipv4") == "isolated",
            "jobs network is not an isolated internal bridge")

    network_checks(compose, project)
    exec_runner("bash", "-c", NSH_FIXTURE)

    # A benign file demonstrates persistence without even simulating a .runner
    # identity or storing any kind of enrollment credential.
    marker = f"preflight-{uuid.uuid4().hex}"
    exec_runner("sh", "-c", 'umask 077; printf "%s\\n" "$1" > /state/preflight-marker', "sh", marker)
    run(*compose, "down", "--timeout", "10")
    retained = read_json(*DOCKER, "volume", "inspect", f"{project}-state")[0]
    require(retained["Labels"]["com.docker.compose.project"] == project, "test state volume was not retained")
    run(*compose, "up", "-d", "--no-build", "--wait", "--wait-timeout", "90")
    check_containers(containers(), project)
    unregistered("preflight-marker")
    require(exec_runner("cat", "/state/preflight-marker", capture=True).strip() == marker, "state did not survive down/up")
    print("Stack, offline Native Shell fixture, and unregistered state persistence passed", flush=True)


def main():
    require(os.environ.get("GITHUB_ACTIONS") == "true"
            and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted"
            and os.environ.get("RUNNER_OS") == "Linux", "refusing to use a non-hosted Docker daemon")
    run(*DOCKER, "version", "--format", "{{.Server.Version}}")
    run(*DOCKER, "compose", "version")
    nonce = uuid.uuid4().hex[:12]
    project = f"metro-runner-preflight-{int(os.environ['GITHUB_RUN_ID'])}-{int(os.environ['GITHUB_RUN_ATTEMPT'])}-{nonce}"
    names = {"volumes": {"state": {"name": f"{project}-state"}},
             "networks": {"jobs": {"name": f"{project}-jobs"}, "egress-out": {"name": f"{project}-out"}}}
    override = names | {"services": {s: {"image": f"{project}-{s}:ci"} for s in ("runner", "egress")}}
    with tempfile.TemporaryDirectory(prefix="runner-preflight-", dir=os.environ["RUNNER_TEMP"]) as d:
        overlay = Path(d) / "names.json"
        overlay.write_text(json.dumps(override))
        compose = (*DOCKER, "compose", "--project-name", project, "--project-directory", str(ROOT),
                   "-f", str(ROOT / "compose.yaml"), "-f", str(overlay))
        resolved = read_json(*compose, "config", "--format", "json")
        require(resolved["name"] == project and set(resolved["services"]) == set(override["services"]),
                "unexpected Compose project or service")
        for kind in ("networks", "volumes"):
            require({k: v["name"] for k, v in resolved[kind].items()}
                    == {k: v["name"] for k, v in names[kind].items()}
                    and all(not v.get("external") for v in resolved[kind].values()),
                    f"refusing to create or clean up {kind} outside this test project")
        try:
            exercise(compose, project)
        finally:
            run(*compose, "down", "-v", "--timeout", "10")
        volumes = run(*DOCKER, "volume", "ls", "--quiet", "--filter",
                      f"label=com.docker.compose.project={project}", capture=True)
        require(not volumes.strip(), "test project left named volumes behind")


if __name__ == "__main__":
    main()
