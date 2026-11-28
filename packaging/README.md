# Packaging and installers

`package.py` assembles release archives from already built inputs (see the
`release.yml` workflow). The scripts below install those prebuilt archives
for one person, without compiling anything. Each one is a standalone file
that's also published as a release asset next to the archives, and each is
listed in `SHA256SUMS` (except `install-metro-macos.sh` while there are no
Mac builds). The latest published release is
[Metro 0.6.4](https://github.com/wnzn/metro/releases/tag/v0.6.4); its
scripts are the 0.6.4 copies, so a change here after `ddbfe42` isn't in
it until the next release
([acceptance](../docs/acceptance.md#published-metro-064-2026-11-28-github-release-v064)).

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
| `install-metro-macos.sh` | macOS desktop, Apple silicon (**not yet verified**; in the source, not released: see [macOS status](#macos-status)) | `~/Applications/Metro Desktop/Metro.app`; records in `~/Library/Application Support/Metro/Installer` | `~/Library/Application Support/Metro/Desktop` |
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
launcher that supports the marker sets up the app's built-in personal
registered runner after the person signs in to the app's own local
workspace, with no Docker. Since 0.6.3 it also writes files and runs
shell commands under the runner's shell policy
([install.md](../docs/install.md#folders-and-shell-commands)).
That runner isn't
attached to any Space until the Space owner picks it under **Manage
Space**. Since Metro 0.6.2, the marker only saves a press: the
same runner can be set up for any signed-in server from **Settings →
Runners**, with or without it. The runner is part of the launcher, not a
separate binary in the package.

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
  - For a loopback `http://` server use exactly `localhost`, `127.0.0.1` or
    `[::1]` with an explicit port from 1 to 65535 (a stable one), and then
    the container uses host networking.
    On Docker Desktop that needs 4.34 or newer with **Settings > Resources
    > Network > Enable host networking**.
  - For non-loopback HTTP the scripts don't use host networking and warn
    outside recognized Tailscale hosts. Prefer HTTPS. The scripts set
    `METRO_ALLOW_INSECURE_HTTP` for older runner versions.
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

Launchers built at or before 28ba4e2 don't read `enable-local-runner`.
Archives whose launcher does read it say so in `RELEASE.json`
(`"local_runner": "installer-marker"`). For an archive without that field,
the installers still verify and install it and keep your choice, but they
report that this build predates local runner support and that no runner
will be set up. They never say the runner is on.

## macOS status

**In the source, not released.** `release.yml`'s `desktop-macos` job builds
Apple silicon only (`macos-arm64`), on `metro-ci-macos-arm64` for trusted
runs and GitHub's `macos-15` otherwise. It packages with `package.py`, ad hoc
signs, installs with `install-metro-macos.sh` and runs
`smoke-macos-launch.sh` where there's a GUI (Aqua) session, or
`smoke-macos-headless.sh` where there isn't (a self-hosted runner started by
a LaunchDaemon; a GitHub-hosted Mac without Aqua fails). Its `dist-desktop-macos-arm64` artifact goes into
every preview and release: `release_assets.MACOS` and
`release_signatures.MACOS_TARGETS` list it, the archive gets its own
Sigstore bundle, and `install-metro-macos.sh` ships beside it.

There's no Developer ID signing or notarization, and no Intel Mac.
`macos_release.py` (Developer ID, notarize, staple) and its tests stay but
no workflow runs it; `release_assets.py` still accepts its notarized
output. In-app updates refuse macOS (`unsupported_platform`), so Mac users
update with the installer.

How that code works:

- NSH v0.6.1 publishes official `nsh-macos-arm64.tar.gz` and
  `nsh-macos-x86_64.tar.gz` (macOS 14+). `nsh package --target macos` ad
  hoc signs, so it needs a Mac.
- `desktop-macos` builds and tests natively, assembles `Metro.app` with
  the stock NSH app at `Contents/Helpers/MetroUI.app`, then installs it and
  checks a bounded start and shutdown. Its output is ad hoc signed;
  `RELEASE.json` says `"signing": "adhoc"`, `"status": "preview-not-verified"`.
- An x64 build under Rosetta isn't supported.
- We never ask anyone to change Gatekeeper.
- `macos_release.py sign` checks the ad hoc archive, imports the Developer
  ID identity into a throwaway keychain, signs from the inside out with the
  hardened runtime and a secure timestamp (never `--deep`), keeps the
  entitlements (refusing `get-task-allow`), notarizes with
  `notarytool --wait`, staples and validates the ticket, and re-archives
  with `"signing": "developer-id"`. It then checks a fresh extraction with
  `codesign`, `stapler validate` and `spctl`.

## Release signing (protected `main` only)

The pipeline below is implemented and covered by local contract tests. It
published Metro 0.6.4 from `ddbfe42` in CI run 46090688619 with Windows
unsigned, as in previous releases. The signatures are the Sigstore bundles for
`SHA256SUMS` and the two Linux archives (in the source, not released, also
the macOS archive), and the GHCR image signatures.
Windows Authenticode signing has never run.
Who may release, which version, and when: `release_policy.py` and
[docs/deploy.md](../docs/deploy.md#releases-from-main-on-a-version-bump).
Only a push to `main` that changes the version releases.

No workflow reads a repository secret. Windows signing, images, the tag and
Sigstore use GitHub OIDC and the workflow's own token.

How a release ships Windows is the checked-in
`windows-release-signing.txt`: `signed` or `unsigned`, nothing else. It says
**`unsigned`** for now (no certificate yet); see
[docs/deploy.md](../docs/deploy.md#windows-releases-signed-or-unsigned).

Release order, on an eligible protected `main` push (a version bump):

1. `release.yml` builds and tests every platform. Its Windows seed goes to
   `windows-distribution.yml`, which checks it in official mode. Unsigned:
   it also builds the unsigned ZIP and MSI and tests them natively, as for a
   test build, and uploads them as `dist-desktop-windows-x64`. Signed: it
   hands the seed to `windows-sign.yml`.
2. `release-publish.yml` rechecks the version bump and eligibility
   (`release_policy.py authorize`), then, only when signed, runs Windows
   signing and verification (`windows-sign.yml`). Unsigned skips it.
3. `release_policy.py tag` creates the immutable `v<version>` tag on exactly
   the pushed commit, or accepts it there on a rerun. It's never moved.
4. `release_assets.py` assembles the release. It requires the Linux,
   Windows and (in the source, not released) ad hoc macOS arm64 desktop
   archives, the MSI, the runner, the sidecars, the scripts and `metro-tui`
   in every desktop archive, and refuses any other macOS archive. The Windows files must match the switch: signed ones carry
   Authenticode signatures; unsigned ones are labelled `unsigned-preview`
   with their commit, the MSI is `….unsigned-preview.msi` without a
   signature, and the scripts are this checkout's.
5. GHCR images are pushed, read back and signed (`release-images.yml`).
6. `SHA256SUMS` and the `.tar.gz` archives (Linux, and macOS in the source) get Sigstore signatures
   (`sign-release-assets.yml`), bound to `refs/heads/main` and the commit.
7. `release_policy.py publish` uploads exactly that set to a staging draft,
   compares every digest GitHub reports, then makes the release public. It
   never overwrites a published release. When Windows is unsigned, the
   release notes say so, warn about SmartScreen and "Unknown publisher",
   and explain how to check the files with `SHA256SUMS` and Sigstore.
