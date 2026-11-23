"""Positive and negative tests for release_policy.py (who, which version, when, and how it publishes)."""

import hashlib
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest

import release_policy as rp

HERE = Path(__file__).resolve().parent
V = "0.3.0"
PREV = "0.2.0"  # the version at push.before for a bump push
SHA = "a" * 40
OTHER = "b" * 40
OFFICIAL = {
    "GITHUB_EVENT_NAME": "push", "GITHUB_REF": "refs/heads/main", "GITHUB_REPOSITORY": "wnzn/metro",
    "GITHUB_REPOSITORY_ID": "1467654868", "GITHUB_WORKFLOW_REF": "wnzn/metro/.github/workflows/ci.yml@refs/heads/main",
    "REF_PROTECTED": "true", "EVENT_DELETED": "false", "EVENT_FORCED": "false", "EVENT_AFTER": SHA,
    "EVENT_BEFORE": OTHER, "GITHUB_SHA": SHA,
}


class FakeGitHub:
    """In-memory GitHub with the same interface as rp.GitHub; records every mutation."""

    def __init__(self):
        self.tags, self.rels, self.mutations, self.next_id = {}, [], [], 1
        self.corrupt_upload = None
        self.race_tag = None
        self.lookups = 0

    def tag_commit(self, version):
        self.lookups += 1
        return self.tags.get(f"v{version}")

    def create_tag(self, version, sha):
        self.mutations.append(("tag", version, sha))
        if self.race_tag:  # someone else created it first
            self.tags.setdefault(f"v{version}", self.race_tag)
            return False
        if f"v{version}" in self.tags:
            return False
        self.tags[f"v{version}"] = sha
        return True

    def releases(self, tag):
        return [r for r in self.rels if r["tag_name"] == tag]

    def release(self, release_id):
        return next(r for r in self.rels if r["id"] == release_id)

    def public_release(self, version):
        self.lookups += 1
        return next((r for r in self.releases(f"v{version}") if not r["draft"]), None)

    def create_release(self, version, prerelease, sha):
        self.mutations.append(("create", version))
        r = {"id": self.next_id, "tag_name": f"v{version}", "draft": True, "prerelease": prerelease, "assets": [],
             "author": {"login": "github-actions[bot]"}, "body": rp.release_notes(version, sha)}
        self.next_id += 1
        self.rels.append(r)
        return r

    def delete_release(self, release_id):
        self.mutations.append(("delete", release_id))
        self.rels = [r for r in self.rels if r["id"] != release_id]

    def upload(self, release, path):
        data = path.read_bytes() + (b"!" if path.name == self.corrupt_upload else b"")
        asset = {"name": path.name, "state": "uploaded", "digest": "sha256:" + hashlib.sha256(data).hexdigest()}
        self.release(release["id"])["assets"].append(asset)
        self.mutations.append(("upload", path.name))
        return asset

    def publish(self, release_id, prerelease):
        self.mutations.append(("publish", release_id))
        r = self.release(release_id)
        r.update(draft=False, prerelease=prerelease)
        return r


def signed_folder(root, version=V):
    folder = root / "signed"
    folder.mkdir()
    for name in sorted(rp.expected_assets(version)):
        (folder / name).write_bytes(f"bytes of {name}".encode())
    return folder


def published_release(fake, folder, version=V):
    fake.rels.append({"id": 99, "tag_name": f"v{version}", "draft": False, "prerelease": False, "assets": [
        {"name": p.name, "state": "uploaded", "digest": "sha256:" + hashlib.sha256(p.read_bytes()).hexdigest()}
        for p in folder.iterdir()]})


def write_manifests(root, cargo, app):
    (root / "apps/desktop").mkdir(parents=True, exist_ok=True)
    (root / "Cargo.toml").write_text(f'[workspace]\nmembers = []\n\n[workspace.package]\nversion = "{cargo}"\n')
    (root / "apps/desktop/nsh.json").write_text(json.dumps({"version": app, "build": 2}))


def git(root, *args):
    subprocess.run(["git", "-C", str(root), "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                    "-c", "commit.gpgsign=false", *args], check=True, capture_output=True)


class VersionTests(unittest.TestCase):
    def write(self, root, cargo, app):
        write_manifests(root, cargo, app)

    def test_checked_in_version(self):
        self.assertEqual(rp.read_version()[0], json.loads((HERE.parent / "apps/desktop/nsh.json").read_text())["version"])
        for cargo, app, want in (("0.3.0", "0.3.0", ("0.3.0", False)),
                                 ("1.0.0-rc-next.1", "1.0.0-rc-next.1", ("1.0.0-rc-next.1", True))):
            with tempfile.TemporaryDirectory() as d:
                self.write(Path(d), cargo, app)
                self.assertEqual(rp.read_version(Path(d)), want)
        for cargo, app, msg in (("0.3.0", "0.3.1", "differ"), ("0.3", "0.3", "not a supported"),
                                ("0.3.0", "0.3.0-ci", "differ"), ("v0.3.0", "v0.3.0", "not a supported"),
                                ("1.2.3-01", "1.2.3-01", "not a supported"),
                                ("1.2.3+meta", "1.2.3+meta", "not a supported"),
                                ("01.2.3", "01.2.3", "not a supported")):
            with self.subTest(cargo=cargo, app=app), tempfile.TemporaryDirectory() as d:
                self.write(Path(d), cargo, app)
                with self.assertRaisesRegex(rp.PolicyError, msg):
                    rp.read_version(Path(d))

    def test_expected_assets_are_the_signers_set(self):
        names = rp.expected_assets(V)
        self.assertIn(f"Metro-{V}-windows-x64.msi", names)
        self.assertIn(f"Metro-{V}-linux-x64.tar.gz.sha256", names)
        self.assertIn(f"Metro-{V}-windows-x64.zip.sha256", names)
        self.assertFalse(any("macos" in n for n in names))  # macOS builds are off for now: Windows and Linux only
        self.assertIn("SHA256SUMS.sigstore.json", names)
        self.assertIn("IMAGES.txt", names)
        self.assertNotIn(f"Metro-{V}-windows-x64.unsigned-preview.msi", names)

    def test_prior_version_comes_from_the_push_before_commit_not_working_tree(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            self.write(root, V, V)
            subprocess.run(["git", "init", "-q", d], check=True)
            git(root, "add", "Cargo.toml", "apps/desktop/nsh.json")
            git(root, "commit", "-qm", "previous version")
            good = rp.git_head(root)
            (root / "Cargo.toml").write_text("not current TOML")
            self.assertEqual(rp.previous_version(good, root), V)
            # Mismatched manifests at before are unreadable, not an error.
            (root / "Cargo.toml").write_text('[workspace]\nmembers = []\n\n[workspace.package]\nversion = "0.4.0"\n')
            git(root, "commit", "-qam", "half a bump")
            mismatched = rp.git_head(root)
            self.assertIsNone(rp.previous_version(mismatched, root))
            # No before version: missing, all zeros (branch creation), malformed, unknown commit.
            for before in (None, "", "0" * 40, "HEAD", good.upper(), good[:12], good + "\n", OTHER):
                with self.subTest(before=before):
                    self.assertIsNone(rp.previous_version(before, root))


class ProvenanceTests(unittest.TestCase):
    def test_only_the_protected_main_push_of_ci_yml(self):
        self.assertEqual(rp.check_provenance(OFFICIAL, SHA), SHA)
        self.assertEqual((rp.BRANCH_REF, rp.CALLER),
                         ("refs/heads/main", "wnzn/metro/.github/workflows/ci.yml@refs/heads/main"))
        bad = {
            "tag push (even pointing at main)": {"GITHUB_REF": "refs/tags/v0.3.0"},
            "pull request into main": {"GITHUB_EVENT_NAME": "pull_request", "GITHUB_REF": "refs/pull/7/merge"},
            "pull_request_target": {"GITHUB_EVENT_NAME": "pull_request_target"},
            "manual run on main": {"GITHUB_EVENT_NAME": "workflow_dispatch"},
            "schedule": {"GITHUB_EVENT_NAME": "schedule"},
            "push to master (no longer the release branch)": {"GITHUB_REF": "refs/heads/master"},
            "master push run by master's ci.yml": {
                "GITHUB_REF": "refs/heads/master",
                "GITHUB_WORKFLOW_REF": "wnzn/metro/.github/workflows/ci.yml@refs/heads/master"},
            "ci.yml from master": {"GITHUB_WORKFLOW_REF": "wnzn/metro/.github/workflows/ci.yml@refs/heads/master"},
            "push to another branch": {"GITHUB_REF": "refs/heads/main-copy"},
            "fork or other repository": {"GITHUB_REPOSITORY": "someone/metro"},
            "renamed repository id": {"GITHUB_REPOSITORY_ID": "1"},
            "other top-level workflow": {"GITHUB_WORKFLOW_REF": "wnzn/metro/.github/workflows/other.yml@refs/heads/main"},
            "ci.yml from a tag": {"GITHUB_WORKFLOW_REF": "wnzn/metro/.github/workflows/ci.yml@refs/tags/v0.3.0"},
            "ci.yml, nonliteral case": {"GITHUB_WORKFLOW_REF": "wnzn/metro/.github/workflows/ci.yml@refs/heads/Main"},
            "repository, nonliteral case": {"GITHUB_REPOSITORY": "WNZN/metro"},
            "unprotected main": {"REF_PROTECTED": "false"},
            "branch deletion": {"EVENT_DELETED": "true"},
            "missing deleted flag": {"EVENT_DELETED": ""},
            "force push": {"EVENT_FORCED": "true"},
            "missing forced flag": {"EVENT_FORCED": ""},
            "nonliteral branch": {"GITHUB_REF": "refs/heads/Main"},
            "pushed commit differs": {"EVENT_AFTER": OTHER},
            "invalid sha": {"GITHUB_SHA": "HEAD", "EVENT_AFTER": "HEAD"},
        }
        for label, change in bad.items():
            with self.subTest(label), self.assertRaises(rp.PolicyError):
                rp.check_provenance({**OFFICIAL, **change}, SHA)
        with self.assertRaisesRegex(rp.PolicyError, "checked-out commit"):
            rp.check_provenance(OFFICIAL, OTHER)


class DecideTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.folder = signed_folder(Path(self.tmp.name))
        self.fake = FakeGitHub()

    def tearDown(self):
        self.tmp.cleanup()

    def test_version_bump_states(self):
        self.assertEqual(rp.decide(self.fake, V, SHA, PREV), "new")  # bump, tag missing, nothing published
        self.fake.tags[f"v{V}"] = SHA
        self.assertEqual(rp.decide(self.fake, V, SHA, PREV), "resume")  # rerun of the failed bump run
        published_release(self.fake, self.folder)
        self.assertEqual(rp.decide(self.fake, V, SHA, PREV), "done")  # rerun after it published
        with self.assertRaisesRegex(rp.PolicyError, "used version"):
            rp.decide(self.fake, V, OTHER, PREV)  # a later bump back to a published version
        with self.assertRaisesRegex(rp.PolicyError, "used version"):
            rp.decide(self.fake, V, OTHER, "0.4.0")  # a downgrade to a published version
        self.assertEqual(self.fake.mutations, [])  # deciding never changes anything

    def test_no_bump_never_releases_and_never_reads_tags_or_releases(self):
        def conflicting_tag(fake):
            fake.tags[f"v{V}"] = OTHER

        def release_without_tag(fake):
            published_release(fake, self.folder)

        def published_elsewhere(fake):
            fake.tags[f"v{V}"] = OTHER
            published_release(fake, self.folder)

        def other_asset_set(fake):
            fake.tags[f"v{V}"] = SHA
            published_release(fake, self.folder)
            fake.rels[0]["assets"].pop()

        setups = {"tag missing, nothing published": lambda fake: None, "conflicting unpublished tag": conflicting_tag,
                  "release without its tag": release_without_tag, "published at another commit": published_elsewhere,
                  "published with another asset set": other_asset_set}
        for prior, state in ((V, "unchanged"), (None, "no-previous-version")):
            for label, setup in setups.items():
                with self.subTest(prior=prior, state=label):
                    fake = FakeGitHub()
                    setup(fake)
                    self.assertEqual(rp.decide(fake, V, SHA, prior), state)
                    self.assertEqual((fake.lookups, fake.mutations), (0, []))
        self.assertEqual((rp.bump(V, V), rp.bump(V, None), rp.bump(V, PREV)), ("unchanged", "no-previous-version", None))

    def test_refusals(self):
        self.fake.tags[f"v{V}"] = OTHER
        with self.assertRaisesRegex(rp.PolicyError, "nothing is published"):
            rp.decide(self.fake, V, SHA, PREV)  # conflicting unpublished tag
        self.fake.rels.append({"id": 5, "tag_name": f"v{V}", "draft": True, "assets": []})
        with self.assertRaisesRegex(rp.PolicyError, "nothing is published"):
            rp.decide(self.fake, V, SHA, PREV)  # a draft is not a release
        fake = FakeGitHub()
        published_release(fake, self.folder)
        with self.assertRaisesRegex(rp.PolicyError, "tag is missing"):
            rp.decide(fake, V, SHA, PREV)
        fake.tags[f"v{V}"] = SHA
        fake.rels[0]["prerelease"] = True
        with self.assertRaisesRegex(rp.PolicyError, "wrong prerelease status"):
            rp.decide(fake, V, SHA, PREV)
        fake.rels[0]["prerelease"] = False
        fake.rels[0]["assets"].pop()
        with self.assertRaisesRegex(rp.PolicyError, "never overwritten"):
            rp.decide(fake, V, SHA, PREV)


class TagTests(unittest.TestCase):
    def test_created_once_never_moved(self):
        fake = FakeGitHub()
        self.assertEqual(rp.create_tag(fake, V, SHA), SHA)
        self.assertEqual(rp.create_tag(fake, V, SHA), SHA)  # rerun: no second creation
        self.assertEqual([m for m in fake.mutations if m[0] == "tag"], [("tag", V, SHA)])
        with self.assertRaisesRegex(rp.PolicyError, "never moved"):
            rp.create_tag(fake, V, OTHER)
        self.assertEqual(fake.tags[f"v{V}"], SHA)

    def test_concurrent_creator_wins_and_we_fail(self):
        fake = FakeGitHub()
        fake.race_tag = OTHER
        with self.assertRaisesRegex(rp.PolicyError, "never moved"):
            rp.create_tag(fake, V, SHA)
        self.assertEqual(fake.tags[f"v{V}"], OTHER)


class PublishTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.folder = signed_folder(Path(self.tmp.name))
        self.fake = FakeGitHub()
        self.fake.tags[f"v{V}"] = SHA

    def tearDown(self):
        self.tmp.cleanup()

    def test_stages_then_publishes_exactly_the_verified_set(self):
        self.assertEqual(rp.publish(self.fake, V, SHA, self.folder, False), "published")
        (rel,) = self.fake.rels
        self.assertFalse(rel["draft"])
        self.assertEqual({a["name"] for a in rel["assets"]}, rp.expected_assets(V))
        kinds = [m[0] for m in self.fake.mutations]
        self.assertEqual(kinds[0], "create")
        self.assertEqual(kinds[-1], "publish")  # public only after every upload was checked
        self.assertEqual(kinds.count("upload"), len(rp.expected_assets(V)))
        # A rerun after success changes nothing.
        before = list(self.fake.mutations)
        self.assertEqual(rp.publish(self.fake, V, SHA, self.folder, False), "already published")
        self.assertEqual(self.fake.mutations, before)

    def test_prerelease_is_never_latest(self):
        rp.publish(self.fake, V, SHA, self.folder, True)
        self.assertTrue(self.fake.rels[0]["prerelease"])

    def test_resume_replaces_only_its_own_draft(self):
        old = self.fake.create_release(V, False, SHA)
        unrelated = self.fake.create_release("0.2.0", False, OTHER)
        rp.publish(self.fake, V, SHA, self.folder, False)
        self.assertIn(("delete", old["id"]), self.fake.mutations)
        self.assertNotIn(("delete", unrelated["id"]), self.fake.mutations)

    def test_never_mutates_manual_or_formerly_public_draft(self):
        for overrides in ({"author": {"login": "maintainer"}}, {"body": rp.release_notes(V, OTHER)},
                          {"published_at": "2026-01-01T00:00:00Z"}):
            with self.subTest(overrides):
                fake = FakeGitHub()
                fake.tags[f"v{V}"] = SHA
                fake.create_release(V, False, SHA).update(overrides)
                writes = list(fake.mutations)
                with self.assertRaisesRegex(rp.PolicyError, "not an unpublished staging draft"):
                    rp.publish(fake, V, SHA, self.folder, False)
                self.assertEqual(fake.mutations, writes)

    def test_refusals_leave_nothing_public(self):
        self.fake.corrupt_upload = "SHA256SUMS"
        with self.assertRaisesRegex(rp.PolicyError, "other bytes"):
            rp.publish(self.fake, V, SHA, self.folder, False)
        self.assertTrue(all(r["draft"] for r in self.fake.rels))
        self.assertNotIn("publish", [m[0] for m in self.fake.mutations])

        fake = FakeGitHub()
        fake.tags[f"v{V}"] = OTHER
        with self.assertRaisesRegex(rp.PolicyError, "does not point"):
            rp.publish(fake, V, SHA, self.folder, False)
        self.assertEqual(fake.mutations, [])

        (self.folder / "extra.txt").write_text("x")
        with self.assertRaisesRegex(rp.PolicyError, "exact signed set"):
            rp.publish(FakeGitHub(), V, SHA, self.folder, False)
        (self.folder / "extra.txt").unlink()

    def test_existing_public_release_is_verified_never_overwritten(self):
        published_release(self.fake, self.folder)
        self.fake.rels[0]["assets"][0]["digest"] = "sha256:" + "0" * 64
        with self.assertRaisesRegex(rp.PolicyError, "other bytes"):
            rp.publish(self.fake, V, SHA, self.folder, False)
        self.assertEqual(self.fake.mutations, [])


class Recorder(BaseHTTPRequestHandler):
    log = []
    bodies = {}
    routes = {}

    def log_message(self, *_):
        pass

    def handle_any(self):
        length = int(self.headers.get("Content-Length") or 0)
        Recorder.bodies[self.command, self.path] = self.rfile.read(length)
        Recorder.log.append((self.command, self.path, self.headers.get("Authorization")))
        status, body, headers = Recorder.routes.get((self.command, self.path.split("?")[0]), (404, b"{}", {}))
        self.send_response(status)
        for k, v in headers.items():
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    do_GET = do_POST = do_PATCH = do_DELETE = handle_any


class ClientTests(unittest.TestCase):
    def setUp(self):
        Recorder.log, Recorder.routes, Recorder.bodies = [], {}, {}
        self.server = HTTPServer(("127.0.0.1", 0), Recorder)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.base = f"http://127.0.0.1:{self.server.server_port}"
        self.api = rp.GitHub("tok-secret", api=self.base, insecure_loopback=True)

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()

    def test_annotated_tags_are_followed_and_422_is_rechecked(self):
        Recorder.routes = {
            ("GET", "/repos/wnzn/metro/git/ref/tags/v0.3.0"): (200, json.dumps(
                {"ref": "refs/tags/v0.3.0", "object": {"type": "tag", "sha": "c" * 40}}).encode(), {}),
            ("GET", f"/repos/wnzn/metro/git/tags/{'c' * 40}"): (200, json.dumps(
                {"object": {"type": "commit", "sha": SHA}}).encode(), {}),
            ("POST", "/repos/wnzn/metro/git/refs"): (422, b'{"message":"Reference already exists"}', {}),
        }
        self.assertEqual(self.api.tag_commit(V), SHA)
        self.assertFalse(self.api.create_tag(V, SHA))
        self.assertTrue(all(auth == "Bearer tok-secret" for _, _, auth in Recorder.log))

    def test_token_never_leaves_the_api_host(self):
        with self.assertRaisesRegex(rp.PolicyError, "refusing to send the token"):
            self.api.call("GET", "https://evil.example/x")
        with self.assertRaisesRegex(rp.PolicyError, "https"):
            rp.GitHub("t", api="http://api.github.com")
        Recorder.routes = {("GET", "/repos/wnzn/metro/releases/tags/v0.3.0"): (
            302, b"", {"Location": "https://objects.example/blob?sig=1"})}
        with self.assertRaisesRegex(rp.PolicyError, "HTTP 302"):
            self.api.public_release(V)
        Recorder.routes = {}
        with self.assertRaisesRegex(rp.PolicyError, "HTTP 404") as e:
            self.api.call("GET", "/repos/wnzn/metro/releases")
        self.assertNotIn("tok-secret", str(e.exception))

    def test_release_api_pins_sha_and_only_promotes_after_draft(self):
        releases = "/repos/wnzn/metro/releases"
        Recorder.routes = {
            ("POST", releases): (201, b'{"id": 42, "draft": true}', {}),
            ("PATCH", f"{releases}/42"): (200, b'{"id": 42, "draft": false}', {}),
        }
        draft = self.api.create_release(V, True, SHA)
        self.assertTrue(draft["draft"])
        create = json.loads(Recorder.bodies["POST", releases])
        self.assertEqual((create["tag_name"], create["target_commitish"]), ("v" + V, SHA))
        self.assertEqual((create["draft"], create["prerelease"], create["make_latest"]), (True, True, "false"))
        self.assertFalse(self.api.publish(draft["id"], True)["draft"])
        patch = json.loads(Recorder.bodies["PATCH", f"{releases}/42"])
        self.assertEqual(patch, {"draft": False, "prerelease": True, "make_latest": "false"})


class CliTests(unittest.TestCase):
    def run_cli(self, *args, env=None, script=HERE / "release_policy.py"):
        base = {"PATH": os.environ["PATH"], "GH_TOKEN": "tok-secret"}
        return subprocess.run([sys.executable, "-I", str(script), *args],
                              env={**base, **(env or {})}, capture_output=True, text=True, cwd=script.parent.parent)

    def test_version(self):
        r = self.run_cli("version")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn(f"version={rp.read_version()[0]}\n", r.stdout)

    def test_official_provenance_at_the_real_checkout(self):
        head = rp.git_head()
        r = self.run_cli("provenance", env={**OFFICIAL, "GITHUB_SHA": head, "EVENT_AFTER": head})
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn(f"sha={head}\n", r.stdout)

    def test_non_official_runs_stop_before_any_network(self):
        head, version = rp.git_head(), rp.read_version()[0]
        commands = ("decide", f"authorize --version {version}", f"tag --version {version}",
                    f"verify-tag --version {version}", f"publish --version {version} --dir .")
        for change in ({"GITHUB_REF": "refs/tags/v0.2.0"}, {"GITHUB_EVENT_NAME": "pull_request"},
                       {"GITHUB_EVENT_NAME": "workflow_dispatch"}, {"GITHUB_REF": "refs/heads/master"},
                       {"GITHUB_REF": "refs/heads/master",
                        "GITHUB_WORKFLOW_REF": "wnzn/metro/.github/workflows/ci.yml@refs/heads/master"}):
            env = {**OFFICIAL, "GITHUB_SHA": head, "EVENT_AFTER": head, "EVENT_BEFORE": head, **change,
                   "GITHUB_API_URL": "http://127.0.0.1:9"}  # unreachable, and plain http is refused anyway
            for command in commands:
                r = self.run_cli(*command.split(), env=env)
                self.assertEqual(r.returncode, 1, (change, command))
                self.assertIn("not an official release run", r.stderr, (change, command))
                self.assertNotIn("tok-secret", r.stdout + r.stderr)

    def test_official_push_without_a_version_bump_is_ci_only_and_offline(self):
        head, version = rp.git_head(), rp.read_version()[0]
        for before, state in ((head, "unchanged"), ("0" * 40, "no-previous-version"), (OTHER, "no-previous-version"),
                              ("garbage", "no-previous-version"), (None, "no-previous-version")):
            env = {**OFFICIAL, "GITHUB_SHA": head, "EVENT_AFTER": head, "EVENT_BEFORE": before,
                   "GITHUB_API_URL": "http://127.0.0.1:9"}  # any GitHub call would fail ("must be https")
            if before is None:
                del env["EVENT_BEFORE"]
            with self.subTest(before=before):
                r = self.run_cli("decide", env=env)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertIn(f"release=false\nstate={state}\nversion={version}\n", r.stdout)
                for command in (f"authorize --version {version}", f"tag --version {version}"):
                    r = self.run_cli(*command.split(), env=env)
                    self.assertEqual(r.returncode, 1, command)
                    self.assertIn("did not change the version", r.stderr, command)
                    self.assertNotIn("proceed=", r.stdout, command)
                for command in (f"verify-tag --version {version}", f"publish --version {version} --dir ."):
                    self.assertIn("must be https", self.run_cli(*command.split(), env=env).stderr, command)

    def test_official_version_bump_reaches_the_github_checks(self):
        """A copy of the policy in a throwaway repository: PREV at push.before, V at the pushed commit."""
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "packaging/release-signatures").mkdir(parents=True)
            (root / "apps/desktop").mkdir(parents=True)
            script = root / "packaging/release_policy.py"
            script.write_bytes((HERE / "release_policy.py").read_bytes())
            (root / "packaging/release-signatures/release_signatures.py").write_bytes(
                (HERE / "release-signatures/release_signatures.py").read_bytes())
            subprocess.run(["git", "init", "-q", d], check=True)
            shas = []
            for version in (PREV, V):
                write_manifests(root, version, version)
                git(root, "add", "-A")
                git(root, "commit", "-qm", version)
                shas.append(rp.git_head(root))
            env = {**OFFICIAL, "GITHUB_SHA": shas[1], "EVENT_AFTER": shas[1], "EVENT_BEFORE": shas[0],
                   "GITHUB_API_URL": "http://127.0.0.1:9"}
            for command in ("decide", f"authorize --version {V}", f"tag --version {V}"):
                with self.subTest(command=command):
                    r = self.run_cli(*command.split(), env=env, script=script)
                    self.assertEqual(r.returncode, 1, r.stdout)
                    self.assertIn("must be https", r.stderr)  # past the bump gate, at the GitHub client
                    self.assertNotIn("tok-secret", r.stdout + r.stderr)
            r = self.run_cli("decide", env={**env, "EVENT_BEFORE": shas[1]}, script=script)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn("release=false\nstate=unchanged\n", r.stdout)
            r = self.run_cli("decide", env={**env, "GITHUB_REF": "refs/heads/master"}, script=script)
            self.assertIn("not an official release run", r.stderr)


if __name__ == "__main__":
    unittest.main()
