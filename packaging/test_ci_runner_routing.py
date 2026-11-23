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


PR = {
    "pull_request": {
        "base": {"ref": "main", "repo": {"id": METRO}}, "head": {"repo": {"id": METRO}},
        "user": {"type": "User", "login": "alice"}, "author_association": "MEMBER",
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

    def route(self, kind, payload, **overrides):
        with tempfile.TemporaryDirectory() as d:
            event_path, output = Path(d) / "event.json", Path(d) / "output"
            event_path.write_text(json.dumps(payload) if not isinstance(payload, str) else payload)
            env = dict(os.environ, GITHUB_REPOSITORY="wnzn/metro", GITHUB_REPOSITORY_ID=str(METRO),
                       GITHUB_ACTOR="maintainer", GITHUB_EVENT_NAME=kind, GITHUB_REF="refs/heads/main",
                       GITHUB_REF_PROTECTED="true", GITHUB_EVENT_PATH=str(event_path), GITHUB_OUTPUT=str(output))
            env.update(overrides)
            r = subprocess.run(["bash", "-e", "-c", self.selector["run"]], env=env, text=True,
                               capture_output=True, check=True)
            self.assertNotIn("alice", r.stdout)
            self.assertNotIn("maintainer", r.stdout)
            return output.read_text().strip().removeprefix("desktop_runner="), r.stdout

    def test_pr_routing_uses_real_event_and_does_not_require_actor_author_equality(self):
        for association, expect in (("OWNER", LOCAL), ("MEMBER", LOCAL), ("COLLABORATOR", LOCAL), ("CONTRIBUTOR", HOSTED)):
            payload = deepcopy(PR)
            payload["pull_request"]["author_association"] = association
            with self.subTest(association=association):
                self.assertEqual(self.route("pull_request", payload)[0], expect)
        for side in ("base", "head"):
            payload = deepcopy(PR)
            payload["pull_request"][side]["repo"]["id"] = 123
            self.assertEqual(self.route("pull_request", payload)[0], HOSTED)
        for ref in ("master", "Main"):
            payload = deepcopy(PR)
            payload["pull_request"]["base"]["ref"] = ref
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

    def test_single_read_only_desktop_job_and_one_toolchain_per_runner(self):
        self.assertEqual(self.ci["permissions"], {"contents": "read"})
        self.assertNotIn("permissions", self.desktop)
        self.assertNotIn("secrets", self.desktop)
        self.assertEqual(self.desktop["needs"], "workflows")
        self.assertIs(self.selector, self.workflow_gate["steps"][0])
        self.assertEqual(self.workflow_gate["outputs"]["desktop_runner"], "${{ steps.desktop-route.outputs.desktop_runner }}")
        self.assertEqual(self.desktop["runs-on"],
                         "${{ needs.workflows.outputs.desktop_runner == 'metro-ci-linux-x64' "
                         "&& 'metro-ci-linux-x64' || 'ubuntu-24.04' }}")
        for job in ("rust", "containers", "workflows", "release-policy"):
            self.assertEqual(self.ci["jobs"][job]["runs-on"], HOSTED)
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
            {"run": command, "working-directory": "apps/desktop"}
            for command in ("nsh check", "nsh test", "nsh build --release")
        ])
