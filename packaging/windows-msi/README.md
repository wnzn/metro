# Metro per-user MSI (`Metro-<version>-windows-x64.msi`)

A Windows Installer package for the desktop app, built from the same verified
payload tree as the portable ZIP. The ZIP stays as it is. The MSI only copies
files: it downloads, compiles and runs nothing on the user's machine, and its
only custom actions set properties (MSI type 51). It installs per-user, so no
administrator rights are needed when the destination folder is writable by the
user.

Status: the builder, its static checks and the Linux tests run on Linux. The
native `msiexec` checks in `test-msi-windows.ps1` are written for a Windows CI
runner and **have not run yet**. The wizard has not been seen on screen. Nothing
here has been installed on a real Windows machine.

Under Wine 10.0, the same lifecycle was checked from a script with no UI: fixture
products plus an unsigned preview built from a frozen payload. Wine has its own
implementation of Windows Installer, so those results are Wine-only evidence.
They don't show how native Windows behaves.

## Build interface (for the Windows distribution and signing lanes)

```sh
# 1. Pinned toolchain (msitools 0.106 from Debian 13; each .deb and the unpacked tree are hash-checked).
python3 -I packaging/windows-msi/build_msi.py fetch-tools --dest "$TOOLS"        # new or empty dir

# 2. Optional: verify and unpack a ZIP into a payload tree (or use an already verified tree).
python3 -I packaging/windows-msi/build_msi.py extract \
  --zip Metro-$VERSION-windows-x64.zip --checksum-file Metro-$VERSION-windows-x64.zip.sha256 --dest "$PAYLOAD_PARENT"
#    prints the payload dir: $PAYLOAD_PARENT/Metro-$VERSION-windows-x64

# 3. Build the unsigned MSI.
python3 -I packaging/windows-msi/build_msi.py build --tools "$TOOLS" \
  --payload "$PAYLOAD" --version "$VERSION" \
  --out dist/Metro-$VERSION-windows-x64.msi [--receipt msi-receipt.json] [--require-file metro-tui.exe]
```

- **Input:** the payload directory, which must match its `RELEASE.json` exactly
  (`kind: desktop`, `target: windows-x64`, `version == --version`,
  `local_runner: installer-marker`, every listed file present with its SHA-256,
  nothing unlisted, no links). `Metro.exe`, `metro-server.exe` and
  `ui/Metro.exe` are always required. Use `--require-file metro-tui.exe` once
  payloads ship it. Every listed file is installed byte for byte, including
  root `metro-tui.exe`. The MSI never adds it to `PATH`, never gives it a
  shortcut and never starts it.
- **Output:** one unsigned MSI at `--out`, which must not exist yet and must be
  named `Metro-<version>-windows-x64.msi`. A JSON summary goes to stdout, and to
  `--receipt` if given: sha256, ProductVersion, ProductCode, PackageCode,
  UpgradeCode, payload digest, `has_metro_tui`, and the tool and script hashes.
  The exit status is 0 on success and 1 with a `build_msi:` message on failure.
- **Deterministic:** the same payload, version and `SOURCE_DATE_EPOCH` (or
  `--source-date-epoch`, default 1980-01-01) give identical bytes. The
  PackageCode is derived from the payload digest.
- **Signing order:** sign the EXEs, update `RELEASE.json` the way the signed
  ZIP needs it anyway, build the MSI from that tree, then Authenticode-sign the
  MSI and write its `.sha256`. Rebuilding the same version from re-signed EXEs
  keeps the ProductCode and gets a new PackageCode.
- `inspect --tools T --msi FILE --version V [--payload DIR]` re-runs the
  static checks on any MSI, signed or unsigned. It covers the tables, the wizard
  with its Browse dialog, tab order, upgrade rows, launch conditions, per-user
  flags, HKCU-only registry writes, and (with `--payload`) the extracted bytes
  of every file. `build` runs it automatically.
- `--preview` builds the unsigned MSI: test builds, and official releases
  while `packaging/windows-release-signing.txt` says `unsigned`. It requires
  the name `Metro-<version>-windows-x64.unsigned-preview.msi`, names the
  product "Metro (unsigned)" and shows a note on the first page of the
  wizard: the build isn't code-signed, Windows may warn about an unknown
  publisher, and the file should be checked against its SHA-256 checksum
  (`SHA256SUMS`). The wording is true for test builds and releases alike.

The build host needs x86-64 Linux with glib and libxml2, and Python 3.12+.
Builds have been verified on a Debian 13 host. A `debian:trixie-slim`
container ships the same libraries but hasn't been tried.

## What it installs

| | |
| --- | --- |
| Default folder | `%LOCALAPPDATA%\Programs\Metro Desktop\`. This is deliberately not the script's `Programs\Metro` |
| Folder choice | Wizard page *Destination folder and options*: editable path plus **Browse...** (Windows Installer's built-in folder browser with drive list, folder list, Up and New folder, not Explorer's picker); `INSTALLDIR="D:\Any Path"` on the command line |
| Local runner | Checkbox, on by default. It creates the empty `enable-local-runner` file next to `Metro.exe`. `LOCALRUNNER=1\|0` on the command line |
| Start menu | Checkbox, on by default: `Start Menu\Programs\Metro.lnk`. `STARTMENUSHORTCUT=1\|0` |
| Remembered | Folder and both choices (in `HKCU\Software\Metro\Desktop MSI`). Updates reuse them unless overridden |
| Your data | `%LOCALAPPDATA%\Metro\Desktop` is never touched |
| Uninstall | Removes only the files it installed, its shortcut, its registry values and its uninstall entry. Folders are removed only when empty, so files you added stay |

Versions: ProductVersion is `MAJOR.MINOR.(PATCH*100 + rank)`. The rank is 99
for a release, 10+N for `alpha.N`, 30+N for `beta.N`, 50+N for `rc.N`, and 0
for any other prerelease (dev/local builds such as `0.2.0-setup-<sha>`). Build
metadata is ignored. The rules that follow:

- An older build is upgraded in place: the old version is removed first, then
  the new one is installed in the remembered folder.
- A newer installed build blocks a downgrade.
- A *different* build with the same ProductVersion is refused unless
  `METRO_REPLACE_SAME_VERSION=1`.
- Running the very same package again opens Repair/Remove.
- If `install-metro-windows.ps1` already installed Metro for this user (its
  `dev.metro.desktop` uninstall entry exists), the MSI refuses to install and
  explains how to remove that copy first. It never shares or replaces it.

## Tests

- `python3 -B -m unittest discover -s packaging` runs
  `packaging/test_windows_msi.py`. The build tests run when
  `METRO_MSI_TOOLS=$TOOLS` points at a fetched toolchain.
- `build_msi.py fixtures --tools T --dest DIR` builds four "Metro MSI test
  fixture" packages: a separate product with its own UpgradeCode, folder,
  registry key and shortcut, never released.
- On a Windows runner (CI only; it changes the current profile):
  `pwsh -File packaging/windows-msi/test-msi-windows.ps1 -FixturesDir DIR
  [-Msi Metro-V-windows-x64.msi -Version V -RequireFile metro-tui.exe]`.
  It covers:
  - a custom folder with spaces
  - marker and shortcut on by default and off on request
  - upgrade into the remembered folder, repair and reinstall
  - refused downgrade
  - same-version refusal and explicit replacement
  - the legacy-script block
  - uninstall keeping data and unrelated files
  - the default folder
  - drive `D:` when present
  - a Basic-user (non-admin) token via `runas /trustlevel` when Windows allows
    it; otherwise that step is reported as SKIP

## Known limits

- The Visual C++ Redistributable (x64) is checked through its registry key,
  and the result is only a warning on the options page, never a block.
  `install-metro-windows.ps1` checks the DLLs instead.
- x64 packages are not blocked on Windows 11 on Arm. That combination is
  untested.
- Close Metro before updating or uninstalling. With the full UI, Windows shows
  which programs hold files. `/qn` runs can fail or ask for a restart instead.
- Pick a new or empty folder. Windows Installer can replace same-named files
  that are already in the chosen folder.
