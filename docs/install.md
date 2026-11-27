# Installers: desktop and runners

This guide uses published installers. See [Updates](updates.md) for the
update controls and Linux journal diagnostic.

> **Latest release: [Metro 0.6.1](https://github.com/wnzn/metro/releases/tag/v0.6.1)**
> (2026-11-27, from `5384e8a`). Releases are published automatically when a
> version bump is merged into the protected `main` branch
> ([how](#official-releases)). Its `SHA256SUMS` is signed with Sigstore; its
> Windows files are **not** code-signed. What was checked is in
> [acceptance](acceptance.md#published-metro-061-2026-11-27-github-release-v061).
>
> This page describes the per-user installer scripts at `fb7cb06`, the
> Windows MSI from `ff28b09` onwards, and the desktop's runner screen at
> `57409bb`. The local
> [57409bb setup cut](acceptance.md#staged-installers-and-setup-at-57409bb-2026-11-23-project-manager-local) recorded:
>
> - **Verified locally:** `install-metro-linux.sh` and
>   `install-metro-runner-linux.sh` on Debian 13 and CachyOS (install,
>   upgrade, wrong checksum refused, runner choice kept, removal keeps
>   data). On CachyOS, `--install-deps` repaired a missing WebKitGTK. The
>   desktop's own runner and its Settings screen were checked in the native
>   GTK app on Debian 13 only.
> - **Not verified:** the desktop GUI on CachyOS, `install-metro-windows.ps1`,
>   `install-metro-macos.sh`, the `metro-runner-container` scripts, and native
>   Windows or macOS.
>
> Nothing on this page needs a compiler, NSH or a source checkout.

Each installer is a single script, published beside the release archives
and listed in their `SHA256SUMS` ([file list](#official-releases)).

What every installer does:

- It checks the archive's SHA-256 before extracting anything: `--sha256
  HEX`, `--checksum-file FILE` (a `.sha256` sidecar or `SHA256SUMS`), or the
  file found beside the archive. Then it checks every extracted file against
  the archive's `RELEASE.json`, and refuses links, paths outside the archive
  and files the manifest doesn't list.
- It installs for you only. No administrator rights, except an optional
  package install on Linux that you ask for.
- It never creates, rotates or prints a token, never contacts a Metro
  server, and never attaches anything to a Space.

## Desktop on Linux (`install-metro-linux.sh`)

```sh
sh install-metro-linux.sh install --archive Metro-<version>-linux-x64.tar.gz
sh install-metro-linux.sh status
sh install-metro-linux.sh update --archive Metro-<newer>-linux-x64.tar.gz
sh install-metro-linux.sh uninstall
sh install-metro-linux.sh check-deps [--install-deps]
```

- Installs to `~/.local/share/metro-desktop-app/versions/<version>`, with a
  `current` link and an applications-menu entry (`--no-menu` skips it).
  `METRO_INSTALL_DIR` changes the place. Your data in
  `~/.config/metro-desktop` is never touched, including by `uninstall`.
- **Runtime libraries.** Metro needs glibc 2.39+, GTK 4, WebKitGTK 6.0 and
  the GStreamer base plugins. If any is missing, the installer stops,
  changes nothing, and prints the exact command for your distribution:

  | Distribution (`/etc/os-release`) | Command |
  | --- | --- |
  | Arch, CachyOS, EndeavourOS, Manjaro, Garuda | `sudo pacman -S --needed webkitgtk-6.0 gtk4 gst-plugins-base-libs gst-plugins-base` |
  | Debian, Ubuntu, Mint, Pop!_OS | `sudo apt-get install --no-install-recommends libgtk-4-1 libwebkitgtk-6.0-4 gstreamer1.0-plugins-base` |
  | Fedora | `sudo dnf install webkitgtk6.0 gtk4 gstreamer1-plugins-base` |

- **`--install-deps` is opt-in.** Only with it does the installer run that
  fixed command through `sudo`. Your password prompt and the package
  manager's own confirmation are the authorization. Afterwards it checks the
  libraries again and fails if any are still missing. It installs nothing
  else. On other distributions it explains what's needed and stops.

## Desktop on Windows

There are two Windows downloads with the same app inside: the per-user
**MSI** and the **portable ZIP**.

**Releases ship Windows unsigned for now.** Metro doesn't have a Windows
code-signing certificate yet, so the release's ZIP, MSI and `.ps1` scripts
carry no Authenticode signature. Windows SmartScreen warnings and an
"Unknown publisher" prompt are expected. Check every Windows download
against the Sigstore-verified `SHA256SUMS` before you run it
([check a download](#check-a-download)). The checked-in switch
`packaging/windows-release-signing.txt` says `unsigned`; signed releases
come back once a certificate is set up and it says `signed`
([deploy.md](deploy.md#windows-releases-signed-or-unsigned)).

**Unattended checks on native Windows.** In 0.6.1 and earlier releases, the
run's `windows-distribution / preview` job on a GitHub `windows-2025`
runner installed the unsigned MSI into a chosen folder, checked its
payload, ran the server and uninstalled it, and passed the MSI lane's
native tests (upgrade, repair, downgrade, uninstall), all unattended
([0.6.1 release record](acceptance.md#published-metro-061-2026-11-27-github-release-v061), [0.6.0](acceptance.md#published-metro-060-2026-11-26-github-release-v060), [0.5.1](acceptance.md#published-metro-051-2026-11-26-github-release-v051), [0.5.0](acceptance.md#published-metro-050-2026-11-25-github-release-v050), [0.4.0](acceptance.md#published-metro-040-2026-11-25-github-release-v040), [0.3.2](acceptance.md#published-metro-032-2026-11-24-github-release-v032)).
Earlier local desktop GUI smoke tests ran under Wine on Linux, not on
native Windows. **No person has verified the desktop GUI or the MSI
wizard on native Windows.** See the [MSI notes](../packaging/windows-msi/README.md).

### The MSI

- **Per user.** No administrator rights are needed when the folder you pick
  is writable by you. The setup window uses Windows' own Segoe UI font.
- **You choose the folder.** On the page *Destination folder and options*,
  type a path or use **Browse…** (Windows Installer's own folder browser).
  The default is `%LOCALAPPDATA%\Programs\Metro Desktop\`. Pick a new or
  empty folder. From a command line: `INSTALLDIR="D:\Any Path"`.
- **Local runner support** is a checkbox, on by default (`LOCALRUNNER=1|0`):
  it sets up [the desktop's own runner](#the-desktops-own-runner) without a
  press in the app's own local workspace.
- Updates reuse the remembered folder and choices. Remove it from
  **Settings → Apps → Installed apps**. Removal deletes only what it
  installed, so files you added stay, and your data in
  `%LOCALAPPDATA%\Metro\Desktop` is never touched.
- **Its name says whether it's signed.** An unsigned MSI is named
  `Metro-<version>-windows-x64.unsigned-preview.msi`, in test builds and,
  for now, in releases too. It installs as "Metro (unsigned)", and its
  first page says the build isn't code-signed, that Windows may warn about
  an unknown publisher, and to check the file against its SHA-256 checksum
  (`SHA256SUMS`) before installing. Windows SmartScreen may warn about it. Only a signed release
  has `Metro-<version>-windows-x64.msi`, signed with Authenticode ([check
  it](#check-a-download)). The 0.6.1 release has the unsigned
  `Metro-0.6.1-windows-x64.unsigned-preview.msi` only.

### The portable ZIP and `install-metro-windows.ps1`

The ZIP runs from any folder you extract it to
([desktop README](../apps/desktop/README.md#windows-portable-package)).
`install-metro-windows.ps1` installs it for you instead.

- Actions: `Install`, `Update`, `Uninstall`, `Status`, `Verify`. Options:
  `-Sha256`, `-ChecksumFile`, `-NoLocalRunner`, `-LocalRunner`,
  `-NoShortcut`, `-Quiet`.
- Installs to `%LOCALAPPDATA%\Programs\Metro\versions\<version>`, adds a
  Start menu shortcut and a per-user *Installed apps* entry. Your data in
  `%LOCALAPPDATA%\Metro\Desktop` is never touched.
- **It won't install next to the MSI.** If the MSI already installed Metro
  for you, the script refuses `Install` and `Update` and tells you to update
  or remove Metro with the MSI instead. It never installs a second copy or
  changes the MSI's.
- In a test build, and in a release while Windows ships unsigned, the
  script is the unsigned source copy, so Windows PowerShell's execution
  policy may refuse to run it. A signed release's copy is Authenticode
  signed. Don't weaken or bypass your execution policy for Metro.

## Desktop on macOS

**There's no Mac download for now.** CI doesn't build Metro for macOS at
the moment, for Apple silicon or Intel: GitHub's macOS build machines were
too busy, and the builds kept getting cancelled. Releases ship Windows and
Linux only, and test builds don't include a Mac either. There's no date for
when Mac builds come back.

The macOS installer, `install-metro-macos.sh`, stays in the source so Mac
builds can return. It isn't published with releases for now, and it has
never been verified. A Mac release was designed to be signed
with a Developer ID certificate, notarized by Apple and stapled, but that
has never run.

## The desktop's own runner

The desktop app has a runner built in, so you don't install one to give
your Spaces tools on this computer. It's **in the source, not released**
(after Metro 0.6.1); in Metro 0.6.1 and earlier it works only in the app's
own local workspace and only when the installer asked for it.

- **Set it up in the app.** Open **Settings → Runners → This computer's
  runner** and press **Set up**. Metro creates **one personal registered
  runner** for the server and account you're signed in to, local or remote,
  and keeps its credential in one private file on this computer. Nothing is
  created until you press it.
- **It runs while Metro is open.** Later launches start it again for that
  server and account, without another press. It stops when you sign out,
  switch server or account, or quit Metro. It updates with the app.
- **List and read only.** It lists and reads files in each attached Space's
  own folder on this computer. It never runs commands, checks, MCP or
  Docker, and opens no port.
- **Installer choice.** The desktop installers still turn on **local runner
  support** for a new install, by placing an empty `enable-local-runner`
  file next to the launcher (on macOS, beside `Metro.app` in its private
  install folder; the signed bundle is never changed). With it, Metro sets
  the runner up without a press after you sign in to the app's own local
  workspace (**Use locally** / **On this computer**). `--no-local-runner`
  (Windows `-NoLocalRunner`, MSI `LOCALRUNNER=0`) leaves that off;
  `--local-runner` turns it on. An update keeps your previous choice unless
  you pass one of them. Only launchers whose `RELEASE.json` says
  `"local_runner": "installer-marker"` act on the file.
- **Turn it off** by deleting it: the **Delete…** control under this
  computer's runner removes the registration from the server and turns it
  off here. The Space folders on this computer stay. **Set up** creates a
  new one.

**Settings → Runners → This computer's runner** (`57409bb`) shows its name,
its state and its read-only tools. It's hidden without the launcher (for
example in a browser) or with an older launcher.

- **Choose where to attach it.** You can use the
  [local runner offer](released-0.5.0.md#this-computers-runner) when
  creating a Space or choosing existing Spaces from **Settings → Runners**.
  You can also attach it from **Space settings → Runner**. These choices
  never replace an existing assignment. Each attached Space gets its
  own folder, with **Show folder** and **Copy path** under this computer's
  runner.
- **Recovery is explicit.** If its token is no longer accepted, or setup was
  interrupted, the state says so and nothing is retried on its own.
  **Reconnect…** first explains what it will do, then:
  - if the recorded runner is still one of your personal runners, it gets a
    new token; the old one stops working at once, and its Space attachments
    stay;
  - otherwise, Metro creates a new personal runner, which isn't attached to
    any Space.
  - After an interrupted setup, an unused runner may be left in your list;
    delete it there if you don't need it.

This is separate from a Space's **Local tools** switch, and from a
standalone runner below, which you need only for another machine, for
checks and MCP, or for a runner that keeps going while Metro is closed. See the
[desktop README](../apps/desktop/README.md#three-different-ways-to-give-a-space-tools).

## A standalone runner (operators)

For any Metro server, local or remote. First create the runner in
**Settings → Runners → Add runner**; its credential is shown once. In the
source, not released (0.6.2), the app shows the matching steps on tabs
next to it: **Linux**, **Docker run** and **Docker Compose** (**Cloud** is
coming soon). Save the credential to a file only you can read, without
echoing it:

```sh
( umask 077; IFS= read -rs -p 'Runner credential: ' t; echo
  printf '%s\n' "$t" > "$HOME/.config/metro-runner.token" )
```

### Native Linux (`install-metro-runner-linux.sh`)

Needs Linux 5.6+ and glibc 2.36+.

```sh
sh install-metro-runner-linux.sh install --archive Metro-runner-<version>-linux-x64.tar.gz
sh install-metro-runner-linux.sh configure --server-url https://metro.example.com \
  --workspace "$HOME/metro-workspace" --token-file "$HOME/.config/metro-runner.token"
sh install-metro-runner-linux.sh doctor
sh install-metro-runner-linux.sh run              # foreground
sh install-metro-runner-linux.sh install-service  # writes a systemd --user unit, doesn't start it
systemctl --user daemon-reload && systemctl --user enable --now metro-runner.service
```

- Installs to `~/.local/share/metro-runner`; settings go to
  `~/.config/metro-runner/runner.env` (mode 600). Only the token file's
  **path** is stored; the token is read when the runner starts.
- `--workspace` is an existing directory you own. Each Space gets
  `spaces/<space-id>/` inside it, and it's never deleted.
- `uninstall` keeps the configuration (unless `--purge-config`), the
  workspace and the token file. `status` and `doctor` explain what's there
  or missing.

### In a container (`metro-runner-container.sh` / `.ps1`)

For macOS or Windows with Docker Desktop (Linux containers), or Docker or
Podman on Linux. It runs the official Linux runner image.

```sh
sh metro-runner-container.sh setup --server-url https://metro.example.com \
  --token-file "$HOME/.config/metro-runner.token" --version <version>
sh metro-runner-container.sh start
sh metro-runner-container.sh status
```

```powershell
.\metro-runner-container.ps1 -Action Setup -ServerUrl https://metro.example.com -TokenFile $HOME\metro-runner.token -Version <version>
.\metro-runner-container.ps1 -Action Start
```

- Other actions: `stop`, `restart`, `logs`, `update`, `remove
  [--purge-config]`. Options: `--image REF` (pin a digest with
  `REPO@sha256:…`) instead of `--version`, `--volume` (default
  `metro-runner-workspace`, never deleted by `remove`), `--name`, and
  `--engine docker|podman` (shell script). PowerShell uses the same actions
  with `-Action`, `-Image`, `-Volume`, `-Name` and `-PurgeConfig`.
- **The token file is mounted read-only** and read when the container
  starts. It's never in the container's configuration, environment settings
  or command line, so `docker inspect` doesn't show it. Prefer this over the
  manual `--env-file` setup in [runners.md](runners.md).
- On a Linux host, the token file must be readable by the image's runner
  user (uid 1000); otherwise use the native runner.
- The image named by `--version` is the GHCR runner image
  `ghcr.io/wnzn/metro-runner:v<version>`, published in 0.6.1
  (`linux/amd64`, `linux/arm64`).

### Networking

A runner only connects out to the server; no port is opened or published.

- Use the server's `https://` address. This works from anywhere, including
  containers on the default network.
- For a loopback `http://` server use exactly `localhost`, `127.0.0.1` or
  `[::1]` with an explicit port (1–65535) that stays the same, meaning a
  server on the same computer. The container script then uses host
  networking: native on Linux, and on Docker Desktop only 4.34 or newer with
  **Settings → Resources → Network → Enable host networking**.
- For other hosts, the published scripts don't use host networking. They
  warn on plain HTTP outside recognized Tailscale hosts. Prefer HTTPS.
  The scripts still set `METRO_ALLOW_INSECURE_HTTP` on non-loopback
  HTTP for compatibility with older runner binaries.
- The app's own local workspace (**Use locally**) runs only while Metro is
  open. For it, use [the desktop's own runner](#the-desktops-own-runner)
  instead of a standalone one.

### Then

The runner serves nothing until it's attached: a Space owner picks it in
**Manage → Runner** ([runners.md](runners.md)). Check and MCP tools need the
native runner with `METRO_SANDBOX=docker` ([runner.md](runner.md)).

## Test builds from GitHub Actions

**For testing only.** Every push to `main` that doesn't release, and pull
requests that change a build input, build installable test files. Pull requests that change only docs or tests don't
([deploy.md](deploy.md#ci-and-publishing)). A test build is **not a release**,
**not signed**, and kept only for a short time. Use one only if you accept
that.

- Open the run under the repository's **Actions** tab. Artifacts can be
  downloaded only while signed in to GitHub. They expire after 7 or 14 days.
- `preview-assets`: the Linux and Windows desktop archives, the Windows
  `….unsigned-preview.msi`, the Linux runner archive, their `.sha256` files,
  the installer scripts and a `SHA256SUMS`.
- The checksums only show that a file wasn't damaged in transit. They don't
  show who built it. Nothing in a test build is signed by Metro's
  publishers.
- A test build's `RELEASE.json` says so (`unsigned-preview` on Windows). The
  installers say so too.
- There's no Mac test build for now ([macOS](#desktop-on-macos)).
- A run's tests aren't all guaranteed to pass. Check the run's status before
  you rely on one of its files.

## Official releases

> **This process published [Metro 0.6.1](https://github.com/wnzn/metro/releases/tag/v0.6.1)**
> on 2026-11-27, from `5384e8a`, in CI run
> [45949761195](https://github.com/wnzn/metro/actions/runs/45949761195).
> There's no Windows code-signing setup, so releases ship Windows unsigned
> for now ([deploy.md](deploy.md#windows-releases-signed-or-unsigned)).

Releases are made by CI, not by hand. A release starts only from a
**protected, non-deleted, non-forced push to the `main` branch** of
`wnzn/metro` **that changes the version**, run by its own `ci.yml`, for
exactly the pushed commit. The version is the checked-in one. The Cargo
workspace and the desktop app's `nsh.json` must name the same version.

- **Only a version bump releases.** A maintainer bumps the version in both
  files in a pull request and merges it. That push to `main` starts the
  release. Every other push to `main` runs the tests and test builds but
  releases nothing.
- **A new version is released once.** A bump to a version that's already
  in use, or that finds a conflicting tag, fails instead.
- **The tag is immutable.** CI creates `v<version>` on exactly that commit
  after the tests, the platform checks, and the Windows gate: Windows
  signing and verification, or, while Windows ships unsigned, the native
  install and self-test of the unsigned Windows build in the same run. It's
  never moved.
- **Then:** every file is checked, the GHCR
  images are pushed and signed, and `SHA256SUMS` is signed with Sigstore.
- **Publishing is automatic.** CI uploads the files and checks every digest
  GitHub reports, then makes the release public. Re-running the bump's run
  after a failure picks up where it stopped. A rerun after success changes nothing. An
  existing public release is never overwritten.
- **Never from anything else.** Pushing a tag, a push to `main` without a
  version bump, a push to any other branch, a pull request or a manual run
  never signs, creates a tag, pushes an image or publishes anything.
- A prerelease version (for example `0.3.0-rc.1`) is published as a GitHub
  prerelease and never marked latest. Images have no `latest` tag.

**Files** (for version `X.Y.Z`):

- The desktop archives `Metro-X.Y.Z-linux-x64.tar.gz` and
  `Metro-X.Y.Z-windows-x64.zip`, each with `metro-tui` inside. There's no
  macOS archive for now ([macOS](#desktop-on-macos)).
- The MSI: `Metro-X.Y.Z-windows-x64.unsigned-preview.msi` while Windows
  ships unsigned (now), `Metro-X.Y.Z-windows-x64.msi` once it's signed.
- The runner archive `Metro-runner-X.Y.Z-linux-x64.tar.gz`.
- A `.sha256` beside each archive and the MSI.
- The five installer scripts (all except `install-metro-macos.sh`).
- `SHA256SUMS`, which lists them all, with its Sigstore bundle
  `SHA256SUMS.sigstore.json`. The two Linux archives have their own
  `.sigstore.json` too.
- `IMAGES.txt`.

### Check a download

1. **The checksum list is signed** by the release workflow, for the release's
   commit on `main`. Find that commit with
   `git ls-remote https://github.com/wnzn/metro refs/tags/vX.Y.Z`. Then
   check it with
   [cosign](https://docs.sigstore.dev/cosign/system_config/installation/):

   ```sh
   commit=<the 40-character commit from git ls-remote>
   cosign verify-blob --bundle SHA256SUMS.sigstore.json \
     --certificate-identity 'https://github.com/wnzn/metro/.github/workflows/sign-release-assets.yml@refs/heads/main' \
     --certificate-oidc-issuer https://token.actions.githubusercontent.com \
     --certificate-github-workflow-ref refs/heads/main \
     --certificate-github-workflow-sha "$commit" SHA256SUMS
   sha256sum -c --ignore-missing SHA256SUMS
   ```

   `packaging/release-signatures/release_signatures.py verify-file
   --version X.Y.Z --revision "$commit" SHA256SUMS` runs the same check. It
   also checks the certificate's repository, branch, trigger and workflow
   fields.
2. **Windows.**
   - **Unsigned release (now).** There's no Authenticode signature to check,
     so step 1 is the check: after `SHA256SUMS` verifies with cosign, compare
     each Windows file with it. In PowerShell,
     `(Get-FileHash .\Metro-X.Y.Z-windows-x64.zip -Algorithm SHA256).Hash`
     prints the hash in capitals; ignoring case, it must equal the hash on
     that file's line in `SHA256SUMS`. The ZIP's `RELEASE.json` says `"mode": "unsigned-preview"` and
     names the release commit under `signing.commit`. Expect SmartScreen
     and an "Unknown publisher" prompt; `Get-AuthenticodeSignature` reports
     `NotSigned`.
   - **Signed release (later).** Right-click the MSI or `Metro.exe` →
     **Properties → Digital Signatures**. The signer must be the publisher
     named in the release notes, with a timestamp. Or, in PowerShell,
     `Get-AuthenticodeSignature .\Metro-X.Y.Z-windows-x64.msi` should say
     `Valid` with that publisher. The publisher isn't known yet; it's the
     validated name on the certificate the maintainers obtain. The release
     copies of the two PowerShell scripts are signed too, so a policy that
     requires signed scripts (`RemoteSigned` or `AllSigned`) can run them.
     SmartScreen may still warn about a new publisher until it builds
     reputation, which signing alone doesn't guarantee.
3. **Images.** `IMAGES.txt` lists each image by digest:
   `ghcr.io/wnzn/metro-server` and `ghcr.io/wnzn/metro-runner` for
   `linux/amd64` and `linux/arm64`, and the optional media image
   `ghcr.io/wnzn/metro-embeddinggemma2` for `linux/amd64` only. There are no
   macOS or Windows containers. Pull by digest, and verify with:

   ```sh
   cosign verify \
     --certificate-identity 'https://github.com/wnzn/metro/.github/workflows/release-images.yml@refs/heads/main' \
     --certificate-oidc-issuer https://token.actions.githubusercontent.com \
     --certificate-github-workflow-ref refs/heads/main \
     --certificate-github-workflow-sha "$commit" \
     'ghcr.io/wnzn/metro-server:vX.Y.Z@sha256:…'
   ```

Operators setting up signing: [deploy.md](deploy.md#ci-and-publishing).

## Troubleshooting and help

See the [desktop README](../apps/desktop/README.md#troubleshooting). Never
send anyone your tokens, keys, sign-in files, `.env` or token files, or raw
logs.
