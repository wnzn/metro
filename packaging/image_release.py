#!/usr/bin/env python3
"""Build, describe, push and verify Metro's release container images.

Used by .github/workflows/release-images.yml and for local test bundles.
Standard library only. Credentials come from a named environment variable,
never from argv, and are never printed.

  build     build one component for one native platform into an OCI archive
            (plus a JSON description) with `docker buildx build`
  describe  check an OCI archive's blobs and print its manifest, config and
            platform as JSON
  load      `docker load` exactly the described image from an OCI archive, via a
            temporary copy that adds a Docker manifest.json (the classic image
            store needs it); the archive itself is never changed
  push      upload the exact blobs and manifests of one or more single-platform
            OCI archives, then publish one image index under a version tag
            (refuses to move a tag that already names different content)
  verify    fetch the tag back and check the index digest, the platform
            descriptors and every per-platform manifest digest
  check-tag         read-only: tag v<version> exists and peels to exactly one commit
                    (GitHub API; never creates or moves a tag)
  check-signatures  check `cosign verify --output json`: exact digest, signed
                    source annotations and any reported Fulcio claims
  evidence  write IMAGES.txt and images.json from verified results
"""

import argparse
import base64
import hashlib
import http.client
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile
from urllib.parse import urlencode, urljoin, urlsplit

INDEX_TYPES = (
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
)
MANIFEST_TYPES = (
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.docker.distribution.manifest.v2+json",
)
OCI_INDEX = INDEX_TYPES[0]
COMPONENTS = {
    # component: (Dockerfile target or None for the media service, image name, non-root user)
    "server": ("server", "metro-server", "10001:10001"),
    "runner": ("runner", "metro-runner", "1000:1000"),
    "media": (None, "metro-embeddinggemma2", "10001"),
}
PLATFORMS = ("linux/amd64", "linux/arm64")
DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")
VERSION = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$")
REPOSITORY = re.compile(r"^[a-z0-9.-]+(:[0-9]+)?/[a-z0-9._/-]+$")
MAX_JSON = 4 * 1024 * 1024


class Fail(Exception):
    pass


def require(condition, message):
    if not condition:
        raise Fail(message)


def sha256_bytes(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def canonical(obj):
    return json.dumps(obj, separators=(",", ":"), sort_keys=True).encode()


def source_annotations(version, revision, source, source_ref=None):
    return {
        "org.opencontainers.image.version": f"v{version}",
        "org.opencontainers.image.revision": revision,
        "org.opencontainers.image.source": source,
        **({"dev.metro.source-ref": source_ref} if source_ref else {}),
    }


# ------------------------------------------------------------------ OCI archive


class Archive:
    """A single-platform OCI image layout in a tar file (buildx type=oci)."""

    def __init__(self, path):
        self.path = Path(path)
        self.tar = tarfile.open(self.path, "r:*")
        self.members = {}
        for member in self.tar.getmembers():
            name = member.name[2:] if member.name.startswith("./") else member.name
            if member.isfile():
                self.members[name] = member

    def close(self):
        self.tar.close()

    def read(self, name, limit=MAX_JSON):
        member = self.members.get(name)
        require(member is not None, f"{self.path.name}: missing {name}")
        require(member.size <= limit, f"{self.path.name}: {name} is too large")
        return self.tar.extractfile(member).read()

    def blob_name(self, digest):
        require(DIGEST.match(digest or ""), f"{self.path.name}: invalid digest")
        return "blobs/sha256/" + digest.split(":", 1)[1]

    def blob(self, digest, limit=MAX_JSON):
        data = self.read(self.blob_name(digest), limit)
        require(sha256_bytes(data) == digest, f"{self.path.name}: blob {digest} does not match")
        return data

    def check_blob(self, digest, size):
        name = self.blob_name(digest)
        member = self.members.get(name)
        require(member is not None, f"{self.path.name}: missing blob {digest}")
        require(member.size == size, f"{self.path.name}: blob {digest} has the wrong size")
        h = hashlib.sha256()
        with self.tar.extractfile(member) as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
        require("sha256:" + h.hexdigest() == digest, f"{self.path.name}: blob {digest} does not match")

    def open_blob(self, digest):
        return self.tar.extractfile(self.members[self.blob_name(digest)])

    def resolve(self):
        """The one image manifest the layout names, as (descriptor, parsed manifest)."""
        layout = json.loads(self.read("oci-layout"))
        require(layout.get("imageLayoutVersion") == "1.0.0", f"{self.path.name}: not an OCI layout")
        index = json.loads(self.read("index.json"))
        manifests = [d for d in index.get("manifests", []) if not is_attestation(d)]
        # buildx writes either the manifest itself or a one-entry index of it.
        while len(manifests) == 1 and manifests[0].get("mediaType") in INDEX_TYPES:
            inner = json.loads(self.blob(manifests[0]["digest"]))
            manifests = [d for d in inner.get("manifests", []) if not is_attestation(d)]
        require(len(manifests) == 1, f"{self.path.name}: expected exactly one image manifest")
        desc = manifests[0]
        require(desc.get("mediaType") in MANIFEST_TYPES, f"{self.path.name}: unexpected manifest type")
        raw = self.blob(desc["digest"])
        require(len(raw) == desc.get("size"), f"{self.path.name}: manifest size mismatch")
        manifest = json.loads(raw)
        require(manifest.get("mediaType", desc["mediaType"]) == desc["mediaType"],
                f"{self.path.name}: manifest media type mismatch")
        return desc, manifest

    def docker_manifest(self):
        """The Docker-archive manifest.json for the resolved image.

        The classic (overlay2) image store of `docker load` reads only this file,
        not index.json; the containerd store reads index.json. Paths point at the
        same blobs the OCI manifest names, layers in the manifest's order.
        """
        _, manifest = self.resolve()
        return [{
            "Config": self.blob_name(manifest["config"]["digest"]),
            "RepoTags": None,
            "Layers": [self.blob_name(layer["digest"]) for layer in manifest["layers"]],
        }]

    def describe(self):
        desc, manifest = self.resolve()
        config_desc = manifest["config"]
        config_raw = self.blob(config_desc["digest"])
        require(len(config_raw) == config_desc["size"], f"{self.path.name}: config size mismatch")
        config = json.loads(config_raw)
        for layer in manifest["layers"]:
            self.check_blob(layer["digest"], layer["size"])
        platform = config["os"] + "/" + config["architecture"]
        if config.get("variant") and config["architecture"] != "arm64":
            platform += "/" + config["variant"]
        cfg = config.get("config") or {}
        return {
            "manifest_digest": desc["digest"],
            "manifest_media_type": desc["mediaType"],
            "manifest_size": desc["size"],
            "config_digest": config_desc["digest"],
            "platform": platform,
            "os": config["os"],
            "architecture": config["architecture"],
            "user": cfg.get("User", ""),
            "labels": cfg.get("Labels") or {},
            "layers": len(manifest["layers"]),
            "compressed_bytes": sum(layer["size"] for layer in manifest["layers"]) + config_desc["size"],
        }

    def blobs_of(self, manifest_digest):
        """Every blob a manifest needs, config and layers, as (digest, size, media type)."""
        manifest = json.loads(self.blob(manifest_digest))
        yield manifest["config"]["digest"], manifest["config"]["size"], manifest["config"]["mediaType"]
        for layer in manifest["layers"]:
            yield layer["digest"], layer["size"], layer["mediaType"]


def is_attestation(desc):
    annotations = desc.get("annotations") or {}
    return (annotations.get("vnd.docker.reference.type") == "attestation-manifest"
            or (desc.get("platform") or {}).get("os") == "unknown")


def describe(path, expect_platform=None, expect_user=None):
    archive = Archive(path)
    try:
        info = archive.describe()
    finally:
        archive.close()
    if expect_platform:
        require(info["platform"] == expect_platform,
                f"{Path(path).name}: platform {info['platform']}, expected {expect_platform}")
    if expect_user is not None:
        require(info["user"] == expect_user,
                f"{Path(path).name}: runs as {info['user']!r}, expected {expect_user!r}")
    user = info["user"].split(":", 1)[0]
    require(user not in ("", "0", "root"), f"{Path(path).name}: image runs as root")
    return info


# ------------------------------------------------------------------ build


def build(args):
    target, name, user = COMPONENTS[args.component]
    require(args.platform in PLATFORMS, f"unsupported platform {args.platform}")
    if args.component == "media":
        require(args.platform == "linux/amd64", "the media service is built for linux/amd64 only")
    require(VERSION.match(args.version), "version must be X.Y.Z[-pre] without a leading v")
    out = Path(args.out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    context = Path(args.context).resolve()
    if args.component == "media":
        context = context / "services" / "embeddinggemma2"
    arch = args.platform.split("/")[1]
    tar = out / f"{name}-{arch}.oci.tar"
    epoch = subprocess.run(["git", "-C", str(args.git_dir or args.context), "log", "-1", "--format=%ct",
                            args.revision], check=True, capture_output=True, text=True).stdout.strip()
    require(epoch.isdigit(), "could not read the commit time")
    cmd = ["docker", "buildx", "build"]
    if args.builder:
        cmd += ["--builder", args.builder]
    cmd += ["--platform", args.platform, "--provenance=false", "--sbom=false", "--pull=false",
            "--build-arg", f"SOURCE_DATE_EPOCH={epoch}",
            "--label", f"org.opencontainers.image.source={args.source}",
            "--label", f"org.opencontainers.image.revision={args.revision}",
            "--label", f"org.opencontainers.image.version=v{args.version}",
            "--label", f"org.opencontainers.image.title={name}",
            *(["--label", f"dev.metro.source-ref={args.source_ref}"] if args.source_ref else []),
            "--output", f"type=oci,dest={tar},compression=gzip,rewrite-timestamp=true"]
    if target:
        cmd += ["--target", target]
    cmd.append(str(context))
    env = dict(os.environ, SOURCE_DATE_EPOCH=epoch)
    subprocess.run(cmd, check=True, env=env)
    info = describe(tar, args.platform, user)
    info.update({"component": args.component, "image": name, "version": args.version,
                 "revision": args.revision, "archive": tar.name,
                 "archive_sha256": file_sha256(tar)})
    require(info["labels"].get("org.opencontainers.image.revision") == args.revision,
            "the image does not carry the expected revision label")
    (out / f"{name}-{arch}.json").write_text(json.dumps(info, indent=2, sort_keys=True) + "\n")
    print(json.dumps({k: info[k] for k in ("image", "platform", "manifest_digest", "config_digest")}))


def write_loadable(archive, dest):
    """Copy an OCI archive to dest, adding the Docker-archive manifest.json.

    The source archive is only read, so published archives and their digests
    never change. An existing manifest.json is accepted only if it names exactly
    the image index.json resolves to; it is then replaced by the same content.
    """
    entries = archive.docker_manifest()
    existing = archive.members.get("manifest.json")
    if existing is not None:
        require(json.loads(archive.read("manifest.json")) == entries,
                f"{archive.path.name}: manifest.json names a different image than index.json")
    data = json.dumps(entries, separators=(",", ":")).encode()
    with tarfile.open(dest, "w", format=tarfile.PAX_FORMAT) as out:
        for member in archive.tar.getmembers():
            name = member.name[2:] if member.name.startswith("./") else member.name
            if name == "manifest.json":
                continue
            require(member.isfile() or member.isdir(), f"{archive.path.name}: unexpected entry {member.name}")
            if member.isfile():
                with archive.tar.extractfile(member) as f:
                    out.addfile(member, f)
            else:
                out.addfile(member)
        info = tarfile.TarInfo("manifest.json")
        info.size = len(data)
        info.mode = 0o644
        out.addfile(info, io.BytesIO(data))
    return entries


def load(args):
    """docker load an archive and tag the exact image it described."""
    info = describe(args.archive, args.platform)
    archive = Archive(args.archive)
    try:
        with tempfile.TemporaryDirectory(prefix="metro-image-load-") as tmp:
            loadable = Path(tmp) / "image.tar"
            write_loadable(archive, loadable)
            proc = subprocess.run(["docker", "load", "-i", str(loadable)],
                                  capture_output=True, text=True)
    finally:
        archive.close()
    require(proc.returncode == 0, f"docker load failed (exit {proc.returncode}): "
            + (proc.stderr.strip() or proc.stdout.strip())[-2000:])
    ids = re.findall(r"sha256:[0-9a-f]{64}", proc.stdout)
    # The classic store names images by config digest, the containerd store by manifest digest.
    wanted = {info["config_digest"], info["manifest_digest"]}
    require(len(ids) == 1 and ids[0] in wanted, "docker load did not load exactly the described image")
    subprocess.run(["docker", "tag", ids[0], args.tag], check=True)
    out = subprocess.run(["docker", "image", "inspect", "--format", "{{.Id}} {{.Os}}/{{.Architecture}}",
                          args.tag], check=True, capture_output=True, text=True).stdout.split()
    require(out[0] in wanted and out[1] == info["platform"], f"{args.tag} is not the described image")
    print(json.dumps({"tag": args.tag, "id": out[0], "platform": out[1],
                      "manifest_digest": info["manifest_digest"]}))


def file_sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


# ------------------------------------------------------------------ registry


class Registry:
    """Minimal OCI distribution client: bearer-token auth, monolithic uploads."""

    def __init__(self, repository, user=None, token=None, allow_http_loopback=False):
        require(REPOSITORY.match(repository), "repository must look like host/owner/name in lower case")
        host, _, self.name = repository.partition("/")
        self.host = host
        loopback = host.split(":")[0] in ("127.0.0.1", "localhost") or host.startswith("[::1]")
        self.scheme = "http" if (allow_http_loopback and loopback) else "https"
        self.user, self.secret = user, token
        self.bearer = None

    def _connect(self, netloc, scheme):
        cls = http.client.HTTPSConnection if scheme == "https" else http.client.HTTPConnection
        return cls(netloc, timeout=120)

    def _auth(self, challenge):
        m = re.match(r"(?i)bearer\s+(.*)", challenge or "")
        if not m:
            require(challenge and challenge.lower().startswith("basic") and self.secret,
                    "registry requires credentials")
            self.bearer = "Basic " + base64.b64encode(f"{self.user}:{self.secret}".encode()).decode()
            return
        params = dict(re.findall(r'(\w+)="([^"]*)"', m.group(1)))
        realm = urlsplit(params.pop("realm"))
        require(realm.scheme == "https" or self.scheme == "http", "token realm must use https")
        params["scope"] = f"repository:{self.name}:pull,push" if self.secret else f"repository:{self.name}:pull"
        conn = self._connect(realm.netloc, realm.scheme)
        headers = {}
        if self.secret:
            headers["Authorization"] = "Basic " + base64.b64encode(
                f"{self.user}:{self.secret}".encode()).decode()
        try:
            conn.request("GET", f"{realm.path}?{urlencode(params)}", headers=headers)
            resp = conn.getresponse()
            body = resp.read(MAX_JSON)
            require(resp.status == 200, f"registry token request failed with HTTP {resp.status}")
        finally:
            conn.close()
        data = json.loads(body)
        token = data.get("token") or data.get("access_token")
        require(token, "registry did not return a token")
        self.bearer = "Bearer " + token

    def request(self, method, path, body=None, headers=None, ok=(200,), stream=None, length=None):
        headers = dict(headers or {})
        for attempt in (0, 1):
            if self.bearer:
                headers["Authorization"] = self.bearer
            target = urlsplit(path) if path.startswith("http") else None
            netloc = target.netloc if target else self.host
            require(netloc == self.host, "registry redirected an upload to another host")
            url = (target.path + ("?" + target.query if target.query else "")) if target else path
            conn = self._connect(netloc, self.scheme)
            try:
                if stream is not None:
                    headers["Content-Length"] = str(length)
                    conn.putrequest(method, url)
                    for k, v in headers.items():
                        conn.putheader(k, v)
                    conn.endheaders()
                    for chunk in iter(lambda: stream.read(1 << 20), b""):
                        conn.send(chunk)
                else:
                    conn.request(method, url, body=body, headers=headers)
                resp = conn.getresponse()
                data = resp.read(MAX_JSON) if method != "HEAD" else b""
                if resp.status == 401 and attempt == 0 and stream is None:
                    self._auth(resp.getheader("WWW-Authenticate"))
                    continue
                require(resp.status in ok, f"{method} {self.name}: HTTP {resp.status}")
                return resp.status, dict((k.lower(), v) for k, v in resp.getheaders()), data
            finally:
                conn.close()
        raise Fail(f"{method} {self.name}: not authorized")

    def login(self):
        status, headers, _ = self.request("GET", "/v2/", ok=(200, 401))
        if status == 401:
            self._auth(headers.get("www-authenticate"))
            self.request("GET", "/v2/")

    def has_blob(self, digest):
        status, _, _ = self.request("HEAD", f"/v2/{self.name}/blobs/{digest}", ok=(200, 404))
        return status == 200

    def put_blob(self, digest, size, stream):
        if self.has_blob(digest):
            return False
        _, headers, _ = self.request("POST", f"/v2/{self.name}/blobs/uploads/", ok=(202,))
        location = headers.get("location")
        require(location, "registry gave no upload location")
        location = urljoin(f"{self.scheme}://{self.host}/v2/{self.name}/blobs/uploads/", location)
        sep = "&" if "?" in location else "?"
        self.request("PUT", f"{location}{sep}{urlencode({'digest': digest})}", ok=(201,),
                     headers={"Content-Type": "application/octet-stream"}, stream=stream, length=size)
        return True

    def put_manifest(self, reference, media_type, raw):
        _, headers, _ = self.request("PUT", f"/v2/{self.name}/manifests/{reference}", body=raw,
                                     headers={"Content-Type": media_type}, ok=(201,))
        got = headers.get("docker-content-digest")
        require(got in (None, sha256_bytes(raw)), "registry stored a different manifest digest")
        return sha256_bytes(raw)

    def get_manifest(self, reference, accept):
        status, headers, data = self.request(
            "GET", f"/v2/{self.name}/manifests/{reference}",
            headers={"Accept": ", ".join(accept)}, ok=(200, 404))
        if status == 404:
            return None, None
        return data, headers.get("content-type", "").split(";")[0]


def build_index(infos, version, revision, source, source_ref=None):
    manifests = []
    for info in sorted(infos, key=lambda i: i["platform"]):
        os_, arch = info["platform"].split("/")[:2]
        manifests.append({
            "mediaType": info["manifest_media_type"],
            "digest": info["manifest_digest"],
            "size": info["manifest_size"],
            "platform": {"architecture": arch, "os": os_},
        })
    platforms = [m["platform"]["os"] + "/" + m["platform"]["architecture"] for m in manifests]
    require(len(set(platforms)) == len(platforms), "two archives have the same platform")
    index = {
        "schemaVersion": 2,
        "mediaType": OCI_INDEX,
        "manifests": manifests,
        "annotations": source_annotations(version, revision, source, source_ref),
    }
    return canonical(index)


def registry_from_args(args, write):
    token = None
    if args.token_env:
        token = os.environ.get(args.token_env)
        require(token, f"{args.token_env} is empty")
    require(token or not write, "push needs --token-env")
    return Registry(args.repository, user=args.user or "token", token=token,
                    allow_http_loopback=args.allow_http_loopback)


def push(args):
    require(VERSION.match(args.version), "version must be X.Y.Z[-pre] without a leading v")
    tag = "v" + args.version
    reg = registry_from_args(args, write=True)
    archives = [Archive(p) for p in args.archives]
    try:
        infos = [a.describe() for a in archives]
        for info in infos:
            require(info["platform"] in PLATFORMS, f"unexpected platform {info['platform']}")
            if args.source_ref:
                for key, value in source_annotations(args.version, args.revision, args.source, args.source_ref).items():
                    require(info["labels"].get(key) == value, f"{info['platform']}: source label {key} differs")
        if args.expect_platforms:
            got = sorted(i["platform"] for i in infos)
            require(got == sorted(args.expect_platforms.split(",")),
                    f"platforms {got} differ from the expected set")
        index = build_index(infos, args.version, args.revision, args.source, args.source_ref)
        index_digest = sha256_bytes(index)
        reg.login()
        existing, _ = reg.get_manifest(tag, INDEX_TYPES + MANIFEST_TYPES)
        if existing is not None:
            require(sha256_bytes(existing) == index_digest,
                    f"{args.repository}:{tag} already exists with different content; refusing to move it")
        for archive, info in zip(archives, infos, strict=True):
            for digest, size, _ in archive.blobs_of(info["manifest_digest"]):
                with archive.open_blob(digest) as stream:
                    reg.put_blob(digest, size, stream)
            raw = archive.blob(info["manifest_digest"])
            reg.put_manifest(info["manifest_digest"], info["manifest_media_type"], raw)
        if existing is None:
            reg.put_manifest(tag, OCI_INDEX, index)
    finally:
        for a in archives:
            a.close()
    result = {
        "repository": args.repository, "tag": tag, "index_digest": index_digest,
        "revision": args.revision, "source_ref": args.source_ref,
        "platforms": {i["platform"]: {"manifest_digest": i["manifest_digest"],
                                      "config_digest": i["config_digest"]} for i in infos},
    }
    write_json(args.result, result)
    print(json.dumps({"repository": args.repository, "tag": tag, "index_digest": index_digest}))


def verify(args):
    expected = json.loads(Path(args.expect).read_text())
    tag = expected["tag"]
    reg = registry_from_args(args, write=False)
    reg.login()
    raw, media = reg.get_manifest(tag, INDEX_TYPES)
    require(raw is not None, f"{args.repository}:{tag} was not found")
    require(media in INDEX_TYPES, f"{args.repository}:{tag} is not an image index")
    require(sha256_bytes(raw) == expected["index_digest"], f"{args.repository}:{tag} index digest differs")
    raw_by_digest, _ = reg.get_manifest(expected["index_digest"], INDEX_TYPES)
    require(raw_by_digest == raw, "fetching the index by digest returned different bytes")
    index = json.loads(raw)
    seen = {}
    for desc in index["manifests"]:
        p = desc.get("platform") or {}
        require(set(p) <= {"os", "architecture"}, "unexpected platform fields")
        platform = f"{p.get('os')}/{p.get('architecture')}"
        require(platform not in seen, f"duplicate platform {platform}")
        seen[platform] = desc["digest"]
        want = expected["platforms"].get(platform)
        require(want is not None, f"unexpected platform {platform} in the index")
        require(desc["digest"] == want["manifest_digest"], f"{platform}: manifest digest differs")
        m_raw, m_type = reg.get_manifest(desc["digest"], MANIFEST_TYPES)
        require(m_raw is not None and sha256_bytes(m_raw) == desc["digest"], f"{platform}: manifest missing")
        require(m_type == desc["mediaType"], f"{platform}: manifest media type differs")
        manifest = json.loads(m_raw)
        require(manifest["config"]["digest"] == want["config_digest"], f"{platform}: config digest differs")
    require(sorted(seen) == sorted(expected["platforms"]), "the index lacks an expected platform")
    print(json.dumps({"verified": f"{args.repository}:{tag}", "index_digest": expected["index_digest"],
                      "platforms": sorted(seen)}))


def evidence(args):
    results = [json.loads(Path(p).read_text()) for p in args.results]
    require(re.fullmatch(r"[0-9a-f]{40}", args.revision), "--revision must be a full commit SHA")
    lines = [
        f"# Metro container images v{args.version}",
        f"# Source commit: {args.revision}",
    ]
    machine = {"version": args.version, "revision": args.revision, "images": []}
    if args.source_url or args.source_ref:
        require(args.source_url and args.source_ref, "--source-url and --source-ref go together")
        lines += [f"# Source: {args.source_url} {args.source_ref} (git tag v{args.version} -> {args.revision})"]
        machine["source"] = {"url": args.source_url, "ref": args.source_ref,
                             "tag": f"v{args.version}", "revision": args.revision}
    lines.append("# Pull by digest. The tag is the version only; there is no 'latest'.")
    for r in results:
        require(r.get("revision", args.revision) == args.revision,
                f"{r['repository']} was pushed from another commit")
        if args.source_ref:
            require(r.get("source_ref") == args.source_ref, f"{r['repository']} was pushed from another ref")
    if args.signer_identity:
        require(args.signer_issuer, "--signer-identity needs --signer-issuer")
        lines += [
            "# Every index and per-platform digest below is signed keyless with Sigstore cosign",
            "# and was verified before this file was written against this publisher identity,",
            "# the commit above (Fulcio claim and signed annotation) and the exact digest:",
            f"#   certificate identity: {args.signer_identity}",
            f"#   OIDC issuer:          {args.signer_issuer}",
            f"#   signed annotation:    org.opencontainers.image.revision={args.revision}",
            "# Verify:  cosign verify --certificate-identity '<identity>' \\",
            "#            --certificate-oidc-issuer '<issuer>' \\",
            f"#            --certificate-github-workflow-sha {args.revision} \\",
            f"#            -a org.opencontainers.image.revision={args.revision} '<reference@digest>'",
        ]
        machine["signature"] = {"type": "sigstore-cosign-keyless", "identity": args.signer_identity,
                                "issuer": args.signer_issuer, "revision": args.revision}
    else:
        lines.append("# NOT SIGNED: these digests carry no publisher signature.")
    lines += ["# Columns: reference  architecture(s)", ""]
    for r in sorted(results, key=lambda r: r["repository"]):
        platforms = sorted(r["platforms"])
        archs = ",".join(platforms)
        lines.append(f"{r['repository']}:{r['tag']}@{r['index_digest']}  {archs}")
        for p in platforms:
            lines.append(f"{r['repository']}@{r['platforms'][p]['manifest_digest']}  {p}")
        lines.append("")
        machine["images"].append({
            "repository": r["repository"], "tag": r["tag"],
            "ref": f"{r['repository']}:{r['tag']}@{r['index_digest']}",
            "index_digest": r["index_digest"],
            "platforms": {p: {**r["platforms"][p],
                              "ref": f"{r['repository']}@{r['platforms'][p]['manifest_digest']}"}
                          for p in platforms},
        })
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    (out / "IMAGES.txt").write_text("\n".join(lines))
    write_json(out / "images.json", machine)
    print((out / "IMAGES.txt").read_text(), end="")


SHA1 = re.compile(r"^[0-9a-f]{40}$")
FULCIO = {
    # GitHub Actions certificate claims exactly as `cosign verify --output json`
    # reports them (seen with cosign v3.1.4). cosign itself enforces them through
    # the --certificate-* flags; this re-checks whatever it reports.
    "Issuer": "issuer", "1.3.6.1.4.1.57264.1.1": "issuer",
    "Subject": "identity",
    "githubWorkflowTrigger": "trigger", "1.3.6.1.4.1.57264.1.2": "trigger",
    "githubWorkflowSha": "revision", "1.3.6.1.4.1.57264.1.3": "revision",
    "githubWorkflowRepository": "workflow_repository", "1.3.6.1.4.1.57264.1.5": "workflow_repository",
    "githubWorkflowRef": "source_ref", "1.3.6.1.4.1.57264.1.6": "source_ref",
}


def check_tag(args):
    """Read only. GitHub API, token from the environment (never argv)."""
    require(SHA1.fullmatch(args.commit), "--commit must be a full commit SHA")
    require(re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", args.repository), "bad --repository")
    require(re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?", args.tag), "bad --tag")
    token = os.environ.get(args.token_env or "", "")
    api = urlsplit(args.api_url)
    require(api.scheme == "https" or (args.allow_http_loopback and api.hostname == "127.0.0.1"),
            "the API must use https")

    def get(path):
        cls = http.client.HTTPSConnection if api.scheme == "https" else http.client.HTTPConnection
        conn = cls(api.netloc, timeout=60)
        headers = {"Accept": "application/vnd.github+json", "User-Agent": "metro-release-images",
                   "X-GitHub-Api-Version": "2022-11-28"}
        if token:
            headers["Authorization"] = f"Bearer {token}"
        try:
            conn.request("GET", api.path.rstrip("/") + path, headers=headers)
            resp = conn.getresponse()
            body = resp.read(MAX_JSON)
        finally:
            conn.close()
        require(resp.status != 404, f"tag {args.tag} does not exist in {args.repository}")
        require(resp.status == 200, f"GitHub API answered HTTP {resp.status}")
        return json.loads(body)

    ref = get(f"/repos/{args.repository}/git/ref/tags/{args.tag}")
    require(isinstance(ref, dict) and ref.get("ref") == f"refs/tags/{args.tag}",
            f"tag {args.tag} does not exist in {args.repository}")
    obj = ref["object"]
    for _ in range(4):  # an annotated tag peels to its target
        if obj["type"] != "tag":
            break
        require(SHA1.fullmatch(obj["sha"]), "annotated tag has an invalid SHA")
        obj = get(f"/repos/{args.repository}/git/tags/{obj['sha']}")["object"]
    require(obj["type"] == "commit", f"tag {args.tag} does not name a commit")
    require(obj["sha"] == args.commit, f"tag {args.tag} names {obj['sha']}, not {args.commit}")
    print(json.dumps({"tag": args.tag, "commit": obj["sha"]}))


def check_signatures(args):
    """Every signature cosign verified names this digest and carries the expected source."""
    require(DIGEST.fullmatch(args.digest), "bad --digest")
    require(SHA1.fullmatch(args.revision), "--revision must be a full commit SHA")
    entries = json.loads(Path(args.cosign_json).read_text())
    require(isinstance(entries, list) and entries, "cosign reported no verified signatures")
    want_annotations = source_annotations(args.version, args.revision, args.source_url, args.source_ref)
    want_claims = {"revision": args.revision, "source_ref": args.source_ref,
                   "identity": args.identity, "issuer": args.issuer,
                   "workflow_repository": args.workflow_repository, "trigger": "push"}
    claims_seen = set()
    for entry in entries:
        critical = entry.get("critical") or {}
        require(critical.get("image", {}).get("docker-manifest-digest") == args.digest,
                "a signature names another digest")
        reference = critical.get("identity", {}).get("docker-reference", "")
        require(reference in (args.repository, f"{args.repository}@{args.digest}"),
                "a signature names another repository")
        optional = entry.get("optional") or {}
        for key, value in want_annotations.items():
            require(optional.get(key) == value, f"signed annotation {key} is missing or differs")
        for key, claim in FULCIO.items():
            if key in optional and want_claims.get(claim) is not None:
                require(optional[key] == want_claims[claim], f"certificate claim {key} differs")
                claims_seen.add(claim)
    print(json.dumps({"digest": args.digest, "signatures": len(entries), "claims_checked": sorted(claims_seen)}))


def write_json(path, data):
    if path:
        Path(path).write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)

    b = sub.add_parser("build")
    b.add_argument("--component", choices=sorted(COMPONENTS), required=True)
    b.add_argument("--platform", required=True)
    b.add_argument("--version", required=True)
    b.add_argument("--revision", required=True)
    b.add_argument("--source", required=True)
    b.add_argument("--source-ref", help="git ref the build came from, recorded as dev.metro.source-ref")
    b.add_argument("--context", default=".")
    b.add_argument("--git-dir", help="repository for the commit time (default: --context)")
    b.add_argument("--builder")
    b.add_argument("--out", required=True)

    lo = sub.add_parser("load")
    lo.add_argument("archive")
    lo.add_argument("--platform", required=True)
    lo.add_argument("--tag", required=True)

    d = sub.add_parser("describe")
    d.add_argument("archive")
    d.add_argument("--platform")
    d.add_argument("--user")

    for name in ("push", "verify"):
        s = sub.add_parser(name)
        s.add_argument("--repository", required=True)
        s.add_argument("--user", help="registry user name (not secret)")
        s.add_argument("--token-env", help="name of the environment variable holding the registry token")
        s.add_argument("--allow-http-loopback", action="store_true", help=argparse.SUPPRESS)
        if name == "push":
            s.add_argument("--version", required=True)
            s.add_argument("--revision", required=True)
            s.add_argument("--source", required=True)
            s.add_argument("--source-ref")
            s.add_argument("--expect-platforms")
            s.add_argument("--result")
            s.add_argument("archives", nargs="+")
        else:
            s.add_argument("--expect", required=True)

    t = sub.add_parser("check-tag")
    t.add_argument("--repository", required=True, help="owner/name on GitHub")
    t.add_argument("--tag", required=True)
    t.add_argument("--commit", required=True)
    t.add_argument("--token-env", help="name of the environment variable holding a read token")
    t.add_argument("--api-url", default="https://api.github.com")
    t.add_argument("--allow-http-loopback", action="store_true", help=argparse.SUPPRESS)

    c = sub.add_parser("check-signatures")
    c.add_argument("--cosign-json", required=True)
    c.add_argument("--repository", required=True)
    c.add_argument("--digest", required=True)
    c.add_argument("--revision", required=True)
    c.add_argument("--version", required=True)
    c.add_argument("--source-url", required=True)
    c.add_argument("--source-ref", required=True)
    c.add_argument("--identity")
    c.add_argument("--issuer")
    c.add_argument("--workflow-repository")

    e = sub.add_parser("evidence")
    e.add_argument("--version", required=True)
    e.add_argument("--revision", required=True)
    e.add_argument("--out", required=True)
    e.add_argument("--source-url")
    e.add_argument("--source-ref")
    e.add_argument("--signer-identity")
    e.add_argument("--signer-issuer")
    e.add_argument("results", nargs="+")

    args = p.parse_args(argv)
    try:
        if args.cmd == "build":
            build(args)
        elif args.cmd == "load":
            load(args)
        elif args.cmd == "describe":
            print(json.dumps(describe(args.archive, args.platform, args.user), indent=2, sort_keys=True))
        elif args.cmd == "push":
            push(args)
        elif args.cmd == "verify":
            verify(args)
        elif args.cmd == "check-tag":
            check_tag(args)
        elif args.cmd == "check-signatures":
            check_signatures(args)
        else:
            evidence(args)
    except (Fail, KeyError, ValueError, OSError, tarfile.TarError, http.client.HTTPException,
            subprocess.CalledProcessError) as exc:
        print(f"image_release: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
