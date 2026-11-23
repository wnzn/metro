"""Regular desktop CI routes the real job without granting forks runner access."""

import ast
import os
import subprocess
import unittest

from test_release_workflow import load, yaml


METRO = 1467654868
HOSTED = "ubuntu-24.04"
LOCAL = "metro-ci-linux-x64"


def route(expression, github):
    """Evaluate the simple Actions expression from the YAML, not a policy copy."""
    expression = expression.strip().removeprefix("${{").removesuffix("}}")
    tree = ast.parse(" ".join(expression.split()).replace("&&", " and ").replace("||", " or "), mode="eval")

    def evaluate(node):
        if isinstance(node, ast.Constant):
            return node.value
        if isinstance(node, ast.Name):
            return {"github": github, "true": True, "false": False}[node.id]
        if isinstance(node, ast.Attribute):
            return (evaluate(node.value) or {}).get(node.attr)
        if isinstance(node, ast.BoolOp):
            for item in node.values:
                last = evaluate(item)
                if bool(last) == isinstance(node.op, ast.Or):
                    break
            return last
        if isinstance(node, ast.Compare) and len(node.ops) == 1 and isinstance(node.ops[0], (ast.Eq, ast.NotEq)):
            a, b = evaluate(node.left), evaluate(node.comparators[0])
            if isinstance(a, str) and isinstance(b, str):
                a, b = a.lower(), b.lower()  # Actions string comparisons ignore case.
            return (a == b) == isinstance(node.ops[0], ast.Eq)
        raise AssertionError(f"Unsupported routing syntax: {ast.dump(node)}")

    return evaluate(tree.body)


def event(kind, **changes):
    context = {
        "repository": "wnzn/metro", "repository_id": str(METRO), "actor": "alice", "triggering_actor": "alice",
        "event_name": kind, "ref": "refs/heads/main" if kind == "push" else "refs/pull/1/merge",
        "ref_protected": kind == "push",
        "event": {"sender": {"login": "alice", "type": "User"}, "deleted": False, "forced": False},
    }
    if kind == "pull_request":
        context["event"]["pull_request"] = {
            "base": {"ref": "main", "repo": {"id": METRO}}, "head": {"repo": {"id": METRO}},
            "user": {"type": "User", "login": "alice"}, "author_association": "OWNER",
        }
    for dotted, value in changes.items():
        *parents, field = dotted.split("__")
        target = context
        for key in parents:
            target = target[key]
        target[field] = value
    return context


@unittest.skipIf(yaml is None and not os.environ.get("CI"), "PyYAML is not installed")
class DesktopRoutingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if yaml is None:
            raise AssertionError("PyYAML is required in CI")
        cls.ci = load("ci.yml")
        cls.desktop = cls.ci["jobs"]["desktop"]

    def test_only_protected_main_pushes_and_trusted_same_repo_prs_use_the_runner(self):
        cases = (
            (LOCAL, event("push")),
            (LOCAL, event("pull_request")),
            (LOCAL, event("pull_request", event__pull_request__author_association="MEMBER")),
            (LOCAL, event("pull_request", event__pull_request__author_association="COLLABORATOR")),
            (HOSTED, event("push", ref="refs/heads/master")),
            (HOSTED, event("push", ref_protected=False)),
            (HOSTED, event("push", event__forced=True)),
            (HOSTED, event("push", event__deleted=True)),
            (HOSTED, event("push", repository="fork/metro")),
            (HOSTED, event("push", repository_id="123")),
            (HOSTED, event("pull_request", event__pull_request__head__repo__id=123)),
            (HOSTED, event("pull_request", event__pull_request__head__repo=None)),
            (HOSTED, event("pull_request", event__pull_request__base__repo__id=123)),
            (HOSTED, event("pull_request", event__pull_request__base__ref="master")),
            (HOSTED, event("pull_request", event__pull_request__author_association="CONTRIBUTOR")),
            (HOSTED, event("pull_request", event__pull_request__user__type="Bot")),
            (HOSTED, event("pull_request", event__sender__type="Bot")),
            (HOSTED, event("pull_request", actor="dependabot[bot]", triggering_actor="dependabot[bot]",
                           event__sender__login="dependabot[bot]", event__pull_request__user__login="dependabot[bot]")),
            (HOSTED, event("pull_request", event__pull_request__user__login="someone-else")),
            (HOSTED, event("pull_request", triggering_actor="someone-else")),
            (HOSTED, event("workflow_dispatch")),
        )
        for index, (expected, context) in enumerate(cases):
            with self.subTest(case=index):
                self.assertEqual(route(self.desktop["runs-on"], context), expected)

    def test_single_read_only_desktop_job_and_one_toolchain_per_runner(self):
        self.assertEqual(self.ci["permissions"], {"contents": "read"})
        self.assertNotIn("permissions", self.desktop)
        self.assertNotIn("secrets", self.desktop)
        self.assertNotIn("needs", self.desktop)
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

    def test_case_variant_pr_base_fails_before_checkout(self):
        # The scheduler expression cannot distinguish case variants. The
        # inline preflight must reject them without loading PR code.
        script = self.desktop["steps"][0]["run"]
        env = dict(os.environ, GITHUB_REPOSITORY="wnzn/metro", GITHUB_REPOSITORY_ID=str(METRO),
                   GITHUB_EVENT_NAME="pull_request", GITHUB_BASE_REF="Main")
        result = subprocess.run(["bash", "-xc", script], env=env, text=True, capture_output=True, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("test Main = main", result.stderr)
        self.assertNotIn("sha256sum", result.stderr)
