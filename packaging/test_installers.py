"""Isolated tests for the installer scripts.

Every test works in a throwaway HOME under a temporary directory (set TMPDIR
to choose where). Package managers, sudo, systemctl and docker are replaced
by recording fakes on PATH, so nothing on the host is installed, started or
changed. Real engines are exercised only by the CI jobs.
"""

import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile

import package

HERE = Path(__file__).resolve().parent
DESKTOP = HERE / "install-metro-linux.sh"
RUNNER = HERE / "install-metro-runner-linux.sh"
CONTAINER = HERE / "metro-runner-container.sh"
MACOS = HERE / "install-metro-macos.sh"
WINDOWS = HERE / "install-metro-windows.ps1"
CONTAINER_PS1 = HERE / "metro-runner-container.ps1"
TRUE = shutil.which("true")
# Validation only: none of these is ever connected to.
BAD_LOOPBACK = (
    "http://127.0.0.1.evil.example:80", "http://127.0.0.2:8787", "http://127.1:8787", "http://localhost",
    "http://localhost:", "http://localhost:0", "http://localhost:65536", "http://localhost:99999",
    "http://localhost:80x", "http://localhost:+80", "http://localhost:8787:1", "http://localhostx:80",
    "http://LOCALHOST:80", "http://[::1]", "http://[::1]80", "http://[::1:80", "http://[::1]]:80",
    "http://[0:0:0:0:0:0:0:1]:80", "http://u@localhost:80", "http://localhost@evil.example:80",
    "http://0.0.0.0:80", "http://127.0.0.1 :80",
)
GOOD_LOOPBACK = ("http://127.0.0.1:41234", "http://localhost:1", "http://[::1]:65535",
                 "http://localhost:41234/base")
HOST_MUTATORS = {"docker", "podman", "sudo", "doas", "pkexec", "su", "systemctl", "apt", "apt-get", "dpkg",
                 "pacman", "dnf", "yum", "rpm", "zypper", "flatpak", "snap"}


def sha(data):
    return hashlib.sha256(data).hexdigest()


def write_exec(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    path.chmod(0o755)


def build_archive(out_dir, top, files, kind, target, version, extra_members=(), tamper=None, fields=None):
    """A package.py-shaped archive: <top>/... plus RELEASE.json, and its .sha256."""
    manifest = {"kind": kind, "target": target, "version": version,
                "files": {name: sha(data) for name, (data, _) in files.items()}, **(fields or {})}
    if tamper:
        tamper(manifest)
    members = dict(files)
    members["RELEASE.json"] = ((json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode(), 0o644)
    is_zip = top.endswith("windows-x64")
    name = top + (".zip" if is_zip else ".tar.gz")
    path = out_dir / name
    if is_zip:
        with zipfile.ZipFile(path, "w") as z:
            for rel, (data, mode) in sorted(members.items()):
                info = zipfile.ZipInfo(f"{top}/{rel}")
                info.external_attr = (stat.S_IFREG | mode) << 16
                z.writestr(info, data)
    else:
        with tarfile.open(path, "w:gz", format=tarfile.USTAR_FORMAT) as t:
            for rel, (data, mode) in sorted(members.items()):
                info = tarfile.TarInfo(f"{top}/{rel}")
                info.size, info.mode = len(data), mode
                t.addfile(info, io.BytesIO(data))
            for info, data in extra_members:
                t.addfile(info, io.BytesIO(data) if data is not None else None)
    (out_dir / (name + ".sha256")).write_text(f"{sha(path.read_bytes())}  {name}\n")
    return path


def desktop_files(with_menu=True):
    binary = Path(TRUE).read_bytes()
    files = {
        "Metro": (binary, 0o755), "metro-server": (binary, 0o755), "ui/Metro": (binary, 0o755),
        "ui/nsh/app.json": (b"{}", 0o644), "START.txt": (b"start\n", 0o644),
    }
    if with_menu:
        files["install-desktop"] = ((HERE / "install-desktop.sh").read_bytes(), 0o755)
        for dest, source in package.linux_icon_assets().items():
            files[dest] = (source.read_bytes(), 0o644)
    return files


class Sandbox:
    """Throwaway HOME, fake tools on PATH and a call log."""

    def __init__(self, root):
        self.root = root
        self.home = root / "home with space"
        self.home.mkdir()
        self.bin = root / "fakebin"
        self.bin.mkdir()
        self.log = root / "calls.log"
        self.log.touch()
        self.missing = root / "missing-libs"  # fake ldd reports these as missing
        self.os_release = root / "os-release"
        self.os_release.write_text('ID=debian\nVERSION_ID="13"\n')
        write_exec(self.bin / "ldd", f"""#!/bin/sh
[ -s "{self.missing}" ] && while IFS= read -r l; do printf '\\t%s => not found\\n' "$l"; done < "{self.missing}"
exit 0
""")
        for tool in ("sudo", "apt-get", "pacman", "dnf", "systemctl"):
            write_exec(self.bin / tool, f"""#!/bin/sh
printf '%s\\n' "{tool} $*" >> "{self.log}"
case "{tool}:$1" in
    sudo:*) exec "$@" ;;
    apt-get:install|pacman:-S|dnf:install) : > "{self.missing}" ;;
esac
""")
        # Host tools minus anything that could change the system: those are
        # only ever the recording fakes above (or absent).
        self.tools = root / "tools"
        self.tools.mkdir()
        for directory in ("/usr/bin", "/bin"):
            for tool in Path(directory).iterdir():
                if tool.name not in HOST_MUTATORS and not (self.tools / tool.name).exists():
                    (self.tools / tool.name).symlink_to(tool)
        self.env = {
            "PATH": f"{self.bin}:{self.tools}", "HOME": str(self.home), "LANG": "C.UTF-8",
            "METRO_OS_RELEASE_FILE": str(self.os_release),
        }

    def run(self, script, *args, check=False, env=None, stdin=None):
        result = subprocess.run(["/bin/sh", str(script), *map(str, args)], env={**self.env, **(env or {})},
                                cwd=self.root, text=True, capture_output=True, timeout=60, stdin=stdin)
        if check and result.returncode != 0:
            raise AssertionError(f"{script.name} {args} failed:\n{result.stdout}\n{result.stderr}")
        return result

    def calls(self):
        return self.log.read_text().splitlines()


class DesktopInstallerTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.box = Sandbox(self.root)
        self.dist = self.root / "downloads with space"
        self.dist.mkdir()
        self.app_root = self.box.home / ".local/share/metro-desktop-app"
        self.data = self.box.home / ".config/metro-desktop"
        self.data.mkdir(parents=True)
        (self.data / "desktop.json").write_text("keep")

    def tearDown(self):
        self.tmp.cleanup()

    def archive(self, version="0.3.0", **kw):
        top = f"Metro-{version}-linux-x64"
        files = kw.pop("files", None) or desktop_files()
        kw.setdefault("fields", {"local_runner": package.LOCAL_RUNNER})
        return build_archive(self.dist, top, files, "desktop", "linux-x64", version, **kw)

    def test_install_update_uninstall_keep_data_and_local_runner_choice(self):
        a = self.archive()
        out = self.box.run(DESKTOP, "install", "--archive", a, check=True).stdout
        self.assertIn("Checksum OK", out)
        current = self.app_root / "current"
        self.assertEqual(os.readlink(current), "versions/0.3.0")
        marker = current / "enable-local-runner"
        self.assertTrue(marker.is_file() and not marker.is_symlink() and marker.stat().st_size == 0)
        entry = self.box.home / ".local/share/applications/dev.metro.desktop.desktop"
        self.assertIn(str(self.app_root / "versions/0.3.0/Metro"), entry.read_text())

        b = self.archive("0.3.1")
        self.box.run(DESKTOP, "update", "--archive", b, "--no-local-runner", check=True)
        self.assertEqual(os.readlink(current), "versions/0.3.1")
        self.assertEqual(sorted(p.name for p in (self.app_root / "versions").iterdir()), ["0.3.1"])
        self.assertFalse(marker.exists())
        self.box.run(DESKTOP, "update", "--archive", b, check=True)  # keeps "off"
        self.assertFalse(marker.exists())
        self.box.run(DESKTOP, "update", "--archive", b, "--local-runner", check=True)
        self.assertTrue(marker.is_file())
        self.box.run(DESKTOP, "update", "--archive", b, check=True)  # keeps "on"
        self.assertTrue(marker.is_file())
        self.assertIn("Local runner support: on", self.box.run(DESKTOP, "status", check=True).stdout)

        out = self.box.run(DESKTOP, "uninstall", check=True).stdout
        self.assertIn(str(self.data), out)
        self.assertFalse(self.app_root.exists())
        self.assertFalse(entry.exists())
        self.assertEqual((self.data / "desktop.json").read_text(), "keep")
        self.assertEqual(self.box.calls(), [])  # no package manager, sudo or systemctl

    def test_update_requires_install_and_old_archives_without_menu_script(self):
        # Like the early 0.2.0 candidates: no menu script, no local_runner capability.
        a = self.archive(files=desktop_files(with_menu=False), fields={})
        r = self.box.run(DESKTOP, "update", "--archive", a)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("use install", r.stderr)
        out = self.box.run(DESKTOP, "install", "--archive", a, check=True).stdout
        self.assertIn("no applications-menu integration", out)
        self.assertIn("predates it, so no runner will be set up", out)
        self.assertNotIn("Local runner support is on", out)
        self.assertTrue((self.app_root / "current/enable-local-runner").is_file())  # the choice is kept
        self.assertIn("requested (this build predates it", self.box.run(DESKTOP, "status", check=True).stdout)
        out = self.box.run(DESKTOP, "update", "--archive", self.archive("0.3.1"), check=True).stdout
        self.assertIn("Local runner support is on", out)

    def assert_refused(self, archive, message, *extra):
        r = self.box.run(DESKTOP, "install", "--archive", archive, *extra)
        self.assertNotEqual(r.returncode, 0, r.stdout)
        self.assertIn(message, r.stderr)
        self.assertFalse((self.app_root / "current").exists())
        self.assertFalse(self.app_root.exists() and any(self.app_root.iterdir()))

    def test_refuses_bad_checksums_and_tampered_archives(self):
        a = self.archive()
        self.assert_refused(a, "checksum mismatch", "--sha256", "0" * 64)
        Path(f"{a}.sha256").write_text(f"{'1' * 64}  {a.name}\n")
        self.assert_refused(a, "checksum mismatch")
        Path(f"{a}.sha256").unlink()
        self.assert_refused(a, "no checksum")
        sums = self.dist / "SHA256SUMS"
        sums.write_text(f"{sha(a.read_bytes())}  {a.name}\n")
        self.box.run(DESKTOP, "install", "--archive", a, check=True)  # SHA256SUMS fallback works
        self.box.run(DESKTOP, "uninstall", check=True)

        def wrong_hash(m):
            m["files"]["Metro"] = "0" * 64
        self.assert_refused(self.archive("0.4.0", tamper=wrong_hash), "do not match its RELEASE.json")

        def unlisted(m):
            del m["files"]["START.txt"]
        self.assert_refused(self.archive("0.4.1", tamper=unlisted), "does not list")

        link = tarfile.TarInfo("Metro-0.4.2-linux-x64/evil")
        link.type, link.linkname = tarfile.SYMTYPE, "/etc/passwd"
        self.assert_refused(self.archive("0.4.2", extra_members=[(link, None)]), "other than regular files")

        escape = tarfile.TarInfo("Metro-0.4.3-linux-x64/../../escape")
        escape.size = 1
        self.assert_refused(self.archive("0.4.3", extra_members=[(escape, b"x")]), "must all be inside")
        self.assertFalse((self.root / "escape").exists())

        def runner_kind(m):
            m["kind"] = "runner"
        self.assert_refused(self.archive("0.4.4", tamper=runner_kind), "does not describe")

    def test_refuses_foreign_install_directory(self):
        self.app_root.mkdir(parents=True)
        (self.app_root / "someone-elses-file").write_text("x")
        self.assert_refused_keep = self.box.run(DESKTOP, "install", "--archive", self.archive())
        self.assertIn("not created by this installer", self.assert_refused_keep.stderr)
        r = self.box.run(DESKTOP, "uninstall")
        self.assertNotEqual(r.returncode, 0)
        self.assertTrue((self.app_root / "someone-elses-file").exists())

    def test_missing_runtime_explains_then_install_deps_runs_fixed_list(self):
        a = self.archive()
        self.box.missing.write_text("libwebkitgtk-6.0.so.4\nlibgtk-4.so.1\n")
        r = self.box.run(DESKTOP, "install", "--archive", a)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("libwebkitgtk-6.0.so.4", r.stderr)
        self.assertIn("sudo apt-get install --no-install-recommends libgtk-4-1 libwebkitgtk-6.0-4 "
                      "gstreamer1.0-plugins-base", r.stderr)
        self.assertIn("--install-deps", r.stderr)
        self.assertEqual(self.box.calls(), [])
        self.assertFalse(self.app_root.exists())

        self.box.os_release.write_text('ID=cachyos\nID_LIKE=arch\n')
        r = self.box.run(DESKTOP, "install", "--archive", a)
        self.assertIn("sudo pacman -S --needed webkitgtk-6.0 gtk4 gst-plugins-base-libs gst-plugins-base", r.stderr)
        self.assertEqual(self.box.calls(), [])

        self.box.run(DESKTOP, "install", "--archive", a, "--install-deps", check=True)
        self.assertEqual(self.box.calls(), [
            "sudo pacman -S --needed webkitgtk-6.0 gtk4 gst-plugins-base-libs gst-plugins-base",
            "pacman -S --needed webkitgtk-6.0 gtk4 gst-plugins-base-libs gst-plugins-base",
        ])
        self.assertTrue((self.app_root / "current/Metro").is_file())

    def test_install_deps_still_missing_and_unknown_distro(self):
        a = self.archive()
        self.box.os_release.write_text("ID=fedora\n")
        write_exec(self.box.bin / "dnf", f'#!/bin/sh\nprintf "%s\\n" "dnf $*" >> "{self.box.log}"\n')
        self.box.missing.write_text("libwebkitgtk-6.0.so.4\n")
        r = self.box.run(DESKTOP, "install", "--archive", a, "--install-deps")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("still missing", r.stderr)
        self.assertIn("dnf install webkitgtk6.0 gtk4 gstreamer1-plugins-base", self.box.calls())
        self.assertFalse(self.app_root.exists())
        self.box.os_release.write_text("ID=gentoo\n")
        r = self.box.run(DESKTOP, "install", "--archive", a, "--install-deps")
        self.assertIn("with your distribution's package manager", r.stderr)


def runner_files(fake_runner):
    return {
        "metro-runner": (fake_runner.encode(), 0o755),
        "run-runner": ((HERE / "run-runner.sh").read_bytes(), 0o755),
        "START.txt": (b"start\n", 0o644),
    }


# Prints what the real runner would get, never the token itself.
FAKE_RUNNER = """#!/bin/sh
printf 'url=%s ws=%s token_sha=%s insecure=%s file=%s\\n' "$METRO_SERVER_URL" "$METRO_WORKSPACE" \
  "$(printf %s "$METRO_RUNNER_TOKEN" | sha256sum | cut -c1-16)" "${METRO_ALLOW_INSECURE_HTTP:-unset}" \
  "${METRO_RUNNER_TOKEN_FILE:-unset}"
"""


class RunnerInstallerTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.box = Sandbox(self.root)
        self.archive = build_archive(self.root, "Metro-runner-0.3.0-linux-x64", runner_files(FAKE_RUNNER),
                                     "runner", "linux-x64", "0.3.0")
        self.ws = self.box.home / "work space"
        self.ws.mkdir()
        (self.ws / "spaces/s1").mkdir(parents=True)
        self.token = self.box.home / "runner token"
        self.token.write_text("metro_rt_v2_secret-value")
        self.token.chmod(0o600)
        self.config = self.box.home / ".config/metro-runner/runner.env"
        self.install_root = self.box.home / ".local/share/metro-runner"

    def tearDown(self):
        self.tmp.cleanup()

    def configure(self, url="https://metro.example", **kw):
        return self.box.run(RUNNER, "configure", "--server-url", url, "--workspace", kw.get("ws", self.ws),
                            "--token-file", kw.get("token", self.token))

    def test_full_lifecycle_never_exposes_the_token(self):
        self.box.run(RUNNER, "install", "--archive", self.archive, check=True)
        self.assertEqual(os.readlink(self.install_root / "current"), "versions/0.3.0")
        self.assertEqual(self.configure("https://metro.example/").returncode, 0)
        text = self.config.read_text()
        self.assertEqual(stat.S_IMODE(self.config.stat().st_mode), 0o600)
        self.assertNotIn("secret-value", text)
        self.assertIn('METRO_SERVER_URL="https://metro.example"', text)
        self.assertIn("Everything looks ready", self.box.run(RUNNER, "doctor", check=True).stdout)

        r = self.box.run(RUNNER, "run", check=True, env={"METRO_ALLOW_INSECURE_HTTP": "1", "METRO_RUNNER_TOKEN": "x"})
        expected = sha(b"metro_rt_v2_secret-value")[:16]
        self.assertIn(f"url=https://metro.example ws={self.ws} token_sha={expected} insecure=unset file=unset", r.stdout)
        self.assertNotIn("secret-value", r.stdout + r.stderr)

        out = self.box.run(RUNNER, "install-service", check=True).stdout
        self.assertIn("not started", out)
        unit = self.box.home / ".config/systemd/user/metro-runner.service"
        self.assertIn(f'ExecStart="{self.install_root}/install-metro-runner-linux.sh" run', unit.read_text())
        self.assertEqual(self.box.calls(), [])  # writing the unit starts nothing

        update = build_archive(self.root, "Metro-runner-0.3.1-linux-x64", runner_files(FAKE_RUNNER),
                               "runner", "linux-x64", "0.3.1")
        out = self.box.run(RUNNER, "update", "--archive", update, check=True).stdout
        self.assertIn("Existing settings kept", out)
        self.assertEqual(os.readlink(self.install_root / "current"), "versions/0.3.1")

        self.box.run(RUNNER, "uninstall", check=True)
        self.assertEqual(self.box.calls(), ["systemctl --user disable --now metro-runner.service",
                                            "systemctl --user daemon-reload"])
        self.assertFalse(self.install_root.exists() or unit.exists())
        self.assertTrue(self.config.exists() and self.token.exists() and (self.ws / "spaces/s1").is_dir())
        self.box.run(RUNNER, "uninstall", "--purge-config", check=True)
        self.assertFalse(self.config.exists())
        self.assertTrue(self.token.exists() and (self.ws / "spaces/s1").is_dir())

    def test_reinstall_configured_runner_without_service(self):
        self.box.run(RUNNER, "install", "--archive", self.archive, check=True)
        self.assertEqual(self.configure().returncode, 0)
        config = self.config.read_bytes()
        token_digest = sha(self.token.read_bytes())
        marker = self.ws / "spaces/s1/keep.txt"
        marker.write_text("keep")

        result = self.box.run(RUNNER, "update", "--archive", self.archive)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Existing settings kept", result.stdout)
        self.assertNotIn("Restart it", result.stdout)
        current = self.install_root / "current"
        self.assertEqual(os.readlink(current), "versions/0.3.0")
        self.assertEqual(current.resolve(strict=True), self.install_root.resolve() / "versions/0.3.0")
        self.assertTrue(os.access(current / "metro-runner", os.X_OK))
        self.assertEqual(self.config.read_bytes(), config)

        self.box.run(RUNNER, "uninstall", check=True)
        self.assertFalse(self.install_root.exists() or self.install_root.is_symlink())
        self.assertEqual(self.config.read_bytes(), config)
        self.assertEqual(sha(self.token.read_bytes()), token_digest)
        self.assertEqual(stat.S_IMODE(self.token.stat().st_mode), 0o600)
        self.assertEqual(marker.read_text(), "keep")
        self.assertEqual(self.box.calls(), [])

    def test_configuration_is_validated(self):
        self.box.run(RUNNER, "install", "--archive", self.archive, check=True)
        for url in ("http://metro.example", "http://10.0.0.5:8787", "ftp://x", "https://u:p@metro.example",
                    "https://metro.example/?q", "https://", "https://:443", *BAD_LOOPBACK):
            r = self.configure(url)
            self.assertNotEqual(r.returncode, 0, url)
        for url in GOOD_LOOPBACK:
            self.assertEqual(self.configure(url).returncode, 0, url)
        self.token.chmod(0o644)
        self.assertIn("mode 600", self.configure().stderr)
        self.token.chmod(0o600)
        link = self.box.home / "token-link"
        link.symlink_to(self.token)
        self.assertIn("not a link", self.configure(token=link).stderr)
        self.assertIn("existing directory", self.configure(ws=self.box.home / "nope").stderr)
        self.assertEqual(self.configure().returncode, 0)
        with self.config.open("a") as f:
            f.write("METRO_RUNNER_TOKEN=inline\n")
        r = self.box.run(RUNNER, "run")
        self.assertIn("unsupported setting METRO_RUNNER_TOKEN", r.stderr)

    def test_self_update_choice(self):
        self.box.run(RUNNER, "install", "--archive", self.archive, check=True)
        # A new configuration: on, with a line saying so.
        r = self.configure()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn('METRO_RUNNER_SELF_UPDATE="1"', self.config.read_text())
        self.assertIn("Self-update is on", r.stdout)
        self.assertIn("self-update: on", self.box.run(RUNNER, "status", check=True).stdout)
        # Explicit off, then reconfiguring keeps it off.
        args = ["configure", "--server-url", "https://metro.example", "--workspace", self.ws, "--token-file", self.token]
        self.assertEqual(self.box.run(RUNNER, *args, "--self-update", "off").returncode, 0)
        self.assertIn('METRO_RUNNER_SELF_UPDATE="0"', self.config.read_text())
        self.assertEqual(self.configure().returncode, 0)
        self.assertIn('METRO_RUNNER_SELF_UPDATE="0"', self.config.read_text())
        self.assertIn("self-update: off", self.box.run(RUNNER, "status", check=True).stdout)
        # A configuration from before this option stays off.
        self.config.write_text(self.config.read_text().replace('METRO_RUNNER_SELF_UPDATE="0"\n', ""))
        self.assertEqual(self.configure().returncode, 0)
        self.assertIn('METRO_RUNNER_SELF_UPDATE="0"', self.config.read_text())
        # Explicit on, and the run passes it to the runner.
        self.assertEqual(self.box.run(RUNNER, *args, "--self-update", "on").returncode, 0)
        self.assertIn('METRO_RUNNER_SELF_UPDATE="1"', self.config.read_text())
        self.assertEqual(self.box.run(RUNNER, "run").returncode, 0)
        self.assertIn("--self-update must be on or off", self.box.run(RUNNER, *args, "--self-update", "yes").stderr)

    def test_refuses_desktop_archive_and_bad_checksum(self):
        desk = build_archive(self.root, "Metro-runner-0.3.2-linux-x64", runner_files(FAKE_RUNNER),
                             "desktop", "linux-x64", "0.3.2")
        self.assertIn("does not describe", self.box.run(RUNNER, "install", "--archive", desk).stderr)
        r = self.box.run(RUNNER, "install", "--archive", self.archive, "--sha256", "a" * 64)
        self.assertIn("checksum mismatch", r.stderr)
        self.assertIn("not configured", self.box.run(RUNNER, "doctor").stdout)


FAKE_DOCKER = r"""#!/bin/sh
printf '%s\n' "docker $*" >> "{log}"
state="{state}"
case "$1 $2" in
    "info "*) exit 0 ;;
    "image inspect") [ -f "$state/image" ] ;;
    "pull "*) : > "$state/image" ;;
    "volume inspect") [ -f "$state/volume" ] ;;
    "volume create") : > "$state/volume"; echo "$3" ;;
    "container inspect")
        [ -f "$state/container" ] || exit 1
        case "$*" in *managed-by*) cat "$state/container" ;; *Running*) echo false ;; *) echo ok ;; esac ;;
    "rm -f") rm -f "$state/container" ;;
    "stop "*|"start "*) exit 0 ;;
    "run -d") echo "{label}" > "$state/container"; echo id ;;
    "run --rm") exit 0 ;;
    *) exit 0 ;;
esac
"""


class ContainerScriptTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.box = Sandbox(self.root)
        self.state = self.root / "engine"
        self.state.mkdir()
        self.fake_docker("metro-runner-container")
        self.token = self.box.home / "runner token"
        self.token.write_text("metro_rt_v2_secret-value")
        self.token.chmod(0o600)
        self.config = self.box.home / ".config/metro-runner/container.env"

    def tearDown(self):
        self.tmp.cleanup()

    def fake_docker(self, label):
        write_exec(self.box.bin / "docker", FAKE_DOCKER.format(log=self.box.log, state=self.state, label=label))

    def docker_calls(self):
        return [c for c in self.box.calls() if c.startswith("docker ")]

    def test_setup_start_update_remove(self):
        r = self.box.run(CONTAINER, "setup", "--server-url", "https://metro.example", "--token-file", self.token,
                         "--version", "0.3.0", check=True)
        self.assertNotIn("secret-value", r.stdout + r.stderr + self.config.read_text())
        self.assertEqual(stat.S_IMODE(self.config.stat().st_mode), 0o600)
        calls = self.docker_calls()
        self.assertIn("docker pull ghcr.io/wnzn/metro-runner:v0.3.0", calls)
        self.assertIn("docker volume create metro-runner-workspace", calls)
        self.assertTrue(any(c.startswith("docker run --rm") and "test -r /run/secrets/metro_runner_token" in c
                            for c in calls))
        self.box.run(CONTAINER, "start", check=True)
        run = next(c for c in self.docker_calls() if c.startswith("docker run -d"))
        self.assertNotIn("secret-value", "\n".join(self.box.calls()))
        for part in ("--restart unless-stopped", "--read-only", "--cap-drop ALL",
                     "--security-opt no-new-privileges:true",
                     "type=volume,source=metro-runner-workspace,target=/workspace",
                     f"type=bind,source={self.token},target=/run/secrets/metro_runner_token,readonly",
                     "METRO_SERVER_URL=https://metro.example"):
            self.assertIn(part, run)
        for forbidden in (" -p ", "--publish", "--network=host", "METRO_ALLOW_INSECURE_HTTP", "--privileged"):
            self.assertNotIn(forbidden, run)

        self.box.run(CONTAINER, "update", "--version", "0.3.1", check=True)
        self.assertIn("IMAGE=ghcr.io/wnzn/metro-runner:v0.3.1", self.config.read_text())
        self.assertIn("docker rm -f metro-runner", self.docker_calls())
        self.box.run(CONTAINER, "remove", "--purge-config", check=True)
        self.assertFalse(self.config.exists())
        self.assertFalse(any("volume rm" in c for c in self.docker_calls()))
        self.assertTrue(self.token.exists())

    def test_loopback_uses_host_network_and_http_elsewhere_is_refused(self):
        r = self.box.run(CONTAINER, "setup", "--server-url", "http://192.168.1.10:8787", "--token-file", self.token,
                         "--version", "0.3.0")
        self.assertIn("unencrypted", r.stderr)
        self.assertEqual(self.docker_calls(), [])
        self.box.run(CONTAINER, "setup", "--server-url", "http://127.0.0.1:41234", "--token-file", self.token,
                     "--image", "metro-runner:ci", check=True)
        self.assertTrue(any("--network=host" in c and "/dev/tcp/127.0.0.1/41234" in c for c in self.docker_calls()))
        self.box.run(CONTAINER, "start", check=True)
        run = next(c for c in self.docker_calls() if c.startswith("docker run -d"))
        self.assertIn("--network=host", run)

    def test_loopback_urls_are_exact_literals_with_a_port(self):
        for url in BAD_LOOPBACK:
            r = self.box.run(CONTAINER, "setup", "--server-url", url, "--token-file", self.token, "--image", "m:ci")
            self.assertNotEqual(r.returncode, 0, url)
        self.assertEqual(self.docker_calls(), [])
        for url in GOOD_LOOPBACK:
            self.box.run(CONTAINER, "setup", "--server-url", url, "--token-file", self.token, "--image", "m:ci",
                         check=True)
            self.assertIn("NETWORK=host", self.config.read_text())
        self.assertTrue(any("/dev/tcp/::1/65535" in c for c in self.docker_calls()))

    def test_token_file_checks_and_foreign_container(self):
        self.token.chmod(0o640)
        r = self.box.run(CONTAINER, "setup", "--server-url", "https://m.example", "--token-file", self.token,
                         "--version", "0.3.0")
        self.assertIn("mode 600", r.stderr)
        self.token.chmod(0o600)
        self.box.run(CONTAINER, "setup", "--server-url", "https://m.example", "--token-file", self.token,
                     "--version", "0.3.0", check=True)
        (self.state / "container").write_text("someone-else\n")
        r = self.box.run(CONTAINER, "remove")
        self.assertIn("not created by this script", r.stderr)
        self.assertTrue((self.state / "container").exists())

    def test_missing_engine_fails_clearly(self):
        (self.box.bin / "docker").unlink()
        r = self.box.run(CONTAINER, "setup", "--server-url", "https://m.example", "--token-file", self.token,
                         "--version", "0.3.0")
        self.assertIn("Docker or Podman is required", r.stderr)


class MacosInstallerTests(unittest.TestCase):
    """Installer logic only, with recording stand-ins for the macOS tools.

    Nothing is launched: the native CI job is the only place Metro.app runs.
    """

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.box = Sandbox(self.root)
        fakes = {
            "uname": 'case "$1" in -s) echo Darwin ;; -m) echo arm64 ;; *) echo Darwin ;; esac',
            "sw_vers": "echo 15.1",
            "codesign": f'printf "%s\\n" "codesign $*" >> "{self.box.log}"',
            "plutil": "echo dev.metro.desktop",
            "ps": "exit 0",
            # Gatekeeper stand-ins: record the call; fail when the test asks for it.
            "xcrun": f'printf "%s\\n" "xcrun $*" >> "{self.box.log}"; [ ! -e "{self.root}/no-ticket" ]',
            "spctl": f'printf "%s\\n" "spctl $*" >> "{self.box.log}"; [ ! -e "{self.root}/gatekeeper-rejects" ]',
        }
        for name, body in fakes.items():
            write_exec(self.box.bin / name, f"#!/bin/sh\n{body}\n")
        self.install = self.box.home / "Applications/Metro Desktop"
        self.state = self.box.home / "Library/Application Support/Metro/Installer"
        self.data = self.box.home / "Library/Application Support/Metro/Desktop"
        self.data.mkdir(parents=True)
        (self.data / "desktop.json").write_text("keep")

    def tearDown(self):
        self.tmp.cleanup()

    def archive(self, arch="arm64", version="0.3.0", signing="adhoc"):
        exe = b"\xcf\xfa\xed\xfe macho"
        files = {
            "Metro.app/Contents/Info.plist": (b"<plist/>", 0o644),
            "Metro.app/Contents/MacOS/Metro": (exe + b"launcher", 0o755),
            "Metro.app/Contents/MacOS/metro-server": (exe + b"server", 0o755),
            "Metro.app/Contents/Helpers/MetroUI.app/Contents/MacOS/Metro": (exe + b"ui", 0o755),
            "START.txt": (b"preview", 0o644), "licenses/NSH-LICENSE-APACHE.txt": (b"license", 0o644),
        }

        def adhoc(m):
            if signing is None:
                m.pop("signing", None)
            else:
                m["signing"] = signing
        return build_archive(self.root, f"Metro-{version}-macos-{arch}", files, "desktop", f"macos-{arch}",
                             version, tamper=adhoc)

    def test_install_folder_holds_only_the_app_and_marker(self):
        a = self.archive()
        out = self.box.run(MACOS, "install", "--archive", a, check=True).stdout
        self.assertIn("ad hoc signed preview", out)
        self.assertEqual(sorted(p.name for p in self.install.iterdir()), ["Metro.app", "enable-local-runner"])
        self.assertEqual(stat.S_IMODE(self.install.stat().st_mode), 0o700)
        self.assertEqual((self.install / "enable-local-runner").stat().st_size, 0)
        self.assertEqual(sorted(p.name for p in self.install.parent.iterdir()), ["Metro Desktop"])  # no leftovers
        self.assertTrue((self.state / "RELEASE.json").is_file() and (self.state / "licenses").is_dir())
        self.assertIn(f"codesign --verify --deep --strict {self.install.parent}/.metro-desktop-stage.",
                      "\n".join(self.box.calls()))

        self.box.run(MACOS, "update", "--archive", self.archive(version="0.3.1"), "--no-local-runner", check=True)
        self.assertEqual(sorted(p.name for p in self.install.iterdir()), ["Metro.app"])
        self.box.run(MACOS, "update", "--archive", self.archive(version="0.3.1"), check=True)  # keeps "off"
        self.assertEqual(sorted(p.name for p in self.install.iterdir()), ["Metro.app"])
        self.assertIn("Metro 0.3.1", self.box.run(MACOS, "status", check=True).stdout)

        (self.install / ".DS_Store").write_bytes(b"finder")
        self.box.run(MACOS, "update", "--archive", self.archive(version="0.3.1"), "--local-runner", check=True)
        (self.install / "notes.txt").write_text("mine")
        r = self.box.run(MACOS, "uninstall")
        self.assertIn("did not put there", r.stderr)
        (self.install / "notes.txt").unlink()
        self.box.run(MACOS, "uninstall", check=True)
        self.assertFalse(self.install.exists() or self.state.exists())
        self.assertEqual((self.data / "desktop.json").read_text(), "keep")

    def test_notarized_release_needs_its_ticket_and_gatekeeper(self):
        a = self.archive(signing="developer-id")
        out = self.box.run(MACOS, "install", "--archive", a, check=True).stdout
        self.assertIn("Developer ID signed and notarized", out)
        calls = "\n".join(self.box.calls())
        self.assertIn(f"xcrun stapler validate {self.install.parent}/.metro-desktop-stage.", calls)
        self.assertIn(f"spctl --assess --type execute {self.install.parent}/.metro-desktop-stage.", calls)
        for marker, message in (("no-ticket", "no valid stapled notarization ticket"),
                                ("gatekeeper-rejects", "Gatekeeper does not accept")):
            (self.root / marker).touch()
            r = self.box.run(MACOS, "update", "--archive", self.archive(version="0.3.1", signing="developer-id"))
            self.assertIn(message, r.stderr)
            self.assertIn("Metro 0.3.0", self.box.run(MACOS, "status", check=True).stdout)  # unchanged
            (self.root / marker).unlink()
        # The installer never touches quarantine or Gatekeeper settings.
        self.assertFalse(any(c.startswith(("xattr", "spctl --master", "spctl --add")) for c in self.box.calls()))

    def test_unknown_signing_is_refused(self):
        for signing in (None, "self-signed"):
            r = self.box.run(MACOS, "install", "--archive", self.archive(signing=signing))
            self.assertIn("does not say how Metro.app is signed", r.stderr)
            self.assertFalse((self.install / "Metro.app").exists())

    def test_wrong_architecture_and_tampering_are_refused(self):
        r = self.box.run(MACOS, "install", "--archive", self.archive("x64"))
        self.assertIn("use the macos-arm64 archive", r.stderr)
        a = self.archive()
        r = self.box.run(MACOS, "install", "--archive", a, "--sha256", "f" * 64)
        self.assertIn("checksum mismatch", r.stderr)
        self.assertFalse(self.install.exists())

    def test_refuses_non_macos_hosts(self):
        with tempfile.TemporaryDirectory() as d:
            box = Sandbox(Path(d))
            a = Path(d) / "Metro-0.3.0-macos-arm64.tar.gz"
            a.write_bytes(b"x")
            r = box.run(MACOS, "install", "--archive", a)
            self.assertNotEqual(r.returncode, 0)
            self.assertIn("for macOS", r.stderr)


@unittest.skipUnless(shutil.which("pwsh"), "PowerShell 7 (pwsh) is not installed")
class PowerShellTests(unittest.TestCase):
    def pwsh(self, *args, env=None):
        return subprocess.run(["pwsh", "-NoLogo", "-NoProfile", "-NonInteractive", *map(str, args)],
                              text=True, capture_output=True, timeout=120, env={**os.environ, **(env or {})})

    def test_scripts_parse(self):
        for script in (WINDOWS, CONTAINER_PS1):
            r = self.pwsh("-Command", f"$e=$null; [System.Management.Automation.Language.Parser]::ParseFile("
                                      f"'{script}', [ref]$null, [ref]$e) | Out-Null; if ($e) {{ $e; exit 1 }}")
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_container_url_validation(self):
        missing = "/nonexistent/metro-token"  # a valid URL gets as far as the token file
        for url in BAD_LOOPBACK + ("http://metro.example:80", "https://", "https://u@metro.example"):
            r = self.pwsh("-File", CONTAINER_PS1, "-Action", "Setup", "-ServerUrl", url, "-TokenFile", missing,
                          "-Version", "0.3.0")
            self.assertNotEqual(r.returncode, 0, url)
            self.assertNotIn("Cannot find path", r.stderr, url)
        for url in GOOD_LOOPBACK + ("https://metro.example", "https://metro.example:8443/base"):
            r = self.pwsh("-File", CONTAINER_PS1, "-Action", "Setup", "-ServerUrl", url, "-TokenFile", missing,
                          "-Version", "0.3.0")
            self.assertNotEqual(r.returncode, 0, url)
            self.assertIn("Cannot find path", r.stderr, url)

    def test_windows_installer_verifies_archives(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d) / "dl dir"
            root.mkdir()
            files = {"Metro.exe": (b"MZ1", 0o755), "metro-server.exe": (b"MZ2", 0o755),
                     "ui/Metro.exe": (b"MZ3", 0o755), "START.txt": (b"s", 0o644)}
            good = build_archive(root, "Metro-0.3.0-windows-x64", files, "desktop", "windows-x64", "0.3.0")
            r = self.pwsh("-File", WINDOWS, "-Action", "Verify", "-Archive", good)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertIn("Archive verified", r.stdout)

            def wrong(m):
                m["files"]["Metro.exe"] = "0" * 64
            bad = build_archive(root, "Metro-0.3.1-windows-x64", files, "desktop", "windows-x64", "0.3.1", tamper=wrong)
            r = self.pwsh("-File", WINDOWS, "-Action", "Verify", "-Archive", bad)
            self.assertNotEqual(r.returncode, 0)
            self.assertIn("do not match", r.stderr)
            r = self.pwsh("-File", WINDOWS, "-Action", "Verify", "-Archive", good, "-Sha256", "0" * 64)
            self.assertIn("checksum mismatch", r.stderr)
            r = self.pwsh("-File", WINDOWS, "-Action", "Install", "-Archive", good)
            self.assertIn("for Windows", r.stderr)


class MacosPackagingTests(unittest.TestCase):
    def test_macho_header_and_platform_checks(self):
        with tempfile.TemporaryDirectory() as d:
            fake = Path(d) / "Metro"
            fake.write_bytes(b"\xcf\xfa\xed\xfe" + (0x01000007).to_bytes(4, "little") + bytes(8))
            with self.assertRaisesRegex(ValueError, "thin 64-bit Mach-O"):
                package.macho(fake, 0x0100000C)
    @unittest.skipIf(sys.platform == "darwin", "checks the refusal on other hosts")
    def test_macos_package_needs_a_mac(self):
        with tempfile.TemporaryDirectory() as d:
            r = subprocess.run([sys.executable, str(HERE / "package.py"), "desktop", "--target", "macos-arm64",
                                "--launcher", "l", "--server", "s", "--ui-dir", "Metro.app", "--nsh-root", "n",
                                "--version", "0.3.0", "--notices", "n", "--out-dir", str(Path(d) / "out")],
                               text=True, capture_output=True, cwd=d)
            self.assertNotEqual(r.returncode, 0)
            self.assertIn("assembled on macOS", r.stderr)
            self.assertEqual(list((Path(d) / "out").iterdir()), [])


if __name__ == "__main__":
    unittest.main()
