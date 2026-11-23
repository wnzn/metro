# Installers: desktop and runners

> **Status: a local test cut, not a release.** This page describes the
> per-user installer scripts at `fb7cb06` and the desktop's runner screen at
> `57409bb`. **No release has been published**, and nothing is signed. The
> local [57409bb setup cut](acceptance.md#staged-installers-and-setup-at-57409bb-2026-11-23-project-manager-local) recorded:
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

Each installer is a single script, planned to be published beside the
release archives and listed in their `SHA256SUMS`. The final release asset
names will be confirmed with the release ([below](#official-releases)).

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

## Desktop on Windows (`install-metro-windows.ps1`)

Until a signed Windows installer is released, the supported Windows path is
the portable ZIP ([desktop README](../apps/desktop/README.md#windows-portable-package)).
This script is in the source but unsigned, so Windows PowerShell's
execution policy may refuse to run it. Don't weaken or bypass your
execution policy for Metro.

- Actions: `Install`, `Update`, `Uninstall`, `Status`, `Verify`. Options:
  `-Sha256`, `-ChecksumFile`, `-NoLocalRunner`, `-LocalRunner`,
  `-NoShortcut`, `-Quiet`.
- Installs to `%LOCALAPPDATA%\Programs\Metro\versions\<version>`, adds a
  Start menu shortcut and a per-user *Installed apps* entry. Your data in
  `%LOCALAPPDATA%\Metro\Desktop` is never touched.
- **Not verified on native Windows.** Every Windows check so far ran under
  Wine on Linux ([acceptance.md](acceptance.md)).

## Desktop on macOS (`install-metro-macos.sh`)

**Not verified, no release yet.** Pull-request and main builds are ad hoc
signed previews that are never attached to a release. A tagged release
build is set up to sign `Metro.app` with a Developer ID certificate, have
Apple notarize it and staple the ticket. No such build has run yet, because
the signing credentials aren't configured. For an archive marked as a
release, the installer requires the stapled ticket and Gatekeeper's
acceptance before it installs anything. Metro never asks you to change
Gatekeeper. It installs to `~/Applications/Metro Desktop/Metro.app` and
never touches `~/Library/Application Support/Metro/Desktop`.

## The desktop's own runner

The desktop installers turn on **local runner support** for a new install.
They do it by placing an empty `enable-local-runner` file next to the
launcher (on macOS, beside `Metro.app` in its private install folder; the
signed bundle is never changed).

- `--no-local-runner` (Windows `-NoLocalRunner`) leaves it off;
  `--local-runner` turns it on. An update keeps your previous choice unless
  you pass one of them.
- Only launchers that support it act on it. Their `RELEASE.json` says
  `"local_runner": "installer-marker"`. With an older archive (including
  builds at or before `28ba4e2`), the installer still installs it, keeps your
  choice, and says no runner will be set up.
- After you sign in to the app's own local workspace (**Use locally** /
  **On this computer**), the app creates **one personal registered runner**
  for that account. It lists and reads files only, never runs commands, MCP
  or Docker, and opens no port. It never runs for a remote server.

**Settings → Runners → This computer's runner** (`57409bb`) shows its name,
its state and its read-only tools. It's hidden on a remote server, without
the launcher, with an older launcher, or when the installer didn't ask.

- **Attach it yourself.** Metro never attaches it. In a Space you own, open
  **Manage** and choose it under **Runner**. Each Space gets its own folder
  on this computer, and the section offers **Show folder** and **Copy path**
  for your Spaces that use it.
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
standalone runner below. See the
[desktop README](../apps/desktop/README.md#three-different-ways-to-give-a-space-tools).

## A standalone runner (operators)

For any Metro server, local or remote. First create the runner in
**Settings → Runners → Add runner**; its credential is shown once. Save it
to a file only you can read, without echoing it:

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
- The image named by `--version` is the planned GHCR runner image, which
  isn't published yet.

### Networking

A runner only connects out to the server; no port is opened or published.

- Use the server's `https://` address. This works from anywhere, including
  containers on the default network.
- `http://` is accepted only for exactly `localhost`, `127.0.0.1` or
  `[::1]` with an explicit port (1–65535) that stays the same, meaning a
  server on the same computer. The container script then uses host
  networking: native on Linux, and on Docker Desktop only 4.34 or newer with
  **Settings → Resources → Network → Enable host networking**.
- Every other address needs `https://`. The scripts never set
  `METRO_ALLOW_INSECURE_HTTP`.
- The app's own local workspace (**Use locally**) runs only while Metro is
  open. For it, use [the desktop's own runner](#the-desktops-own-runner)
  instead of a standalone one.

### Then

The runner serves nothing until it's attached: a Space owner picks it in
**Manage → Runner** ([runners.md](runners.md)). Check and MCP tools need the
native runner with `METRO_SANDBOX=docker` ([runner.md](runner.md)).

## Official releases

> **Nothing has been released or signed yet.** The tag pipeline below is
> in the source (`7bd4c19`, `ad93ac9`, `87396c4`, `0d27e9a`) and has only
> passed local contract tests. It has never run with real credentials. The
> Windows MSI tools aren't committed yet, so the MSI isn't final and the
> portable ZIP stays the Windows path for now.

A release is drafted only after every gate passes, and a person publishes
the draft. A prerelease is never marked latest, and images have no
`latest` tag.

**Files** (release targets, for version `X.Y.Z`): the desktop archives
`Metro-X.Y.Z-linux-x64.tar.gz`, `Metro-X.Y.Z-windows-x64.zip`,
`Metro-X.Y.Z-macos-arm64.tar.gz` and `Metro-X.Y.Z-macos-x64.tar.gz`, each
with `metro-tui` inside; `Metro-X.Y.Z-windows-x64.msi`; the runner archive
`Metro-runner-X.Y.Z-linux-x64.tar.gz`; a `.sha256` beside each archive and
the MSI; the six installer scripts; `SHA256SUMS`, which lists them all; and
`IMAGES.txt`.

A file named `…windows-x64.unsigned-preview.msi`, or a ZIP whose
`RELEASE.json` says `unsigned-preview`, comes from a pull-request or `main`
build. It's unsigned and never part of a release.

### Check a download

1. **The checksum list is signed.** `SHA256SUMS` comes with
   `SHA256SUMS.sigstore.json` (the two Linux archives have their own
   `.sigstore.json` too). Verify it with
   [cosign](https://docs.sigstore.dev/cosign/system_config/installation/):

   ```sh
   cosign verify-blob --bundle SHA256SUMS.sigstore.json \
     --certificate-identity 'https://github.com/wnzn/metro/.github/workflows/sign-release-assets.yml@refs/tags/vX.Y.Z' \
     --certificate-oidc-issuer https://token.actions.githubusercontent.com SHA256SUMS
   sha256sum -c --ignore-missing SHA256SUMS
   ```

   The repository's `packaging/release-signatures/release_signatures.py
   verify-file --version X.Y.Z SHA256SUMS` runs the same check and also
   checks the certificate's repository, tag and workflow fields.
2. **Windows.** Right-click the MSI or `Metro.exe` → **Properties → Digital
   Signatures**. The signer must be the publisher named in the release
   notes, with a timestamp. Or, in PowerShell,
   `Get-AuthenticodeSignature .\Metro-X.Y.Z-windows-x64.msi` should say
   `Valid` with that publisher. The publisher isn't known yet: it's the
   validated name on the certificate the maintainers obtain. Release copies
   of the two PowerShell scripts are signed, so a policy that requires
   signed scripts (`RemoteSigned` or `AllSigned`) can run them; don't
   bypass your execution policy for Metro. SmartScreen may still warn about a new
   publisher until it builds reputation, which signing alone doesn't
   guarantee.
3. **macOS.** Release builds are Developer ID signed, notarized and
   stapled. `install-metro-macos.sh` refuses a release archive without the
   stapled ticket and Gatekeeper's acceptance.
4. **Images.** `IMAGES.txt` lists each image by digest:
   `ghcr.io/wnzn/metro-server` and `ghcr.io/wnzn/metro-runner` for
   `linux/amd64` and `linux/arm64`, and the optional media image
   `ghcr.io/wnzn/metro-embeddinggemma2` for `linux/amd64` only. There are no
   macOS or Windows containers. Pull by digest, and verify with:

   ```sh
   cosign verify \
     --certificate-identity 'https://github.com/wnzn/metro/.github/workflows/release-images.yml@refs/tags/vX.Y.Z' \
     --certificate-oidc-issuer https://token.actions.githubusercontent.com 'ghcr.io/wnzn/metro-server:vX.Y.Z@sha256:…'
   ```

Operators setting up signing: [deploy.md](deploy.md#ci-and-publishing).

## Troubleshooting and help

See the [desktop README](../apps/desktop/README.md#troubleshooting). Never
send anyone your tokens, keys, sign-in files, `.env` or token files, or raw
logs.
