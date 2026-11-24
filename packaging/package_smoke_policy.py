"""Classify a main PR's diff once, for the jobs ci.yml may leave out of that PR.

Three answers come from one `git diff` of merge-base(base, head)..head:

- skip_previews: every path is in a known location that no packaged build
  reads, and none is a build input. Lets the optional package previews skip.
- docs_only: every path is documentation (DOCS below), which no CI job reads.
  Lets `rust`, `containers` and `desktop` skip.
- containers: some path is, or may be, read by ci.yml's `containers` job.
  Only a diff that provably avoids CONTAINER_INPUT, and stays inside
  NOT_CONTAINER, sets it to false.

A path counts toward a skip only when it is a plain file (mode 100644 or
100755) in a location these lists know. Anything else, including paths they
do not know yet, symlinks, submodules, an empty diff or any Git error, runs
everything: skip_previews=false, docs_only=false, containers=true. Only pull
requests to literal main are classified; every other event runs everything.
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
# trees that only ci.yml's own jobs run, the self-hosted runner stack, the
# Compose files that only the containers job checks, and top-level Markdown.
NOT_PACKAGED = re.compile(rb"(?:docs/|tests/|examples/|ops/"
                          rb"|(?:[^/]+\.md|\.env\.example|\.gitignore|compose(?:\.[a-z0-9-]+)?\.yaml)$)")
# Documentation that no CI job builds, tests or packages: Markdown and images
# under docs/, top-level Markdown (README.md, AGENTS.md) and the runner stack's
# Markdown under ops/. Markdown anywhere else (crates/, apps/, packaging/,
# services/, .github/, tests/, examples/, dev/, ...) is not docs-only:
# examples/workspace is a runner workspace the tests read.
DOCS = re.compile(rb"(?:docs/.+\.(?:md|png|jpe?g|gif|svg|webp)|[^/]+\.md|ops/.+\.md)")
# Everything ci.yml's containers job reads: the Dockerfile and its context
# (Cargo files and all of crates/), the Compose files, the media service, the
# container runner scripts, the end-to-end modules library_reference.py
# imports, the Rust toolchain pin, and ci.yml itself.
CONTAINER_INPUT = re.compile(rb"(?:crates/|services/|tests/e2e/|rust-toolchain"
                             rb"|(?:Dockerfile|\.dockerignore|Cargo\.toml|Cargo\.lock|\.gitattributes"
                             rb"|compose(?:\.[^/]*)?\.yaml|packaging/metro-runner-container\.(?:sh|ps1)"
                             rb"|\.github/workflows/ci\.yml)$)")
# Known locations the containers job does not read (unless CONTAINER_INPUT
# claims the path first).
NOT_CONTAINER = re.compile(rb"(?:docs/|apps/|examples/|ops/|packaging/|tests/|\.github/|\.cargo/"
                           rb"|(?:[^/]+\.md|\.env\.example|\.gitignore)$)")

RUN_EVERYTHING = {"skip_previews": False, "docs_only": False, "containers": True}


def git(*args, root=None):
    return subprocess.run(["git", *args], cwd=root, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          timeout=40).stdout


def skippable(path):
    return not BUILD.match(path) and bool(NOT_PACKAGED.match(path))


def documentation(path):
    return bool(DOCS.fullmatch(path)) and skippable(path)


def outside_containers(path):
    return not CONTAINER_INPUT.match(path) and bool(NOT_CONTAINER.match(path))


def changed_paths(base, head, *, root=None):
    """Every path in merge-base(base, head)..head if all are plain files; None if empty, unknown or unreadable."""
    if not SHA.fullmatch(base) or not SHA.fullmatch(head):
        return None
    try:
        if git("rev-parse", "--is-shallow-repository", root=root) == b"true\n":
            # Checkout did not retain credentials. The public remote can serve
            # the immutable commits without them; failure runs everything.
            git("fetch", "--unshallow", "--no-tags", "--no-recurse-submodules", "--filter=blob:none",
                "origin", base, head, root=root)
        if git("rev-parse", "--is-shallow-repository", root=root) != b"false\n":
            return None
        if any(git("cat-file", "-t", sha, root=root) != b"commit\n" for sha in (base, head)):
            return None
        ancestor = git("merge-base", "--all", base, head, root=root).strip()
        if not re.fullmatch(rb"[a-f0-9]{40}", ancestor):
            return None
        # A rename is a deletion plus an addition, so both paths are checked.
        diff = git("diff", "--raw", "-z", "--no-renames", "--no-ext-diff", "--no-textconv", "--ignore-submodules=none",
                   ancestor.decode("ascii"), head, "--", root=root)
    except (OSError, subprocess.SubprocessError):
        return None
    if not diff or not diff.endswith(b"\0"):
        return None
    # Only ordinary files, executable or not. Symlinks, submodules and type
    # changes always run everything, whatever their path.
    parts = diff[:-1].split(b"\0")
    regular = rb"[a-f0-9]+ [a-f0-9]+"
    mode = rb"100(?:644|755)"
    allowed = re.compile(rb"(?::000000 " + mode + rb" " + regular + rb" A"
                         rb"|:" + mode + rb" " + mode + rb" " + regular + rb" M"
                         rb"|:" + mode + rb" 000000 " + regular + rb" D)")
    if len(parts) % 2 or not all(allowed.fullmatch(meta) for meta in parts[::2]):
        return None
    return parts[1::2]


def classify(base, head, *, root=None):
    paths = changed_paths(base, head, root=root)
    if not paths:
        return dict(RUN_EVERYTHING)
    return {"skip_previews": all(map(skippable, paths)),
            "docs_only": all(map(documentation, paths)),
            "containers": not all(map(outside_containers, paths))}


def skip_previews(base, head, *, root=None):
    """True only when merge-base(base, head)..head is non-empty and touches no build input."""
    return classify(base, head, root=root)["skip_previews"]


if __name__ == "__main__":
    main_pr = os.environ.get("GITHUB_EVENT_NAME") == "pull_request" and os.environ.get("GITHUB_BASE_REF") == "main"
    result = classify(os.environ.get("PR_BASE", ""), os.environ.get("PR_HEAD", "")) if main_pr else RUN_EVERYTHING
    # stdout is appended to $GITHUB_OUTPUT as one write; the summary goes to stderr.
    sys.stdout.write("".join(f"{key}={str(value).lower()}\n" for key, value in result.items()))
    print(f"Package previews: {'skip' if result['skip_previews'] else 'build'}; "
          f"docs only: {'yes' if result['docs_only'] else 'no'}; "
          f"containers: {'run' if result['containers'] else 'skip'}.", file=sys.stderr)
