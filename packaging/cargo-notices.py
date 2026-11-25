#!/usr/bin/env python3
"""Collect notices from the built target's Cargo dependency sources."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tomllib


NOTICE_NAME = re.compile(r"^(LICENSE|LICENCE|COPYING|NOTICE|COPYRIGHT|UNLICENSE)([-_.]|$)", re.I)
REGISTRY_PACKAGE = re.compile(r"^([A-Za-z0-9_-]+) v(\S+)(?: \(proc-macro\))?$")
RIG_SPLIT_CRATES = {"rig-agent", "rig-http", "rig-memory", "rig-reqwest"}
# Crates whose package omits the license text that every other crate of the
# same monorepo ships (same license expression): use a sibling's copy.
# cms (RustCrypto/formats, via sigstore-tsa) has no LICENSE files in 0.2.3.
MONOREPO_LICENSE_DONORS = {"cms": ("https://github.com/RustCrypto/formats/", "x509-cert")}
# Crates published without their repository's LICENSE: a reviewed copy of
# that file at the exact commit the crate was packaged from (its
# .cargo_vcs_info.json), pinned by SHA-256.
VENDORED_LICENSES = {
    ("https://github.com/sigstore/sigstore-rust", "6ce3f74f93f80cc06538b2d711cdb687c66ba1ae"): (
        "third-party-licenses/sigstore-rust-6ce3f74f93f80cc06538b2d711cdb687c66ba1ae.LICENSE",
        "c71d239df91726fc519c6eb72d318ec65820627232b2f796219e87dcf35d0ab4",
    ),
}


def git_snapshot(directory):
    return json.loads((directory / ".cargo_vcs_info.json").read_text(encoding="utf-8"))["git"]["sha1"]


def cargo_licenses(source, name, version):
    manifest = tomllib.loads((source / "Cargo.toml").read_text(encoding="utf-8"))["package"]
    paths = {p for p in source.iterdir() if p.is_file() and NOTICE_NAME.match(p.name)}
    if manifest.get("license-file"):
        path = source / manifest["license-file"]
        if not path.resolve().is_relative_to(source.resolve()):
            raise ValueError(f"{name} {version}: license path escapes the Cargo source")
        paths.add(path)
    if not paths and name in RIG_SPLIT_CRATES:
        # Split Rig 0.47.0 crates omit the repository's MIT license. Use
        # the upstream rig-core package's copy only at the identical commit.
        core = source.parent / f"rig-core-{version}"
        if git_snapshot(core) == git_snapshot(source):
            paths.add(core / "LICENSE")
    if not paths and name in MONOREPO_LICENSE_DONORS:
        repo, donor = MONOREPO_LICENSE_DONORS[name]
        for candidate in sorted(source.parent.glob(f"{donor}-[0-9]*")):
            donor_manifest = tomllib.loads((candidate / "Cargo.toml").read_text(encoding="utf-8"))["package"]
            if (manifest.get("repository", "").startswith(repo)
                    and donor_manifest.get("repository", "").startswith(repo)
                    and donor_manifest.get("license") == manifest.get("license")):
                paths.update(p for p in candidate.iterdir() if p.is_file() and NOTICE_NAME.match(p.name))
                break
    vcs = source / ".cargo_vcs_info.json"
    if not paths and vcs.is_file():
        vendored = VENDORED_LICENSES.get((manifest.get("repository", ""), git_snapshot(source)))
        if vendored:
            path = Path(__file__).resolve().parent / vendored[0]
            if hashlib.sha256(path.read_bytes()).hexdigest() != vendored[1]:
                raise ValueError(f"{path.name}: vendored license text changed")
            yield f"{name}-{version}/LICENSE (repository copy at its packaged commit)", path.read_text(encoding="utf-8").rstrip()
            return
    if name == "aws-lc-sys":
        paths.add(source / "aws-lc/LICENSE")
    if not paths:
        raise ValueError(f"{name} {version} omits its license text; review the upstream source")
    for path in sorted(paths):
        if path.is_symlink():
            raise ValueError(f"{name} {version}: unexpected symlink in license files")
        text = path.read_text(encoding="utf-8")
        if not text.strip():
            raise ValueError(f"{path}: empty license text")
        yield path.relative_to(source.parent).as_posix(), text.rstrip()


def collect(packages, target, offline=False):
    registry = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")) / "registry/src"
    dependencies = {}
    entries = subprocess.check_output(
        ["cargo", "tree", "--locked", *(["--offline"] if offline else []), "--target", target,
         "--edges", "normal", "--prefix", "none", "--format", "{p}\t{l}",
         *(value for package in packages for value in ("--package", package))],
        text=True,
    )
    for entry in filter(None, entries.splitlines()):
        spec, license_id = entry.removesuffix(" (*)").split("\t", 1)
        match = REGISTRY_PACKAGE.fullmatch(spec)
        if not match:
            if " (" in spec and Path(spec.rsplit(" (", 1)[1].removesuffix(")")).is_absolute():
                continue  # the project's own workspace packages
            raise ValueError(f"unsupported Cargo source: {spec}")
        dependencies[match.groups()] = license_id
    lines = [f"Third-party Cargo notices ({target})", "Packages: " + ", ".join(sorted(packages)), ""]
    for (name, version), license_id in sorted(dependencies.items()):
        paths = sorted(registry.glob(f"*/{name}-{version}"))
        if len(paths) != 1:
            raise ValueError(f"expected exactly one unpacked Cargo source for {name} {version}")
        lines.extend(["=" * 72, f"{name} {version} — {license_id or 'see license file'}"])
        for path, text in cargo_licenses(paths[0], name, version):
            lines.extend(["", f"-- {path} --", text, ""])
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--package", action="append", required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--offline", action="store_true")
    args = parser.parse_args()
    try:
        notices = collect(args.package, args.target, args.offline)
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_bytes(notices.encode("utf-8"))
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"cargo-notices: {error}\n")


if __name__ == "__main__":
    main()
