"""Regular desktop CI routes the real job without granting forks runner access."""

from copy import deepcopy
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

from test_release_workflow import load, yaml


METRO = 1467654868
HOSTED = "ubuntu-24.04"
LOCAL = "metro-ci-linux-x64"
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
        cls.selector, = [step for step in cls.workflow_gate["steps"] if step.get("id") == "desktop-route"]

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
            return output.read_text().strip().removeprefix("desktop_runner="), r.stdout

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

    def test_release_runs_keep_the_desktop_job_on_github_hosted_runners(self):
        """Evaluate the real runs-on and if expressions over every release-policy outcome."""
        def evaluate(expression, ctx):
            # Exactly the two shapes used by the desktop job; anything else fails this test.
            body = expression.removeprefix("${{ ").removesuffix(" }}")
            runner = ("needs.workflows.outputs.desktop_runner == 'metro-ci-linux-x64' "
                      "&& needs.release-policy.outputs.release != 'true' && 'metro-ci-linux-x64' || 'ubuntu-24.04'")
            gate = ("!cancelled() && needs.workflows.result == 'success' && needs.release-policy.result != 'failure' "
                    "&& (needs.workflows.outputs.desktop != 'false' || needs.release-policy.outputs.release == 'true')")
            if body == runner:
                local = ctx["selected"] == LOCAL and ctx["release"] != "true"
                return LOCAL if local else HOSTED
            self.assertEqual(" ".join(body.split()), gate)
            return (not ctx["cancelled"] and ctx["workflows"] == "success" and ctx["policy"] != "failure"
                    and (ctx.get("desktop", "") != "false" or ctx.get("release") == "true"))

        cases = (
            # (release-policy result, release output, selector choice) -> (desktop runs, runner)
            ("skipped", "", LOCAL, True, LOCAL),          # pull request
            ("success", "false", LOCAL, True, LOCAL),     # main push without a version bump
            ("success", "true", LOCAL, True, HOSTED),     # release run: always GitHub-hosted
            ("success", "true", HOSTED, True, HOSTED),
            ("skipped", "", HOSTED, True, HOSTED),        # fork or untrusted PR
            ("failure", "", LOCAL, False, None),          # no release, and nothing to route
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

    def test_single_read_only_desktop_job_and_one_toolchain_per_runner(self):
        self.assertEqual(self.ci["permissions"], {"contents": "read"})
        self.assertNotIn("permissions", self.desktop)
        self.assertNotIn("secrets", self.desktop)
        self.assertEqual(self.desktop["timeout-minutes"], 45)
        self.assertEqual(self.desktop["needs"], ["workflows", "release-policy"])
        self.assertIs(self.selector, self.workflow_gate["steps"][0])
        self.assertEqual(self.selector["env"], {"GH_TOKEN": "${{ github.token }}"})
        self.assertEqual(self.workflow_gate["outputs"]["desktop_runner"], "${{ steps.desktop-route.outputs.desktop_runner }}")
        self.assertEqual(self.desktop["runs-on"],
                         "${{ needs.workflows.outputs.desktop_runner == 'metro-ci-linux-x64' "
                         "&& needs.release-policy.outputs.release != 'true' "
                         "&& 'metro-ci-linux-x64' || 'ubuntu-24.04' }}")
        self.assertEqual(" ".join(self.desktop["if"].split()),
                         "${{ !cancelled() && needs.workflows.result == 'success' "
                         "&& needs.release-policy.result != 'failure' && (needs.workflows.outputs.desktop != 'false' "
                         "|| needs.release-policy.outputs.release == 'true') }}")
        self.assertIn("desktop", self.ci["jobs"]["release"]["needs"])
        for job in ("rust", "containers", "workflows", "packaging", "release-policy"):
            self.assertEqual(self.ci["jobs"][job]["runs-on"], HOSTED)
        # The slow packaging tests run in their own job, so they do not hold up the desktop job.
        packaging = self.ci["jobs"]["packaging"]
        self.assertEqual(packaging["needs"], ["workflows", "release-policy"])
        self.assertEqual(packaging["steps"][0]["with"], {"persist-credentials": False})
        for suite in ("packaging", "packaging/release-signatures", "packaging/windows-signing"):
            self.assertIn(f"python3 -B -m unittest discover -s {suite} -v\n", packaging["steps"][1]["run"])
        self.assertNotIn("unittest discover", yaml.safe_dump(self.workflow_gate))
        steps = self.desktop["steps"]
        verify, checkout, install, *commands = steps
        self.assertEqual(verify["if"], "runner.environment == 'self-hosted'")
        self.assertIn("sha256sum --check", verify["run"])
        self.assertIn("test \"${NSH_TSC:-}\" = /opt/typescript/lib/tsc", verify["run"])
        self.assertIn("Version 7.0.2", verify["run"])
        self.assertNotIn("curl", verify["run"])
        self.assertEqual(checkout["with"], {"ref": "${{ github.sha }}", "persist-credentials": False})
        self.assertEqual(install["if"], "runner.environment == 'github-hosted'")
        self.assertIn("curl", install["run"])
        self.assertEqual(commands, [
            {"run": "nsh check", "working-directory": "apps/desktop"},
            {"name": "NSH headless tests (all files, isolated workers)",
             "env": {"METRO_NSH_WORKERS": "2"},
             "run": "python3 -B ../../packaging/nsh_ci_tests.py",
             "working-directory": "apps/desktop"},
        ])
        # The release build is package-smoke's `ui` job, not this one.
        self.assertNotIn("nsh build", yaml.safe_dump(self.desktop))
