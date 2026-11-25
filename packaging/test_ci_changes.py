"""Exercise the CI segment classification (pull requests and pushes to main) against real Git histories."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import ci_changes as policy
from test_release_workflow import load, yaml


SEGMENTS = ("rust", "updates", "desktop", "containers", "packaging", "previews")
EVERYTHING = dict.fromkeys(SEGMENTS, True)
NOTHING = dict.fromkeys(SEGMENTS, False)


def only(*names):
    return {segment: segment in names for segment in SEGMENTS}


class GitFixture(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.scratch = Path(scratch.name)
        self.repo = self.scratch / "repo"
        self.repo.mkdir()
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "Test")
        self.git("config", "user.email", "test@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        self.git("config", "diff.renames", "true")
        self.write("crates/server/src/main.rs", "source\n")
        self.write("README.md", "readme\n")
        self.write("AGENTS.md", "agents\n")
        self.write("docs/acceptance.md", "doc\n")
        self.write("packaging/README.md", "packaging prose\n")
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
        self.git("commit", "-qm", "fixture", "--allow-empty")
        return self.git("rev-parse", "HEAD")

    def change(self, *paths):
        """A commit on the fixture's base that edits (or adds) each path; returns its SHA."""
        self.git("checkout", "-q", "--detach", self.base)
        for path in paths:
            self.write(path, f"edited {path}\n")
        return self.commit()

    def classify(self, *paths, push=False):
        return policy.classify(self.base, self.change(*paths), push=push, root=self.repo)


class SegmentTests(GitFixture):
    def test_packaging_readme_with_trailing_newline_is_not_exempt(self):
        for push in (False, True):
            with self.subTest(push=push):
                self.assertEqual(self.classify("packaging/README.md\n", push=push), EVERYTHING)

    def test_documentation_only_runs_nothing(self):
        for paths in (["README.md"], ["docs/acceptance.md"], ["README.md", "AGENTS.md"], ["docs/screenshots/new.png"],
                      ["docs/deploy.md", "docs/diagrams/flow.svg", "docs/a.jpg", "docs/b.jpeg", "docs/c.gif",
                       "docs/d.webp"], ["ops/github-runner/README.md", "ops/github-runner/egress/README.md"],
                      ["CONTRIBUTING.md"], ["apps/desktop/README.md"]):
            for push in (False, True):
                with self.subTest(paths=paths, push=push):
                    self.assertEqual(self.classify(*paths, push=push), NOTHING)

    def test_known_paths_no_ci_job_reads_run_nothing(self):
        # The desktop developer scripts (tsconfig excludes dev/; package.py bundles only
        # imported assets), the runner stack (check-runner-stack.yml tests it) and two dotfiles.
        for paths in (["apps/desktop/dev/screenshots/capture.sh"], ["apps/desktop/dev/brand.py", "README.md"],
                      ["apps/desktop/dev/screenshots/README.md"], ["ops/github-runner/runner.sh"],
                      ["ops/github-runner/compose.yaml", "ops/github-runner/Dockerfile"], [".gitignore", ".env.example"]):
            with self.subTest(paths=paths):
                self.assertEqual(self.classify(*paths), NOTHING)

    def test_each_location_runs_its_segments(self):
        cases = {
            "crates/server/src/main.rs": only("rust", "updates", "containers", "previews"),
            "crates/auth/README.md": only("rust", "updates", "containers", "previews"),
            "crates/server/src/admin_web/admin.browser-test.mjs": only("rust", "updates", "containers", "previews"),
            "crates/tui/tests/pty_qa.py": only("rust", "containers", "previews"),
            "Cargo.lock": only("rust", "updates", "containers", "previews"),
            "rust-toolchain.toml": only("rust", "updates", "containers", "previews"),
            "rust-toolchain": only("rust", "updates", "containers", "previews"),
            ".cargo/config.toml": only("rust", "updates", "previews"),
            "tests/e2e/smoke.py": only("rust", "updates", "containers"),
            "tests/e2e/library_reference.py": only("rust", "containers"),
            "tests/README.md": only("rust"),
            "examples/workspace/check.sh": only("rust"),
            "examples/workspace/README.md": only("rust"),
            "apps/desktop/App.tsx": only("desktop", "previews"),
            "apps/desktop/src/features/Shell.tsx": only("desktop", "previews"),
            "apps/desktop/test/fakeServer.ts": only("desktop", "previews"),
            "apps/desktop/theme/theme.json": only("desktop", "previews"),
            "apps/desktop/tsconfig.json": only("desktop", "previews"),
            "apps/desktop/src/ui/chatControls.tsx": only("desktop", "packaging", "previews"),
            "apps/desktop/src/ui/icons/new.svg": only("desktop", "packaging", "previews"),
            "apps/desktop/src/ui/icons/NOTES.md": only("packaging"),
            "apps/desktop/assets/metro.ico": only("rust", "desktop", "packaging", "previews"),
            "Dockerfile": only("containers", "previews"),
            ".dockerignore": only("containers", "previews"),
            "services/embeddinggemma2/metro_eg2/main.py": only("containers", "previews"),
            "services/embeddinggemma2/README.md": only("containers", "previews"),
            "compose.yaml": only("containers", "packaging"),
            "compose.media.yaml": only("containers", "packaging"),
            "compose.prebuilt.yaml": only("containers", "packaging"),
            "compose.provider-key-file.yaml": only("containers", "packaging"),
            "packaging/package.py": only("packaging", "previews"),
            "packaging/nsh_ci_tests.py": only("desktop", "packaging", "previews"),
            "packaging/README.md": NOTHING,
            "packaging/windows-msi/README.md": only("packaging", "previews"),
            "packaging/windows-msi/build_msi.py": only("packaging", "previews"),
            "packaging/release-signatures/release_signatures.py": only("packaging", "previews"),
            "packaging/metro-runner-container.sh": only("containers", "packaging", "previews"),
            "packaging/metro-runner-container.ps1": only("containers", "packaging", "previews"),
            ".github/workflows/release.yml": only("packaging", "previews"),
            ".github/workflows/check-runner-stack.yml": only("packaging", "previews"),
            ".github/actionlint.yaml": only("packaging", "previews"),
        }
        for path, expected in cases.items():
            for push in (False, True):
                with self.subTest(path=path, push=push):
                    self.assertEqual(self.classify(path, push=push), expected)
                    # Documentation alongside never adds a segment.
                    self.assertEqual(self.classify(path, "README.md", "docs/x.md", push=push), expected)

    def test_updater_inputs_enable_the_fixture_within_rust(self):
        cases = {
            "crates/server/src/updates.rs": only("rust", "updates", "containers", "previews"),
            "crates/runner/src/config.rs": only("rust", "updates", "containers", "previews"),
            "crates/release/trust/sigstore-trusted-root.json": only("rust", "updates", "containers", "previews"),
            "crates/model-protocol/src/lib.rs": only("rust", "updates", "containers", "previews"),
            "crates/url-policy/src/lib.rs": only("rust", "updates", "containers", "previews"),
            "tests/e2e/updates.py": only("rust", "updates", "containers"),
            "packaging/install-metro-runner-linux.sh": only("rust", "updates", "packaging", "previews"),
            "crates/connector/src/lib.rs": only("rust", "containers", "previews"),
            "tests/e2e/pages.py": only("rust", "containers"),
            "tests/e2e/agent_pages.py": only("rust", "containers"),
            "tests/e2e/messaging.py": only("rust", "containers"),
            "tests/e2e/messaging_fixture.py": only("rust", "containers"),
            "tests/e2e/meta_agent.py": only("rust", "containers"),
            "tests/e2e/web_search.py": only("rust", "containers"),
        }
        for path, expected in cases.items():
            for push in (False, True):
                with self.subTest(path=path, push=push):
                    self.assertEqual(self.classify(path, push=push), expected)

    def test_ci_yml_gitattributes_and_version_files_run_everything(self):
        for path in (".github/workflows/ci.yml", ".gitattributes", "Cargo.toml", "apps/desktop/nsh.json"):
            for push in (False, True):
                with self.subTest(path=path, push=push):
                    self.assertEqual(self.classify(path, push=push), EVERYTHING)

    def test_unknown_locations_run_everything(self):
        for path in ("scripts/build.sh", "LICENSE", "deny.toml", "docsy/x.md", "testsuite/x.py", "Docs/x.md",
                     "dev/screenshots/README.md", "compose.yaml.orig", "docs/script.py", "docs/data.json",
                     "docs/page.html", "docs/x.md.orig", "apps/web/index.ts", "apps/desktop.md.bak"):
            with self.subTest(path=path):
                self.assertEqual(self.classify("docs/acceptance.md", path), EVERYTHING)

    def test_segments_are_the_union_over_paths(self):
        self.assertEqual(self.classify("apps/desktop/App.tsx", "tests/e2e/smoke.py"),
                         only("rust", "updates", "desktop", "containers", "previews"))
        self.assertEqual(self.classify("packaging/package.py", "examples/workspace/check.sh"),
                         only("rust", "packaging", "previews"))

    def test_deletions_and_renames_count_both_paths(self):
        self.git("checkout", "-q", "--detach", self.base)
        (self.repo / "crates/server/src/main.rs").unlink()
        self.assertEqual(policy.classify(self.base, self.commit(), root=self.repo),
                         only("rust", "updates", "containers", "previews"))
        self.git("checkout", "-q", "--detach", self.base)
        self.git("mv", "crates/server/src/main.rs", "docs/renamed.md")
        self.assertEqual(policy.classify(self.base, self.commit(), root=self.repo),
                         only("rust", "updates", "containers", "previews"))
        self.git("checkout", "-q", "--detach", self.base)
        (self.repo / "tests/e2e/smoke.py").unlink()
        self.assertEqual(policy.classify(self.base, self.commit(), push=True, root=self.repo),
                         only("rust", "updates", "containers"))

    def test_executable_files_count_like_plain_files(self):
        self.git("checkout", "-q", "--detach", self.base)
        (self.repo / "tests/e2e/smoke.py").chmod(0o755)
        self.write("tests/e2e/run.sh", "#!/bin/sh\n")
        (self.repo / "tests/e2e/run.sh").chmod(0o755)
        self.assertEqual(policy.classify(self.base, self.commit(), root=self.repo), only("rust", "updates", "containers"))

    def test_symlinks_and_unreadable_diffs_run_everything(self):
        self.git("checkout", "-q", "--detach", self.base)
        (self.repo / "docs/link.md").symlink_to("../crates/server/src/main.rs")
        self.assertEqual(policy.classify(self.base, self.commit(), root=self.repo), EVERYTHING)
        self.git("checkout", "-q", "--detach", self.base)
        (self.repo / "tests/e2e/smoke.py").unlink()
        (self.repo / "tests/e2e/smoke.py").symlink_to("../../README.md")
        self.assertEqual(policy.classify(self.base, self.commit(), push=True, root=self.repo), EVERYTHING)
        for base, head in ((self.base, self.base), (self.base, "f" * 40), ("", ""), (self.base, "HEAD"),
                           ("0" * 40, self.base)):
            for push in (False, True):
                with self.subTest(base=base, head=head, push=push):
                    self.assertEqual(policy.classify(base, head, push=push, root=self.repo), EVERYTHING)
        head = self.change("README.md")
        self.assertEqual(policy.classify(self.base, head, root=self.repo), NOTHING)
        with patch.object(policy, "git", side_effect=subprocess.TimeoutExpired("git", 40)):
            self.assertEqual(policy.classify(self.base, head, root=self.repo), EVERYTHING)
            self.assertEqual(policy.classify(self.base, head, push=True, root=self.repo), EVERYTHING)

    def test_empty_commit_runs_everything(self):
        self.git("checkout", "-q", "--detach", self.base)
        self.assertEqual(policy.classify(self.base, self.commit(), push=True, root=self.repo), EVERYTHING)

    def test_pull_requests_diff_from_the_merge_base(self):
        head = self.change("AGENTS.md", "docs/acceptance.md")
        newer_base = self.change("crates/server/src/main.rs")
        self.assertEqual(policy.classify(newer_base, head, root=self.repo), NOTHING)

    def test_pushes_need_before_to_be_an_ancestor(self):
        """A rewritten history (before not an ancestor of the pushed commit) runs everything."""
        docs = self.change("README.md")
        crate = self.change("crates/server/src/main.rs")
        self.assertEqual(policy.classify(self.base, docs, push=True, root=self.repo), NOTHING)
        self.assertEqual(policy.classify(crate, docs, push=True, root=self.repo), EVERYTHING)
        # The same pair as a pull request diffs from their merge base.
        self.assertEqual(policy.classify(crate, docs, root=self.repo), NOTHING)

    def test_pushes_diff_every_commit_since_before(self):
        self.git("checkout", "-q", "--detach", self.base)
        self.write("README.md", "one")
        self.commit()
        self.write("apps/desktop/App.tsx", "two")
        self.commit()
        self.write("docs/x.md", "three")
        head = self.commit()
        self.assertEqual(policy.classify(self.base, head, push=True, root=self.repo), only("desktop", "previews"))

    def test_merge_commit_push_diffs_from_first_parent(self):
        """A merged pull request pushed to main: before is main's old tip."""
        self.git("checkout", "-q", "-b", "feature", self.base)
        self.write("docs/screenshots/a.png", "png")
        self.write("README.md", "new readme")
        self.commit()
        self.git("checkout", "-q", "main")
        self.git("merge", "-q", "--no-ff", "-m", "Merge feature", "feature")
        merge = self.git("rev-parse", "HEAD")
        self.assertEqual(policy.classify(self.base, merge, push=True, root=self.repo), NOTHING)

    def test_shallow_history_resolves_both_commits_or_runs_everything(self):
        head = self.change("README.md")
        shallow = self.scratch / "shallow"
        self.git("clone", "--quiet", "--depth=1", self.repo.as_uri(), str(shallow))
        self.assertEqual(self.git("rev-parse", "--is-shallow-repository", cwd=shallow), "true")
        self.assertEqual(policy.classify(self.base, head, push=True, root=shallow), NOTHING)
        missing = self.scratch / "missing"
        self.git("clone", "--quiet", "--depth=1", self.repo.as_uri(), str(missing))
        self.git("remote", "set-url", "origin", str(self.scratch / "not-a-repository"), cwd=missing)
        self.assertEqual(policy.classify(self.base, head, push=True, root=missing), EVERYTHING)


class EnvironmentTests(GitFixture):
    """The script as ci.yml runs it: event variables in, one line per segment out."""

    def outputs(self, **env):
        clean = {k: v for k, v in os.environ.items()
                 if not k.startswith(("GITHUB_", "PR_", "EVENT_"))}
        r = subprocess.run(["python3", "-I", str(Path(policy.__file__).resolve())], cwd=self.repo,
                           env=clean | env, capture_output=True, text=True, check=False)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        lines = r.stdout.splitlines()
        self.assertEqual([line.split("=")[0] for line in lines], list(SEGMENTS))
        self.assertIn("CI segments:", r.stderr)
        return {key: value == "true" for key, value in (line.split("=", 1) for line in lines)}

    def push(self, before, after, **overrides):
        env = {"GITHUB_EVENT_NAME": "push", "GITHUB_REF": "refs/heads/main", "GITHUB_SHA": after,
               "EVENT_BEFORE": before, "EVENT_AFTER": after, "EVENT_FORCED": "false", "EVENT_DELETED": "false"}
        return self.outputs(**(env | overrides))

    def pull_request(self, head, ref="main"):
        return self.outputs(GITHUB_EVENT_NAME="pull_request", GITHUB_BASE_REF=ref, PR_BASE=self.base, PR_HEAD=head)

    def test_readme_and_screenshot_pushes_run_nothing(self):
        for paths in (["README.md"], ["docs/screenshots/new.png"], ["README.md", "docs/screenshots/new.png"]):
            with self.subTest(paths=paths):
                self.assertEqual(self.push(self.base, self.change(*paths)), NOTHING)

    def test_pushes_and_pull_requests_get_the_same_segments(self):
        for paths, expected in ((["apps/desktop/dev/screenshots/capture.sh"], NOTHING),
                                (["docs/acceptance.md", "packaging/README.md"], NOTHING),
                                (["Cargo.toml", "packaging/README.md"], EVERYTHING),
                                (["crates/server/src/main.rs"], only("rust", "updates", "containers", "previews")),
                                (["apps/desktop/src/features/Shell.tsx"], only("desktop", "previews")),
                                (["packaging/package.py"], only("packaging", "previews")),
                                ([".github/workflows/ci.yml"], EVERYTHING),
                                (["LICENSE"], EVERYTHING)):
            head = self.change(*paths)
            with self.subTest(paths=paths):
                self.assertEqual(self.push(self.base, head), expected)
                self.assertEqual(self.pull_request(head), expected)

    def test_packaging_readme_symlink_is_not_exempt(self):
        readme = self.repo / "packaging/README.md"
        readme.unlink()
        readme.symlink_to("../README.md")
        head = self.commit()
        self.assertEqual(self.push(self.base, head), EVERYTHING)
        self.assertEqual(self.pull_request(head), EVERYTHING)

    def test_unverifiable_pushes_run_everything(self):
        docs = self.change("README.md")
        for name, before, after, overrides in (
                ("new branch", "0" * 40, docs, {}),
                ("no before", "", docs, {}),
                ("short before", self.base[:12], docs, {}),
                ("unknown before", "e" * 40, docs, {}),
                ("forced", self.base, docs, {"EVENT_FORCED": "true"}),
                ("forced unknown", self.base, docs, {"EVENT_FORCED": ""}),
                ("deleted", self.base, docs, {"EVENT_DELETED": "true"}),
                ("after is not github.sha", self.base, docs, {"EVENT_AFTER": self.base}),
                ("other branch", self.base, docs, {"GITHUB_REF": "refs/heads/feature"}),
                ("master", self.base, docs, {"GITHUB_REF": "refs/heads/master"}),
                ("tag", self.base, docs, {"GITHUB_REF": "refs/tags/v1.0.0"}),
                ("manual run", self.base, docs, {"GITHUB_EVENT_NAME": "workflow_dispatch"}),
                ("same commit", docs, docs, {})):
            with self.subTest(name=name):
                self.assertEqual(self.push(before, after, **overrides), EVERYTHING)

    def test_only_pull_requests_into_literal_main_are_classified(self):
        docs = self.change("docs/acceptance.md")
        self.assertEqual(self.pull_request(docs), NOTHING)
        for ref in ("master", "Main", "feature"):
            with self.subTest(ref=ref):
                self.assertEqual(self.pull_request(docs, ref), EVERYTHING)
        self.assertEqual(self.outputs(GITHUB_EVENT_NAME="pull_request", GITHUB_BASE_REF="main"), EVERYTHING)

    def test_a_version_bump_push_runs_every_gate(self):
        for paths in (["Cargo.toml", "apps/desktop/nsh.json"], ["Cargo.toml"], ["apps/desktop/nsh.json"],
                      ["Cargo.toml", "apps/desktop/nsh.json", "README.md", "docs/acceptance.md"]):
            with self.subTest(paths=paths):
                self.assertEqual(self.push(self.base, self.change(*paths)), EVERYTHING)


def evaluate(expression, segment, outputs, release="", cancelled=False, workflows="success", policy="success"):
    """ci.yml's segment-gated job conditions, evaluated; any other shape fails."""
    body = " ".join(expression.split()).removeprefix("${{ ").removesuffix(" }}")
    gate = (f"(needs.workflows.outputs.{segment} != 'false' || needs.release-policy.outputs.release == 'true')")
    plain = f"!cancelled() && {gate}"
    strict = f"!cancelled() && needs.workflows.result == 'success' && needs.release-policy.result != 'failure' && {gate}"
    assert body in (plain, strict), body
    runs = outputs.get(segment, "") != "false" or release == "true"
    if body == strict:
        runs = runs and workflows == "success" and policy != "failure"
    return not cancelled and runs


@unittest.skipIf(yaml is None and not os.environ.get("CI"), "PyYAML is not installed")
class WorkflowWiringTests(unittest.TestCase):
    GATED = {"rust": "rust", "containers": "containers", "packaging": "packaging", "desktop": "desktop"}

    @classmethod
    def setUpClass(cls):
        cls.jobs = load("ci.yml")["jobs"]

    def test_one_step_sets_every_segment_for_prs_and_pushes(self):
        workflow_job = self.jobs["workflows"]
        for name in SEGMENTS:
            self.assertEqual(workflow_job["outputs"][name], f"${{{{ steps.changes.outputs.{name} }}}}")
        step, = [step for step in workflow_job["steps"] if step.get("id") == "changes"]
        self.assertNotIn("if", step)  # runs for every event; the script decides
        self.assertEqual(step["env"], {"PR_BASE": "${{ github.event.pull_request.base.sha }}",
                                       "PR_HEAD": "${{ github.event.pull_request.head.sha }}",
                                       "EVENT_BEFORE": "${{ github.event.before }}",
                                       "EVENT_AFTER": "${{ github.event.after }}",
                                       "EVENT_FORCED": "${{ github.event.forced }}",
                                       "EVENT_DELETED": "${{ github.event.deleted }}"})
        self.assertEqual(step["run"], 'python3 -I packaging/ci_changes.py >> "$GITHUB_OUTPUT"')
        self.assertIs(step, workflow_job["steps"][-1])
        self.assertEqual(sum("ci_changes" in str(s.get("run", "")) for s in workflow_job["steps"]), 1)

    def test_workflows_always_runs_and_only_the_intended_jobs_read_segments(self):
        self.assertNotIn("if", self.jobs["workflows"])
        self.assertNotIn("needs", self.jobs["workflows"])
        readers = {name: {output for output in SEGMENTS if f"needs.workflows.outputs.{output}" in yaml.safe_dump(job)}
                   for name, job in self.jobs.items()}
        self.assertEqual({name: found for name, found in readers.items() if found},
                         {"rust": {"rust", "updates"}, "desktop": {"desktop"}, "containers": {"containers"},
                          "packaging": {"packaging"}, "package-smoke": {"previews"}})
        for name in self.GATED:
            self.assertEqual(self.jobs[name]["needs"], ["workflows", "release-policy"], name)
        self.assertIn("needs.workflows.outputs.previews != 'false'", self.jobs["package-smoke"]["if"])

    def test_segment_gates(self):
        for name, segment in self.GATED.items():
            condition = self.jobs[name]["if"]
            for value, runs in (("true", True), ("false", False), ("", True)):
                with self.subTest(job=name, value=value):
                    # Empty means the classifier did not answer: run.
                    self.assertIs(evaluate(condition, segment, {segment: value}), runs)
                    self.assertIs(evaluate(condition, segment, {}, policy="skipped"), True)
            self.assertFalse(evaluate(condition, segment, {segment: "true"}, cancelled=True))

    def test_a_failed_workflows_job_still_runs_rust_containers_and_packaging(self):
        for name in ("rust", "containers", "packaging"):
            with self.subTest(job=name):
                self.assertTrue(evaluate(self.jobs[name]["if"], name, {}, workflows="failure"))
        self.assertFalse(evaluate(self.jobs["desktop"]["if"], "desktop", {}, workflows="failure"))

    def test_release_runs_every_gate_whatever_the_classifier_said(self):
        for name, segment in self.GATED.items():
            with self.subTest(job=name):
                self.assertTrue(evaluate(self.jobs[name]["if"], segment, {segment: "false"}, release="true"))
                self.assertFalse(evaluate(self.jobs[name]["if"], segment, {segment: "false"}, release="false"))

    def test_readme_only_push_skips_everything_but_workflows(self):
        outputs = {segment: "false" for segment in SEGMENTS}
        for name, segment in self.GATED.items():
            with self.subTest(job=name):
                self.assertFalse(evaluate(self.jobs[name]["if"], segment, outputs, release="false"))
