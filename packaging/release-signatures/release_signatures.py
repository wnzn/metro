#!/usr/bin/env python3
"""Stage, sign and verify the final Metro release artifacts."""

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import tarfile


REPOSITORY = "https://github.com/wnzn/metro"
REPOSITORY_ID = "1467654868"
ISSUER = "https://token.actions.githubusercontent.com"
SOURCE_REF = "refs/heads/main"
MANIFEST = "SHA256SUMS"
BUNDLE = ".sigstore.json"
SIGNER_WORKFLOW = "sign-release-assets.yml"
CALLER_WORKFLOW = "ci.yml"
VERSION = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?")
COMMIT = re.compile(r"[0-9a-f]{40}")
INSTALLERS = {
    "install-metro-linux.sh", "install-metro-runner-linux.sh", "install-metro-macos.sh",
    "install-metro-windows.ps1", "metro-runner-container.sh", "metro-runner-container.ps1",
}
IMAGES = {
    "metro-runner": ("linux/amd64", "linux/arm64"),
    "metro-server": ("linux/amd64", "linux/arm64"),
    "metro-embeddinggemma2": ("linux/amd64",),
}


class ReleaseError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise ReleaseError(message)


def tag_ref(version):
    require(len(version) <= 100 and VERSION.fullmatch(version), "invalid release version")
    return f"refs/tags/v{version}"


def workflow_uri(filename):
    return f"{REPOSITORY}/.github/workflows/{filename}@{SOURCE_REF}"


def require_revision(revision):
    require(isinstance(revision, str) and COMMIT.fullmatch(revision), "expected a full release commit SHA")


def resolve_tag(version):
    ref = tag_ref(version)
    peeled = ref + "^{}"
    # Use the public origin, not a local/remapped remote, local tag or asset
    # metadata. Support both lightweight and annotated release tags.
    env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull, GIT_TERMINAL_PROMPT="0")
    result = subprocess.run(
        ["git", "ls-remote", "--exit-code", REPOSITORY + ".git", ref, peeled],
        cwd=Path.home().anchor, env=env, capture_output=True, timeout=30,
    )
    require(result.returncode == 0, "unable to resolve the exact public release tag")
    matches = {}
    for line in result.stdout.decode("ascii").splitlines():
        digest, sep, name = line.partition("\t")
        require(sep and COMMIT.fullmatch(digest) and name in (ref, peeled) and name not in matches,
                "unexpected response for the public release tag")
        matches[name] = digest
    require(ref in matches, "public release tag is missing")
    return matches.get(peeled, matches[ref])


# The Macs a release ships (release_assets.MACOS): Apple silicon, ad hoc
# signed (no Developer ID, not notarized).
MACOS_TARGETS = ("macos-arm64",)


def direct_archives(version):
    """The .tar.gz archives that get their own Sigstore bundle (Linux and macOS)."""
    return (f"Metro-{version}-linux-x64.tar.gz", f"Metro-runner-{version}-linux-x64.tar.gz",
            *(f"Metro-{version}-{target}.tar.gz" for target in MACOS_TARGETS))

# The checked-in switch for how official releases ship Windows. There is no
# default and no environment override: only this file decides, so changing it
# is a reviewed one-line pull request.
WINDOWS_SIGNING_FILE = Path(__file__).resolve().parents[1] / "windows-release-signing.txt"
WINDOWS_SIGNING_MODES = ("signed", "unsigned")


def parse_windows_signing(text):
    values = [line.strip() for line in text.splitlines() if line.strip() and not line.lstrip().startswith("#")]
    require(len(values) == 1 and values[0] in WINDOWS_SIGNING_MODES,
            "windows-release-signing.txt must hold exactly one value line: signed or unsigned")
    return values[0]


def windows_signing(path=None):
    """`signed` (Authenticode via windows-sign.yml) or `unsigned`, from the checked-in file."""
    path = WINDOWS_SIGNING_FILE if path is None else path
    try:
        text = read_small(path).decode("utf-8")
    except UnicodeDecodeError:
        raise ReleaseError("windows-release-signing.txt is not UTF-8") from None
    return parse_windows_signing(text)


def windows_msi(version, signing=None):
    """The release MSI's name. An unsigned release keeps windows-distribution.yml's unsigned name."""
    signing = windows_signing() if signing is None else signing
    require(signing in WINDOWS_SIGNING_MODES, f"unknown Windows signing mode {signing!r}")
    return f"Metro-{version}-windows-x64" + (".msi" if signing == "signed" else ".unsigned-preview.msi")


def binaries(version, windows=None):
    """The release archives and the MSI; `windows` is the signing mode (default: the checked-in one)."""
    return set(direct_archives(version)) | {
        f"Metro-{version}-windows-x64.zip", windows_msi(version, windows),
    }


def regular_file(path):
    require(stat.S_ISREG(path.lstat().st_mode), f"{path.name!r} is not a plain file")


def exact_files(folder, expected):
    require(stat.S_ISDIR(folder.lstat().st_mode), f"{folder.name!r} is not a plain directory")
    have = {p.name for p in folder.iterdir()}
    require(have == expected, f"Wrong files in {folder.name!r}: missing {sorted(expected - have)!r}; "
            f"unexpected {sorted(have - expected)!r}")
    for name in expected:
        regular_file(folder / name)


def read_small(path, limit=65536):
    regular_file(path)
    with path.open("rb") as stream:
        data = stream.read(limit + 1)
    require(0 < len(data) <= limit, f"{path.name!r} is empty or too large")
    return data


def checksum(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def manifest_bytes(hashes, names):
    return "".join(f"{hashes[name]}  {name}\n" for name in sorted(names)).encode("ascii")


def check_payloads(folder, version):
    base = binaries(version) | INSTALLERS
    hashes = {name: checksum(folder / name) for name in base}
    for name in binaries(version):
        sidecar = f"{name}.sha256"
        data = read_small(folder / sidecar)
        expected = manifest_bytes(hashes, {name})
        # Windows tools may write CRLF. Do not rewrite the sidecar; its raw bytes
        # are themselves covered in the final manifest.
        require(data in (expected, expected.replace(b"\n", b"\r\n")),
                f"pre-signing checksum mismatch: {sidecar}")
        hashes[sidecar] = hashlib.sha256(data).hexdigest()
    return hashes


def image_evidence(path, version, revision=None):
    raw = read_small(path)
    lines = raw.decode("ascii").splitlines()
    require(len(lines) >= 2 and lines[0] == f"# Metro container images v{version}",
            "IMAGES.txt is for another version")
    prefix = "# Source commit: "
    source = lines[1].removeprefix(prefix)
    require(lines[1].startswith(prefix) and COMMIT.fullmatch(source) and (revision is None or source == revision),
            "IMAGES.txt must describe the release source commit")
    records = set()
    row = re.compile(
        r"ghcr\.io/wnzn/(metro-(?:runner|server|embeddinggemma2))(?::([^@ ]+))?"
        r"@sha256:[0-9a-f]{64}  ([a-z0-9/,]+)"
    )
    for line in lines[2:]:
        if not line or line.startswith("#"):
            continue
        match = row.fullmatch(line)
        require(match, "IMAGES.txt contains an unexpected image reference")
        image, tag, platforms = match.groups()
        require(tag is None or tag == f"v{version}", "IMAGES.txt contains a wrong tag")
        entry = (image, tag is not None, platforms)
        require(entry not in records, "IMAGES.txt repeats an image reference")
        records.add(entry)

    expected = set()
    for image, platforms in IMAGES.items():
        if image == "metro-embeddinggemma2" and not any(key[0] == image for key in records):
            continue
        expected.add((image, True, ",".join(platforms)))
        expected.update((image, False, platform) for platform in platforms)
    require(records == expected, "IMAGES.txt has missing or unexpected image platforms")
    return raw


def prepare(assets, image_digests, directory, version, revision):
    tag_ref(version)
    require_revision(revision)
    core = binaries(version) | INSTALLERS
    names = core | {f"{name}.sha256" for name in binaries(version)} | {MANIFEST}
    exact_files(assets, names)
    hashes = check_payloads(assets, version)
    # The aggregate currently excludes sidecars from SHA256SUMS. If it starts
    # listing them, require the complete set, not a subset.
    require(read_small(assets / MANIFEST) in
            (manifest_bytes(hashes, core), manifest_bytes(hashes, hashes)),
            "aggregate SHA256SUMS does not match the staged release files")
    images = image_evidence(image_digests / "IMAGES.txt", version, revision)

    directory.mkdir()
    for name in hashes:
        shutil.copyfile(assets / name, directory / name)
        require(checksum(directory / name) == hashes[name], f"{name} changed while staging")
    (directory / "IMAGES.txt").write_bytes(images)
    hashes["IMAGES.txt"] = hashlib.sha256(images).hexdigest()
    (directory / MANIFEST).write_bytes(manifest_bytes(hashes, hashes))


def check_directory(directory, version, revision=None, signed=True):
    tag_ref(version)
    artifacts = binaries(version)
    names = artifacts | INSTALLERS | {f"{name}.sha256" for name in artifacts} | {MANIFEST, "IMAGES.txt"}
    if signed:
        names.update(f"{name}{BUNDLE}" for name in (MANIFEST, *direct_archives(version)))
    exact_files(directory, names)
    hashes = check_payloads(directory, version)
    hashes["IMAGES.txt"] = hashlib.sha256(image_evidence(directory / "IMAGES.txt", version, revision)).hexdigest()
    require(read_small(directory / MANIFEST) == manifest_bytes(hashes, hashes),
            "final SHA256SUMS does not match the release files")


def run(command, input_data=None):
    # Never forward tool diagnostics that might contain a token or an HTTP
    # request URL. In particular, do not log the OIDC request or its response.
    env = {key: value for key, value in os.environ.items() if not key.startswith(("COSIGN_", "SIGSTORE_"))}
    try:
        return subprocess.run(command, input=input_data, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, check=True, env=env).stdout
    except subprocess.CalledProcessError as exc:
        raise ReleaseError(f"{Path(command[0]).name} {command[1]} failed (exit {exc.returncode})") from None


def unique_object(pairs):
    result = dict(pairs)
    require(len(result) == len(pairs), "bundle contains duplicate JSON keys")
    return result


def check_cert_extensions(bundle, revision):
    """Enforce GitHub provenance claims beyond cosign's issuer and exact SAN checks."""
    require_revision(revision)
    data = json.loads(bundle, object_pairs_hook=unique_object)
    require(data.get("mediaType") == "application/vnd.dev.sigstore.bundle.v0.3+json",
            "expected a Sigstore v0.3 bundle")
    # Cosign has already verified this exact bundle against public trust roots.
    # Only the single leaf certificate format is allowed, not a chain or key.
    cert = base64.b64decode(data["verificationMaterial"]["certificate"]["rawBytes"], validate=True)
    listing = run(["openssl", "asn1parse", "-inform", "DER"], input_data=cert).split(b"\n")
    expectations = {
        9: workflow_uri(SIGNER_WORKFLOW), 10: revision,  # reusable signing workflow URI and commit
        12: REPOSITORY, 13: revision, 14: SOURCE_REF, 15: REPOSITORY_ID,
        18: workflow_uri(CALLER_WORKFLOW), 19: revision, 20: "push",  # top-level workflow and commit
    }
    for number, expected in expectations.items():
        oid = f"1.3.6.1.4.1.57264.1.{number}".encode()
        oid_line = rb"\s*\d+:d=5\s+hl=\d+ l=\s*\d+ prim: OBJECT\s+:" + re.escape(oid)
        indices = [index for index, line in enumerate(listing) if re.fullmatch(oid_line, line)]
        require(len(indices) == 1, f"missing or repeated Fulcio extension {oid.decode()}")
        value = expected.encode("utf-8")
        # Fulcio encodes these extensions as a DER UTF8String inside the
        # extension's OCTET STRING. Compare the entire value, not a substring.
        length = bytes([len(value)]) if len(value) < 128 else bytes([0x81, len(value)])
        encoded = (b"\x0c" + length + value).hex().upper().encode()
        want = rb"\s*\d+:d=5\s+hl=\d+ l=\s*\d+ prim: OCTET STRING\s+\[HEX DUMP\]:" + encoded
        require(re.fullmatch(want, listing[indices[0] + 1]), f"wrong value for Fulcio extension {oid.decode()}")


def verify_file(cosign, artifact, version, revision):
    require_revision(revision)
    tag_ref(version)
    name = artifact.name
    require(name == MANIFEST or name in direct_archives(version),
            "expected SHA256SUMS or a .tar.gz archive named for the exact release version")

    identity = workflow_uri(SIGNER_WORKFLOW)
    regular_file(artifact)
    bundle = artifact.with_name(artifact.name + BUNDLE)
    data = read_small(bundle)
    run([str(cosign), "verify-blob", "--bundle", str(bundle.absolute()),
         "--certificate-identity", identity, "--certificate-oidc-issuer", ISSUER, str(artifact.absolute())])
    require(read_small(bundle) == data, "signature bundle changed during verification")
    check_cert_extensions(data, revision)
    if name == MANIFEST:
        expected = sorted(binaries(version) | INSTALLERS | {"IMAGES.txt"}
                          | {f"{binary}.sha256" for binary in binaries(version)})
        rows = read_small(artifact).decode("ascii").splitlines()
        require(len(rows) == len(expected) and all(
            re.fullmatch(r"[0-9a-f]{64}  " + re.escape(filename), row)
            for row, filename in zip(rows, expected)
        ), "signed SHA256SUMS has unexpected files or is for another version")
    else:
        # The detached signature covers bytes, not the downloaded filename.
        # Check the version inside the authenticated archive as well.
        metadata = f"{name.removesuffix('.tar.gz')}/RELEASE.json"
        with tarfile.open(artifact, "r:gz") as archive:
            matches = [entry for entry in archive if entry.name == metadata]
            require(len(matches) == 1 and matches[0].isfile() and 0 < matches[0].size <= 65536,
                    "signed archive lacks its exact release metadata")
            with archive.extractfile(matches[0]) as stream:
                require(json.load(stream).get("version") == version, "signed archive is for another version")


def verify(cosign, directory, version, revision):
    verify_file(cosign, directory / MANIFEST, version, revision)
    check_directory(directory, version, revision)
    for name in direct_archives(version):
        verify_file(cosign, directory / name, version, revision)


def sign(cosign, directory, version, revision):
    require_revision(revision)
    check_directory(directory, version, revision, signed=False)
    for name in (*direct_archives(version), MANIFEST):
        artifact = directory / name
        bundle = directory / (name + BUNDLE)
        run([str(cosign), "sign-blob", "--yes", "--oidc-provider", "github-actions",
             "--fulcio-auth-flow", "token", "--bundle", str(bundle.absolute()), str(artifact.absolute())])
    verify(cosign, directory, version, revision)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("resolve-tag", "check-tag"):
        command = commands.add_parser(name)
        command.add_argument("--version", required=True)
        if name == "check-tag":
            command.add_argument("--revision", required=True)
    for name in ("prepare", "sign", "verify", "verify-file"):
        command = commands.add_parser(name)
        command.add_argument("--version", required=True)
        command.add_argument("--revision", required=True)
        if name != "prepare":
            command.add_argument("--cosign", type=Path, default=Path("cosign"))
        if name == "verify-file":
            command.add_argument("artifact", type=Path)
        else:
            command.add_argument("--directory", required=True, type=Path)
        if name == "prepare":
            command.add_argument("--assets", required=True, type=Path)
            command.add_argument("--image-digests", required=True, type=Path)
    args = parser.parse_args()
    try:
        if args.command == "resolve-tag":
            print(resolve_tag(args.version))
            return
        if args.command == "check-tag":
            require_revision(args.revision)
            require(resolve_tag(args.version) == args.revision, "public release tag does not point to the pinned commit")
        elif args.command == "prepare":
            prepare(args.assets, args.image_digests, args.directory, args.version, args.revision)
        elif args.command == "sign":
            sign(args.cosign, args.directory, args.version, args.revision)
        elif args.command == "verify":
            verify(args.cosign, args.directory, args.version, args.revision)
        else:
            verify_file(args.cosign, args.artifact, args.version, args.revision)
    except (ReleaseError, OSError, ValueError, TypeError, KeyError, AttributeError,
            subprocess.TimeoutExpired, tarfile.TarError) as error:
        parser.exit(1, f"release_signatures: {error}\n")
    print(f"Release signature gate: {args.command} OK")


if __name__ == "__main__":
    main()
