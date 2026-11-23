"""Exercise the package preview policy against real Git histories."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import package_smoke_policy as policy
from test_release_workflow import load, yaml


class DocsOnlyTests(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.scratch = Path(scratch.name)
        self.repo = self.scratch / "repo"
        self.repo.mkdir()
        self.git("init", "-q")
        self.git("config", "user.name", "Test")
        self.git("config", "user.email", "test@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        self.git("config", "diff.renames", "true")
        self.write("src.rs", "source\n")
        self.write("README.md", "readme\n")
        self.write("AGENTS.md", "agents\n")
        self.write("docs/acceptance.md", "doc\n")
        self.base = self.commit()

    def git(self, *args, cwd=None):
        return subprocess.run(["git", *args], cwd=cwd or self.repo, check=True,
                              capture_output=True, text=True).stdout.strip()

    def write(self, path, contents):
        target = self.repo / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(contents)

    def commit(self):
        self.git("add", "-A")
        self.git("commit", "-qm", "fixture")
        return self.git("rev-parse", "HEAD")

    def test_docs_only_from_immutable_shas_and_ancestor(self):
        self.write("AGENTS.md", "edited agents")
        self.write("docs/acceptance.md", "edited doc")
        head = self.commit()
        self.git("checkout", "--detach", self.base)
        self.write("src.rs", "main advanced independently")
        newer_base = self.commit()
        self.assertTrue(policy.docs_only(newer_base, head, root=self.repo))

    def test_code_change_or_source_deletion_never_skips(self):
        self.write("docs/acceptance.md", "edited doc")
        self.write("src.rs", "new source")
        self.assertFalse(policy.docs_only(self.base, self.commit(), root=self.repo))
        (self.repo / "src.rs").unlink()
        self.assertFalse(policy.docs_only(self.base, self.commit(), root=self.repo))

    def test_source_rename_to_docs_looks_at_both_paths(self):
        self.git("mv", "src.rs", "docs/renamed.md")
        self.assertFalse(policy.docs_only(self.base, self.commit(), root=self.repo))

    def test_narrow_allowlist_does_not_hide_assets_or_symlinks(self):
        self.write("docs/diagrams/screen.svg", "<svg/>")
        self.assertFalse(policy.docs_only(self.base, self.commit(), root=self.repo))
        self.git("checkout", "--detach", self.base)
        (self.repo / "docs/link.md").symlink_to("../src.rs")
        self.assertFalse(policy.docs_only(self.base, self.commit(), root=self.repo))

    def test_empty_missing_or_failed_diff_still_builds(self):
        self.assertFalse(policy.docs_only(self.base, self.base, root=self.repo))
        self.assertFalse(policy.docs_only(self.base, "f" * 40, root=self.repo))
        self.assertFalse(policy.docs_only(self.base, "HEAD", root=self.repo))
        self.write("README.md", "edited readme")
        head = self.commit()
        with patch.object(policy, "git", wraps=policy.git) as git:
            git.side_effect = subprocess.TimeoutExpired("git", 40)
            self.assertFalse(policy.docs_only(self.base, head, root=self.repo))

    def test_shallow_history_resolves_both_immutable_commits_or_builds(self):
        self.write("README.md", "edited readme")
        head = self.commit()
        shallow = self.scratch / "shallow"
        self.git("clone", "--quiet", "--depth=1", self.repo.as_uri(), str(shallow))
        self.assertEqual(self.git("rev-parse", "--is-shallow-repository", cwd=shallow), "true")
        self.assertTrue(policy.docs_only(self.base, head, root=shallow))
        self.git("clone", "--quiet", "--depth=1", self.repo.as_uri(), str(self.scratch / "missing"))
        missing = self.scratch / "missing"
        self.git("remote", "set-url", "origin", str(self.scratch / "not-a-repository"), cwd=missing)
        self.assertFalse(policy.docs_only(self.base, head, root=missing))

    def test_only_prs_to_literal_main_can_skip(self):
        self.write("README.md", "edited readme")
        head = self.commit()
        for kind, ref, code in (("pull_request", "main", 0), ("pull_request", "master", 1),
                                ("pull_request", "Main", 1), ("push", "", 1)):
            with self.subTest(kind=kind, ref=ref):
                env = dict(os.environ, GITHUB_EVENT_NAME=kind, GITHUB_BASE_REF=ref, PR_BASE=self.base, PR_HEAD=head)
                r = subprocess.run(["python3", "-I", str(Path(policy.__file__).resolve())], cwd=self.repo,
                                   env=env, capture_output=True, check=False)
                self.assertEqual(r.returncode, code, r.stdout + r.stderr)

    @unittest.skipIf(yaml is None and not os.environ.get("CI"), "PyYAML is not installed")
    def test_only_package_smoke_uses_the_policy_output(self):
        jobs = load("ci.yml")["jobs"]
        workflow_job = jobs["workflows"]
        self.assertEqual(workflow_job["outputs"]["docs_only"], "${{ steps.preview-policy.outputs.docs_only }}")
        step, = [step for step in workflow_job["steps"] if step.get("id") == "preview-policy"]
        self.assertIn("github.event_name == 'pull_request' && github.base_ref == 'main'", step["if"])
        self.assertEqual(step["env"], {"PR_BASE": "${{ github.event.pull_request.base.sha }}",
                                       "PR_HEAD": "${{ github.event.pull_request.head.sha }}"})
        self.assertIn("needs.workflows.outputs.docs_only != 'true'", jobs["package-smoke"]["if"])
        for name, job in jobs.items():
            if name not in {"package-smoke", "workflows"}:
                self.assertNotIn("docs_only", yaml.safe_dump(job), name)
