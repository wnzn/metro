"""Contract for the separate, read-only self-hosted desktop workflow."""

import os
import subprocess
import unittest

from test_release_workflow import load, yaml


@unittest.skipIf(yaml is None and not os.environ.get("CI"), "PyYAML is not installed")
class SelfHostedWorkflowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if yaml is None:
            raise AssertionError("PyYAML is required in CI")
        cls.workflow = load("self-hosted-ci.yml")

    def test_only_manual_dispatch_and_no_write_grants_or_reusable_calls(self):
        workflow = self.workflow
        self.assertEqual(set(workflow["on"]), {"workflow_dispatch"})
        inputs = workflow["on"]["workflow_dispatch"]["inputs"]
        self.assertEqual(set(inputs), {"smoke"})
        self.assertEqual(inputs["smoke"]["type"], "boolean")
        self.assertIs(inputs["smoke"]["default"], False)
        self.assertEqual(workflow["permissions"], {"contents": "read"})
        self.assertEqual(set(workflow["jobs"]), {"authorize", "desktop"})
        authorize, desktop = (workflow["jobs"][name] for name in ("authorize", "desktop"))
        self.assertEqual(authorize["permissions"], {})
        self.assertNotIn("permissions", desktop)
        self.assertNotIn("secrets.", yaml.safe_dump(workflow))
        self.assertTrue(all("uses" not in job and "environment" not in job for job in workflow["jobs"].values()))
        self.assertTrue(authorize["runs-on"].startswith("ubuntu-"))
        self.assertEqual(desktop["runs-on"], "metro-ci-linux-x64")
        self.assertEqual(desktop["needs"], "authorize")
        self.assertNotIn("if", desktop)  # implicit success() avoids scheduling after a skip or failure

    def test_hosted_guard_requires_literal_identity_and_main_dispatch(self):
        authorize = self.workflow["jobs"]["authorize"]
        guard, = authorize["steps"]
        self.assertNotIn("uses", guard)
        official = {
            "GITHUB_EVENT_NAME": "workflow_dispatch", "GITHUB_REPOSITORY": "wnzn/metro",
            "GITHUB_REPOSITORY_ID": "1467654868", "GITHUB_REF": "refs/heads/main",
            "GITHUB_WORKFLOW_REF": "wnzn/metro/.github/workflows/self-hosted-ci.yml@refs/heads/main",
        }
        cases = (
            ({}, 0), ({"GITHUB_EVENT_NAME": "push"}, 1), ({"GITHUB_EVENT_NAME": "pull_request"}, 1),
            ({"GITHUB_REPOSITORY": "elsewhere/metro"}, 1), ({"GITHUB_REPOSITORY_ID": "123"}, 1),
            ({"GITHUB_REF": "refs/heads/Main"}, 1), ({"GITHUB_REF": "refs/heads/master"}, 1),
            ({"GITHUB_REF": "refs/tags/main"}, 1),
            ({"GITHUB_WORKFLOW_REF": "wnzn/metro/.github/workflows/other.yml@refs/heads/main"}, 1),
        )
        for changes, code in cases:
            with self.subTest(changes=changes):
                result = subprocess.run(["bash", "-c", guard["run"]], env=official | changes, capture_output=True,
                                        text=True, check=False)
                self.assertEqual(result.returncode, code, result.stdout + result.stderr)

    def test_dispatch_sha_and_pinned_tool_are_used_for_all_commands(self):
        steps = self.workflow["jobs"]["desktop"]["steps"]
        checkout, = [step for step in steps if "uses" in step]
        self.assertRegex(checkout["uses"], r"^actions/checkout@[0-9a-f]{40}$")
        self.assertEqual(checkout["with"], {"ref": "${{ github.sha }}", "persist-credentials": False})
        self.assertIn('test "$(git rev-parse HEAD)" = "$GITHUB_SHA"', steps[2]["run"])
        self.assertRegex(steps[0]["env"]["NSH_CLI_SHA256"], r"^[0-9a-f]{64}$")
        self.assertLess(steps[0]["run"].index("sha256sum"), steps[0]["run"].index('--version'))
        self.assertEqual([step["run"] for step in steps[3:]], [
            '"$METRO_NSH" check', '"$METRO_NSH" build --release',
            '"$METRO_NSH" test', '"$METRO_NSH" test src/features/Shell.test.tsx',
        ])
        self.assertEqual([step["working-directory"] for step in steps[3:]], ["apps/desktop"] * 4)
        self.assertEqual([step.get("if") for step in steps[3:]], [None, None, "${{ !inputs.smoke }}", "inputs.smoke"])
