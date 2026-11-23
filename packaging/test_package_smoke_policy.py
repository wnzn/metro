"""Exercise the package preview policy against real Git histories."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import package_smoke_policy as policy
from test_release_workflow import load, yaml


class SkipPreviewsTests(unittest.TestCase):
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
        self.write("crates/server/src/main.rs", "source\n")
        self.write("README.md", "readme\n")
        self.write("AGENTS.md", "agents\n")
        self.write("docs/acceptance.md", "doc\n")
        self.write("tests/e2e/smoke.py", "e2e\n")
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

    def skips(self, *changes):
        self.git("checkout", "-q", "--detach", self.base)
        for path in changes:
            self.write(path, f"edited {path}\n")
        return policy.skip_previews(self.base, self.commit(), root=self.repo)

    def test_docs_only_from_immutable_shas_and_ancestor(self):
        self.write("AGENTS.md", "edited agents")
        self.write("docs/acceptance.md", "edited doc")
        head = self.commit()
        self.git("checkout", "--detach", self.base)
        self.write("crates/server/src/main.rs", "main advanced independently")
        newer_base = self.commit()
        self.assertTrue(policy.skip_previews(newer_base, head, root=self.repo))

    def test_paths_no_package_reads_skip(self):
        for paths in (["docs/acceptance.md"], ["tests/e2e/smoke.py"], ["tests/e2e/new_test.py", "README.md"],
                      ["docs/diagrams/screen.svg"], ["examples/workspace/check.sh"], ["ops/github-runner/runner.sh"],
                      ["compose.yaml", "compose.media.yaml", ".env.example", ".gitignore"]):
            with self.subTest(paths=paths):
                self.assertTrue(self.skips(*paths))

    def test_build_inputs_always_build(self):
        for path in ("crates/server/src/main.rs", "crates/new/Cargo.toml", "apps/desktop/App.tsx",
                     "apps/desktop/assets/metro.ico", "packaging/package.py", "packaging/windows-msi/build_msi.py",
                     "services/embeddinggemma2/Dockerfile", ".github/workflows/release.yml", ".github/actionlint.yaml",
                     "Cargo.toml", "Cargo.lock", "Dockerfile", ".dockerignore", ".gitattributes",
                     ".cargo/config.toml", "rust-toolchain.toml", "rust-toolchain"):
            with self.subTest(path=path):
                self.assertFalse(self.skips("docs/acceptance.md", path))

    def test_unknown_locations_build(self):
        for path in ("scripts/build.sh", "LICENSE", "deny.toml", "docsy/x.md", "testsuite/x.py", "Docs/x.md",
                     "compose.yaml.orig"):
            with self.subTest(path=path):
                self.assertFalse(self.skips(path))

    def test_source_deletion_or_rename_to_docs_builds(self):
        self.write("docs/acceptance.md", "edited doc")
        (self.repo / "crates/server/src/main.rs").unlink()
        self.assertFalse(policy.skip_previews(self.base, self.commit(), root=self.repo))
        self.git("checkout", "-q", "--detach", self.base)
        self.git("mv", "crates/server/src/main.rs", "docs/renamed.md")
        self.assertFalse(policy.skip_previews(self.base, self.commit(), root=self.repo))

    def test_test_deletion_and_executable_scripts_skip(self):
        (self.repo / "tests/e2e/smoke.py").unlink()
        self.assertTrue(policy.skip_previews(self.base, self.commit(), root=self.repo))
        self.git("checkout", "-q", "--detach", self.base)
        (self.repo / "tests/e2e/smoke.py").chmod(0o755)
        self.write("tests/e2e/run.sh", "#!/bin/sh\n")
        (self.repo / "tests/e2e/run.sh").chmod(0o755)
        self.assertTrue(policy.skip_previews(self.base, self.commit(), root=self.repo))

    def test_symlinks_never_skip(self):
        (self.repo / "docs/link.md").symlink_to("../crates/server/src/main.rs")
        self.assertFalse(policy.skip_previews(self.base, self.commit(), root=self.repo))
        self.git("checkout", "-q", "--detach", self.base)
        (self.repo / "tests/e2e/smoke.py").unlink()
        (self.repo / "tests/e2e/smoke.py").symlink_to("../../README.md")
        self.assertFalse(policy.skip_previews(self.base, self.commit(), root=self.repo))

    def test_empty_missing_or_failed_diff_still_builds(self):
        self.assertFalse(policy.skip_previews(self.base, self.base, root=self.repo))
        self.assertFalse(policy.skip_previews(self.base, "f" * 40, root=self.repo))
        self.assertFalse(policy.skip_previews(self.base, "HEAD", root=self.repo))
        self.write("README.md", "edited readme")
        head = self.commit()
        with patch.object(policy, "git", wraps=policy.git) as git:
            git.side_effect = subprocess.TimeoutExpired("git", 40)
            self.assertFalse(policy.skip_previews(self.base, head, root=self.repo))

    def test_shallow_history_resolves_both_immutable_commits_or_builds(self):
        self.write("README.md", "edited readme")
        head = self.commit()
        shallow = self.scratch / "shallow"
        self.git("clone", "--quiet", "--depth=1", self.repo.as_uri(), str(shallow))
        self.assertEqual(self.git("rev-parse", "--is-shallow-repository", cwd=shallow), "true")
        self.assertTrue(policy.skip_previews(self.base, head, root=shallow))
        self.git("clone", "--quiet", "--depth=1", self.repo.as_uri(), str(self.scratch / "missing"))
        missing = self.scratch / "missing"
        self.git("remote", "set-url", "origin", str(self.scratch / "not-a-repository"), cwd=missing)
        self.assertFalse(policy.skip_previews(self.base, head, root=missing))

    def test_only_prs_to_literal_main_can_skip(self):
        self.write("README.md", "edited readme")
        self.write("tests/e2e/smoke.py", "edited e2e")
        docs = self.commit()
        self.write("crates/server/src/main.rs", "edited source")
        crate = self.commit()
        for kind, ref, head, code in (("pull_request", "main", docs, 0), ("pull_request", "main", crate, 1),
                                      ("pull_request", "master", docs, 1), ("pull_request", "Main", docs, 1),
                                      ("push", "", docs, 1), ("push", "main", docs, 1)):
            with self.subTest(kind=kind, ref=ref, head=head):
                env = dict(os.environ, GITHUB_EVENT_NAME=kind, GITHUB_BASE_REF=ref, PR_BASE=self.base, PR_HEAD=head)
                r = subprocess.run(["python3", "-I", str(Path(policy.__file__).resolve())], cwd=self.repo,
                                   env=env, capture_output=True, check=False)
                self.assertEqual(r.returncode, code, r.stdout + r.stderr)

    @unittest.skipIf(yaml is None and not os.environ.get("CI"), "PyYAML is not installed")
    def test_only_package_smoke_uses_the_policy_output(self):
        jobs = load("ci.yml")["jobs"]
        workflow_job = jobs["workflows"]
        self.assertEqual(workflow_job["outputs"]["skip_previews"], "${{ steps.preview-policy.outputs.skip_previews }}")
        step, = [step for step in workflow_job["steps"] if step.get("id") == "preview-policy"]
        self.assertIn("github.event_name == 'pull_request' && github.base_ref == 'main'", step["if"])
        self.assertEqual(step["env"], {"PR_BASE": "${{ github.event.pull_request.base.sha }}",
                                       "PR_HEAD": "${{ github.event.pull_request.head.sha }}"})
        self.assertIn("skip_previews=true", step["run"])
        self.assertIn("needs.workflows.outputs.skip_previews != 'true'", jobs["package-smoke"]["if"])
        for name, job in jobs.items():
            if name not in {"package-smoke", "workflows"}:
                self.assertNotIn("skip_previews", yaml.safe_dump(job), name)
        self.assertNotIn("docs_only", yaml.safe_dump(jobs))
