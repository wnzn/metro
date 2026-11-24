"""Contract tests for the release DAG in .github/workflows (ci.yml, release.yml, release-publish.yml).

They read the YAML (PyYAML; CI installs python3-yaml) and check what GitHub would
only reveal at run time: the permission ceiling of every nested reusable
workflow (validated before any `if:`), the input contracts of each call, who
sees which secret, and the order in which release jobs run.
"""

import os
from pathlib import Path
import re
import unittest

try:
    import yaml
except ImportError:  # pragma: no cover
    yaml = None

import macos_release

ROOT = Path(__file__).resolve().parent.parent
WF = ROOT / ".github/workflows"
LEVEL = {"none": 0, "read": 1, "write": 2}
SCOPES = ("actions", "attestations", "checks", "contents", "deployments", "id-token", "issues", "discussions",
          "packages", "pages", "pull-requests", "security-events", "statuses")
MAIN_IF = """github.event_name == 'push' && github.repository == 'wnzn/metro' && github.ref == 'refs/heads/main'
&& github.repository_id == '1467654868' && github.ref_protected
&& github.event.deleted == false && github.event.forced == false && github.event.after == github.sha
&& github.workflow_ref == 'wnzn/metro/.github/workflows/ci.yml@refs/heads/main'"""


def load(name):
    data = yaml.safe_load((WF / name).read_text())
    data["on"] = data.pop(True, data.get("on"))
    return data


def perms(value, inherited):
    """Effective permission map for a `permissions:` value (None = inherit)."""
    if value is None:
        return dict(inherited)
    if value in ("read-all", "write-all"):
        return {s: value.split("-")[0] for s in SCOPES}
    return {s: (value or {}).get(s, "none") for s in SCOPES}


def exceeds(requested, allowed):
    return sorted(s for s in SCOPES if LEVEL[requested.get(s, "none")] > LEVEL[allowed.get(s, "none")])


def steps_text(job):
    return "\n".join(str(s.get("run", "")) + str(s.get("with", "")) + str(s.get("env", ""))
                     for s in job.get("steps", []))


def uploads(job):
    return [s["with"]["name"] for s in job.get("steps", []) if "upload-artifact" in str(s.get("uses", ""))]


def downloads(job):
    return [s.get("with", {}) for s in job.get("steps", []) if "download-artifact" in str(s.get("uses", ""))]


def matches_conjunction(expression, context):
    # Evaluate the actual YAML guard; intentionally accepts only simple
    # conjunctions. Actions string comparisons are case-insensitive, so the
    # separate executable provenance test also checks exact casing.
    def value(token):
        if token.startswith("'"):
            return token[1:-1].lower()
        if token == "false":
            return False
        out = context[token]
        return out.lower() if isinstance(out, str) else out

    def matches(term):
        words = term.split()
        if len(words) == 1:
            return bool(value(words[0]))
        left, operator, right = words
        if operator != "==":
            raise AssertionError(f"unexpected guard operator: {operator!r}")
        return value(left) == value(right)

    return all(matches(term) for term in expression.split("&&"))


@unittest.skipIf(yaml is None and not os.environ.get("CI"), "PyYAML is not installed (CI installs python3-yaml)")
class ReleaseWorkflowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if yaml is None:
            raise AssertionError("PyYAML is required in CI")
        cls.ci = load("ci.yml")
        cls.release = load("release.yml")
        cls.publish = load("release-publish.yml")

    def walk(self, caller, file, allowed, chain):
        """Recursively apply GitHub's nested permission ceiling; returns the problems found."""
        problems = []
        path = WF / file
        if not path.is_file():
            return [f"{' > '.join(chain)}: {file} does not exist"]
        wf = load(file)
        top = perms(wf.get("permissions"), allowed)
        for name, job in wf["jobs"].items():
            requested = perms(job.get("permissions"), top)
            where = " > ".join(chain + [f"{file}:{name}"])
            if job.get("permissions") is not None:
                over = exceeds(requested, allowed)
                if over:
                    problems.append(f"{where} requests {over} beyond the caller's grant")
            if str(job.get("uses", "")).startswith("./"):
                target = job["uses"].removeprefix("./.github/workflows/")
                problems += self.check_call(job, target, where)
                problems += self.walk(name, target, {s: min(requested[s], allowed[s], key=LEVEL.get)
                                                     for s in SCOPES}, chain + [f"{file}:{name}"])
        return problems

    def check_call(self, job, target, where):
        path = WF / target
        if not path.is_file():
            return []
        call = load(target)["on"]["workflow_call"] or {}
        inputs = call.get("inputs") or {}
        given = job.get("with") or {}
        problems = [f"{where}: unknown input {k!r} for {target}" for k in given if k not in inputs]
        problems += [f"{where}: missing required input {k!r} for {target}"
                     for k, v in inputs.items() if v.get("required") and k not in given]
        declared = set((call.get("secrets") or {}))
        passed = job.get("secrets") or {}
        if passed == "inherit":
            problems.append(f"{where}: secrets: inherit is not allowed")
        else:
            problems += [f"{where}: passes undeclared secret {k}" for k in passed if k not in declared]
        return problems

    def test_every_call_tree_fits_its_callers_grant(self):
        """GitHub refuses to start a run when any nested job, even one skipped by `if:`,
        requests more than its caller granted."""
        problems = []
        ci_top = perms(self.ci.get("permissions"), {s: "none" for s in SCOPES})
        for name, job in self.ci["jobs"].items():
            if str(job.get("uses", "")).startswith("./"):
                granted = perms(job.get("permissions"), ci_top)
                target = job["uses"].removeprefix("./.github/workflows/")
                problems += self.check_call(job, target, f"ci.yml:{name}")
                problems += self.walk(name, target, granted, [f"ci.yml:{name}"])
        self.assertEqual(problems, [])

    def test_pull_requests_get_a_read_only_secretless_tree(self):
        smoke = self.ci["jobs"]["package-smoke"]
        self.assertEqual(smoke["uses"], "./.github/workflows/release.yml")
        self.assertEqual(smoke["with"], {"release": False})
        self.assertEqual(smoke["permissions"], {"contents": "read"})
        self.assertNotIn("secrets", smoke)
        for name, job in self.release["jobs"].items():
            self.assertIn(job.get("permissions", {"contents": "read"}), ({"contents": "read"}, {}), name)

    def test_triggers_and_official_jobs(self):
        on = self.ci["on"]
        self.assertEqual(set(on), {"pull_request", "push"})
        self.assertEqual(on["push"], {"branches": ["main"]})
        self.assertEqual(set(self.release["on"]), {"workflow_call"})
        self.assertEqual(set(self.publish["on"]), {"workflow_call"})
        for name in ("ci.yml", "release.yml", "release-publish.yml"):
            self.assertNotIn("pull_request_target", (WF / name).read_text())
            self.assertNotIn("secrets: inherit", (WF / name).read_text())
        release, publish = self.ci["jobs"]["release"], self.ci["jobs"]["publish"]
        self.assertEqual(" ".join(self.ci["jobs"]["release-policy"]["if"].split()), " ".join(MAIN_IF.split()))
        self.assertEqual(" ".join(publish["if"].split()),
                         "needs.release-policy.outputs.release == 'true' && " + " ".join(MAIN_IF.split()))
        self.assertEqual(release["if"], "needs.release-policy.outputs.release == 'true'")
        self.assertEqual(release["with"], {"release": True, "version": "${{ needs.release-policy.outputs.version }}"})
        self.assertEqual(release["permissions"], {"contents": "read"})
        self.assertEqual(set(release["needs"]), {"rust", "desktop", "containers", "workflows", "release-policy"})
        self.assertEqual(publish["needs"], ["release-policy", "release"])
        self.assertEqual(publish["uses"], "./.github/workflows/release-publish.yml")
        self.assertEqual(publish["permissions"], {"contents": "write", "packages": "write", "id-token": "write"})
        self.assertEqual(publish["with"], {"version": "${{ needs.release-policy.outputs.version }}"})
        self.assertIs(self.ci["concurrency"]["cancel-in-progress"], False)
        self.assertIn("'metro-official-main'", self.ci["concurrency"]["group"])

    def test_real_job_guards_admit_only_eligible_official_pushes(self):
        jobs = self.ci["jobs"]
        official = {"github.event_name": "push", "github.repository": "wnzn/metro", "github.repository_id": "1467654868",
                    "github.ref": "refs/heads/main", "github.ref_protected": True,
                    "github.event.deleted": False, "github.event.forced": False,
                    "github.event.after": "a" * 40, "github.sha": "a" * 40,
                    "github.workflow_ref": "wnzn/metro/.github/workflows/ci.yml@refs/heads/main",
                    "needs.release-policy.outputs.release": "true"}
        cases = (
            ("main", {}, True),
            ("already public", {"needs.release-policy.outputs.release": "false"}, True),
            ("tag", {"github.ref": "refs/tags/v0.3.0"}, False),
            ("trunk", {"github.ref": "refs/heads/trunk"}, False),
            ("other branch", {"github.ref": "refs/heads/main-copy"}, False),
            ("PR to main", {"github.event_name": "pull_request", "github.ref": "refs/pull/1/merge"}, False),
            ("manual main", {"github.event_name": "workflow_dispatch"}, False),
            ("deleted main", {"github.event.deleted": True}, False),
            ("forced main", {"github.event.forced": True}, False),
            ("unprotected main", {"github.ref_protected": False}, False),
            ("wrong repo ID", {"github.repository_id": "123"}, False),
            ("fork", {"github.repository": "someone/metro"}, False),
            ("wrong workflow", {"github.workflow_ref": "wnzn/metro/.github/workflows/extra.yml@refs/heads/main"}, False),
            ("wrong revision", {"github.event.after": "b" * 40}, False),
        )
        for name, update, policy in cases:
            with self.subTest(name=name):
                context = official | update
                self.assertIs(matches_conjunction(jobs["release-policy"]["if"], context), policy)
                self.assertIs(matches_conjunction(jobs["publish"]["if"], context), policy and name != "already public")

    def test_apple_credentials_reach_only_macos_signing_steps(self):
        names = set(macos_release.SECRET_ENV)
        self.assertNotIn("secrets", self.release["on"]["workflow_call"])
        declared = self.publish["on"]["workflow_call"]["secrets"]
        self.assertEqual(set(declared), names)
        self.assertTrue(all(v == {"required": False} for v in declared.values()))
        self.assertNotIn("secrets", self.ci["jobs"]["release"])
        self.assertEqual(self.ci["jobs"]["publish"]["secrets"], {n: f"${{{{ secrets.{n} }}}}" for n in names})
        text = (WF / "ci.yml").read_text()
        self.assertEqual(sorted(set(re.findall(r"secrets\.([A-Z0-9_]+)", text))), sorted(names))
        for name, job in (self.release["jobs"] | self.publish["jobs"]).items():
            refs = set(re.findall(r"secrets\.([A-Z0-9_]+)", yaml.safe_dump(job)))
            if name != "macos-sign":
                self.assertEqual(refs, set(), f"{name} must not see signing secrets")
        sign = self.publish["jobs"]["macos-sign"]
        self.assertNotIn("env", {k for k in sign if k == "env" and "secrets" in str(sign[k])})
        holders = [s["name"] for s in sign["steps"] if "secrets." in str(s.get("env", ""))]
        self.assertEqual(holders, ["Signing credentials are configured (checked by name only)",
                                   "Developer ID sign, notarize, staple, re-archive; verify a fresh extraction"])
        for step in sign["steps"]:
            if "secrets." in str(step.get("env", "")):
                self.assertEqual(set(re.findall(r"secrets\.([A-Z0-9_]+)", str(step["env"]))), names)
                self.assertNotIn("set -x", step["run"])

    def test_macos_release_jobs(self):
        jobs = self.release["jobs"]
        build, sign = jobs["desktop-macos"], self.publish["jobs"]["macos-sign"]
        self.assertNotIn("if", build)  # pull requests and main still build and smoke both Macs
        self.assertEqual(uploads(build)[-1], "preview-not-verified-adhoc-${{ matrix.target }}")
        self.assertEqual(sign["if"], "needs.tag.outputs.proceed == 'true'")
        self.assertEqual(sign["needs"], "tag")
        matrix = {(i["runner"], i["target"]) for i in sign["strategy"]["matrix"]["include"]}
        self.assertEqual(matrix, {("macos-15", "macos-arm64"), ("macos-15-intel", "macos-x64")})
        self.assertEqual(matrix, {(i["runner"], i["target"]) for i in build["strategy"]["matrix"]["include"]})
        self.assertEqual([d.get("name") for d in downloads(sign)], ["preview-not-verified-adhoc-${{ matrix.target }}"])
        text = steps_text(sign)
        order = ["macos_release.py check-credentials", "macos_release.py sign", "macos_release.py verify-archive",
                 "install-metro-macos.sh", " install --archive", "macos_release.py verify-app",
                 "smoke-macos-launch.sh", " uninstall"]
        positions = [text.find(o) for o in order]
        self.assertTrue(all(p >= 0 for p in positions) and positions == sorted(positions), positions)
        self.assertEqual(uploads(sign)[-1], "dist-desktop-${{ matrix.target }}")
        self.assertIn("smoke-macos-launch.sh", steps_text(build))

    def test_only_final_outputs_are_named_dist(self):
        names = {(job, u) for job, j in self.release["jobs"].items() for u in uploads(j) if u.startswith("dist-")}
        self.assertEqual(names, {("runner-linux", "dist-runner-linux-x64"),
                                 ("desktop-linux", "dist-desktop-linux-x64")})
        self.assertEqual(uploads(self.publish["jobs"]["macos-sign"])[-1], "dist-desktop-${{ matrix.target }}")
        self.assertEqual(uploads(self.release["jobs"]["desktop-windows"])[-1], "seed-desktop-windows-x64")
        win = self.release["jobs"]["windows-distribution"]
        self.assertEqual(win["uses"], "./.github/workflows/windows-distribution.yml")
        self.assertEqual(win["with"], {"release": "${{ inputs.release }}",
                                       "version": "${{ needs.meta.outputs.version }}"})
        self.assertEqual(set(win["needs"]), {"meta", "desktop-windows"})

    def test_preview_aggregate_runs_only_in_read_only_mode(self):
        assets = self.release["jobs"]["assets"]
        self.assertEqual(set(assets["needs"]), {"meta", "runner-linux", "windows-distribution", "desktop-linux",
                                                "desktop-macos"})
        self.assertEqual(assets["if"], "${{ !inputs.release }}")
        self.assertIn("--mode smoke", steps_text(assets))
        self.assertEqual(uploads(assets), ["preview-assets"])

    def test_official_work_checks_exact_revision_before_credentials(self):
        meta = steps_text(self.release["jobs"]["meta"])
        self.assertIn("release_policy.py provenance", meta)
        sign = self.publish["jobs"]["macos-sign"]["steps"]
        guard = next(i for i, s in enumerate(sign) if "protected" in s.get("name", ""))
        first_secret = next(i for i, s in enumerate(sign) if "secrets." in str(s.get("env", "")))
        self.assertLess(guard, first_secret)
        self.assertIn("release_policy.py verify-tag", sign[guard]["run"])
        self.assertEqual(sign[guard]["env"]["EVENT_FORCED"], "${{ github.event.forced }}")
        self.assertEqual(sign[0]["with"]["ref"], "${{ github.sha }}")
        tag = self.publish["jobs"]["tag"]
        self.assertEqual(tag["permissions"], {"contents": "write"})
        self.assertEqual(tag["steps"][0]["with"]["ref"], "${{ github.sha }}")
        self.assertIn('release_policy.py tag --version "$VERSION"', steps_text(tag))
        for name, job in self.publish["jobs"].items():
            if name not in ("authorize", "windows-sign", "tag"):
                self.assertIn("tag", job["needs"] if isinstance(job["needs"], list) else [job["needs"]], name)

    def test_publication_order_and_promotion(self):
        jobs = self.publish["jobs"]
        self.assertEqual(set(jobs), {"authorize", "tag", "windows-sign", "macos-sign", "assets", "images", "sign", "publish"})
        self.assertIn('release_policy.py authorize --version "$VERSION"', steps_text(jobs["authorize"]))
        self.assertEqual(jobs["windows-sign"]["needs"], "authorize")
        self.assertEqual(jobs["windows-sign"]["if"], "needs.authorize.outputs.proceed == 'true'")
        self.assertEqual(jobs["windows-sign"]["uses"], "./.github/workflows/windows-sign.yml")
        self.assertEqual(jobs["windows-sign"]["with"], {"version": "${{ inputs.version }}"})
        self.assertEqual(jobs["windows-sign"]["permissions"], {"contents": "read", "id-token": "write"})
        self.assertEqual(jobs["tag"]["needs"], "windows-sign")
        assets = jobs["assets"]
        self.assertEqual(set(assets["needs"]), {"tag", "macos-sign"})
        self.assertNotIn("permissions", assets)
        self.assertEqual(downloads(assets), [{"pattern": "dist-*", "path": "${{ runner.temp }}/collected",
                                               "merge-multiple": True, "digest-mismatch": "error"}])
        self.assertIn("release_assets.py assemble --version \"$VERSION\" --mode release", steps_text(assets))
        self.assertEqual(uploads(assets), ["release-assets"])
        self.assertEqual(set(jobs["images"]["needs"]), {"tag", "assets"})
        self.assertEqual(jobs["images"]["uses"], "./.github/workflows/release-images.yml")
        self.assertEqual(jobs["images"]["with"], {"version": "${{ inputs.version }}", "push": True})
        self.assertEqual(jobs["images"]["permissions"], {"contents": "read", "packages": "write", "id-token": "write"})
        self.assertEqual(set(jobs["sign"]["needs"]), {"tag", "assets", "images"})
        self.assertEqual(jobs["sign"]["uses"], "./.github/workflows/sign-release-assets.yml")
        self.assertEqual(jobs["sign"]["with"], {"version": "${{ inputs.version }}", "revision": "${{ github.sha }}"})
        self.assertEqual(jobs["sign"]["permissions"], {"contents": "read", "id-token": "write"})
        self.assertEqual(set(jobs["publish"]["needs"]), {"tag", "assets", "images", "sign"})
        self.assertEqual(jobs["publish"]["permissions"], {"contents": "write"})
        self.assertEqual(self.publish.get("permissions"), {"contents": "read"})
        draft = jobs["publish"]
        self.assertEqual([d["name"] for d in downloads(draft)], ["signed-release-assets"])
        text = steps_text(draft)
        self.assertIn("install_cosign.sh", text)
        self.assertIn('release_signatures.py verify --version "$VERSION"', text)
        self.assertIn('--revision "$GITHUB_SHA"', text)
        self.assertIn("release_policy.py publish", text)
        self.assertLess(text.find("release_signatures.py verify"), text.find("release_policy.py publish"))
        self.assertEqual(draft["steps"][0]["with"]["ref"], "${{ github.sha }}")

    def test_every_desktop_build_ships_and_runs_the_terminal_client(self):
        jobs = self.release["jobs"]
        for name in ("desktop-windows", "desktop-linux", "desktop-macos"):
            text = steps_text(jobs[name])
            self.assertNotIn("steps.tui", yaml.safe_dump(jobs[name]), name)  # no optional discovery
            self.assertRegex(text, r"cargo build --locked --release --target \S+ --bin metro-desktop --bin metro-server "
                                   r"--bin metro-tui", name)
            self.assertIn("--package metro-desktop --package metro-server --package metro-tui", text, name)
            self.assertRegex(text, r'--tui "\$rel[\\/]metro-tui(\.exe)?"', name)
            self.assertIn("--help", text[text.find("--tui"):], name)
            self.assertIn("Metro conversations", text, name)
        gate = steps_text(self.publish["jobs"]["macos-sign"])
        self.assertLess(gate.find("verify-app"), gate.find("Contents/MacOS/metro-tui\" --help"))

    def test_ci_runs_the_terminal_client_in_a_real_pty(self):
        rust = self.ci["jobs"]["rust"]
        builds = [s["run"] for s in rust["steps"] if "--bin metro-runner" in str(s.get("run", ""))]
        self.assertEqual(builds, ["cargo build --locked --bin metro-server --bin metro-runner --bin metro-connector "
                                  "--bin metro-tui"])
        pty = [s for s in rust["steps"] if "pty_qa.py" in str(s.get("run", ""))]
        self.assertEqual(len(pty), 1)
        self.assertLessEqual(pty[0]["timeout-minutes"], 10)
        self.assertIn("python3 -B crates/tui/tests/pty_qa.py --bin-dir target/debug", pty[0]["run"])
        self.assertIn("timeout --kill-after=", pty[0]["run"])

    def test_signer_and_image_contracts_line_up(self):
        signer = (WF / "sign-release-assets.yml").read_text()
        self.assertIn("name: release-assets", signer)
        self.assertIn("name: release-image-digests", signer)
        self.assertIn("name: signed-release-assets", signer)
        images = (WF / "release-images.yml").read_text()
        self.assertIn("name: release-image-digests", images)


if __name__ == "__main__":
    unittest.main()
