# Update checks and installation

> **Released in Metro 0.5.0.**
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
| Portable copy or older, unmigrated Linux install | Shows a notice and an official download link. Metro doesn't overwrite a portable folder or silently migrate old install locks. |
| Windows MSI, script install or ZIP | Manual only. Metro doesn't start an MSI or install helper, or offer Windows rollback. Verify the matching prebuilt files before updating manually. The published 0.6.2 Windows files are unsigned; no native Windows desktop run by a person is recorded. |

An older Linux install needs a one-time, explicit lock migration to use
managed updates. Use the matching verified prebuilt archive and Linux installer
from 0.5.0 or later,
with Metro, its bundled server and other installers stopped. The installer
operation is `update --archive FILE --migrate-update-lock`. Retire old
installer copies; an old script may not honor the new lock. Do not migrate
an installation whose recovery status is uncertain.

## Uncertain update journal

If Metro can't verify the update journal, **stop Metro, its bundled server
and any installer**. Keep the installation, update records, workspace
gate, live database and backup. Don't remove locks or records, change the
`current` pointer, overwrite a database, run an old launcher on that
workspace, or run install, update or uninstall to clear the condition.

Run `sh install-metro-linux.sh status` with the **verified official
installer** from 0.6.2 for a read-only check. Use the same install and data locations
as the affected app. A reported phase isn't proof that recovery is safe.
Contact the
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
