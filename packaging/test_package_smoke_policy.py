"""Exercise the PR change classification (previews, docs only, containers) against real Git histories."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import package_smoke_policy as policy
from test_release_workflow import load, yaml


class GitFixture(unittest.TestCase):
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


class SkipPreviewsTests(GitFixture):
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

    def outputs(self, kind, ref, head):
        env = dict(os.environ, GITHUB_EVENT_NAME=kind, GITHUB_BASE_REF=ref, PR_BASE=self.base, PR_HEAD=head)
        r = subprocess.run(["python3", "-I", str(Path(policy.__file__).resolve())], cwd=self.repo,
                           env=env, capture_output=True, text=True, check=False)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        lines = r.stdout.splitlines()
        self.assertEqual([line.split("=")[0] for line in lines], ["skip_previews", "docs_only", "containers"])
        return dict(line.split("=", 1) for line in lines)

    def test_only_prs_to_literal_main_are_classified(self):
        self.write("README.md", "edited readme")
        self.write("tests/e2e/smoke.py", "edited e2e")
        tests = self.commit()
        self.write("crates/server/src/main.rs", "edited source")
        crate = self.commit()
        self.git("checkout", "-q", "--detach", self.base)
        self.write("docs/acceptance.md", "edited doc")
        docs = self.commit()
        everything = {"skip_previews": "false", "docs_only": "false", "containers": "true"}
        for kind, ref, head, expected in (
                ("pull_request", "main", docs, {"skip_previews": "true", "docs_only": "true", "containers": "false"}),
                ("pull_request", "main", tests, {"skip_previews": "true", "docs_only": "false", "containers": "true"}),
                ("pull_request", "main", crate, everything),
                ("pull_request", "master", docs, everything), ("pull_request", "Main", docs, everything),
                ("push", "", docs, everything), ("push", "main", docs, everything)):
            with self.subTest(kind=kind, ref=ref, head=head):
                self.assertEqual(self.outputs(kind, ref, head), expected)


class ClassifyTests(GitFixture):
    """docs_only and containers come from the same diff as skip_previews."""

    def classify(self, *changes):
        self.git("checkout", "-q", "--detach", self.base)
        for path in changes:
            self.write(path, f"edited {path}\n")
        return policy.classify(self.base, self.commit(), root=self.repo)

    def test_documentation_only_skips_rust_containers_and_desktop(self):
        for paths in (["docs/acceptance.md"], ["README.md", "AGENTS.md"], ["docs/screenshots/new.png"],
                      ["docs/deploy.md", "docs/diagrams/flow.svg", "docs/a.jpg", "docs/b.jpeg", "docs/c.gif",
                       "docs/d.webp"],
                      ["ops/github-runner/README.md", "ops/github-runner/egress/README.md"], ["CONTRIBUTING.md"]):
            with self.subTest(paths=paths):
                self.assertEqual(self.classify(*paths),
                                 {"skip_previews": True, "docs_only": True, "containers": False})

    def test_everything_else_is_not_documentation(self):
        for path in ("tests/e2e/smoke.py", "examples/workspace/README.md", "examples/workspace/check.sh",
                     "crates/auth/README.md", "apps/desktop/README.md", "packaging/README.md",
                     "services/embeddinggemma2/README.md", ".github/workflows/README.md", "tests/README.md",
                     "dev/screenshots/README.md", "docs/script.py", "docs/data.json", "docs/page.html",
                     "docs/x.md.orig", "Docs/x.md", "ops/github-runner/runner.sh", "compose.yaml", ".env.example",
                     ".gitignore", "LICENSE", "scripts/build.sh"):
            with self.subTest(path=path):
                self.assertFalse(self.classify("docs/acceptance.md", path)["docs_only"])

    def test_test_only_change_skips_previews_but_runs_rust_and_containers(self):
        self.assertEqual(self.classify("tests/e2e/smoke.py"),
                         {"skip_previews": True, "docs_only": False, "containers": True})
        self.assertEqual(self.classify("tests/e2e/library_reference.py", "docs/acceptance.md"),
                         {"skip_previews": True, "docs_only": False, "containers": True})

    def test_container_inputs_run_the_containers_job(self):
        for path in ("Dockerfile", ".dockerignore", "compose.yaml", "compose.media.yaml", "compose.prebuilt.yaml",
                     "compose.provider-key-file.yaml", "services/embeddinggemma2/Dockerfile",
                     "services/embeddinggemma2/metro_eg2/main.py", "packaging/metro-runner-container.sh",
                     "packaging/metro-runner-container.ps1", "tests/e2e/library_reference.py",
                     "tests/e2e/library.py", "tests/e2e/new_module.py", "Cargo.toml", "Cargo.lock",
                     "rust-toolchain", "rust-toolchain.toml", ".github/workflows/ci.yml", ".gitattributes",
                     # The images compile crates/ and the job runs the resulting server and runner.
                     "crates/server/src/main.rs", "crates/runner/src/main.rs", "crates/desktop/src/main.rs",
                     # Unknown locations run it too.
                     "LICENSE", "scripts/build.sh", "dev/screenshots/capture.sh", "compose.yaml.orig",
                     "Docs/x.md"):
            with self.subTest(path=path):
                self.assertTrue(self.classify("docs/acceptance.md", path)["containers"])

    def test_paths_the_containers_job_never_reads_skip_it(self):
        for paths in (["apps/desktop/App.tsx"], ["packaging/package.py", "packaging/windows-msi/build_msi.py"],
                      [".github/workflows/release.yml", ".github/actionlint.yaml"], ["examples/workspace/check.sh"],
                      ["ops/github-runner/compose.yaml", "ops/github-runner/Dockerfile"], [".cargo/config.toml"],
                      ["tests/README.md"], [".env.example", ".gitignore", "docs/install.md"]):
            with self.subTest(paths=paths):
                self.assertFalse(self.classify(*paths)["containers"])

    def test_unreadable_or_unusual_diffs_run_everything(self):
        everything = {"skip_previews": False, "docs_only": False, "containers": True}
        self.assertEqual(policy.classify(self.base, self.base, root=self.repo), everything)
        self.assertEqual(policy.classify(self.base, "f" * 40, root=self.repo), everything)
        self.assertEqual(policy.classify("", "", root=self.repo), everything)
        self.git("checkout", "-q", "--detach", self.base)
        (self.repo / "docs/link.md").symlink_to("acceptance.md")
        self.assertEqual(policy.classify(self.base, self.commit(), root=self.repo), everything)
        self.git("checkout", "-q", "--detach", self.base)
        self.write("docs/acceptance.md", "edited doc")
        head = self.commit()
        with patch.object(policy, "git", side_effect=subprocess.TimeoutExpired("git", 40)):
            self.assertEqual(policy.classify(self.base, head, root=self.repo), everything)
        # Documentation is always a subset of what the package previews may skip.
        self.git("checkout", "-q", "--detach", self.base)
        self.git("mv", "crates/server/src/main.rs", "docs/renamed.md")
        self.assertEqual(policy.classify(self.base, self.commit(), root=self.repo), everything)


def evaluate(expression, event, outputs, workflows="success", cancelled=False):
    """The two shapes ci.yml's rust and containers conditions use; anything else fails."""
    body = " ".join(expression.split()).removeprefix("${{ ").removesuffix(" }}")
    rust = ("!cancelled() && (github.event_name == 'push' || needs.workflows.outputs.docs_only != 'true')")
    containers = ("!cancelled() && (github.event_name == 'push' || (needs.workflows.outputs.docs_only != 'true' "
                  "&& needs.workflows.outputs.containers != 'false'))")
    assert body in (rust, containers), body
    pr_runs = outputs.get("docs_only", "") != "true"
    if body == containers:
        pr_runs = pr_runs and outputs.get("containers", "") != "false"
    return not cancelled and (event == "push" or pr_runs)


@unittest.skipIf(yaml is None and not os.environ.get("CI"), "PyYAML is not installed")
class WorkflowWiringTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.jobs = load("ci.yml")["jobs"]

    def test_one_step_sets_all_three_outputs(self):
        workflow_job = self.jobs["workflows"]
        for name in ("skip_previews", "docs_only", "containers"):
            self.assertEqual(workflow_job["outputs"][name], f"${{{{ steps.change-policy.outputs.{name} }}}}")
        step, = [step for step in workflow_job["steps"] if step.get("id") == "change-policy"]
        self.assertEqual(step["if"], "github.event_name == 'pull_request' && github.base_ref == 'main'")
        self.assertEqual(step["env"], {"PR_BASE": "${{ github.event.pull_request.base.sha }}",
                                       "PR_HEAD": "${{ github.event.pull_request.head.sha }}"})
        self.assertEqual(step["run"], 'python3 -I packaging/package_smoke_policy.py >> "$GITHUB_OUTPUT"')
        self.assertEqual(sum("package_smoke_policy" in str(s.get("run", "")) for s in workflow_job["steps"]), 1)

    def test_each_output_is_read_only_where_intended(self):
        self.assertIn("needs.workflows.outputs.skip_previews != 'true'", self.jobs["package-smoke"]["if"])
        readers = {name: {output for output in ("skip_previews", "docs_only", "containers")
                          if f"needs.workflows.outputs.{output}" in yaml.safe_dump(job)}
                   for name, job in self.jobs.items()}
        self.assertEqual({name: found for name, found in readers.items() if found},
                         {"package-smoke": {"skip_previews"}, "rust": {"docs_only"},
                          "desktop": {"docs_only"}, "containers": {"docs_only", "containers"}})
        for name in ("rust", "containers"):
            self.assertEqual(self.jobs[name]["needs"], "workflows", name)
        self.assertNotIn("if", self.jobs["workflows"])
        self.assertNotIn("if", self.jobs["packaging"])
        self.assertTrue({"rust", "containers", "desktop", "workflows", "packaging"} <= set(self.jobs["release"]["needs"]))

    def test_pushes_always_run_rust_and_containers(self):
        for name in ("rust", "containers"):
            condition = self.jobs[name]["if"]
            for outputs in ({}, {"docs_only": "true", "containers": "false"}):
                for workflows in ("success", "failure", "skipped"):
                    with self.subTest(job=name, outputs=outputs, workflows=workflows):
                        self.assertTrue(evaluate(condition, "push", outputs, workflows))
            self.assertFalse(evaluate(condition, "push", {}, cancelled=True))

    def test_pull_requests_skip_only_on_an_explicit_classification(self):
        cases = (  # outputs -> (rust runs, containers runs)
            ({"skip_previews": "true", "docs_only": "true", "containers": "false"}, (False, False)),
            ({"skip_previews": "true", "docs_only": "false", "containers": "true"}, (True, True)),
            ({"skip_previews": "false", "docs_only": "false", "containers": "false"}, (True, False)),
            ({"skip_previews": "false", "docs_only": "false", "containers": "true"}, (True, True)),
            ({}, (True, True)),  # policy step skipped or workflows failed first: run everything
        )
        for outputs, (rust, containers) in cases:
            with self.subTest(outputs=outputs):
                self.assertIs(evaluate(self.jobs["rust"]["if"], "pull_request", outputs), rust)
                self.assertIs(evaluate(self.jobs["containers"]["if"], "pull_request", outputs), containers)
