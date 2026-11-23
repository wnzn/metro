#!/usr/bin/env python3
"""Developer ID signing, notarization and stapling of a macOS desktop archive.

Tag builds only. Input: the ad hoc signed `Metro-<v>-macos-<arch>.tar.gz` that
`package.py` wrote (verified against its sidecar and RELEASE.json). Output: the
same archive name in a different directory, re-signed with the Developer ID
identity from a disposable keychain (hardened runtime, secure timestamp, nested
code first, never --deep), accepted by Apple's notary service, stapled, and
re-archived with a new RELEASE.json and .sha256. The result is then extracted
again with the system tar (as the installer does) and checked with codesign,
stapler and spctl. Nothing is written to the output directory unless every check
passes.

Credentials come only from these environment variables (checked by name; their
values are never printed): see SECRET_ENV. MACOS_SIGNING_TEAM_ID is an optional,
non-secret repository variable; when set, the identity must belong to that team.
"""

import argparse
import base64
import binascii
import contextlib
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import secrets
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile

# `python3 -I` does not put the script's folder on sys.path; package.py lives beside this file.
sys.path.insert(0, str(Path(__file__).resolve().parent))
import package  # noqa: E402

SECRET_ENV = (
    "MACOS_SIGNING_P12_BASE64",    # Developer ID Application certificate + private key, .p12, base64
    "MACOS_SIGNING_P12_PASSWORD",  # the .p12 export password
    "APPLE_NOTARY_KEY_ID",         # App Store Connect API key id (10 characters)
    "APPLE_NOTARY_ISSUER_ID",      # App Store Connect issuer id (UUID)
    "APPLE_NOTARY_KEY_P8",         # the AuthKey_<id>.p8 contents (PEM)
)
TEAM_ENV = "MACOS_SIGNING_TEAM_ID"
FORBIDDEN_ENTITLEMENTS = ("com.apple.security.get-task-allow",)
TARGETS = ("macos-arm64", "macos-x64")
# Code in the outer Metro.app, signed in this order (inside out).
INNER_APP = "Contents/Helpers/MetroUI.app"
INNER_EXE = INNER_APP + "/Contents/MacOS/Metro"
SERVER = "Contents/MacOS/metro-server"
LAUNCHER = "Contents/MacOS/Metro"
TUI = "Contents/MacOS/metro-tui"  # optional terminal client; signed like metro-server
MACHO_MAGIC = {b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf", b"\xce\xfa\xed\xfe", b"\xfe\xed\xfa\xce",
               b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca"}


class ReleaseError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise ReleaseError(message)


def sha256(path):
    with open(path, "rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def private_write(path, data):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as stream:
        stream.write(data)


# --- credentials -----------------------------------------------------------

def missing_credentials(env):
    return [name for name in SECRET_ENV if not env.get(name, "").strip()]


class Credentials:
    """Validated signing and notary credentials; errors name variables, never values."""

    def __init__(self, env):
        missing = missing_credentials(env)
        require(not missing, "missing signing credentials: " + ", ".join(missing))
        try:
            self.p12 = base64.b64decode("".join(env["MACOS_SIGNING_P12_BASE64"].split()), validate=True)
        except (binascii.Error, ValueError):
            raise ReleaseError("MACOS_SIGNING_P12_BASE64 is not valid base64") from None
        require(len(self.p12) > 100, "MACOS_SIGNING_P12_BASE64 does not hold a .p12 file")
        self.p12_password = env["MACOS_SIGNING_P12_PASSWORD"]
        self.key_id = env["APPLE_NOTARY_KEY_ID"].strip()
        require(re.fullmatch(r"[A-Z0-9]{10}", self.key_id), "APPLE_NOTARY_KEY_ID must be a 10-character key id")
        self.issuer = env["APPLE_NOTARY_ISSUER_ID"].strip()
        require(re.fullmatch(r"[0-9a-fA-F]{8}(-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}", self.issuer),
                "APPLE_NOTARY_ISSUER_ID must be the issuer UUID")
        self.p8 = env["APPLE_NOTARY_KEY_P8"].strip() + "\n"
        require(self.p8.startswith("-----BEGIN PRIVATE KEY-----") and "-----END PRIVATE KEY-----" in self.p8,
                "APPLE_NOTARY_KEY_P8 must be the PEM contents of the AuthKey .p8 file")
        team = env.get(TEAM_ENV, "").strip()
        require(not team or re.fullmatch(r"[A-Z0-9]{10}", team), f"{TEAM_ENV} must be a 10-character team id")
        self.team = team or None
        self.redact = [v for v in (env["MACOS_SIGNING_P12_BASE64"], self.p12_password, self.key_id, self.issuer,
                                   env["APPLE_NOTARY_KEY_P8"].strip()) if v]


# --- commands ---------------------------------------------------------------

class Runner:
    """Runs a tool with captured output. Failures name the tool, never its arguments,
    and any known secret in the output is replaced before it is shown."""

    def __init__(self):
        self.secrets = []

    def redact(self, *values):
        self.secrets.extend(v for v in values if v)

    def clean(self, text):
        for value in sorted(self.secrets, key=len, reverse=True):
            text = text.replace(value, "[redacted]")
        return text

    def __call__(self, argv, check=True, cwd=None):
        argv = [str(a) for a in argv]
        result = subprocess.run(argv, capture_output=True, text=True, cwd=cwd)
        if check and result.returncode != 0:
            tool = " ".join(argv[:2] if argv[0] in ("security", "xcrun") else argv[:1])
            detail = self.clean((result.stderr or result.stdout or "").strip())[-2000:]
            raise ReleaseError(f"{tool} failed (exit {result.returncode}): {detail}")
        return result


def keychain_list(output):
    return [line.strip().strip('"') for line in output.splitlines() if line.strip()]


def developer_id(output, team):
    found = []
    for line in output.splitlines():
        m = re.fullmatch(r'\s*\d+\)\s+([0-9A-F]{40})\s+"(Developer ID Application: .+ \(([A-Z0-9]{10})\))"\s*', line)
        if m:
            found.append(m.groups())
    found = list(dict.fromkeys(found))
    require(len(found) == 1, f"the certificate must hold exactly one valid Developer ID Application identity "
                             f"(found {len(found)})")
    sha1, name, found_team = found[0]
    require(team is None or found_team == team, f"the Developer ID identity is not from team {TEAM_ENV}")
    return {"sha1": sha1, "name": name, "team": found_team}


@contextlib.contextmanager
def signing_keychain(run, creds, work):
    """A throwaway keychain holding only the Developer ID identity; removed afterwards
    and the user's keychain search list restored, even on failure."""
    keychain = work / "metro-signing.keychain-db"
    password = secrets.token_hex(32)
    run.redact(password)
    original = keychain_list(run(["security", "list-keychains", "-d", "user"]).stdout)
    created = listed = False
    p12 = work / "developer-id.p12"
    try:
        run(["security", "create-keychain", "-p", password, keychain])
        created = True
        run(["security", "set-keychain-settings", "-lut", "7200", keychain])
        run(["security", "unlock-keychain", "-p", password, keychain])
        private_write(p12, creds.p12)
        try:
            run(["security", "import", p12, "-k", keychain, "-f", "pkcs12", "-P", creds.p12_password,
                 "-T", "/usr/bin/codesign"])
        finally:
            p12.unlink(missing_ok=True)
        run(["security", "set-key-partition-list", "-S", "apple-tool:,apple:,codesign:", "-s", "-k", password, keychain])
        run(["security", "list-keychains", "-d", "user", "-s", keychain, *original])
        listed = True
        found = run(["security", "find-identity", "-v", "-p", "codesigning", keychain]).stdout
        yield keychain, developer_id(found, creds.team)
    finally:
        if listed:
            run(["security", "list-keychains", "-d", "user", "-s", *original], check=False)
        if created:
            run(["security", "delete-keychain", keychain], check=False)


# --- archive in / out -------------------------------------------------------

def read_manifest(path):
    return json.loads(Path(path).read_text())


def check_sidecar(archive):
    sidecar = archive.with_name(archive.name + ".sha256")
    require(sidecar.is_file(), f"{archive.name}: missing {sidecar.name}")
    m = re.fullmatch(r"([0-9a-f]{64})  (\S+)\n?", sidecar.read_text())
    require(m and m.group(2) == archive.name, f"{sidecar.name}: expected '<sha256>  {archive.name}'")
    require(sha256(archive) == m.group(1), f"{archive.name}: checksum mismatch with {sidecar.name}")


def archive_parts(archive):
    m = re.fullmatch(r"(Metro-(v?\d+\.\d+\.\d+(?:[-+][A-Za-z0-9][A-Za-z0-9.-]{0,30})?)-(macos-arm64|macos-x64))\.tar\.gz",
                     archive.name)
    require(m, f"{archive.name}: expected Metro-<version>-macos-<arm64|x64>.tar.gz")
    return m.groups()  # top, version, target


def extract_python(archive, dest, top):
    """Our own archives: regular files only, all under top/."""
    with tarfile.open(archive, "r:gz") as tar:
        for member in tar:
            require(member.isfile() and member.name.startswith(top + "/")
                    and ".." not in member.name.split("/"), f"{archive.name}: unexpected entry {member.name}")
            out = dest / member.name
            out.parent.mkdir(parents=True, exist_ok=True)
            with tar.extractfile(member) as src, open(out, "wb") as dst:
                shutil.copyfileobj(src, dst)
            out.chmod(stat.S_IMODE(member.mode))
    return dest / top


def check_tree(root, manifest):
    have = {p.relative_to(root).as_posix() for p in root.rglob("*") if not p.is_dir()}
    require(not any(p.is_symlink() for p in root.rglob("*")), "the package contains links")
    files = manifest.get("files", {})
    require(have == set(files) | {"RELEASE.json"}, "package files differ from RELEASE.json")
    for name, digest in files.items():
        require(sha256(root / name) == digest, f"{name} does not match RELEASE.json")


def code_files(app):
    """Every Mach-O or executable file in the bundle (each must be one we sign)."""
    found = set()
    for path in app.rglob("*"):
        if path.is_file():
            with open(path, "rb") as s:
                magic = s.read(4)
            if magic in MACHO_MAGIC or path.stat().st_mode & 0o111:
                found.add(path.relative_to(app).as_posix())
    return found


# --- signing ----------------------------------------------------------------

def entitlements(run, path):
    """The entitlements the code is signed with now (preserved as they are, never added to)."""
    out = run(["codesign", "-d", "--entitlements", "-", "--xml", path]).stdout.strip()
    if not out:
        return {}
    start = out.find("<?xml")
    try:
        ents = plistlib.loads(out[start if start >= 0 else 0:].encode())
    except plistlib.InvalidFileException:
        raise ReleaseError(f"{path.name}: cannot read its entitlements") from None
    require(isinstance(ents, dict), f"{path.name}: unexpected entitlements")
    bad = [k for k in FORBIDDEN_ENTITLEMENTS if ents.get(k)]
    require(not bad, f"{path.name} is signed with {', '.join(bad)}, which notarization rejects; "
                     "a reviewed entitlement change is needed")
    return ents


def details(run, path):
    out = run(["codesign", "-d", "--verbose=4", path])
    text = out.stderr + out.stdout
    fields = {}
    authorities = []
    for line in text.splitlines():
        key, _, value = line.partition("=")
        if key == "Authority":
            authorities.append(value)
        elif key in ("Identifier", "TeamIdentifier", "Timestamp", "Signed Time"):
            fields[key] = value
        elif key == "CodeDirectory v":
            m = re.search(r"flags=0x[0-9a-f]+\(([^)]*)\)", line)
            fields["flags"] = m.group(1).split(",") if m else []
    fields["authorities"] = authorities
    return fields


def check_signed(run, path, team, identity_name=None):
    d = details(run, path)
    name = path.name
    require(d["authorities"][:1] == [identity_name] if identity_name else
            bool(d["authorities"]) and d["authorities"][0].startswith("Developer ID Application: "),
            f"{name} is not signed with the Developer ID identity")
    require("Developer ID Certification Authority" in d["authorities"] and "Apple Root CA" in d["authorities"],
            f"{name}: the signature does not chain to Apple's Developer ID CA")
    require(d.get("TeamIdentifier") == team, f"{name}: unexpected team identifier")
    require(d.get("Timestamp") and "Signed Time" not in d, f"{name}: no secure timestamp")
    require("runtime" in d.get("flags", []), f"{name}: hardened runtime is not enabled")
    return d


def unsigned_digest(run, path, work):
    copy = Path(tempfile.mkstemp(dir=work, prefix="unsigned-")[1])
    shutil.copyfile(path, copy)
    result = run(["codesign", "--remove-signature", copy], check=False)
    require(result.returncode == 0 or "not signed" in result.stderr, f"{path.name}: cannot remove its signature")
    digest = sha256(copy)
    copy.unlink()
    return digest


def executables(manifest):
    """The executables Metro.app must hold: RELEASE.json decides whether metro-tui ships."""
    tui = manifest.get("tui")
    require(tui in (None, "Metro.app/" + TUI), "RELEASE.json names an unexpected terminal client")
    return [INNER_EXE, SERVER, *([TUI] if tui else []), LAUNCHER]


def sign_app(run, app, keychain, identity, work, manifest):
    """Developer ID signatures inside out: MetroUI.app, metro-server, metro-tui, then Metro.app."""
    exes = executables(manifest)
    require(code_files(app) == set(exes),
            "Metro.app must hold exactly the launcher, metro-server, MetroUI.app"
            + (" and metro-tui" if TUI in exes else "") + " executables")
    order = [app / INNER_APP, app / SERVER, *([app / TUI] if TUI in exes else []), app]
    plans = []
    for i, target in enumerate(order):
        ident = details(run, target).get("Identifier")
        require(ident, f"{target.name}: no signing identifier")
        ents = entitlements(run, target)
        plans.append((target, ident, ents))
    before = {rel: unsigned_digest(run, app / rel, work) for rel in exes}
    recorded = {}
    for i, (target, ident, ents) in enumerate(plans):
        argv = ["codesign", "--force", "--sign", identity["sha1"], "--keychain", keychain,
                "--options", "runtime", "--timestamp", "--identifier", ident]
        if ents:
            ent_file = work / f"entitlements-{i}.plist"
            ent_file.write_bytes(plistlib.dumps(ents))
            argv += ["--entitlements", ent_file]
        run([*argv, target])
        run(["codesign", "--verify", "--strict", "--verbose=2", target])
        recorded[("Metro.app/" + target.relative_to(app).as_posix()) if target != app else "Metro.app"] = sorted(ents)
    run(["codesign", "--verify", "--deep", "--strict", "--verbose=2", app])
    for rel in exes:
        check_signed(run, app / rel, identity["team"], identity["name"])
        require(unsigned_digest(run, app / rel, work) == before[rel],
                f"{rel}: signing changed more than the code signature")
    for target in (app / INNER_APP, app):
        check_signed(run, target, identity["team"], identity["name"])
    return recorded


def snapshot(root):
    return {p.relative_to(root).as_posix(): sha256(p) for p in root.rglob("*") if p.is_file()}


def notarize(run, creds, app, work):
    """Submit, wait, require Accepted, staple, validate. Returns submission id and ticket files."""
    key = work / f"AuthKey_{creds.key_id}.p8"
    private_write(key, creds.p8.encode())
    auth = ["--key", key, "--key-id", creds.key_id, "--issuer", creds.issuer]
    try:
        zipped = work / "notary" / "Metro.zip"
        zipped.parent.mkdir()
        run(["ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", app, zipped])
        out = run(["xcrun", "notarytool", "submit", zipped, *auth, "--wait", "--timeout", "2h",
                   "--output-format", "json"], check=False)
        try:
            result = json.loads(out.stdout)
        except json.JSONDecodeError:
            raise ReleaseError(f"xcrun notarytool submit failed (exit {out.returncode}): "
                               + run.clean((out.stderr or out.stdout).strip())[-2000:]) from None
        submission = str(result.get("id", ""))
        status = result.get("status")
        if status != "Accepted":
            if re.fullmatch(r"[0-9a-fA-F-]{36}", submission):
                log = run(["xcrun", "notarytool", "log", submission, *auth], check=False)
                print(run.clean(log.stdout)[-20000:], file=sys.stderr)
            raise ReleaseError(f"notarization {submission or '(no id)'} finished with status {status!r}, not Accepted")
        require(re.fullmatch(r"[0-9a-fA-F-]{36}", submission), "notarytool returned no submission id")
    finally:
        key.unlink(missing_ok=True)
    before = snapshot(app)
    run(["xcrun", "stapler", "staple", app])
    run(["xcrun", "stapler", "validate", app])
    after = snapshot(app)
    ticket = sorted(k for k in after if before.get(k) != after[k])
    require(ticket and set(before) <= set(after), "stapling did not add a ticket file to the bundle")
    require(not any(p.is_symlink() for p in app.rglob("*")), "the stapled bundle contains links")
    return submission, ticket


def official_start(target):
    return ("Metro for macOS.\n"
            "Signed with a Developer ID certificate, notarized by Apple and stapled.\n\n"
            f"Requires macOS {package.MACOS_MIN} or newer on "
            f"{'Apple silicon' if target == 'macos-arm64' else 'an Intel Mac'}.\n"
            "Install with install-metro-macos.sh, which places Metro.app in\n"
            "~/Applications/Metro Desktop/ and keeps your data in\n"
            "~/Library/Application Support/Metro/Desktop.\n")


def sign(run, archive, out_dir, env):
    creds = Credentials(env)
    run.redact(*creds.redact)
    archive = archive.resolve()
    out_dir = out_dir.resolve()
    top, version, target = archive_parts(archive)
    require(out_dir != archive.parent, "--out-dir must differ from the ad hoc archive's directory")
    final = out_dir / archive.name
    require(not final.exists() and not final.with_name(final.name + ".sha256").exists(),
            f"{final} already exists")
    check_sidecar(archive)
    out_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".metro-sign-", dir=os.environ.get("RUNNER_TEMP")) as work:
        work = Path(work)
        work.chmod(0o700)
        stage = extract_python(archive, work / "in", top)
        manifest = read_manifest(stage / "RELEASE.json")
        require((manifest.get("kind"), manifest.get("target"), manifest.get("version"), manifest.get("signing"))
                == ("desktop", target, version, "adhoc"),
                f"{archive.name}: RELEASE.json must describe the ad hoc {version} {target} desktop package")
        check_tree(stage, manifest)
        app = stage / "Metro.app"
        secrets_dir = work / "secrets"
        secrets_dir.mkdir(mode=0o700)
        with signing_keychain(run, creds, secrets_dir) as (keychain, identity):
            ents = sign_app(run, app, keychain, identity, work, manifest)
        submission, ticket = notarize(run, creds, app, secrets_dir)

        (stage / "START.txt").write_text(official_start(target))
        manifest.pop("status", None)
        manifest.pop("files", None)
        renderer = dict(manifest.get("renderer", {}))
        renderer["packaged_signed_sha256"] = sha256(app / INNER_EXE)
        renderer["code_resources_sha256"] = sha256(app / INNER_APP / "Contents/_CodeSignature/CodeResources")
        manifest.update(
            signing="developer-id", status="notarized", signing_team=identity["team"], hardened_runtime=True,
            entitlements=ents, renderer=renderer,
            notarization={"service": "notarytool", "submission_id": submission, "status": "Accepted",
                          "stapled": True, "ticket_files": ["Metro.app/" + t for t in ticket]},
        )
        (stage / "RELEASE.json").unlink()
        manifest["files"] = {p.relative_to(stage).as_posix(): sha256(p) for p in package.files(stage)}
        (stage / "RELEASE.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
        built = work / "out" / archive.name
        built.parent.mkdir()
        package.archive(stage, built, top, False)
        (built.parent / (archive.name + ".sha256")).write_text(f"{sha256(built)}  {archive.name}\n")
        verify_archive(run, built, identity["team"])
        os.replace(built.parent / (archive.name + ".sha256"), final.with_name(final.name + ".sha256"))
        os.replace(built, final)
    return final


# --- verification of what ships ----------------------------------------------

def verify_app(run, app, team, manifest):
    """The app as a user gets it: valid, Developer ID, notarized ticket stapled, Gatekeeper accepts."""
    exes = executables(manifest)
    require(code_files(app) == set(exes), "Metro.app holds unexpected or missing executables")
    run(["codesign", "--verify", "--deep", "--strict", "--verbose=2", app])
    for rel in (INNER_APP, *exes, ""):
        check_signed(run, app / rel if rel else app, team)
    run(["xcrun", "stapler", "validate", app])
    out = run(["spctl", "--assess", "--type", "execute", "--verbose=4", app])
    text = out.stderr + out.stdout
    require(": accepted" in text and "source=Notarized Developer ID" in text,
            "Gatekeeper does not accept Metro.app as a notarized Developer ID app")


def verify_archive(run, archive, team=None):
    """Extract with the system tar into a fresh folder and verify that copy."""
    top, version, target = archive_parts(archive)
    check_sidecar(archive)
    with tempfile.TemporaryDirectory(prefix=".metro-verify-", dir=os.environ.get("RUNNER_TEMP")) as dest:
        dest = Path(dest)
        run(["tar", "-xzf", archive, "-C", dest])
        require(sorted(p.name for p in dest.iterdir()) == [top], f"{archive.name}: unexpected top-level entries")
        root = dest / top
        manifest = read_manifest(root / "RELEASE.json")
        check_tree(root, manifest)
        require((manifest.get("kind"), manifest.get("target"), manifest.get("version"), manifest.get("signing"),
                 manifest.get("status")) == ("desktop", target, version, "developer-id", "notarized")
                and manifest.get("notarization", {}).get("status") == "Accepted"
                and manifest.get("notarization", {}).get("stapled") is True,
                f"{archive.name}: RELEASE.json does not describe a notarized Developer ID package")
        require(team is None or manifest.get("signing_team") == team, f"{archive.name}: unexpected signing team")
        for ticket in manifest["notarization"].get("ticket_files", []):
            require((root / ticket).is_file(), f"{archive.name}: the stapled ticket {ticket} did not survive")
        verify_app(run, root / "Metro.app", manifest["signing_team"], manifest)
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("check-credentials", help="report missing credential variables by name")
    s = sub.add_parser("sign", help="sign, notarize, staple and re-archive an ad hoc archive")
    s.add_argument("--archive", required=True, type=Path)
    s.add_argument("--out-dir", required=True, type=Path)
    v = sub.add_parser("verify-archive", help="verify a final archive from a fresh extraction")
    v.add_argument("archive", type=Path)
    a = sub.add_parser("verify-app", help="verify an installed Metro.app against its RELEASE.json")
    a.add_argument("--app", required=True, type=Path)
    a.add_argument("--manifest", required=True, type=Path)
    args = parser.parse_args()
    run = Runner()
    try:
        if args.command == "check-credentials":
            missing = missing_credentials(os.environ)
            if missing:
                parser.exit(1, "missing signing credentials (repository secrets): " + ", ".join(missing) + "\n")
            print("all signing credentials are set: " + ", ".join(SECRET_ENV))
            return
        require(sys.platform == "darwin", "signing and verification run on macOS")
        if args.command == "sign":
            print(sign(run, args.archive, args.out_dir, os.environ))
        elif args.command == "verify-archive":
            m = verify_archive(run, args.archive.resolve())
            print(f"{args.archive.name}: notarized Developer ID package (team {m['signing_team']}), ticket stapled")
        else:
            m = read_manifest(args.manifest)
            require(m.get("signing") == "developer-id" and re.fullmatch(r"[A-Z0-9]{10}", m.get("signing_team", "")),
                    "the manifest does not describe a Developer ID package")
            verify_app(run, args.app, m["signing_team"], m)
            print(f"{args.app}: accepted, notarized, ticket stapled")
    except (OSError, ValueError, tarfile.TarError, ReleaseError) as error:
        parser.exit(1, f"macos_release: {run.clean(str(error))}\n")


if __name__ == "__main__":
    main()
