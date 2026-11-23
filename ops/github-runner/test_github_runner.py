"""Contract tests for ops/github-runner. No Docker daemon, GitHub call or real
token is used: compose is only rendered with `docker compose config`, and the
entrypoint and host helper run against stub config.sh, gh and docker.

Run: python3 -B -m unittest discover -s ops/github-runner -v
"""

import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
TOKEN = "AABBCCDDEEFFGGHHIIJJKKLLMM0123"  # synthetic, shaped like a registration token


def run(cmd, **kw):
    return subprocess.run(cmd, text=True, capture_output=True, timeout=120, **kw)


class Dockerfile(unittest.TestCase):
    text = (HERE / "Dockerfile").read_text(encoding="utf-8")

    def test_pinned_inputs(self):
        self.assertRegex(self.text, r"(?m)^FROM ubuntu:24\.04@sha256:[0-9a-f]{64}$")
        self.assertIn("ARG RUNNER_VERSION=2.338.2\n", self.text)
        self.assertIn("ARG RUNNER_SHA256=baebf204307bac6193480852732f0aba2ae97b6c5597c244f4d5558390147c40\n", self.text)
        ci = (REPO / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        nsh = re.search(r"NSH_ARCHIVE_SHA256: ([0-9a-f]{64})", ci).group(1)
        self.assertIn(f"ARG NSH_SHA256={nsh}\n", self.text)
        self.assertIn("ARG NSH_VERSION=v0.6.1\n", self.text)
        self.assertEqual(self.text.count("sha256sum --check -"), 2)
        self.assertRegex(self.text, r'--snapshot "\$UBUNTU_SNAPSHOT"')

    def test_non_root_and_no_secrets(self):
        self.assertIn("USER 1001:1001", self.text)
        for word in ("TOKEN", "GH_", "GITHUB_TOKEN", "--replace", "docker.sock"):
            self.assertNotIn(word, self.text)


@unittest.skipUnless(shutil.which("docker") and run(["docker", "compose", "version"]).returncode == 0,
                     "docker compose CLI not available")
class Compose(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        r = run(["docker", "compose", "-f", str(HERE / "compose.yaml"), "config", "--format", "json"])
        if r.returncode != 0:
            raise AssertionError(r.stderr)
        cls.cfg = json.loads(r.stdout)

    def test_runner_hardening(self):
        s = self.cfg["services"]["runner"]
        self.assertEqual(s["user"], "1001:1001")
        self.assertTrue(s["read_only"])
        self.assertFalse(s.get("privileged", False))
        self.assertEqual(s["cap_drop"], ["ALL"])
        self.assertNotIn("cap_add", s)
        self.assertIn("no-new-privileges:true", s["security_opt"])
        self.assertLessEqual(float(s["cpus"]), 4)
        self.assertLessEqual(int(s["mem_limit"]), 6 * 1024**3)
        self.assertEqual(s["memswap_limit"], s["mem_limit"])
        self.assertTrue(0 < int(s["pids_limit"]) <= 4096)
        self.assertNotIn("ports", s)
        self.assertNotIn("network_mode", s)
        self.assertEqual(set(s["networks"]), {"jobs"})
        self.assertEqual(str(s["sysctls"]["net.ipv6.conf.all.disable_ipv6"]), "1")

    def test_no_host_paths_or_docker_socket(self):
        for name, s in self.cfg["services"].items():
            for v in s.get("volumes", []):
                self.assertEqual(v["type"], "volume", f"{name} mounts something other than a volume: {v}")
                self.assertNotIn("docker.sock", json.dumps(v))
            self.assertNotIn("ports", s, name)
            self.assertFalse(s.get("privileged", False), name)
        mounts = {v["target"]: v for v in self.cfg["services"]["runner"]["volumes"]}
        self.assertEqual(set(mounts), {"/state", "/runner"})
        self.assertEqual(mounts["/state"]["source"], "state")
        self.assertNotIn("source", mounts["/runner"])  # anonymous scratch

    def test_proxy_environment(self):
        env = self.cfg["services"]["runner"]["environment"]
        for k in ("HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"):
            self.assertEqual(env[k], "http://egress:3128")
        for k in ("NO_PROXY", "no_proxy"):
            self.assertEqual(env[k], "localhost,127.0.0.1,::1")
        for v in env.values():
            self.assertNotRegex(str(v), r"(?i)token|ghp_|gho_|github_pat")

    def test_networks(self):
        jobs = self.cfg["networks"]["jobs"]
        self.assertTrue(jobs["internal"])
        self.assertFalse(jobs.get("enable_ipv6", False))
        self.assertEqual(jobs["driver_opts"]["com.docker.network.bridge.gateway_mode_ipv4"], "isolated")
        out = self.cfg["networks"]["egress-out"]
        self.assertFalse(out.get("internal", False))
        self.assertFalse(out.get("enable_ipv6", False))
        egress = self.cfg["services"]["egress"]
        self.assertEqual(set(egress["networks"]), {"jobs", "egress-out"})
        self.assertTrue(egress["read_only"])
        self.assertEqual(egress["cap_drop"], ["ALL"])
        self.assertIn("no-new-privileges:true", egress["security_opt"])
        self.assertNotRegex(egress["user"], r"^0(:|$)|^root")
        self.assertTrue(egress["build"]["context"].endswith("ops/github-runner/egress"))

    def test_registration_survives_down(self):
        self.assertEqual(self.cfg["volumes"]["state"]["name"], "metro-ci-runner-state")


class Entrypoint(unittest.TestCase):
    """Real entrypoint.sh against a stub runner (config.sh records argv and env)."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="metro-runner-test-"))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.src = self.tmp / "opt-runner"
        self.src.mkdir()
        (self.tmp / "scratch").mkdir()
        (self.tmp / "state").mkdir()
        self.log = self.tmp / "config.log"
        config = self.src / "config.sh"
        config.write_text(f"""#!/usr/bin/env bash
set -eu
{{ printf 'ARGV'; printf ' %q' "$@"; printf '\\n'; printf 'TOKEN_ENV=%s\\n' "${{ACTIONS_RUNNER_INPUT_TOKEN:+set}}"; }} >> {self.log}
case "$*" in *{TOKEN}*) echo leaked >> {self.log} ;; esac
[ "${{ACTIONS_RUNNER_INPUT_TOKEN:-}}" = "{TOKEN}" ] || exit 3
if [ "${{1:-}}" = remove ]; then rm -f .runner .credentials .credentials_rsaparams; exit 0; fi
name=""; while [ $# -gt 0 ]; do [ "$1" = --name ] && name="$2"; shift; done
printf '\\xef\\xbb\\xbf{{"agentId": 7, "agentName": "%s", "gitHubUrl": "https://github.com/wnzn/metro"}}' "$name" > .runner
echo creds > .credentials; echo rsa > .credentials_rsaparams
""")
        config.chmod(0o755)
        self.env = {**os.environ, "METRO_RUNNER_SRC": str(self.src), "METRO_RUNNER_SCRATCH": str(self.tmp / "scratch"),
                    "METRO_RUNNER_STATE": str(self.tmp / "state")}

    def entry(self, *args, stdin="", **env):
        return run(["bash", str(HERE / "entrypoint.sh"), *args], input=stdin, env={**self.env, **env})

    def test_register_is_idempotent_and_keeps_the_token_out_of_argv(self):
        name = "metro-ci-linux-0123456789ab"
        r = self.entry("register", stdin=TOKEN + "\n", METRO_RUNNER_NAME=name)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertNotIn(TOKEN, r.stdout + r.stderr)
        log = self.log.read_text()
        self.assertNotIn("leaked", log)
        self.assertIn("TOKEN_ENV=set", log)
        for arg in ("--url https://github.com/wnzn/metro", "--labels metro-ci-linux-x64", "--no-default-labels",
                    "--disableupdate", "--unattended", f"--name {name}"):
            self.assertIn(arg, log)
        self.assertNotIn("--replace", log)
        for f in (".runner", ".credentials", ".credentials_rsaparams"):
            self.assertEqual((self.tmp / "state" / f).stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.entry("status", "--name-only").stdout.strip(), name)
        again = self.entry("register", stdin=TOKEN + "\n", METRO_RUNNER_NAME=name)
        self.assertEqual(again.returncode, 0, again.stderr)
        self.assertIn("already registered", again.stdout)
        self.assertEqual(log.count("ARGV"), self.log.read_text().count("ARGV"))
        other = self.entry("register", stdin=TOKEN + "\n", METRO_RUNNER_NAME="metro-ci-linux-ffffffffffff")
        self.assertNotEqual(other.returncode, 0)

        removed = self.entry("remove", stdin=TOKEN + "\n")
        self.assertEqual(removed.returncode, 0, removed.stderr)
        self.assertFalse(any((self.tmp / "state").iterdir()))
        self.assertEqual(self.entry("status", "--name-only").stdout.strip(), "")
        self.assertEqual(self.entry("remove", stdin="").returncode, 0)  # nothing to remove: still fine

    def test_rejects_bad_input(self):
        for name in ("", "other-runner", "metro-ci-linux-UPPER", "metro-ci-linux-a b"):
            r = self.entry("register", stdin=TOKEN + "\n", METRO_RUNNER_NAME=name)
            self.assertNotEqual(r.returncode, 0, name)
        for token in ("", "short", "has space in it 012345678901234", "a;rm -rf /;0123456789012345"):
            r = self.entry("register", stdin=token + "\n", METRO_RUNNER_NAME="metro-ci-linux-0123456789ab")
            self.assertNotEqual(r.returncode, 0, token)
            self.assertFalse(self.log.exists())
        self.assertNotEqual(self.entry("bogus").returncode, 0)

    def test_foreign_saved_registration_is_refused(self):
        state = self.tmp / "state"
        (state / ".runner").write_text('{"agentName": "metro-ci-linux-0123456789ab", "gitHubUrl": "https://github.com/evil/metro"}')
        (state / ".credentials").write_text("x")
        (state / ".credentials_rsaparams").write_text("x")
        r = self.entry("status")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("not for https://github.com/wnzn/metro", r.stderr)


class Hooks(unittest.TestCase):
    def test_started_clears_home_tmp_tool_cache_but_keeps_actions(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for rel in ("home/runner/.cache/x", "tmp/y", "runner/actions-runner/_work/_tool/node/z",
                        "runner/actions-runner/_work/_actions/a/b", "runner/actions-runner/_work/metro/metro/README.md",
                        "outside/keep"):
                (root / rel).parent.mkdir(parents=True, exist_ok=True)
                (root / rel).write_text("x")
            env = {**os.environ, "METRO_RUNNER_TEST_ROOT": str(root)}
            self.assertEqual(run(["bash", str(HERE / "job-started.sh")], env=env).returncode, 0)
            self.assertFalse(any((root / "home/runner").iterdir()))
            self.assertFalse(any((root / "tmp").iterdir()))
            self.assertFalse(any((root / "runner/actions-runner/_work/_tool").iterdir()))
            self.assertTrue((root / "runner/actions-runner/_work/_actions/a/b").exists())
            self.assertEqual(run(["bash", str(HERE / "job-completed.sh")], env=env).returncode, 0)
            self.assertFalse(any((root / "runner/actions-runner/_work").iterdir()))
            self.assertTrue((root / "outside/keep").exists())

    def test_hooks_use_fixed_paths_without_test_root(self):
        for hook in ("job-started.sh", "job-completed.sh"):
            text = (HERE / hook).read_text()
            self.assertIn('root=""', text)
            self.assertNotRegex(text, r'\$HOME|\$\{HOME')


class HostHelper(unittest.TestCase):
    """runner.sh against stub gh and docker on PATH."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="metro-runner-helper-"))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        bin_dir = self.tmp / "bin"
        bin_dir.mkdir()
        self.calls = self.tmp / "calls.log"
        (bin_dir / "gh").write_text(f"""#!/usr/bin/env bash
printf 'gh %s\\n' "$*" >> {self.calls}
case "$*" in
  "api repos/wnzn/metro --jq"*) echo "wnzn/metro public" ;;
  *fork-pr-contributor-approval*) cat "{self.tmp}/policy" ;;
  *registration-token*|*remove-token*) echo {TOKEN} ;;
  *"actions/runners --jq"*|*"--paginate repos/wnzn/metro/actions/runners"*) cat "{self.tmp}/remote" 2>/dev/null || true ;;
  *"-X DELETE"*) ;;
  *"actions/runners/"*) echo true ;;
esac
""")
        (bin_dir / "docker").write_text(f"""#!/usr/bin/env bash
stdin="$(cat)"
printf 'docker %s | stdin=%s\\n' "$*" "$stdin" >> {self.calls}
case "$*" in *"status --name-only"*) cat "{self.tmp}/local" 2>/dev/null ;; esac
exit 0
""")
        for f in ("gh", "docker"):
            (bin_dir / f).chmod(0o755)
        (self.tmp / "policy").write_text("all_external_contributors\n")
        self.env = {**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}", "METRO_RUNNER_MACHINE_ID": "test-machine"}

    def helper(self, *args):
        return run(["bash", str(HERE / "runner.sh"), *args], env=self.env)

    def name(self):
        r = run(["bash", "-c", f'source <(sed -n "/^runner_name()/,/^}}/p" {HERE / "runner.sh"}); runner_name'], env=self.env)
        return r.stdout.strip()

    def test_refuses_without_all_external_contributors(self):
        for policy in ("first_time_contributors", "first_time_contributors_new_to_github", ""):
            (self.tmp / "policy").write_text(policy + "\n")
            r = self.helper("setup")
            self.assertNotEqual(r.returncode, 0, policy)
            self.assertIn("all_external_contributors", r.stderr)
            calls = self.calls.read_text()
            self.assertNotIn("registration-token", calls)
            self.assertNotIn("docker", calls)
            self.assertNotRegex(calls, r"(?m)^gh api -X (PUT|PATCH)")

    def test_setup_streams_the_token_on_stdin_only(self):
        name = self.name()
        self.assertRegex(name, r"^metro-ci-linux-[0-9a-f]{12}$")
        r = self.helper("setup")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertNotIn(TOKEN, r.stdout + r.stderr)
        register = [line for line in self.calls.read_text().splitlines() if " register" in line]
        self.assertEqual(len(register), 1)
        argv, stdin = register[0].split(" | stdin=")
        self.assertNotIn(TOKEN, argv)
        self.assertEqual(stdin, TOKEN)
        self.assertIn(f"METRO_RUNNER_NAME={name}", argv)
        self.assertIn("run -T --rm", argv)

    def test_setup_never_takes_over_an_existing_runner(self):
        (self.tmp / "remote").write_text("42\n")
        r = self.helper("setup")
        self.assertNotEqual(r.returncode, 0)
        self.assertNotIn("registration-token", self.calls.read_text())

    def test_setup_is_idempotent(self):
        (self.tmp / "remote").write_text("42\n")
        (self.tmp / "local").write_text(self.name() + "\n")
        r = self.helper("setup")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("already registered", r.stdout)
        self.assertNotIn("registration-token", self.calls.read_text())

    def test_remove_uses_a_removal_token_on_stdin(self):
        (self.tmp / "remote").write_text("42\n")
        (self.tmp / "local").write_text(self.name() + "\n")
        r = self.helper("remove")
        self.assertEqual(r.returncode, 0, r.stderr)
        remove = [line for line in self.calls.read_text().splitlines() if line.endswith(f"stdin={TOKEN}")]
        self.assertEqual(len(remove), 1)
        self.assertIn(" runner remove", remove[0].split(" | stdin=")[0])

    def test_remove_with_nothing_registered(self):
        r = self.helper("remove")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("nothing to remove", r.stdout)


class Static(unittest.TestCase):
    def test_scripts_parse_and_have_no_trace_or_secret_echo(self):
        for script in ("entrypoint.sh", "job-started.sh", "job-completed.sh", "runner.sh"):
            path = HERE / script
            self.assertEqual(run(["bash", "-n", str(path)]).returncode, 0, script)
            text = path.read_text()
            self.assertNotRegex(text, r"(?m)^\s*set -[a-z]*x")
            self.assertNotRegex(text, r"echo[^\n]*\$\{?token")
            self.assertNotIn("--replace", text.replace("no --replace", "").replace("There's no --replace", ""))


if __name__ == "__main__":
    unittest.main()
