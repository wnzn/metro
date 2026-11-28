# Update checks and installation

> Update checks and managed Linux desktop updates were released in Metro 0.5.0.
> Windows in-app updates and Windows journal diagnostics described below are
> **in the source, not released** (Metro 0.6.5). Windows updates in the
> published 0.6.4 release are manual.
> Do not use repository installer scripts to update an installed release.
> Use the verified prebuilt files and installers from the corresponding
> published release; see [Install](install.md#check-a-download).

**Settings → About & updates** shows the app's version, build and install
type, the connected server, update status, last check, and release notes
when available. **Check for updates** is manual; automatic checks are on
by default for release builds, shortly after startup and then about daily.
You can turn them off here. Source and preview builds don't check for or
offer updates. Offline or unverified status is not confirmation that there
is no update.

Checks contact GitHub and its release asset hosts, and may fetch updated
verification data from `tuf-repo-cdn.sigstore.dev` when needed. They don't
send your account, device ID, monitoring events or usage data. Checking
doesn't download the app archive. An offer requires a verified published
release, not just a newer version string.

| Install type | What Metro does |
| --- | --- |
| Managed Linux desktop | **Update to … → Download and verify** downloads and verifies the signed checksums and archive against the pinned Sigstore workflow identity. **Restart now** starts the trial. Metro can roll back when safe; uncertain recovery blocks startup instead of overwriting live data. The bundled runner updates with the app. |
| Windows ZIP installed by the Windows script (0.6.5 source) | **Update to … → Download and verify → Restart now** stages the verified ZIP beside the current version and tries the new version before switching. Metro can roll back when safe. Start Metro from Start or from `<root>\Metro.exe`, which starts the current version. Older script installs need the one-time update below. |
| Windows MSI (0.6.5 source) | **Update to … → Download and verify → Restart now** closes Metro, runs Windows Installer, and starts Metro again. It keeps the install folder and choices. See [MSI updates](#msi-updates). |
| Portable copy (including an extracted Windows ZIP) | Shows an official download link. Metro doesn't overwrite a portable folder. |
| Older, unmigrated Linux install | Shows an official download link. Metro doesn't silently migrate old install locks. |

## One-time migration for older installs

Stop Metro, its bundled server and other installers before migrating. Do not
migrate if recovery is uncertain.

- **Linux:** Use the matching verified prebuilt archive and installer from
  0.5.0 or later. Run `update --archive FILE --migrate-update-lock`. Retire
  old installer copies that may not honor the update lock.
- **Windows script (0.6.5 source):** An app installed by an older script
  doesn't migrate itself. Once 0.6.5 or later is published, manually update
  with the verified Windows installer script from that release and its
  matching ZIP: `.\install-metro-windows.ps1 -Action Update -Archive .\Metro-<version>-windows-x64.zip`.
  This turns on in-app updates. Retire script copies from 0.4.0 and older;
  they don't check the update journal. Use the stable entry point, not an
  older launcher under `versions\`: launchers from 0.6.4 and earlier
  don't honor the update handoff lock.

## MSI updates

This behaviour is **in the source, not released**. The app checks the MSI
against the Sigstore-signed `SHA256SUMS`. The helper rechecks the exact
file before handing it to Windows Installer. This is not Authenticode
signing. The update is per user, with no administrator prompt. Windows
Installer shows progress and may show its own error message.

Metro backs up the stopped local workspace database before the upgrade.
It removes the backup after the new app first displays its window; it
doesn't restore the database or downgrade the MSI automatically. If the
new app never reaches that point, the backup stays.

If Windows Installer fails and Metro confirms the old version is intact,
**About & updates** shows the failure. Check again to retry. If Metro
asks you to restart Windows, restart Windows, then open Metro again.
If Windows Installer is still working, Metro doesn't open the workspace.
After an interruption Metro checks Windows Installer and the installed
version before reopening the workspace. If the checks disagree, Metro
stops and keeps the journal, staged MSI and backup.

## Uncertain update journal

If Metro can't verify the update journal, **stop Metro, its bundled server
and any installer**. Keep the installation, update records, workspace
gate, live database and backup. Don't remove locks or records, change the
`current` pointer, overwrite a database, run an old launcher on that
workspace, or run install, update or uninstall to clear the condition.

For a read-only check, use the **verified official installer** and the
same install and data locations as the affected app:

- **Linux:** `sh install-metro-linux.sh status` (published in 0.6.4).
- **Windows script install:** `.\install-metro-windows.ps1 -Action Status`
  (in the 0.6.5 source, not released).
- **Windows MSI:** Keep the staged MSI. The script's `Status` action doesn't
  diagnose the MSI journal. Share Metro's recovery message instead.

A reported phase isn't proof that recovery is safe. Contact the
[Metro maintainers](https://github.com/wnzn/metro/issues) with the
diagnostic message (redact private paths), not private databases, backups
or credentials.

## Runner and server updates

- The desktop's built-in runner is part of the app and updates with it; it
  never self-updates separately.
- **Settings → Runners** can show version, presence and update status.
  Eligible standalone Linux runners can self-update when the personal
  runner owner or organization admin explicitly requests an update.
  The request carries a version, not a URL or code. The runner downloads
  and verifies the release independently. The installer defaults to
  allowing these requests for newly configured installations; existing
  settings stay as they were, and an old install needs its matching
  installer's explicit lock migration. There is no unattended install.
- For a container runner, Settings can show the new pinned image and pull
  command. The operator pulls and recreates the container, keeping
  configuration and mounts; restarting the old container won't change its
  image. The app does not control Docker.
- A release server checks daily by default (`METRO_UPDATE_CHECK=off`
  disables it; the bundled local server has it off). It can put a verified
  operator notice in `/admin`, **About & updates** and its logs, and
  exposes it to org admins at `GET /v1/server/update`. It doesn't download
  or install a server update.

The [acceptance record](acceptance.md#source-only-updater-lane-and-reviewed-integration-2026-11-25-local)
lists the local updater checks against specific earlier source cuts. They
do not demonstrate a live trust refresh or a published-to-published update.
