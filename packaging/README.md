# Packaging and installers

`package.py` assembles release archives from already built inputs (see the
`release.yml` workflow). The scripts below install those prebuilt archives
for one person, without compiling anything. Each one is a standalone file
that's also published as a release asset next to the archives, and each is
listed in `SHA256SUMS`. Nothing is published yet: until a tagged release
exists, use archives you built or got from CI.

Every installer checks the archive's SHA-256 before it extracts anything.
It takes `--sha256 HEX`, `--checksum-file` (a `.sha256` sidecar or
`SHA256SUMS`), or picks up the file beside the archive automatically. Then
it checks every extracted file against the archive's `RELEASE.json`
manifest and refuses links, paths outside the archive folder, and files
the manifest doesn't list. It never creates, rotates or prints a token, and
it never attaches anything to a Space.

| Script | Platform | Installs to | Never touches |
| --- | --- | --- | --- |
| `install-metro-linux.sh` | Linux x64 desktop | `~/.local/share/metro-desktop-app/versions/<v>` + `current` link, applications menu entry | `~/.config/metro-desktop` (your data) |
| `install-metro-windows.ps1` | Windows x64 desktop | `%LOCALAPPDATA%\Programs\Metro\versions\<v>`, Start menu shortcut, per-user Installed apps entry | `%LOCALAPPDATA%\Metro\Desktop` |
| `install-metro-macos.sh` | macOS desktop (**not yet verified**, CI preview only) | `~/Applications/Metro Desktop/Metro.app`; records in `~/Library/Application Support/Metro/Installer` | `~/Library/Application Support/Metro/Desktop` |
| `install-metro-runner-linux.sh` | Operator-managed runner, Linux x64 native | `~/.local/share/metro-runner`, settings in `~/.config/metro-runner/runner.env` (mode 600, no token) | the workspace and the token file |
| `metro-runner-container.sh` / `.ps1` | Operator-managed runner on macOS or Windows (or Linux) with Docker or Podman | container `metro-runner` from `ghcr.io/wnzn/metro-runner:v<v>`, named volume at `/workspace` | the volume and the token file |

## Desktop: local runner support

The desktop installers turn on local runner support by default. They do it
by writing an empty regular file named `enable-local-runner`:

- **Linux, Windows:** next to the top-level `Metro` / `Metro.exe`.
- **macOS:** beside `Metro.app`, in the private (mode 700) install folder.
  That folder holds only `Metro.app`, the marker and Finder's `.DS_Store`.
  The signed bundle itself is never modified.

To leave it off, pass `--no-local-runner` (or `-NoLocalRunner` on Windows).
An update keeps the previous choice unless you pass `--local-runner` or
`--no-local-runner`. The installer itself never contacts a server. A
launcher that supports the marker sets up the person's own registered
runner after they sign in on this device, with list and read access only
and no Docker. That runner isn't attached to any Space until the Space
owner picks it under **Manage Space**.

## Desktop: Linux runtime libraries

The Linux desktop needs glibc 2.39+, GTK 4, WebKitGTK 6.0 and the GStreamer
base plugins. When any of them is missing, the installer stops, changes
nothing and prints the exact command for your distribution:

| Family (`/etc/os-release` `ID`/`ID_LIKE`) | Command |
| --- | --- |
| Arch, CachyOS, EndeavourOS, Manjaro, Garuda | `sudo pacman -S --needed webkitgtk-6.0 gtk4 gst-plugins-base-libs gst-plugins-base` |
| Debian, Ubuntu, Mint, Pop!_OS | `sudo apt-get install --no-install-recommends libgtk-4-1 libwebkitgtk-6.0-4 gstreamer1.0-plugins-base` |
| Fedora | `sudo dnf install webkitgtk6.0 gtk4 gstreamer1-plugins-base` |

With `--install-deps` it runs that fixed command through `sudo`. Your
password prompt and the package manager's own confirmation are the
authorization. It then checks the libraries again and fails if any are
still missing. On other distributions it explains what to install and
stops.

## Runners

- **Native Linux:**
  `install-metro-runner-linux.sh install --archive Metro-runner-<v>-linux-x64.tar.gz`,
  then `configure --server-url https://… --workspace DIR --token-file FILE`,
  then `run`. Or use `install-service` to write a `systemd --user` unit,
  which it doesn't start. It needs Linux 5.6+ (openat2) and glibc 2.36+.
  `doctor` explains anything that's missing.
- **macOS or Windows:** `metro-runner-container` runs the official Linux
  runner image.
  - The token file is bind-mounted read-only and read when the container
    starts. It's never in the container's configuration, environment
    settings or command line. No port is published.
  - A `https://` server works with the default network.
  - `http://` is accepted only for loopback (`127.0.0.1`, `localhost`,
    `::1`) on a stable port, and then the container uses host networking.
    On Docker Desktop that needs 4.34 or newer with **Settings > Resources
    > Network > Enable host networking**.
  - `METRO_ALLOW_INSECURE_HTTP` is never set.
  - On Linux hosts the token file must be readable by the image's runner
    user (uid 1000). Otherwise use the native runner.
- Neither script replaces the desktop app's own local runner. They're for
  runners an operator manages separately.

## Compatibility with earlier archives

The installers read the manifest layout that `package.py` has always
written, so these local 0.2.0 candidates install as they are (verified
2026-11-23):

- `dist/Metro-0.2.0-linux-x64.tar.gz`, built before the
  applications-menu integration. It installs, and the installer says the
  package has no menu entry.
- `dist/linux-testing/Metro-0.2.0-composer-28ba4e225761-linux-x64.tar.gz`
  (commit 28ba4e2). It installs with the menu entry.
- `dist/Metro-runner-0.2.0-linux-x64.tar.gz`.

Launchers built at or before 28ba4e2 don't read `enable-local-runner`, so
the marker does nothing with those builds. Only launchers that implement
it act on it.

## macOS status

This is **not yet verified**, and there are no production builds.

- NSH v0.6.1 publishes official `nsh-macos-arm64.tar.gz` and
  `nsh-macos-x86_64.tar.gz` (macOS 14+). `nsh package --target macos` ad
  hoc signs, so it needs a Mac.
- The `desktop-macos` CI jobs (`macos-15` for Apple silicon,
  `macos-15-intel` for Intel; GitHub offers Intel runners until August
  2027) build and test natively. They assemble `Metro.app`, with the stock
  NSH app at `Contents/Helpers/MetroUI.app`, then install it and check a
  bounded start and shutdown.
- Their output is ad hoc signed, named `preview-not-verified-adhoc-*`, and
  never attached to a release.
- There's no Developer ID signing or notarization, because there are no
  credentials. Until there are, macOS may refuse to open such a build. We
  never ask anyone to change Gatekeeper.
- An x64 build under Rosetta isn't supported.
