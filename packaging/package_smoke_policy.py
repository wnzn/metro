"""Skip optional package previews only for a proven docs-only main PR diff."""

import os
import re
import subprocess
import sys


SHA = re.compile(r"[a-f0-9]{40}")
DOC = re.compile(rb"(?:AGENTS\.md|README\.md|docs/[A-Za-z0-9][A-Za-z0-9._-]*\.md)")


def git(*args, root=None):
    return subprocess.run(["git", *args], cwd=root, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          timeout=40).stdout


def docs_only(base, head, *, root=None):
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
        # A rename is a deletion plus an addition, so both sides must be docs.
        diff = git("diff", "--raw", "-z", "--no-renames", "--no-ext-diff", "--no-textconv", "--ignore-submodules=none",
                   ancestor.decode("ascii"), head, "--", root=root)
        if not diff or not diff.endswith(b"\0"):
            return False
        # Only ordinary, non-executable files. Refuse symlinks, submodules and
        # mode changes even when their path looks like Markdown.
        parts = diff[:-1].split(b"\0")
        regular = rb"[a-f0-9]+ [a-f0-9]+"
        allowed = re.compile(rb"(?::000000 100644 " + regular + rb" A"
                             rb"|:100644 100644 " + regular + rb" M"
                             rb"|:100644 000000 " + regular + rb" D)")
        return len(parts) % 2 == 0 and all(
            allowed.fullmatch(meta) and DOC.fullmatch(path) for meta, path in zip(parts[::2], parts[1::2], strict=True))
    except (OSError, subprocess.SubprocessError):
        return False


if __name__ == "__main__":
    main_pr = os.environ.get("GITHUB_EVENT_NAME") == "pull_request" and os.environ.get("GITHUB_BASE_REF") == "main"
    skip = main_pr and docs_only(os.environ.get("PR_BASE", ""), os.environ.get("PR_HEAD", ""))
    print("Package previews: skip (verified documentation only)." if skip else "Package previews: build.")
    sys.exit(0 if skip else 1)
