"""Tests for image_release.py and the release-images workflow contract.

No Docker, network or registry: synthetic OCI archives and an in-process
fake registry on loopback that demands a bearer token obtained with the
registry credential (like ghcr.io).
"""

import base64
import contextlib
import gzip
import hashlib
import http.server
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import textwrap
import threading
import unittest
from urllib.parse import parse_qs, urlsplit

sys.path.insert(0, str(Path(__file__).resolve().parent))
import image_release as ir  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / ".github" / "workflows" / "release-images.yml"
SECRET = "ghs_fake_registry_token_" + "x" * 20
REV = "0123456789abcdef0123456789abcdef01234567"


def digest(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def make_archive(path, arch, user="10001:10001", nested=False, payload=b"metro", revision=REV):
    layer = gzip.compress(payload + arch.encode(), mtime=0)
    config = json.dumps({
        "architecture": arch, "os": "linux",
        "config": {"User": user, "Labels": ir.source_annotations("1.2.3", revision, "https://x/y", "refs/heads/main")},
        "rootfs": {"type": "layers", "diff_ids": [digest(payload)]},
    }).encode()
    manifest = json.dumps({
        "schemaVersion": 2, "mediaType": ir.MANIFEST_TYPES[0],
        "config": {"mediaType": "application/vnd.oci.image.config.v1+json",
                   "digest": digest(config), "size": len(config)},
        "layers": [{"mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
                    "digest": digest(layer), "size": len(layer)}],
    }).encode()
    desc = {"mediaType": ir.MANIFEST_TYPES[0], "digest": digest(manifest), "size": len(manifest)}
    blobs = [layer, config, manifest]
    if nested:
        inner = json.dumps({"schemaVersion": 2, "mediaType": ir.OCI_INDEX, "manifests": [desc]}).encode()
        blobs.append(inner)
        desc = {"mediaType": ir.OCI_INDEX, "digest": digest(inner), "size": len(inner)}
    index = json.dumps({"schemaVersion": 2, "manifests": [desc]}).encode()
    with tarfile.open(path, "w") as tar:
        def add(name, data):
            info = tarfile.TarInfo(name)
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))
        add("oci-layout", b'{"imageLayoutVersion":"1.0.0"}')
        add("index.json", index)
        for blob in blobs:
            add("blobs/sha256/" + digest(blob).split(":")[1], blob)
    return {"manifest": digest(manifest), "config": digest(config)}


class FakeRegistry(http.server.ThreadingHTTPServer):
    def __init__(self):
        super().__init__(("127.0.0.1", 0), Handler)
        self.blobs, self.manifests, self.uploads = {}, {}, {}
        self.issued = set()
        self.auth_headers = []
        self.git_objects = {}

    @property
    def host(self):
        return f"127.0.0.1:{self.server_port}"


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, status, body=b"", headers=None):
        self.send_response(status)
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def authorized(self):
        auth = self.headers.get("Authorization", "")
        self.server.auth_headers.append(auth)
        if auth.startswith("Bearer ") and auth[7:] in self.server.issued:
            return True
        self.reply(401, headers={"WWW-Authenticate":
                                 f'Bearer realm="http://{self.server.host}/token",service="fake"'})
        return False

    def body(self):
        return self.rfile.read(int(self.headers.get("Content-Length", 0)))

    def do_GET(self):
        url = urlsplit(self.path)
        if url.path.startswith("/repos/wnzn/metro/git/"):
            entry = self.server.git_objects.get(url.path)
            return self.reply(200, json.dumps(entry).encode()) if entry else self.reply(404)
        if url.path == "/token":
            q = parse_qs(url.query)
            expected = "Basic " + base64.b64encode(f"actor:{SECRET}".encode()).decode()
            pull_only = q["scope"][0].endswith(":pull")
            if self.headers.get("Authorization") != expected and not pull_only:
                return self.reply(401)
            token = os.urandom(8).hex()
            self.server.issued.add(token)
            return self.reply(200, json.dumps({"token": token}).encode())
        if not self.authorized():
            return
        if url.path == "/v2/":
            return self.reply(200, b"{}")
        m = re.fullmatch(r"/v2/(.+)/manifests/(.+)", url.path)
        if m:
            entry = self.server.manifests.get((m.group(1), m.group(2)))
            if entry is None:
                return self.reply(404)
            media, raw = entry
            return self.reply(200, raw, {"Content-Type": media, "Docker-Content-Digest": digest(raw)})
        self.reply(404)

    def do_HEAD(self):
        if not self.authorized():
            return
        m = re.fullmatch(r"/v2/(.+)/blobs/(.+)", self.path)
        self.reply(200 if m and m.group(2) in self.server.blobs else 404)

    def do_POST(self):
        if not self.authorized():
            return
        uid = os.urandom(4).hex()
        self.server.uploads[uid] = True
        name = re.fullmatch(r"/v2/(.+)/blobs/uploads/", self.path).group(1)
        self.reply(202, headers={"Location": f"/v2/{name}/blobs/uploads/{uid}?state=x"})

    def do_PUT(self):
        if not self.authorized():
            return
        url = urlsplit(self.path)
        data = self.body()
        m = re.fullmatch(r"/v2/(.+)/blobs/uploads/(\w+)", url.path)
        if m:
            want = parse_qs(url.query)["digest"][0]
            if digest(data) != want or not self.server.uploads.pop(m.group(2), None):
                return self.reply(400)
            self.server.blobs[want] = data
            return self.reply(201)
        m = re.fullmatch(r"/v2/(.+)/manifests/(.+)", url.path)
        if m:
            doc = json.loads(data)
            refs = [doc["config"]["digest"]] + [x["digest"] for x in doc["layers"]] if "config" in doc else []
            refs_m = [x["digest"] for x in doc.get("manifests", [])]
            if any(r not in self.server.blobs for r in refs) or any(
                    (m.group(1), r) not in self.server.manifests for r in refs_m):
                return self.reply(400)
            entry = (self.headers["Content-Type"], data)
            self.server.manifests[(m.group(1), m.group(2))] = entry
            self.server.manifests[(m.group(1), digest(data))] = entry
            return self.reply(201, headers={"Docker-Content-Digest": digest(data)})
        self.reply(404)


@contextlib.contextmanager
def registry():
    server = FakeRegistry()
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server
    finally:
        server.shutdown()
        server.server_close()


def run(argv):
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        code = ir.main(argv)
    return code, out.getvalue(), err.getvalue()


class ArchiveTests(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp(prefix="metro-image-test-"))
        self.addCleanup(lambda: [p.unlink() for p in self.dir.iterdir()] and self.dir.rmdir())

    def test_describe_reads_exact_digests_and_platform(self):
        for nested in (False, True):
            path = self.dir / f"a{nested}.tar"
            want = make_archive(path, "arm64", nested=nested)
            info = ir.describe(path, "linux/arm64", "10001:10001")
            self.assertEqual((info["manifest_digest"], info["config_digest"], info["platform"]),
                             (want["manifest"], want["config"], "linux/arm64"))

    def test_describe_refuses_root_wrong_platform_and_tampering(self):
        path = self.dir / "root.tar"
        make_archive(path, "amd64", user="")
        with self.assertRaisesRegex(ir.Fail, "root"):
            ir.describe(path)
        path = self.dir / "ok.tar"
        make_archive(path, "amd64")
        with self.assertRaisesRegex(ir.Fail, "expected linux/arm64"):
            ir.describe(path, "linux/arm64")
        # Replace the layer blob with different bytes under the same name.
        bad = self.dir / "bad.tar"
        with tarfile.open(path) as src, tarfile.open(bad, "w") as dst:
            for member in src.getmembers():
                data = src.extractfile(member).read()
                if member.name.startswith("blobs/") and data[:2] == b"\x1f\x8b":
                    data = gzip.compress(b"evil" + b"x" * (len(data) - 30), mtime=0)[:len(data)].ljust(len(data), b"\0")
                member.size = len(data)
                dst.addfile(member, io.BytesIO(data))
        with self.assertRaisesRegex(ir.Fail, "does not match"):
            ir.describe(bad)


class RegistryTests(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp(prefix="metro-image-test-"))
        self.addCleanup(self.cleanup)
        self.amd = self.dir / "amd.tar"
        self.arm = self.dir / "arm.tar"
        self.want = {"linux/amd64": make_archive(self.amd, "amd64"),
                     "linux/arm64": make_archive(self.arm, "arm64", nested=True)}
        os.environ["TEST_REGISTRY_TOKEN"] = SECRET

    def cleanup(self):
        os.environ.pop("TEST_REGISTRY_TOKEN", None)
        for p in self.dir.iterdir():
            p.unlink()
        self.dir.rmdir()

    def push(self, reg, *archives, version="1.2.3", repo="metro-server"):
        result = self.dir / f"{repo}.json"
        code, out, err = run(["push", "--repository", f"{reg.host}/owner/{repo}", "--allow-http-loopback",
                              "--user", "actor", "--token-env", "TEST_REGISTRY_TOKEN",
                              "--version", version, "--revision", REV, "--source", "https://x/y", "--source-ref", "refs/heads/main",
                              "--expect-platforms", "linux/amd64,linux/arm64", "--result", str(result),
                              *map(str, archives)])
        self.assertNotIn(SECRET, out + err)
        return code, err, result

    def test_push_then_verify_exact_digests_and_platforms(self):
        with registry() as reg:
            code, err, result = self.push(reg, self.amd, self.arm)
            self.assertEqual(code, 0, err)
            data = json.loads(result.read_text())
            self.assertEqual({p: v["manifest_digest"] for p, v in data["platforms"].items()},
                             {p: v["manifest"] for p, v in self.want.items()})
            media, raw = reg.manifests[("owner/metro-server", "v1.2.3")]
            self.assertEqual(digest(raw), data["index_digest"])
            index = json.loads(raw)
            self.assertEqual([m["platform"] for m in index["manifests"]],
                             [{"architecture": "amd64", "os": "linux"}, {"architecture": "arm64", "os": "linux"}])
            self.assertNotIn("latest", {ref for _, ref in reg.manifests})
            self.assertEqual(index["annotations"], {
                "org.opencontainers.image.version": "v1.2.3", "org.opencontainers.image.revision": REV,
                "org.opencontainers.image.source": "https://x/y", "dev.metro.source-ref": "refs/heads/main"})
            self.assertEqual(data["revision"], REV)
            code, out, err = run(["verify", "--repository", f"{reg.host}/owner/metro-server",
                                  "--allow-http-loopback", "--expect", str(result)])
            self.assertEqual(code, 0, err)
            # The credential only ever went to the token endpoint, never to /v2 as Basic.
            self.assertFalse(any(h.startswith("Basic") for h in reg.auth_headers))

            # Same content again is a no-op; different content never moves the tag.
            self.assertEqual(self.push(reg, self.amd, self.arm)[0], 0)
            other = self.dir / "other.tar"
            make_archive(other, "arm64", payload=b"changed")
            code, err, _ = self.push(reg, self.amd, other)
            self.assertEqual(code, 1)
            self.assertIn("refusing to move it", err)
            self.assertEqual(reg.manifests[("owner/metro-server", "v1.2.3")], (media, raw))

    def test_push_rejects_mislabelled_platform_before_registry_access(self):
        other = self.dir / "other.tar"
        make_archive(other, "arm64", revision="0" * 40)
        with registry() as reg:
            code, err, _ = self.push(reg, self.amd, other)
            self.assertEqual(code, 1)
            self.assertIn("source label org.opencontainers.image.revision differs", err)
            self.assertEqual(reg.manifests, {})
            self.assertEqual(reg.auth_headers, [])

    def test_verify_detects_missing_platform_and_wrong_digest(self):
        with registry() as reg:
            code, err, result = self.push(reg, self.amd, self.arm)
            self.assertEqual(code, 0, err)
            good = json.loads(result.read_text())
            for mutate, message in (
                (lambda d: d["platforms"].pop("linux/arm64"), "unexpected platform"),
                (lambda d: d["platforms"]["linux/amd64"].update(config_digest="sha256:" + "0" * 64),
                 "config digest differs"),
                (lambda d: d.update(index_digest="sha256:" + "1" * 64), "index digest differs"),
            ):
                data = json.loads(json.dumps(good))
                mutate(data)
                result.write_text(json.dumps(data))
                code, _, err = run(["verify", "--repository", f"{reg.host}/owner/metro-server",
                                    "--allow-http-loopback", "--expect", str(result)])
                self.assertEqual(code, 1)
                self.assertIn(message, err)

    def test_push_refuses_unexpected_platform_set_and_missing_token(self):
        with registry() as reg:
            code, err, _ = self.push(reg, self.amd)
            self.assertEqual(code, 1)
            self.assertIn("differ from the expected set", err)
            os.environ["TEST_REGISTRY_TOKEN"] = ""
            code, err, _ = self.push(reg, self.amd, self.arm)
            self.assertEqual(code, 1)
            self.assertIn("is empty", err)
            self.assertEqual(reg.manifests, {})

    def test_evidence_lists_pinned_refs_with_architectures_and_signer(self):
        with registry() as reg:
            _, _, server = self.push(reg, self.amd, self.arm)
            _, _, runner = self.push(reg, self.amd, self.arm, repo="metro-runner")
            out = self.dir / "evidence"
            code, _, err = run(["evidence", "--version", "1.2.3", "--revision", REV, "--out", str(out),
                                "--source-url", "https://github.com/wnzn/metro", "--source-ref", "refs/heads/main",
                                "--signer-identity", "https://github.com/wnzn/metro/.github/workflows/release-images.yml@refs/heads/main",
                                "--signer-issuer", "https://token.actions.githubusercontent.com",
                                str(server), str(runner)])
            self.assertEqual(code, 0, err)
            text = (out / "IMAGES.txt").read_text()
            data = json.loads((out / "images.json").read_text())
            for image in data["images"]:
                self.assertIn(f"{image['ref']}  linux/amd64,linux/arm64", text)
                self.assertRegex(image["ref"], r":v1\.2\.3@sha256:[0-9a-f]{64}$")
                for platform, desc in image["platforms"].items():
                    self.assertIn(f"{desc['ref']}  {platform}", text)
            self.assertEqual(data["signature"]["type"], "sigstore-cosign-keyless")
            self.assertIn("certificate identity: https://github.com/wnzn/metro/.github/workflows/"
                          "release-images.yml@refs/heads/main", text)
            self.assertIn(f"# Source commit: {REV}", text)
            self.assertIn(f"refs/heads/main (git tag v1.2.3 -> {REV})", text)
            self.assertIn(f"org.opencontainers.image.revision={REV}", text)
            self.assertEqual(data["source"], {"url": "https://github.com/wnzn/metro", "ref": "refs/heads/main",
                                              "tag": "v1.2.3", "revision": REV})
            self.assertEqual(data["signature"]["revision"], REV)
            for f in out.iterdir():
                f.unlink()
            out.rmdir()
            code, _, _ = run(["evidence", "--version", "1.2.3", "--revision", REV, "--out", str(out),
                              str(server)])
            self.assertIn("NOT SIGNED", (out / "IMAGES.txt").read_text())
            for f in out.iterdir():
                f.unlink()
            out.rmdir()


class PublishedSourceTests(unittest.TestCase):
    def test_version_tag_must_peel_to_triggering_commit(self):
        with registry() as reg:
            api = "/repos/wnzn/metro/git/"
            ref = api + "ref/tags/v1.2.3"
            args = ["check-tag", "--repository", "wnzn/metro", "--tag", "v1.2.3", "--commit", REV,
                    "--api-url", "http://" + reg.host, "--allow-http-loopback"]
            self.assertIn("does not exist", run(args)[2])
            annotated = "1" * 40
            tag = {"ref": "refs/tags/v1.2.3", "object": {"type": "tag", "sha": annotated}}
            reg.git_objects[ref] = tag
            reg.git_objects[api + "tags/" + annotated] = {"object": {"type": "commit", "sha": REV}}
            self.assertEqual(run(args)[0], 0)
            tag["object"] = {"type": "commit", "sha": REV}
            self.assertEqual(run(args)[0], 0)
            tag["object"]["sha"] = "2" * 40
            self.assertIn("not " + REV, run(args)[2])
            tag["object"]["type"] = "tree"
            self.assertIn("does not name a commit", run(args)[2])

    def test_signature_output_requires_exact_digest_and_signed_source(self):
        repo, ref = "ghcr.io/wnzn/metro-server", "refs/heads/main"
        source = "https://github.com/wnzn/metro"
        identity = f"{source}/.github/workflows/release-images.yml@{ref}"
        image_digest = digest(b"index")
        good = {"critical": {"identity": {"docker-reference": f"{repo}@{image_digest}"},
                             "image": {"docker-manifest-digest": image_digest}},
                "optional": ir.source_annotations("1.2.3", REV, source, ref)}
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "cosign.json"
            args = ["check-signatures", "--cosign-json", str(path), "--repository", repo, "--digest", image_digest,
                    "--revision", REV, "--version", "1.2.3", "--source-url", source, "--source-ref", ref,
                    "--identity", identity, "--issuer", "https://token.actions.githubusercontent.com",
                    "--workflow-repository", "wnzn/metro"]
            path.write_text(json.dumps([good]))
            self.assertEqual(run(args)[0], 0)
            for mutate, message in (
                (lambda d: d["critical"]["image"].update({"docker-manifest-digest": digest(b"other")}), "another digest"),
                (lambda d: d["optional"].update({"org.opencontainers.image.revision": "0" * 40}), "signed annotation"),
                (lambda d: d["optional"].pop("dev.metro.source-ref"), "signed annotation"),
                (lambda d: d["optional"].update(Subject=identity.replace(ref, "refs/tags/v1.2.3")), "certificate claim"),
            ):
                with self.subTest(message):
                    value = json.loads(json.dumps(good))
                    mutate(value)
                    path.write_text(json.dumps([value]))
                    code, _, err = run(args)
                    self.assertNotEqual(code, 0)
                    self.assertIn(message, err)


class FakeGitHub(http.server.ThreadingHTTPServer):
    def __init__(self, refs, tags):
        super().__init__(("127.0.0.1", 0), GitHubHandler)
        self.refs, self.tags, self.auth = refs, tags, []


class GitHubHandler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        self.server.auth.append(self.headers.get("Authorization"))
        m = re.fullmatch(r"/repos/wnzn/metro/git/ref/tags/(.+)", self.path)
        n = re.fullmatch(r"/repos/wnzn/metro/git/tags/(\w+)", self.path)
        if m and m.group(1) in self.server.refs:
            body = {"ref": f"refs/tags/{m.group(1)}", "object": self.server.refs[m.group(1)]}
        elif n and n.group(1) in self.server.tags:
            body = {"object": self.server.tags[n.group(1)]}
        else:
            self.send_response(404)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        data = json.dumps(body).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


class CheckTagTests(unittest.TestCase):
    OTHER = "f" * 40

    def check(self, server, tag):
        os.environ["TEST_GH_TOKEN"] = SECRET
        try:
            return run(["check-tag", "--repository", "wnzn/metro", "--tag", tag, "--commit", REV,
                        "--token-env", "TEST_GH_TOKEN", "--allow-http-loopback",
                        "--api-url", f"http://127.0.0.1:{server.server_port}"])
        finally:
            os.environ.pop("TEST_GH_TOKEN")

    def test_lightweight_and_annotated_tags_must_name_exactly_the_commit(self):
        refs = {"v1.0.0": {"type": "commit", "sha": REV},
                "v1.0.1": {"type": "tag", "sha": "a" * 40},
                "v1.0.2": {"type": "commit", "sha": self.OTHER},
                "v1.0.3": {"type": "tag", "sha": "b" * 40},
                "v1.0.4": {"type": "tree", "sha": REV}}
        tags = {"a" * 40: {"type": "commit", "sha": REV}, "b" * 40: {"type": "commit", "sha": self.OTHER}}
        server = FakeGitHub(refs, tags)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        try:
            for tag, ok, message in (("v1.0.0", True, ""), ("v1.0.1", True, ""),
                                     ("v1.0.2", False, "not " + REV), ("v1.0.3", False, "not " + REV),
                                     ("v1.0.4", False, "does not name a commit"),
                                     ("v9.9.9", False, "does not exist")):
                code, out, err = self.check(server, tag)
                self.assertEqual(code == 0, ok, (tag, err))
                self.assertIn(message, err)
                self.assertNotIn(SECRET, out + err)
            self.assertTrue(all(a == f"Bearer {SECRET}" for a in server.auth))
        finally:
            server.shutdown()
            server.server_close()

    def test_refuses_plain_http_and_bad_input(self):
        for argv in (["--api-url", "http://api.example"], ["--commit", "abc"], ["--tag", "main"]):
            base = {"--repository": "wnzn/metro", "--tag": "v1.0.0", "--commit": REV,
                    "--api-url": "https://api.github.com"}
            base[argv[0]] = argv[1]
            code, _, _ = run(["check-tag", *[x for kv in base.items() for x in kv]])
            self.assertEqual(code, 1, argv)


class CheckSignaturesTests(unittest.TestCase):
    REPO = "ghcr.io/wnzn/metro-server"
    DIGEST = "sha256:" + "7" * 64
    IDENTITY = "https://github.com/wnzn/metro/.github/workflows/release-images.yml@refs/heads/main"
    ISSUER = "https://token.actions.githubusercontent.com"

    def entry(self, keyless=True):
        # Shape of `cosign verify --output json` (cosign v3.1.4): critical names the digest,
        # optional holds the signed annotations and, for keyless, the certificate claims.
        optional = {"org.opencontainers.image.revision": REV, "org.opencontainers.image.version": "v1.2.3",
                    "org.opencontainers.image.source": "https://github.com/wnzn/metro",
                    "dev.metro.source-ref": "refs/heads/main"}
        if keyless:
            optional |= {"Issuer": self.ISSUER, "Subject": self.IDENTITY,
                         "1.3.6.1.4.1.57264.1.1": self.ISSUER, "1.3.6.1.4.1.57264.1.2": "push",
                         "1.3.6.1.4.1.57264.1.3": REV, "1.3.6.1.4.1.57264.1.5": "wnzn/metro",
                         "1.3.6.1.4.1.57264.1.6": "refs/heads/main",
                         "githubWorkflowRef": "refs/heads/main", "githubWorkflowRepository": "wnzn/metro",
                         "githubWorkflowSha": REV, "githubWorkflowTrigger": "push"}
        return {"critical": {"identity": {"docker-reference": f"{self.REPO}@{self.DIGEST}"},
                             "image": {"docker-manifest-digest": self.DIGEST},
                             "type": "https://sigstore.dev/cosign/sign/v1"},
                "optional": optional}

    def check(self, entries):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(entries, f)
        self.addCleanup(os.unlink, f.name)
        return run(["check-signatures", "--cosign-json", f.name, "--repository", self.REPO,
                    "--digest", self.DIGEST, "--revision", REV, "--version", "1.2.3",
                    "--source-url", "https://github.com/wnzn/metro", "--source-ref", "refs/heads/main",
                    "--identity", self.IDENTITY, "--issuer", self.ISSUER, "--workflow-repository", "wnzn/metro"])

    def test_accepts_exact_digest_annotations_and_claims(self):
        code, out, err = self.check([self.entry()])
        self.assertEqual(code, 0, err)
        self.assertEqual(json.loads(out)["claims_checked"],
                         ["identity", "issuer", "revision", "source_ref", "trigger", "workflow_repository"])
        # Old-style references name the repository without the digest.
        e = self.entry(keyless=False)
        e["critical"]["identity"]["docker-reference"] = self.REPO
        self.assertEqual(self.check([e])[0], 0)

    def test_rejects_any_mismatch(self):
        mutations = {
            "another digest": lambda e: e["critical"]["image"].update({"docker-manifest-digest": "sha256:" + "8" * 64}),
            "another repository": lambda e: e["critical"]["identity"].update(
                {"docker-reference": "ghcr.io/evil/metro-server"}),
            "revision annotation": lambda e: e["optional"].update({"org.opencontainers.image.revision": "0" * 40}),
            "missing ref annotation": lambda e: e["optional"].pop("dev.metro.source-ref"),
            "tag workflow ref": lambda e: e["optional"].update({"githubWorkflowRef": "refs/tags/v1.2.3"}),
            "other commit claim": lambda e: e["optional"].update({"1.3.6.1.4.1.57264.1.3": "0" * 40}),
            "other identity": lambda e: e["optional"].update(
                {"Subject": self.IDENTITY.replace("refs/heads/main", "refs/heads/dev")}),
            "other trigger": lambda e: e["optional"].update({"githubWorkflowTrigger": "workflow_dispatch"}),
            "fork repository": lambda e: e["optional"].update({"githubWorkflowRepository": "evil/metro"}),
        }
        for name, mutate in mutations.items():
            good, bad = self.entry(), self.entry()
            mutate(bad)
            code, _, err = self.check([good, bad])
            self.assertEqual(code, 1, name)
            self.assertIn("image_release:", err)
        self.assertEqual(self.check([])[0], 1)


class WorkflowContractTests(unittest.TestCase):
    """Text checks on .github/workflows/release-images.yml (no YAML parser in the stdlib)."""

    @classmethod
    def setUpClass(cls):
        cls.text = WORKFLOW.read_text()
        cls.jobs = dict(re.findall(r"(?ms)^  ([a-z-]+):\n(.*?)(?=^  [a-z-]+:\n|\Z)",
                                   cls.text.split("\njobs:\n", 1)[1]))

    def test_reusable_only_and_takes_no_secrets(self):
        head = self.text.split("\njobs:\n", 1)[0]
        self.assertIn("on:\n  workflow_call:\n", head)
        for trigger in ("push:", "pull_request", "workflow_dispatch", "schedule:"):
            self.assertNotRegex(head, rf"(?m)^  {trigger}")
        self.assertNotIn("secrets:", self.text)
        self.assertNotRegex(self.text, r"secrets\.")
        self.assertIn("\npermissions: {}\n", head)
        self.assertRegex(head, r"version:\n\s+description: .*\n\s+type: string\n\s+required: true")

    def test_least_privilege_per_job(self):
        self.assertEqual(set(self.jobs), {"guard", "build", "media", "publish", "sign"})
        for name, body in self.jobs.items():
            perms = re.search(r"(?m)^    permissions:\n((?:      .*\n)+)", body).group(1)
            granted = dict(re.findall(r"(\S+): (\S+)", perms))
            want = {"contents": "read"}
            if name in ("publish", "sign"):
                want["packages"] = "write"
            if name == "sign":
                want["id-token"] = "write"
            self.assertEqual(granted, want, name)

    def test_publishes_only_from_protected_main_of_wnzn_metro_via_ci(self):
        head = self.text.split("\njobs:\n", 1)[0]
        for needle in ("OFFICIAL_REPOSITORY: wnzn/metro\n", "OFFICIAL_REPOSITORY_ID: '1467654868'\n",
                       "OFFICIAL_REF: refs/heads/main\n",
                       "OFFICIAL_CALLER: wnzn/metro/.github/workflows/ci.yml@refs/heads/main\n"):
            self.assertIn(needle, head)
        guard = self.jobs["guard"]
        for needle in ('[ "$EVENT" = push ]', '[ "$REPOSITORY" = "$OFFICIAL_REPOSITORY" ]',
                       '[ "$REPOSITORY_ID" = "$OFFICIAL_REPOSITORY_ID" ]',
                       '[ "$REF_TYPE" = branch ]', '[ "$REF" = "$OFFICIAL_REF" ]', '[ "$REF_PROTECTED" = true ]',
                       '[ "$DELETED" = false ]', '[ "$AFTER" = "$GITHUB_SHA" ]', '[ "$CALLER" = "$OFFICIAL_CALLER" ]',
                       "REF_PROTECTED: ${{ github.ref_protected }}", "DELETED: ${{ github.event.deleted }}",
                       "CALLER: ${{ github.workflow_ref }}"):
            self.assertIn(needle, guard)
        # push: false exits before the checks; everything after the exit is the publish gate.
        self.assertLess(guard.index('[ "$PUSH" = true ] ||'), guard.index('[ "$EVENT" = push ]'))
        gate = ("if: >- inputs.push && github.event_name == 'push' && github.repository == 'wnzn/metro' && "
                "github.repository_id == '1467654868' && github.ref == 'refs/heads/main' && github.ref_type == 'branch' && "
                "github.ref_protected && toJSON(github.event.deleted) == 'false' && github.event.after == github.sha && "
                "github.workflow_ref == 'wnzn/metro/.github/workflows/ci.yml@refs/heads/main' &&")
        for job in ("publish", "sign"):
            self.assertIn(gate, " ".join(self.jobs[job].split()), job)
        # Outputs come only from the sign job, which runs only when publishing.
        self.assertEqual(re.findall(r"value: \$\{\{ jobs\.(\w+)\.", self.text), ["sign"] * 3)
        for job in ("guard", "build", "media"):
            self.assertNotIn("packages:", self.jobs[job])
            self.assertNotRegex(self.jobs[job], r"image_release\.py push|cosign|docker push|docker login")

    def test_runtime_guard_refuses_nonofficial_events(self):
        guard = self.jobs["guard"].split("        run: |\n", 1)[1].split("      - uses:", 1)[0]
        cmd = ["bash", "-c", textwrap.dedent(guard)]
        caller = "wnzn/metro/.github/workflows/ci.yml@refs/heads/main"
        env = dict(os.environ, PUSH="true", VERSION="1.2.3", EVENT="push", REPOSITORY="wnzn/metro",
                   REPOSITORY_ID="1467654868", REF="refs/heads/main", REF_TYPE="branch", REF_PROTECTED="true",
                   DELETED="false", AFTER=REV, GITHUB_SHA=REV, CALLER=caller, OFFICIAL_REPOSITORY="wnzn/metro",
                   OFFICIAL_REPOSITORY_ID="1467654868", OFFICIAL_REF="refs/heads/main", OFFICIAL_CALLER=caller)
        self.assertEqual(subprocess.run(cmd, env=env, capture_output=True).returncode, 0)
        for bad in (dict(REF="refs/tags/v1.2.3", REF_TYPE="tag"), dict(REF="refs/heads/dev"),
                    dict(REF="refs/heads/Main"), dict(EVENT="pull_request"), dict(EVENT="workflow_dispatch"),
                    dict(REPOSITORY_ID="1"), dict(REPOSITORY="WNZN/metro"), dict(REF_PROTECTED="false"),
                    dict(DELETED="true"), dict(DELETED=""), dict(AFTER="0" * 40), dict(CALLER=caller.replace("ci.yml", "other.yml"))):
            with self.subTest(bad):
                self.assertNotEqual(subprocess.run(cmd, env=env | bad, capture_output=True).returncode, 0)
                self.assertEqual(subprocess.run(cmd, env=env | bad | {"PUSH": "false"}, capture_output=True).returncode, 0)

    def test_tag_is_checked_never_created_and_names_only_the_version(self):
        guard, sign = self.jobs["guard"], self.jobs["sign"]
        check = ('check-tag --repository "$GITHUB_REPOSITORY" \\\n            --tag "v$VERSION" '
                 '--commit "$GITHUB_SHA" --token-env GH_READ_TOKEN')
        self.assertIn(check, guard)
        self.assertIn('--tag "v$VERSION" --commit "$GITHUB_SHA" --token-env GH_READ_TOKEN', sign)
        for job, write in ((self.jobs["publish"], "image_release.py push"), (sign, "cosign sign")):
            self.assertLess(job.index("check-tag"), job.index(write))
        self.assertIn('sys.argv[1] == workspace == app', guard)
        executable_lines = "\n".join(line for line in self.text.splitlines() if not line.lstrip().startswith("#"))
        for forbidden in ("git tag", "git push", "/git/refs", "gh release", "GITHUB_REF_NAME", "ref_name",
                          "refs/tags/v$VERSION:", "contents: write"):
            self.assertNotIn(forbidden, executable_lines)
        self.assertNotIn("github.ref_name", self.text)
        self.assertIn('--source-ref "$OFFICIAL_REF" --expect-platforms', self.jobs["publish"])
        self.assertIn('--source-ref "$GITHUB_REF" --builder', self.jobs["build"])

    def test_promotion_order_and_signature_binding(self):
        self.assertIn("needs: [guard, build, media]", self.jobs["publish"])
        self.assertIn("needs: [guard, publish]", self.jobs["sign"])
        self.assertIn("ubuntu-24.04-arm", self.jobs["guard"])
        self.assertIn("image_smoke.py", self.jobs["build"])
        sign = self.jobs["sign"]
        self.assertIn('identity="$source_url/.github/workflows/release-images.yml@$OFFICIAL_REF"', sign)
        self.assertIn('source_url="https://github.com/$OFFICIAL_REPOSITORY"', sign)
        signed, verified = sign.split("cosign verify", 1)
        annotations = ['-a "org.opencontainers.image.revision=$GITHUB_SHA"', '-a "org.opencontainers.image.version=v$VERSION"',
                       '-a "org.opencontainers.image.source=$source_url"', '-a "dev.metro.source-ref=$OFFICIAL_REF"']
        for a in annotations:
            self.assertIn(a, signed.split("cosign sign", 1)[1])
            self.assertIn(a, verified)
        for flag in ('--certificate-identity "$identity"', '--certificate-oidc-issuer "$issuer"',
                     '--certificate-github-workflow-repository "$OFFICIAL_REPOSITORY"',
                     '--certificate-github-workflow-ref "$OFFICIAL_REF"',
                     '--certificate-github-workflow-sha "$GITHUB_SHA"', "--certificate-github-workflow-trigger push",
                     '--output json "$repo@$digest"', "image_release.py check-signatures"):
            self.assertIn(flag, verified)
        self.assertLess(sign.index("check-signatures"), sign.index("name: release-image-digests"))
        self.assertIn('--source-ref "$OFFICIAL_REF"', sign.split("image_release.py evidence", 1)[1])
        self.assertNotIn("latest", self.text.replace("no `latest`", "").replace("No `latest`", ""))

    def test_registry_token_never_in_argv(self):
        for line in self.text.splitlines():
            if "REGISTRY_TOKEN" in line and "github.token" not in line:
                self.assertRegex(line, r"--token-env REGISTRY_TOKEN|printf '%s' \"\$REGISTRY_TOKEN\" \| docker login",
                                 line)
        for line in self.text.splitlines():
            if "docker login" in line or "cosign " in line:
                self.assertNotRegex(line, r"--password(?!-stdin)|\s-p\s|--registry-password", "credential in argv")

    def test_actions_pinned_to_commits(self):
        for ref in re.findall(r"(?m)^\s+(?:- )?uses:\s+(\S+)", self.text):
            self.assertRegex(ref, r"@[0-9a-f]{40}$")
        self.assertRegex(self.text, r"BUILDKIT_IMAGE: moby/buildkit:v[0-9.]+@sha256:[0-9a-f]{64}")
        self.assertRegex(self.text, r"COSIGN_SHA256: [0-9a-f]{64}")


@unittest.skipUnless(shutil.which("docker") and subprocess.run(
    ["docker", "compose", "version"], capture_output=True).returncode == 0, "docker compose not available")
class PrebuiltComposeTests(unittest.TestCase):
    def config(self, **env):
        base = {k: v for k, v in os.environ.items() if not k.startswith("METRO_")}
        return subprocess.run(
            ["docker", "compose", "--env-file", os.devnull, "-f", "compose.yaml", "-f", "compose.prebuilt.yaml",
             "--profile", "runner", "--profile", "reusable-runner", "config", "--format", "json"],
            cwd=ROOT, env=base | env, capture_output=True, text=True)

    def test_versioned_and_pinned_images_replace_builds(self):
        for mode, env, server, runner in (
            ("version", {"METRO_VERSION": "1.2.3"}, "ghcr.io/wnzn/metro-server:v1.2.3", "ghcr.io/wnzn/metro-runner:v1.2.3"),
            ("digest", {"METRO_SERVER_IMAGE": "r/s:v1@sha256:" + "a" * 64, "METRO_RUNNER_IMAGE": "r/r:v1@sha256:" + "b" * 64},
             "r/s:v1@sha256:" + "a" * 64, "r/r:v1@sha256:" + "b" * 64),
        ):
            with self.subTest(mode=mode):
                proc = self.config(**env)
                self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
                services = json.loads(proc.stdout)["services"]
                self.assertEqual({n: s["image"] for n, s in services.items()},
                                 {"server": server, "runner": runner, "registered-runner": runner})
                for name, service in services.items():
                    # Absent and JSON null both mean no build; even {} enables default build settings.
                    self.assertIsNone(service.get("build"), f"{name} still has a build definition")
                    self.assertEqual(service["pull_policy"], "missing", name)
                self.assertNotIn("latest", proc.stdout)

    def test_refuses_to_guess_a_version(self):
        for env, missing in (({}, "server|runner|registered-runner"),
                             ({"METRO_VERSION": ""}, "server|runner|registered-runner"),
                             ({"METRO_SERVER_IMAGE": "r/s:v1"}, "runner|registered-runner"),
                             ({"METRO_RUNNER_IMAGE": "r/r:v1"}, "server")):
            with self.subTest(env=env):
                proc = self.config(**env)
                self.assertNotEqual(proc.returncode, 0)
                self.assertRegex(proc.stderr, rf'service "(?:{missing})" has neither an image nor a build context')


if __name__ == "__main__":
    unittest.main()
