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
import threading
import unittest
from urllib.parse import parse_qs, urlsplit

sys.path.insert(0, str(Path(__file__).resolve().parent))
import image_release as ir  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / ".github" / "workflows" / "release-images.yml"
SECRET = "ghs_fake_registry_token_" + "x" * 20


def digest(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def make_archive(path, arch, user="10001:10001", nested=False, payload=b"metro"):
    layer = gzip.compress(payload + arch.encode(), mtime=0)
    config = json.dumps({
        "architecture": arch, "os": "linux",
        "config": {"User": user, "Labels": {"org.opencontainers.image.revision": "r"}},
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
                              "--version", version, "--revision", "abc", "--source", "https://x/y",
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
            code, _, err = run(["evidence", "--version", "1.2.3", "--revision", "abc", "--out", str(out),
                                "--signer-identity", "https://github.com/o/r/.github/workflows/release-images.yml@refs/tags/v1.2.3",
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
            self.assertIn("certificate identity: https://github.com/o/r/", text)
            for f in out.iterdir():
                f.unlink()
            out.rmdir()
            code, _, _ = run(["evidence", "--version", "1.2.3", "--revision", "abc", "--out", str(out),
                              str(server)])
            self.assertIn("NOT SIGNED", (out / "IMAGES.txt").read_text())
            for f in out.iterdir():
                f.unlink()
            out.rmdir()


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

    def test_tag_guard_and_promotion_order(self):
        guard = self.jobs["guard"]
        for needle in ('[ "$EVENT" = push ]', '"refs/tags/v$VERSION"', '"$REF_NAME" = "v$VERSION"',
                       '"refs/tags/v$VERSION^{commit}")" = "$GITHUB_SHA"'):
            self.assertIn(needle, guard)
        self.assertIn("needs: [guard, build, media]", self.jobs["publish"])
        self.assertIn("needs: [guard, publish]", self.jobs["sign"])
        self.assertIn("ubuntu-24.04-arm", guard)
        self.assertIn("image_smoke.py", self.jobs["build"])
        # Signed identity is this workflow at this tag; the digests file is uploaded after verification.
        sign = self.jobs["sign"]
        self.assertIn("cosign sign --yes --recursive", sign)
        self.assertIn("/.github/workflows/release-images.yml@refs/tags/v$VERSION", sign)
        self.assertLess(sign.index("cosign verify"), sign.index("name: release-image-digests"))
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
            ["docker", "compose", "-f", "compose.yaml", "-f", "compose.prebuilt.yaml",
             "--profile", "runner", "--profile", "reusable-runner", "config", "--format", "json"],
            cwd=ROOT, env=base | env, capture_output=True, text=True)

    def test_versioned_and_pinned_images_replace_builds(self):
        for env, server, runner in (
            ({"METRO_VERSION": "1.2.3"}, "ghcr.io/wnzn/metro-server:v1.2.3", "ghcr.io/wnzn/metro-runner:v1.2.3"),
            ({"METRO_SERVER_IMAGE": "r/s:v1@sha256:" + "a" * 64, "METRO_RUNNER_IMAGE": "r/r:v1@sha256:" + "b" * 64},
             "r/s:v1@sha256:" + "a" * 64, "r/r:v1@sha256:" + "b" * 64),
        ):
            proc = self.config(**env)
            self.assertEqual(proc.returncode, 0, proc.stderr[-400:])
            services = json.loads(proc.stdout)["services"]
            self.assertEqual({n: s["image"] for n, s in services.items()},
                             {"server": server, "runner": runner, "registered-runner": runner})
            self.assertFalse(any("build" in s for s in services.values()))
            self.assertNotIn("latest", proc.stdout)

    def test_refuses_to_guess_a_version(self):
        proc = self.config()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("set METRO_VERSION", proc.stderr)


if __name__ == "__main__":
    unittest.main()
