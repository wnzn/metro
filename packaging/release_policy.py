#!/usr/bin/env python3
"""Metro's public release policy: who may release, which version, and when.

The only official trigger is a protected, non-deleted, non-force `push` to
refs/heads/main of wnzn/metro, run by the top-level ci.yml at that branch,
for exactly the pushed commit. Tags, pull requests, other branches and manual
runs never release or publish.

The version is the checked-in one: Cargo `[workspace.package] version` and
apps/desktop/nsh.json `version` must be the same semantic version (a
prerelease such as 0.3.0-rc.1 is published as a GitHub prerelease, never
latest). Its immutable marker is the tag v<version>, created by this workflow
after CI, the platform smoke gates and the Windows gate: Authenticode signing
and verification (windows-sign.yml) when packaging/windows-release-signing.txt
says `signed`, or, when it says `unsigned`, the unsigned Windows build that
windows-distribution.yml built and tested natively in the release run. It is
never moved.

Only a version bump releases. A main push whose checked-in version equals the
version at push.before is CI only, and so is a push whose before commit is
missing, all zeros or unreadable. Such pushes never look at tags or releases,
so they never fail on them.

Eligibility for a main push that changed the version (`decide`):

  tag v<version>     published release   state       outcome
  missing            none                new         release
  at this commit     none                resume      release (a rerun of the bump run after a failure)
  at this commit     exact asset set     done        nothing to do (success)
  at another commit  exists              -           fail: this push changed to a used version
  at another commit  none                -           fail: conflicting unpublished tag
  missing            exists              -           fail: release without its tag
  at this commit     other asset set     -           fail: never overwritten

Without a bump the state is `unchanged` (same version as push.before) or
`no-previous-version` (before missing, all zeros or unreadable): CI only.

Commands (GitHub API via GH_TOKEN; the token is never printed):

  version      print the checked-in version
  windows-signing  print the checked-in Windows release mode (signed or unsigned; no network)
  provenance   fail unless this is the official main push (no network)
  decide       provenance, version bump, eligibility; writes GITHUB_OUTPUT
  authorize    recheck the bump and eligibility before the Windows gate (read only);
               also outputs windows_signing for release-publish.yml
  tag          create refs/tags/v<version> at this commit (idempotent, never moved)
  verify-tag   verify the exact tag before signing (read only)
  publish      stage a draft with exactly the verified files, check every
               uploaded digest, then publish it (idempotent on rerun)
"""

import argparse
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib
from urllib.parse import quote, urlsplit

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(Path(__file__).resolve().parent / "release-signatures"))
import release_signatures  # noqa: E402  (the signer's exact final file set)

REPOSITORY = "wnzn/metro"
REPOSITORY_ID = "1467654868"
BRANCH_REF = "refs/heads/main"
CALLER = f"{REPOSITORY}/.github/workflows/ci.yml@{BRANCH_REF}"
# Use the subset supported by the packaging, MSI, image and signature tools.
SEMVER = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-([0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*))?")
SHA = re.compile(r"[0-9a-f]{40}")
MAX_ASSET = 2 * 1024 ** 3


class PolicyError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise PolicyError(message)


def sha256(path):
    with open(path, "rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


# --- version ------------------------------------------------------------------

def parse_versions(cargo, app):
    crate = tomllib.loads(cargo).get("workspace", {}).get("package", {}).get("version", "")
    app = json.loads(app).get("version")
    m = SEMVER.fullmatch(crate) if isinstance(crate, str) else None
    require(m and len(crate) <= 100 and len(m[4] or "") <= 40
            and all(not (part.isdigit() and part.startswith("0") and len(part) > 1)
                    for part in (m[4] or "").split(".")),
            f"Cargo.toml [workspace.package] version {crate!r} is not a supported semantic version")
    require(crate == app, f"Cargo.toml version {crate} and apps/desktop/nsh.json version {app} differ")
    return crate, m[4] is not None


def read_version(root=ROOT):
    return parse_versions((root / "Cargo.toml").read_text(), (root / "apps/desktop/nsh.json").read_text())


def previous_version(before, root=ROOT):
    """The checked-in version at the push's before commit, or None when there is none.

    None (never an error) for a missing, all-zero (branch creation) or malformed
    before SHA, a commit that is not in the checkout, and manifests that do not
    parse. Such a push is never a version bump. Only the two checked-in manifest
    blobs are read; nothing from the previous revision runs.
    """
    if not isinstance(before, str) or not SHA.fullmatch(before) or before == "0" * 40:
        return None
    versions = []
    try:
        for path in ("Cargo.toml", "apps/desktop/nsh.json"):
            result = subprocess.run(["git", "-C", str(root), "cat-file", "blob", f"{before}:{path}"],
                                    capture_output=True, text=True, check=True)
            versions.append(result.stdout)
        return parse_versions(*versions)[0]
    except (PolicyError, OSError, ValueError, TypeError, AttributeError, subprocess.CalledProcessError,
            tomllib.TOMLDecodeError):
        return None


def expected_assets(version):
    """Exactly what sign-release-assets.yml returns (release_signatures.check_directory)."""
    artifacts = release_signatures.binaries(version)
    names = artifacts | release_signatures.INSTALLERS | {f"{n}.sha256" for n in artifacts}
    names |= {release_signatures.MANIFEST, "IMAGES.txt"}
    names |= {n + release_signatures.BUNDLE
              for n in (release_signatures.MANIFEST, *release_signatures.direct_archives(version))}
    return names


# --- provenance -----------------------------------------------------------------

def check_provenance(env, head):
    """The official trigger, from the runner's environment and the checked-out commit."""
    sha = env.get("GITHUB_SHA", "")
    checks = (
        (env.get("GITHUB_EVENT_NAME") == "push", "the event is not a push"),
        (env.get("GITHUB_REF") == BRANCH_REF, f"the ref is not {BRANCH_REF}"),
        (env.get("GITHUB_REPOSITORY") == REPOSITORY and env.get("GITHUB_REPOSITORY_ID") == REPOSITORY_ID,
         f"the repository is not {REPOSITORY}"),
        (env.get("GITHUB_WORKFLOW_REF") == CALLER, f"the top-level workflow is not {CALLER}"),
        (env.get("REF_PROTECTED") == "true", f"{BRANCH_REF} is not protected"),
        (env.get("EVENT_DELETED") == "false", "the push deleted the branch"),
        (env.get("EVENT_FORCED") == "false", "the push was forced or could not be verified as non-force"),
        (bool(SHA.fullmatch(sha)) and env.get("EVENT_AFTER") == sha, "the pushed commit is not github.sha"),
        (head == sha, "the checked-out commit is not github.sha"),
    )
    failed = [message for ok, message in checks if not ok]
    require(not failed, "not an official release run: " + "; ".join(failed))
    return sha


def git_head(root=ROOT):
    return subprocess.run(["git", "-C", str(root), "rev-parse", "HEAD"], capture_output=True, text=True,
                          check=True).stdout.strip()


# --- GitHub API -----------------------------------------------------------------

class GitHub:
    """Minimal REST client. Send the token only to GitHub over HTTPS."""

    def __init__(self, token, repo=REPOSITORY, api="https://api.github.com", insecure_loopback=False):
        require(token, "GH_TOKEN is empty")
        self.token, self.repo = token, repo
        parts = urlsplit(api)
        loopback = parts.hostname in ("127.0.0.1", "localhost")
        require(api == "https://api.github.com" or (insecure_loopback and loopback and parts.scheme == "http"),
                "the GitHub API URL must be https://api.github.com")
        self.api = api.rstrip("/")
        self.origins = {("https", "api.github.com"), ("https", "uploads.github.com")}
        if insecure_loopback and loopback:
            self.origins = {(parts.scheme, parts.netloc)}

    def call(self, method, url, body=None, ok=(200,), headers=None, data=None):
        url = url if url.startswith(("https://", "http://")) else self.api + url
        parts = urlsplit(url)
        require((parts.scheme, parts.netloc) in self.origins, "refusing to send the token outside GitHub")
        conn = (http.client.HTTPSConnection if parts.scheme == "https" else http.client.HTTPConnection)(
            parts.netloc, timeout=120)
        head = {"Authorization": f"Bearer {self.token}", "Accept": "application/vnd.github+json",
                "X-GitHub-Api-Version": "2022-11-28", "User-Agent": "metro-release-policy", **(headers or {})}
        payload = data if data is not None else (json.dumps(body).encode() if body is not None else None)
        if body is not None:
            head["Content-Type"] = "application/json"
        try:
            conn.request(method, parts.path + (f"?{parts.query}" if parts.query else ""), body=payload, headers=head)
            resp = conn.getresponse()
            raw = resp.read()
        finally:
            conn.close()
        require(resp.status in ok, f"GitHub API {method} {parts.path} returned HTTP {resp.status}")
        return resp.status, (json.loads(raw) if raw and raw[:1] in (b"{", b"[") else raw)

    def tag_commit(self, version):
        status, ref = self.call("GET", f"/repos/{self.repo}/git/ref/tags/{quote('v' + version)}", ok=(200, 404))
        if status == 404:
            return None
        require(isinstance(ref, dict) and ref.get("ref") == f"refs/tags/v{version}", "unexpected tag lookup reply")
        obj = ref["object"]
        for _ in range(10):  # existing annotated tags must also resolve exactly
            if obj["type"] != "tag":
                break
            require(SHA.fullmatch(obj["sha"]), "invalid Git tag object")
            _, tag = self.call("GET", f"/repos/{self.repo}/git/tags/{obj['sha']}")
            obj = tag["object"]
        require(obj["type"] == "commit" and SHA.fullmatch(obj["sha"]), f"tag v{version} does not name a commit")
        return obj["sha"]

    def create_tag(self, version, sha):
        status, _ = self.call("POST", f"/repos/{self.repo}/git/refs",
                              body={"ref": f"refs/tags/v{version}", "sha": sha}, ok=(201, 422))
        return status == 201

    def releases(self, tag):
        found = []
        for page in range(1, 21):
            _, batch = self.call("GET", f"/repos/{self.repo}/releases?per_page=100&page={page}")
            found += [r for r in batch if r.get("tag_name") == tag]
            if len(batch) < 100:
                return found
        raise PolicyError("too many releases to search")

    def release(self, release_id):
        return self.call("GET", f"/repos/{self.repo}/releases/{release_id}")[1]

    def public_release(self, version):
        status, release = self.call("GET", f"/repos/{self.repo}/releases/tags/{quote('v' + version)}", ok=(200, 404))
        if status == 404:
            return None
        require(release.get("tag_name") == f"v{version}" and type(release.get("draft")) is bool,
                "unexpected release lookup reply")
        return None if release["draft"] else release

    def create_release(self, version, prerelease, sha):
        return self.call("POST", f"/repos/{self.repo}/releases", ok=(201,), body={
            "tag_name": f"v{version}", "name": f"Metro {version}", "draft": True, "prerelease": prerelease,
            "target_commitish": sha,
            "make_latest": "false", "generate_release_notes": False,
            "body": release_notes(version, sha),
        })[1]

    def delete_release(self, release_id):
        self.call("DELETE", f"/repos/{self.repo}/releases/{release_id}", ok=(204,))

    def upload(self, release, path):
        url = release["upload_url"].split("{", 1)[0] + "?name=" + quote(path.name)
        with path.open("rb") as data:
            return self.call("POST", url, ok=(201,), data=data, headers={
                "Content-Type": "application/octet-stream", "Content-Length": str(os.fstat(data.fileno()).st_size),
            })[1]

    def publish(self, release_id, prerelease):
        return self.call("PATCH", f"/repos/{self.repo}/releases/{release_id}", body={
            "draft": False, "prerelease": prerelease, "make_latest": "false" if prerelease else "legacy"})[1]


# --- decisions ------------------------------------------------------------------

def bump(version, prior):
    """The no-release state for a push that did not change the version, or None for a bump."""
    if prior is None:
        return "no-previous-version"
    if prior == version:
        return "unchanged"
    return None


def decide(api, version, sha, prior):
    """Eligibility of this push. `prior` is previous_version(push.before).

    Without a version bump this returns before any GitHub call, so tags and
    releases can never fail such a push.
    """
    quiet = bump(version, prior)
    if quiet:
        return quiet
    tag = api.tag_commit(version)
    public = api.public_release(version)
    if tag is None:
        require(public is None, f"release v{version} is published but its tag is missing")
        return "new"
    if tag == sha:
        if public is None:
            return "resume"
        require(public.get("prerelease") is ("-" in version),
                f"published release v{version} has the wrong prerelease status; it is never overwritten")
        check_assets(public, expected_assets(version), f"published v{version} (never overwritten)")
        return "done"
    require(public is not None,
            f"tag v{version} points at {tag[:12]}, not this commit, and nothing is published for it: "
            "bump the version or have a maintainer review and remove that tag")
    raise PolicyError(f"release v{version} is already published at {tag[:12]}; "
                      f"this push changed the version from {prior} to a used version")


def create_tag(api, version, sha):
    """Create v<version> at exactly this commit, or accept it if it already is there. Never move it."""
    existing = api.tag_commit(version)
    if existing is None:
        api.create_tag(version, sha)
        existing = api.tag_commit(version)  # also covers a concurrent creator
    require(existing == sha, f"tag v{version} points at {str(existing)[:12]}, not {sha[:12]}; it is never moved")
    return existing


def check_assets(release, expected, where):
    assets = release.get("assets", [])
    names = [a["name"] for a in assets]
    require(len(names) == len(set(names)) and set(names) == set(expected),
            f"{where}: release assets differ from the verified set")
    for asset in assets:
        require(asset.get("state") == "uploaded", f"{where}: {asset['name']} is not fully uploaded")
        digest = asset.get("digest") or ""
        require(re.fullmatch(r"sha256:[0-9a-f]{64}", digest), f"{where}: {asset['name']} has no GitHub digest")
        if isinstance(expected, dict):
            require(digest == "sha256:" + expected[asset["name"]], f"{where}: {asset['name']} has other bytes")


def release_notes(version, sha, windows=None):
    """The release body. `windows` is the signing mode (default: the checked-in one)."""
    windows = release_signatures.windows_signing() if windows is None else windows
    notes = (f"Metro {version}. Verify downloads with SHA256SUMS and its Sigstore bundle "
             "SHA256SUMS.sigstore.json. Container images by digest: IMAGES.txt.\n\n")
    if windows == "unsigned":
        msi = release_signatures.windows_msi(version, windows)
        notes += (
            "**The Windows downloads are not code-signed.** Metro doesn't have a Windows code-signing "
            f"certificate yet, so Metro-{version}-windows-x64.zip (its RELEASE.json says "
            f"`unsigned-preview`), {msi} and the two .ps1 scripts carry no Authenticode signature. "
            "The MSI installs as \"Metro (unsigned)\" and says so on its first page. Windows SmartScreen "
            "warnings and an \"Unknown publisher\" prompt are expected, and PowerShell may refuse the scripts under a policy that requires signed "
            "scripts.\n\n"
            "Before you run a Windows file, verify SHA256SUMS with its Sigstore bundle, then check the "
            "file against it:\n\n"
            "```sh\n"
            "cosign verify-blob --bundle SHA256SUMS.sigstore.json \\\n"
            "  --certificate-identity "
            "'https://github.com/wnzn/metro/.github/workflows/sign-release-assets.yml@refs/heads/main' \\\n"
            "  --certificate-oidc-issuer https://token.actions.githubusercontent.com \\\n"
            "  --certificate-github-workflow-ref refs/heads/main \\\n"
            f"  --certificate-github-workflow-sha {sha} SHA256SUMS\n"
            "sha256sum -c --ignore-missing SHA256SUMS\n"
            "```\n\n"
            "On Windows, compare `(Get-FileHash <file> -Algorithm SHA256).Hash` with the file's line in "
            "SHA256SUMS, ignoring case. Details: "
            "https://github.com/wnzn/metro/blob/main/docs/install.md#check-a-download\n\n")
    else:
        require(windows == "signed", f"unknown Windows signing mode {windows!r}")
    return notes + f"<!-- metro-release: {sha} -->"


def require_own_draft(release, version, sha):
    require(release.get("draft") is True and release.get("published_at") is None
            and release.get("tag_name") == f"v{version}"
            and release.get("author", {}).get("login") == "github-actions[bot]"
            and release.get("body") == release_notes(version, sha),
            f"not an unpublished staging draft owned by Metro CI for v{version} at {sha[:12]}")


def publish(api, version, sha, folder, prerelease):
    """Stage a draft with exactly the verified files, then make it public. Idempotent on rerun."""
    files = {p.name: p for p in folder.iterdir()}
    require(all(p.is_file() and not p.is_symlink() for p in files.values()), "the release folder holds non-files")
    require(set(files) == expected_assets(version), "the release folder is not the exact signed set")
    require(all(p.stat().st_size <= MAX_ASSET for p in files.values()), "an asset is too large")
    local = {name: sha256(path) for name, path in files.items()}
    require(api.tag_commit(version) == sha, f"tag v{version} does not point at {sha[:12]}")
    public = api.public_release(version)
    if public is not None:  # a successful earlier attempt: verify, never touch
        require(public.get("prerelease") is prerelease, f"published v{version} has the wrong prerelease status")
        check_assets(public, local, f"published v{version}")
        return "already published"
    # ci.yml serializes release publication (its `publish` job). Replace only a
    # proven, interrupted staging draft for this SHA, never a manual draft or a
    # release that was published.
    old = api.releases(f"v{version}")
    require(len(old) <= 1, f"more than one release or staging draft for v{version}")
    if old:
        require_own_draft(api.release(old[0]["id"]), version, sha)
        api.delete_release(old[0]["id"])
    require(api.public_release(version) is None, "a public release appeared before staging")
    staged = api.create_release(version, prerelease, sha)
    require_own_draft(staged, version, sha)
    for name in sorted(local):
        api.upload(staged, files[name])
    require(api.tag_commit(version) == sha, f"tag v{version} moved while staging")
    require(api.public_release(version) is None, "a public release appeared while staging")
    draft = api.release(staged["id"])
    require_own_draft(draft, version, sha)
    check_assets(draft, local, "staged draft")
    final = api.publish(staged["id"], prerelease)
    require(final.get("draft") is False and final.get("tag_name") == f"v{version}"
            and bool(final.get("prerelease")) == prerelease, "the release did not publish as expected")
    check_assets(api.release(staged["id"]), local, f"published v{version}")
    require(api.tag_commit(version) == sha, f"tag v{version} moved while publishing")
    return "published"


# --- CLI --------------------------------------------------------------------------

def outputs(values):
    path = os.environ.get("GITHUB_OUTPUT")
    lines = "".join(f"{k}={v}\n" for k, v in values.items())
    if path:
        with open(path, "a") as stream:
            stream.write(lines)
    print(lines, end="")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("version")
    sub.add_parser("windows-signing")
    sub.add_parser("provenance")
    sub.add_parser("decide")
    for name in ("authorize", "tag", "verify-tag", "publish"):
        p = sub.add_parser(name)
        p.add_argument("--version", required=True)
        if name == "publish":
            p.add_argument("--dir", required=True, type=Path)
    args = parser.parse_args()
    try:
        version, prerelease = read_version()
        if args.command == "version":
            outputs({"version": version, "prerelease": str(prerelease).lower()})
            return
        # The checked-in switch; an unreadable or unknown value fails every other command.
        windows = release_signatures.windows_signing()
        if args.command == "windows-signing":
            outputs({"windows_signing": windows})
            return
        sha = check_provenance(os.environ, git_head())
        if args.command == "provenance":
            outputs({"version": version, "prerelease": str(prerelease).lower(), "sha": sha})
            return
        if args.command != "decide":
            require(args.version == version, f"--version {args.version} is not the checked-in version {version}")
        prior = None
        if args.command in ("decide", "authorize", "tag"):
            # Only a version bump releases. Without one, stop before any GitHub call.
            prior = previous_version(os.environ.get("EVENT_BEFORE", ""))
            quiet = bump(version, prior)
            # authorize and tag run only after decide chose a release, so a
            # missing bump there is an inconsistency, never a quiet skip.
            require(not quiet or args.command == "decide",
                    f"this push did not change the version ({quiet}); refusing to {args.command}")
            if quiet:
                print(f"release_policy: {quiet} (before: {prior or 'none'}, now: {version}); CI only, no release",
                      file=sys.stderr)
                outputs({"release": "false", "state": quiet, "version": version,
                         "prerelease": str(prerelease).lower(), "windows_signing": windows})
                return
        api = GitHub(os.environ.get("GH_TOKEN", ""), api=os.environ.get("GITHUB_API_URL", "https://api.github.com"))
        if args.command == "decide":
            state = decide(api, version, sha, prior)
            outputs({"release": str(state in ("new", "resume")).lower(), "state": state, "version": version,
                     "prerelease": str(prerelease).lower(), "windows_signing": windows})
            return
        if args.command in ("authorize", "tag"):
            proceed = decide(api, version, sha, prior) in ("new", "resume")
            if proceed and args.command == "tag":
                create_tag(api, version, sha)
                # Close the race with an out-of-band publisher during tagging.
                proceed = decide(api, version, sha, prior) == "resume"
            outputs({"proceed": str(proceed).lower(), "windows_signing": windows})
        elif args.command == "verify-tag":
            require(api.tag_commit(version) == sha, f"tag v{version} does not point at {sha[:12]}")
            require(api.public_release(version) is None, "the version is already public; refusing to sign again")
        else:
            outputs({"result": publish(api, version, sha, args.dir, prerelease),
                     "url": f"https://github.com/{REPOSITORY}/releases/tag/v{version}"})
    except (PolicyError, release_signatures.ReleaseError, OSError, ValueError, KeyError, TypeError,
            http.client.HTTPException, subprocess.CalledProcessError, tomllib.TOMLDecodeError) as error:
        parser.exit(1, f"release_policy: {error}\n")


if __name__ == "__main__":
    main()
