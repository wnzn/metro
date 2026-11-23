"""Skip optional package previews only for a main PR whose diff provably touches no build input.

A path can let the previews be skipped only when it is a plain file (mode 100644
or 100755) in a known location that no packaged build reads, and it is not a
build input. Anything else, including paths this list does not know yet, builds.
"""

import os
import re
import subprocess
import sys


SHA = re.compile(r"[a-f0-9]{40}")
# Inputs of release.yml and the trees it calls. Always relevant, even under a
# location listed in NOT_PACKAGED. crates/desktop/build.rs reads apps/desktop.
BUILD = re.compile(rb"(?:\.github/|apps/desktop/|crates/|packaging/|services/|\.cargo/|rust-toolchain"
                   rb"|(?:Cargo\.toml|Cargo\.lock|Dockerfile|\.dockerignore|\.gitattributes)$)")
# Locations that no package preview reads: docs, the end-to-end and example
# trees that only ci.yml's own jobs run, the self-hosted runner stack, and the
# Compose files that only the containers job checks.
NOT_PACKAGED = re.compile(rb"(?:docs/|tests/|examples/|ops/"
                          rb"|(?:AGENTS\.md|README\.md|\.env\.example|\.gitignore|compose(?:\.[a-z0-9-]+)?\.yaml)$)")


def git(*args, root=None):
    return subprocess.run(["git", *args], cwd=root, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          timeout=40).stdout


def skippable(path):
    return not BUILD.match(path) and bool(NOT_PACKAGED.match(path))


def skip_previews(base, head, *, root=None):
    """True only when merge-base(base, head)..head is non-empty and touches no build input."""
    if not SHA.fullmatch(base) or not SHA.fullmatch(head):
        return False
    try:
        if git("rev-parse", "--is-shallow-repository", root=root) == b"true\n":
            # Checkout did not retain credentials. The public remote can serve
            # the immutable commits without them; failure keeps the previews.
            git("fetch", "--unshallow", "--no-tags", "--no-recurse-submodules", "--filter=blob:none",
                "origin", base, head, root=root)
        if git("rev-parse", "--is-shallow-repository", root=root) != b"false\n":
            return False
        if any(git("cat-file", "-t", sha, root=root) != b"commit\n" for sha in (base, head)):
            return False
        ancestor = git("merge-base", "--all", base, head, root=root).strip()
        if not re.fullmatch(rb"[a-f0-9]{40}", ancestor):
            return False
        # A rename is a deletion plus an addition, so both paths are checked.
        diff = git("diff", "--raw", "-z", "--no-renames", "--no-ext-diff", "--no-textconv", "--ignore-submodules=none",
                   ancestor.decode("ascii"), head, "--", root=root)
        if not diff or not diff.endswith(b"\0"):
            return False
        # Only ordinary files, executable or not. Symlinks, submodules and type
        # changes always build, whatever their path.
        parts = diff[:-1].split(b"\0")
        regular = rb"[a-f0-9]+ [a-f0-9]+"
        mode = rb"100(?:644|755)"
        allowed = re.compile(rb"(?::000000 " + mode + rb" " + regular + rb" A"
                             rb"|:" + mode + rb" " + mode + rb" " + regular + rb" M"
                             rb"|:" + mode + rb" 000000 " + regular + rb" D)")
        return len(parts) % 2 == 0 and all(
            allowed.fullmatch(meta) and skippable(path) for meta, path in zip(parts[::2], parts[1::2], strict=True))
    except (OSError, subprocess.SubprocessError):
        return False


if __name__ == "__main__":
    main_pr = os.environ.get("GITHUB_EVENT_NAME") == "pull_request" and os.environ.get("GITHUB_BASE_REF") == "main"
    skip = main_pr and skip_previews(os.environ.get("PR_BASE", ""), os.environ.get("PR_HEAD", ""))
    print("Package previews: skip (no build input changed)." if skip else "Package previews: build.")
    sys.exit(0 if skip else 1)
