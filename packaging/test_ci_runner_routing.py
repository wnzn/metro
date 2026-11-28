"""CI routes trusted events to Metro's self-hosted runners and everything else to GitHub's.

The `workflows` job's selector decides once per run (self_hosted=true|false);
every Linux job and the macOS build follow it, and nothing that signs or
publishes ever runs on a self-hosted label.
"""

from copy import deepcopy
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

from test_release_workflow import WF, load, yaml


METRO = 1467654868
HOSTED = "false"
LOCAL = "true"
LINUX = "${{ needs.workflows.outputs.self_hosted == 'true' && 'metro-ci-linux-docker' || 'ubuntu-24.04' }}"
RELEASE_LINUX = "${{ inputs.self_hosted && 'metro-ci-linux-docker' || 'ubuntu-24.04' }}"
RELEASE_MAC = "${{ inputs.self_hosted && 'metro-ci-macos-arm64' || 'macos-15' }}"
SELF_HOSTED = {"metro-ci-linux-docker", "metro-ci-macos-arm64"}
AUTHOR = {"type": "User", "login": "alice", "id": 176448921}
FAKE_TOKEN = "synthetic-test-only"


PR = {
    "pull_request": {
        "base": {"ref": "main", "repo": {"id": METRO}}, "head": {"repo": {"id": METRO}},
        "user": AUTHOR, "author_association": "NONE",
    },
}
PUSH = {"ref": "refs/heads/main", "sender": {"type": "User"}, "deleted": False, "forced": False}


@unittest.skipIf(yaml is None and not os.environ.get("CI"), "PyYAML is not installed")
class DesktopRoutingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if yaml is None:
            raise AssertionError("PyYAML is required in CI")
        cls.ci = load("ci.yml")
        cls.desktop = cls.ci["jobs"]["desktop"]
        cls.workflow_gate = cls.ci["jobs"]["workflows"]
        cls.selector, = [step for step in cls.workflow_gate["steps"] if step.get("id") == "runner-route"]

    def route(self, kind, payload, *, answer=None, status=0, lookup=False, **overrides):
        with tempfile.TemporaryDirectory() as d:
            event_path, output = Path(d) / "event.json", Path(d) / "output"
            event_path.write_text(json.dumps(payload) if not isinstance(payload, str) else payload)
            marker, gh = Path(d) / "gh-called", Path(d) / "gh"
            answer = {"user": AUTHOR, "permission": "write"} if answer is None else answer
            body = answer if isinstance(answer, str) else json.dumps(answer)
            gh.write_text(f"""#!/usr/bin/env python3
import os
from pathlib import Path
import sys
assert os.environ.get("GH_TOKEN") == {FAKE_TOKEN!r}
assert os.environ.get("GH_PROMPT_DISABLED") == "1"
assert not set(os.environ).intersection({{"GH_HOST", "GH_DEBUG", "GITHUB_EVENT_PATH", "GITHUB_TOKEN"}})
assert sys.argv[1:] == [
    "api", "--hostname", "github.com", "--method", "GET", "repos/wnzn/metro/collaborators/alice/permission",
]
Path({str(marker)!r}).touch()
print({body!r})
print("synthetic debug response", file=sys.stderr)
sys.exit({status!r})
""")
            gh.chmod(0o700)
            env = dict(os.environ, GITHUB_REPOSITORY="wnzn/metro", GITHUB_REPOSITORY_ID=str(METRO),
                       GITHUB_ACTOR="maintainer", GITHUB_EVENT_NAME=kind, GITHUB_REF="refs/heads/main",
                       GITHUB_REF_PROTECTED="true", GITHUB_EVENT_PATH=str(event_path), GITHUB_OUTPUT=str(output),
                       GH_TOKEN=FAKE_TOKEN, GH_HOST="untrusted.invalid", GH_DEBUG="api",
                       PATH=f"{d}:{os.environ['PATH']}")
            env.update(overrides)
            r = subprocess.run(["bash", "-e", "-c", self.selector["run"]], env=env, text=True,
                               capture_output=True, check=True)
            self.assertEqual(marker.exists(), lookup)
            self.assertNotIn(FAKE_TOKEN, r.stdout + r.stderr)
            self.assertNotIn("synthetic debug response", r.stdout + r.stderr)
            self.assertNotIn("alice", r.stdout)
            self.assertNotIn("maintainer", r.stdout)
            text = output.read_text().strip()
            self.assertRegex(text, r"^self_hosted=(true|false)$")
            return text.removeprefix("self_hosted="), r.stdout

    def test_pr_routing_uses_verified_permission_not_association(self):
        for role in ("write", "maintain", "admin"):
            with self.subTest(role=role):
                self.assertEqual(self.route("pull_request", PR, answer={"user": AUTHOR, "permission": role},
                                            lookup=True)[0], LOCAL)
        for association in ("MEMBER", "CONTRIBUTOR", "NONE"):
            payload = deepcopy(PR)
            payload["pull_request"]["author_association"] = association
            with self.subTest(association=association):
                self.assertEqual(self.route("pull_request", payload, answer={"user": AUTHOR, "permission": "read"},
                                            lookup=True)[0], HOSTED)
        for side in ("base", "head"):
            payload = deepcopy(PR)
            payload["pull_request"][side]["repo"]["id"] = 123
            self.assertEqual(self.route("pull_request", payload)[0], HOSTED)
        for ref in ("master", "Main"):
            payload = deepcopy(PR)
            payload["pull_request"]["base"]["ref"] = ref
            self.assertEqual(self.route("pull_request", payload)[0], HOSTED)

    def test_permission_failures_and_identity_mismatches_stay_hosted(self):
        for answer, status, reason in (
            ({"permission": "admin", "user": AUTHOR | {"id": 123}}, 0, "identity mismatch"),
            ({"permission": "admin", "user": AUTHOR | {"login": "someone-else"}}, 0, "identity mismatch"),
            ({"permission": "admin", "user": AUTHOR | {"id": str(AUTHOR["id"])}}, 0, "identity mismatch"),
            ({"permission": "unknown", "user": AUTHOR}, 0, "does not have verified write access"),
            ("invalid JSON", 0, "unavailable or invalid"),
            ({}, 0, "unavailable or invalid"),
            ("HTTP 403", 1, "permission lookup failed"),
            ("HTTP 404", 1, "permission lookup failed"),
        ):
            with self.subTest(reason=reason):
                runner, log = self.route("pull_request", PR, answer=answer, lookup=True, status=status)
                self.assertEqual(runner, HOSTED)
                self.assertIn(reason, log)
        self.assertEqual(self.route("pull_request", PR, GH_TOKEN="")[0], HOSTED)

    def test_invalid_author_id_or_login_is_rejected_before_gh(self):
        for bad in ({"id": None}, {"id": "176448921"}, {"id": True}, {"id": 0},
                    {"login": "alice/../../"}, {"login": "alice?x=1"}):
            payload = deepcopy(PR)
            payload["pull_request"]["user"].update(bad)
            self.assertEqual(self.route("pull_request", payload)[0], HOSTED)

    def test_bots_wrong_repository_and_uncertainty_stay_hosted_with_a_reason(self):
        bot_author = deepcopy(PR)
        bot_author["pull_request"]["user"]["type"] = "Bot"
        cases = (
            (bot_author, {}, "bot PR author"),
            (PR | {"sender": {"type": "Bot"}}, {}, "event sender is a bot"),
            (PR, {"GITHUB_ACTOR": "dependabot[bot]"}, "bot workflow actor"),
            (PR, {"GITHUB_REPOSITORY_ID": "1"}, "not the official Metro repository"),
            (PR, {"GITHUB_REPOSITORY": "elsewhere/metro"}, "not the official Metro repository"),
            ("invalid JSON", {}, "missing or incomplete"),
            ({"pull_request": {"head": None}}, {}, "missing or incomplete"),
        )
        for payload, env, reason in cases:
            with self.subTest(reason=reason):
                runner, log = self.route("pull_request", payload, **env)
                self.assertEqual(runner, HOSTED)
                self.assertIn(reason, log)

    def test_only_protected_nonforced_main_pushes_route_to_metro(self):
        self.assertEqual(self.route("push", PUSH)[0], LOCAL)
        self.assertEqual(self.route("push", PUSH | {"ref": "refs/heads/master"}, GITHUB_REF="refs/heads/master")[0], HOSTED)
        self.assertEqual(self.route("push", PUSH, GITHUB_REF_PROTECTED="false")[0], HOSTED)
        for flag in ("forced", "deleted"):
            self.assertEqual(self.route("push", PUSH | {flag: True})[0], HOSTED)
        self.assertEqual(self.route("workflow_dispatch", {})[0], HOSTED)

    def test_release_runs_use_the_same_queue_as_other_trusted_runs(self):
        """Evaluate the real runs-on and if expressions over every release-policy outcome."""
        def evaluate(expression, ctx):
            # Exactly the two shapes used by the desktop job; anything else fails this test.
            body = expression.removeprefix("${{ ").removesuffix(" }}")
            gate = ("!cancelled() && needs.workflows.result == 'success' && needs.release-policy.result != 'failure' "
                    "&& (needs.workflows.outputs.desktop != 'false' || needs.release-policy.outputs.release == 'true')")
            if expression == LINUX:
                return "metro-ci-linux-docker" if ctx["selected"] == "true" else "ubuntu-24.04"
            self.assertEqual(" ".join(body.split()), gate)
            return (not ctx["cancelled"] and ctx["workflows"] == "success" and ctx["policy"] != "failure"
                    and (ctx.get("desktop", "") != "false" or ctx.get("release") == "true"))

        cases = (
            # (release-policy result, release output, selector output) -> (desktop runs, runner)
            ("skipped", "", LOCAL, True, "metro-ci-linux-docker"),     # trusted pull request
            ("success", "false", LOCAL, True, "metro-ci-linux-docker"),  # main push without a version bump
            ("success", "true", LOCAL, True, "metro-ci-linux-docker"),   # release run
            ("success", "true", HOSTED, True, "ubuntu-24.04"),
            ("skipped", "", HOSTED, True, "ubuntu-24.04"),             # fork or untrusted PR
            ("skipped", "", "", True, "ubuntu-24.04"),                 # no answer: hosted
            ("failure", "", LOCAL, False, None),                       # no release, and nothing to route
        )
        for policy, release, selected, runs, runner in cases:
            ctx = {"policy": policy, "release": release, "selected": selected, "workflows": "success",
                   "cancelled": False}
            with self.subTest(ctx=ctx):
                self.assertIs(evaluate(self.desktop["if"], ctx), runs)
                if runs:
                    self.assertEqual(evaluate(self.desktop["runs-on"], ctx), runner)
        for workflows in ("failure", "skipped"):
            self.assertFalse(evaluate(self.desktop["if"], {"policy": "skipped", "workflows": workflows,
                                                           "cancelled": False}))
        self.assertFalse(evaluate(self.desktop["if"], {"policy": "success", "workflows": "success", "cancelled": True}))
        # Only an explicit desktop=false skips it (PR or push); a release run still runs it.
        for desktop, release, runs in (("false", "", False), ("false", "false", False), ("true", "", True),
                                       ("", "", True), ("false", "true", True)):
            self.assertIs(evaluate(self.desktop["if"], {"policy": "success" if release else "skipped",
                                                        "workflows": "success", "cancelled": False,
                                                        "desktop": desktop, "release": release}), runs)

    def test_single_read_only_desktop_job_with_pinned_nsh(self):
        self.assertEqual(self.ci["permissions"], {"contents": "read"})
        self.assertNotIn("permissions", self.desktop)
        self.assertNotIn("secrets", self.desktop)
        self.assertEqual(self.desktop["timeout-minutes"], 45)
        self.assertEqual(self.desktop["needs"], ["workflows", "release-policy"])
        self.assertIs(self.selector, self.workflow_gate["steps"][0])
        self.assertEqual(self.selector["env"], {"GH_TOKEN": "${{ github.token }}"})
        self.assertEqual(self.workflow_gate["outputs"]["self_hosted"], "${{ steps.runner-route.outputs.self_hosted }}")
        self.assertEqual(self.desktop["runs-on"], LINUX)
        self.assertEqual(" ".join(self.desktop["if"].split()),
                         "${{ !cancelled() && needs.workflows.result == 'success' "
                         "&& needs.release-policy.result != 'failure' && (needs.workflows.outputs.desktop != 'false' "
                         "|| needs.release-policy.outputs.release == 'true') }}")
        self.assertIn("desktop", self.ci["jobs"]["release"]["needs"])
        # The slow packaging tests run in their own job, so they do not hold up the desktop job.
        packaging = self.ci["jobs"]["packaging"]
        self.assertEqual(packaging["needs"], ["workflows", "release-policy"])
        tests, = [s for s in packaging["steps"] if "unittest discover" in str(s.get("run", ""))]
        for suite in ("packaging", "packaging/release-signatures", "packaging/windows-signing"):
            self.assertIn(f"python3 -B -m unittest discover -s {suite} -v\n", tests["run"])
        self.assertIn("for tool in shellcheck desktop-file-validate gio pwsh; do command -v", tests["run"])
        apt, = [s for s in packaging["steps"] if "apt-get" in str(s.get("run", ""))]
        self.assertEqual(apt["if"], "runner.environment == 'github-hosted'")  # no sudo on the self-hosted runner
        pwsh, = [s for s in packaging["steps"] if "PWSH_SHA256" in str(s.get("env", ""))]
        self.assertEqual(pwsh["if"], "runner.environment == 'self-hosted'")
        self.assertRegex(pwsh["env"]["PWSH_SHA256"], r"^[0-9a-f]{64}$")
        self.assertLess(pwsh["run"].index("sha256sum --check"), pwsh["run"].index("tar -xzf"))
        self.assertNotIn("unittest discover", yaml.safe_dump(self.workflow_gate))
        steps = self.desktop["steps"]
        guard, checkout, install, *commands = steps
        self.assertEqual(guard["if"], "runner.environment == 'self-hosted'")
        self.assertEqual(checkout["with"], {"ref": "${{ github.sha }}", "persist-credentials": False})
        self.assertNotIn("if", install)  # the pinned NSH is downloaded on every runner
        self.assertIn('"$NSH_ARCHIVE_SHA256" "$archive" | sha256sum --check -', install["run"])
        self.assertEqual(commands, [
            {"run": "nsh check", "working-directory": "apps/desktop"},
            {"name": "NSH headless tests (all files, isolated workers)",
             "env": {"METRO_NSH_WORKERS": "2"},
             "run": "python3 -B ../../packaging/nsh_ci_tests.py",
             "working-directory": "apps/desktop"},
        ])
        # The release build is package-smoke's `ui` job, not this one.
        self.assertNotIn("nsh build", yaml.safe_dump(self.desktop))


class RunnerMapTests(unittest.TestCase):
    """Every job in every workflow: GitHub-hosted, or self-hosted only through the selector."""

    @classmethod
    def setUpClass(cls):
        if yaml is None:
            raise unittest.SkipTest("PyYAML is not installed")
        cls.workflows = {path.name: load(path.name) for path in sorted(WF.glob("*.yml"))}

    def test_the_runner_map(self):
        got = {(file, name): job.get("runs-on", job.get("uses"))
               for file, wf in self.workflows.items() for name, job in wf["jobs"].items()}
        want_ci = {"rust": LINUX, "desktop": LINUX, "containers": LINUX, "packaging": LINUX,
                   "workflows": "ubuntu-24.04", "release-policy": "ubuntu-24.04",
                   "windows-update-builds": "windows-2025", "package-smoke": "./.github/workflows/release.yml",
                   "release": "./.github/workflows/release.yml", "publish": "./.github/workflows/release-publish.yml"}
        want_release = {"meta": RELEASE_LINUX, "runner-linux": RELEASE_LINUX, "ui": RELEASE_LINUX,
                        "desktop-linux": RELEASE_LINUX, "assets": RELEASE_LINUX, "desktop-macos": RELEASE_MAC,
                        "desktop-windows": "windows-2025",
                        "windows-distribution": "./.github/workflows/windows-distribution.yml"}
        self.assertEqual({n: r for (f, n), r in got.items() if f == "ci.yml"}, want_ci)
        self.assertEqual({n: r for (f, n), r in got.items() if f == "release.yml"}, want_release)
        self.assertEqual(got[("self-hosted-ci.yml", "desktop")], "metro-ci-linux-docker")
        self.assertEqual(got[("self-hosted-ci.yml", "authorize")], "ubuntu-24.04")
        # Nothing that signs, publishes or holds an OIDC token reaches a self-hosted label, and no
        # workflow still names the retired metro-ci-linux-x64 runner.
        for (file, name), runner in got.items():
            text = json.dumps(self.workflows[file]["jobs"][name])
            self.assertNotIn("metro-ci-linux-x64", text, (file, name))
            if file in ("ci.yml", "release.yml", "self-hosted-ci.yml"):
                continue
            with self.subTest(file=file, job=name):
                self.assertFalse(any(label in str(runner) for label in SELF_HOSTED))
        for file in ("release-publish.yml", "sign-release-assets.yml", "release-images.yml", "windows-sign.yml",
                     "windows-distribution.yml", "check-runner-stack.yml"):
            self.assertNotIn("metro-ci-", (WF / file).read_text(), file)

    def test_only_trusted_runs_choose_self_hosted_and_release_yml_gets_a_boolean(self):
        ci = self.workflows["ci.yml"]["jobs"]
        for name in ("package-smoke", "release"):
            self.assertEqual(ci[name]["with"]["self_hosted"], "${{ needs.workflows.outputs.self_hosted == 'true' }}")
            self.assertIn("workflows", ci[name]["needs"])
        spec = self.workflows["release.yml"]["on"]["workflow_call"]["inputs"]["self_hosted"]
        self.assertEqual((spec["type"], spec["default"]), ("boolean", False))
        # Labels only ever come from fixed strings in these expressions, never from an input value.
        for file in ("ci.yml", "release.yml"):
            for name, job in self.workflows[file]["jobs"].items():
                runner = str(job.get("runs-on", ""))
                if "${{" in runner:
                    self.assertIn(runner, (LINUX, RELEASE_LINUX, RELEASE_MAC), (file, name))

    def test_every_self_hosted_job_rechecks_the_event_before_checkout(self):
        guards = []
        for file in ("ci.yml", "release.yml"):
            for name, job in self.workflows[file]["jobs"].items():
                if not any(label in str(job.get("runs-on", "")) for label in SELF_HOSTED):
                    continue
                with self.subTest(file=file, job=name):
                    first = job["steps"][0]
                    self.assertEqual(first["if"], "runner.environment == 'self-hosted'")
                    self.assertNotIn("uses", first)
                    checkout = next(i for i, s in enumerate(job["steps"]) if "actions/checkout" in str(s.get("uses")))
                    self.assertGreater(checkout, 0)
                    mac = "macos" in job["runs-on"]
                    self.assertEqual(first["env"], {"WANT_OS": "macOS" if mac else "Linux",
                                                    "WANT_ARCH": "ARM64" if mac else "X64"})
                    guards.append(first["run"])
        self.assertEqual(len(guards), 10)
        self.assertEqual(len(set(guards)), 1)  # one script, everywhere
        self.check_guard(guards[0])

    def check_guard(self, script):
        with tempfile.TemporaryDirectory() as d:
            def run(kind, payload, **env):
                event = Path(d) / "event.json"
                event.write_text(json.dumps(payload))
                base = {"PATH": os.environ["PATH"], "GITHUB_REPOSITORY": "wnzn/metro",
                        "GITHUB_REPOSITORY_ID": str(METRO), "GITHUB_EVENT_NAME": kind, "GITHUB_REF": "refs/heads/main",
                        "GITHUB_BASE_REF": "main" if kind == "pull_request" else "", "GITHUB_EVENT_PATH": str(event),
                        "RUNNER_OS": "Linux", "RUNNER_ARCH": "X64", "WANT_OS": "Linux", "WANT_ARCH": "X64"}
                return subprocess.run(["bash", "-c", script], env=base | env, capture_output=True).returncode

            fork = deepcopy(PR)
            fork["pull_request"]["head"]["repo"]["id"] = 123
            self.assertEqual(run("push", PUSH), 0)
            self.assertEqual(run("pull_request", PR), 0)
            for kind, payload, env in (
                ("pull_request", fork, {}), ("pull_request", PR, {"GITHUB_BASE_REF": "release"}),
                ("pull_request", {}, {}), ("push", PUSH, {"GITHUB_REF": "refs/heads/feature"}),
                ("push", PUSH, {"GITHUB_REPOSITORY": "someone/metro"}), ("push", PUSH, {"GITHUB_REPOSITORY_ID": "1"}),
                ("workflow_dispatch", {}, {}), ("pull_request_target", PR, {}),
                ("push", PUSH, {"RUNNER_OS": "macOS"}), ("push", PUSH, {"RUNNER_ARCH": "ARM64"}),
            ):
                with self.subTest(kind=kind, env=env):
                    self.assertNotEqual(run(kind, payload, **env), 0)
