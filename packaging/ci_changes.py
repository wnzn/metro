"""Decide which parts of ci.yml a change needs, from one `git diff`.

ci.yml's `workflows` job runs this for a pull request into main (the diff of
merge-base(base, head)..head) and for a push to main (the diff of
github.event.before..github.sha). It prints one answer per segment, and
"true" means run:

- rust:       the `rust` job (Cargo build, clippy, tests, end-to-end scripts).
- desktop:    the `desktop` job (nsh check and nsh test in apps/desktop).
- containers: the `containers` job (images, Compose files, media service).
- packaging:  the `packaging` job (packaging, release and signing unit tests).
- previews:   `package-smoke`, the read-only release.yml build.

Each path gets the union of the segments of every rule in RULES it matches. A
path no rule knows runs everything. Documentation (Markdown and images under
docs/, top-level Markdown, Markdown under ops/ and apps/desktop/), the desktop
developer scripts in apps/desktop/dev/, ops/ (its own workflow,
check-runner-stack.yml, tests it), .gitignore and .env.example are known and
need no segment, so a change made only of them runs none.

A path counts only when it is a plain file (mode 100644 or 100755). Anything
else runs every segment: symlinks, submodules and type changes, an empty diff,
any Git error, other events or base branches, and for pushes a missing,
all-zero (new branch) or non-ancestor `before`, a forced push or a deleted
ref. The two version files (Cargo.toml and apps/desktop/nsh.json) also run
every segment, so a version bump, the only push that can release, runs every
gate (ci.yml enforces this a second way too).
"""

import os
import re
import subprocess
import sys


SEGMENTS = ("rust", "desktop", "containers", "packaging", "previews")
ALL = frozenset(SEGMENTS)
NONE = frozenset()
SHA = re.compile(r"[a-f0-9]{40}")
ZERO = "0" * 40
# apps/desktop/ except dev/ and Markdown: what nsh check, nsh test and nsh
# package read. tsconfig.json excludes dev/, and package.py rejects any
# bundled asset that App.tsx does not import.
DESKTOP_SOURCE = r"apps/desktop/(?!dev/)(?!.*\.md$).+"

# (path pattern, segments). Patterns match the whole path.
RULES = tuple((re.compile(pattern.encode()), frozenset(segments)) for pattern, segments in (
    # Read by every job, or change how every file is checked out.
    (r"\.github/workflows/ci\.yml|\.gitattributes", ALL),
    # The checked-in version: a release can only come from a change here.
    (r"Cargo\.toml|apps/desktop/nsh\.json", ALL),
    # Known, and read by no ci.yml job.
    (r"docs/.+\.(?:md|png|jpe?g|gif|svg|webp)|[^/]+\.md|ops/.+\.md|apps/desktop/.+\.md", NONE),
    # This exact release-notes companion is prose, not a packaging input.
    (r"packaging/README\.md", NONE),
    (r"apps/desktop/dev/.+|ops/.+|\.gitignore|\.env\.example", NONE),
    # Rust workspace and everything its tests and end-to-end scripts read.
    # crates/ is also the Docker build context and a release build input.
    (r"crates/.+|Cargo\.lock|rust-toolchain(?:\.toml)?", {"rust", "containers", "previews"}),
    (r"\.cargo/.+", {"rust", "previews"}),
    (r"tests/e2e/.+", {"rust", "containers"}),  # containers runs library_reference.py
    (r"tests/.+|examples/.+", {"rust"}),
    # crates/desktop/build.rs embeds the icon; package.py and the signing tests read the assets.
    (r"apps/desktop/assets/.+", {"rust", "desktop", "packaging", "previews"}),
    # package.py and the packaging tests read the icon modules, icons and font.
    (r"apps/desktop/src/ui/.+", {"packaging"}),
    (DESKTOP_SOURCE, {"desktop", "previews"}),
    (r"packaging/nsh_ci_tests\.py", {"desktop"}),
    # The container images, the media service and the Compose files.
    (r"Dockerfile|\.dockerignore|services/.+", {"containers", "previews"}),
    # test_image_release.py reads compose.yaml and compose.prebuilt.yaml.
    (r"compose(?:\.[a-z0-9-]+)?\.yaml", {"containers", "packaging"}),
    # The packaging tests read every workflow; release.yml and its calls build the previews.
    (r"packaging/(?!README\.md$).+|\.github/.+", {"packaging", "previews"}),
    (r"packaging/metro-runner-container\.(?:sh|ps1)", {"containers"}),
))


def run_everything():
    return dict.fromkeys(SEGMENTS, True)


def git(*args, root=None):
    return subprocess.run(["git", *args], cwd=root, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          timeout=40).stdout


def segments(path):
    """The segments one changed path needs; every segment when no rule knows it."""
    found = [needed for pattern, needed in RULES if pattern.fullmatch(path)]
    return frozenset().union(*found) if found else ALL


def changed_paths(base, head, *, push=False, root=None):
    """Every path in the diff if all are plain files; None if empty, unknown or unreadable.

    A pull request diffs merge-base(base, head)..head. A push diffs base..head
    and requires base to be an ancestor of head (otherwise it was rewritten).
    """
    if not SHA.fullmatch(base) or not SHA.fullmatch(head) or ZERO in (base, head):
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
        if push and ancestor.decode("ascii") != base:
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


def classify(base, head, *, push=False, root=None):
    paths = changed_paths(base, head, push=push, root=root)
    if not paths:
        return run_everything()
    needed = frozenset().union(*map(segments, paths))
    return {segment: segment in needed for segment in SEGMENTS}


def from_environment(env, root=None):
    """The answer for this run's event, from the variables ci.yml's step sets."""
    kind = env.get("GITHUB_EVENT_NAME")
    if kind == "pull_request" and env.get("GITHUB_BASE_REF") == "main":
        return classify(env.get("PR_BASE", ""), env.get("PR_HEAD", ""), root=root)
    if (kind == "push" and env.get("GITHUB_REF") == "refs/heads/main"
            and env.get("EVENT_FORCED") == "false" and env.get("EVENT_DELETED") == "false"
            and env.get("EVENT_AFTER") == env.get("GITHUB_SHA")):
        return classify(env.get("EVENT_BEFORE", ""), env.get("GITHUB_SHA", ""), push=True, root=root)
    return run_everything()


if __name__ == "__main__":
    result = from_environment(os.environ)
    # stdout is appended to $GITHUB_OUTPUT as one write; the summary goes to stderr.
    sys.stdout.write("".join(f"{key}={str(value).lower()}\n" for key, value in result.items()))
    print("CI segments: " + ", ".join(f"{key} {'run' if value else 'skip'}" for key, value in result.items()) + ".",
          file=sys.stderr)
