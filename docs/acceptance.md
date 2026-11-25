# Acceptance

What has been demonstrated, and how to reproduce it. Anything not listed here
hasn't been demonstrated.

## Published: Metro 0.4.0 (2026-11-25, GitHub release v0.4.0)

**What was published.** The GitHub release
[Metro 0.4.0](https://github.com/wnzn/metro/releases/tag/v0.4.0), tag
`v0.4.0`, public (not a draft, not a prerelease, marked latest), published
2026-11-25T06:58:45Z by `github-actions[bot]`. The tag points at commit
`1a743e59d4ec6c1da85d09d92c99457561f64e55` (the merge of pull request #15,
the 0.4.0 bump on `main`). It was made by the push-to-`main` CI run
[45560122825](https://github.com/wnzn/metro/actions/runs/45560122825)
(conclusion `success`). `release-policy` decided `release=true`,
`state=new`, `version=0.4.0`, `prerelease=false`,
`windows_signing=unsigned`. The jobs `release / *`,
`release / windows-distribution / seed` and `/ preview`,
`publish / authorize`, `/ tag`, `/ assets`, `/ images / guard`,
`/ images / build` (amd64, arm64), `/ images / media`,
`/ images / publish`, `/ images / sign`, `/ sign / authorize`, `/ sign / sign`
and `/ publish` all passed; `publish / windows-sign` was skipped (Windows
unsigned). The pull request's own CI (run 45558661121) passed before the
merge.

It contains everything on `main` up to `1a743e5`, including the source of
the [staged gate at 4175475](#staged-spaces-with-several-agents-per-connection-model-lists-and-the-desktop-redesign-at-4175475-2026-11-25-project-manager-local)
(several agents per Space, per-connection model lists, the desktop
redesign) and the providers settings redesign (pull request #13). That
gate ran on local builds, not on these release files.

**Assets** (18):

| File | Signature |
| --- | --- |
| `Metro-0.4.0-linux-x64.tar.gz` + `.sha256` | `Metro-0.4.0-linux-x64.tar.gz.sigstore.json` |
| `Metro-0.4.0-windows-x64.zip` + `.sha256` | none (no Authenticode) |
| `Metro-0.4.0-windows-x64.unsigned-preview.msi` + `.sha256` | none (no Authenticode) |
| `Metro-runner-0.4.0-linux-x64.tar.gz` + `.sha256` | `Metro-runner-0.4.0-linux-x64.tar.gz.sigstore.json` |
| `install-metro-linux.sh`, `install-metro-runner-linux.sh`, `install-metro-windows.ps1`, `metro-runner-container.sh`, `metro-runner-container.ps1` | none of their own; listed in `SHA256SUMS` |
| `IMAGES.txt` | listed in `SHA256SUMS` |
| `SHA256SUMS` | `SHA256SUMS.sigstore.json` |

**Images** (public on GHCR, from `IMAGES.txt`; no `latest` tag):

| Reference | Index digest | Platforms |
| --- | --- | --- |
| `ghcr.io/wnzn/metro-server:v0.4.0` | `sha256:2bc9dd1b1ed45a91235299793200ceb517b0e927e920e8963fa303611ec0802c` | `linux/amd64`, `linux/arm64` |
| `ghcr.io/wnzn/metro-runner:v0.4.0` | `sha256:a3f12f00eea1bea5061eaa370da49108b756465f88fe9eea0f50c6681772d0b6` | `linux/amd64`, `linux/arm64` |
| `ghcr.io/wnzn/metro-embeddinggemma2:v0.4.0` | `sha256:a0c322d7135c08442b09065b28b4efb82b1959999b6f7f7c61592113a1494853` | `linux/amd64` (no weights) |

**How it was checked** (2026-11-25, release lane, from a Debian 13 host; no
downloaded file was executed):

```sh
gh release view v0.4.0 --repo wnzn/metro \
  --json tagName,publishedAt,assets,targetCommitish,isDraft,isPrerelease   # 18 assets, isDraft false, isPrerelease false
gh release list --repo wnzn/metro                                          # "Metro 0.4.0  Latest  v0.4.0", then "Metro 0.3.2  v0.3.2"
gh api repos/wnzn/metro/git/ref/tags/v0.4.0                                # commit 1a743e59…
git ls-remote --tags origin                                                # v0.3.1 a033e85, v0.3.2 d943797, v0.4.0 1a743e5
gh run view 45560122825 --repo wnzn/metro --json jobs                      # the jobs above, all success
# Anonymous GHCR pull token, then HEAD on /v2/wnzn/<image>/manifests/v0.4.0:
#   HTTP 200 for all three images; docker-content-digest equals IMAGES.txt;
#   the index lists the platforms above
gh release download v0.4.0 --repo wnzn/metro …   # all 18 assets into a fresh, empty ~/.cache dir
sha256sum -c SHA256SUMS                                                    # all 14 entries OK
sha256sum -c <each>.sha256                                                 # the 4 archives (with the MSI): OK
```

- The `SHA256SUMS.sigstore.json` bundle (Sigstore bundle v0.3, one Rekor
  entry, log index 4095390086) signs a SHA-256 digest equal to
  `SHA256SUMS`'s (`51deb6a7…b5da`). Its Fulcio certificate names
  `https://github.com/wnzn/metro/.github/workflows/sign-release-assets.yml@refs/heads/main`,
  issuer `https://token.actions.githubusercontent.com`, ref
  `refs/heads/main`, commit `1a743e59…`, the calling workflow
  `ci.yml@refs/heads/main` and run 45560122825 attempt 1. That was read
  from the certificate with `openssl x509`; **`cosign verify-blob` wasn't
  run** here (cosign isn't installed), so the signature itself and the
  Rekor inclusion weren't checked by this run.
- The image signatures weren't checked with `cosign verify` here either.
  `IMAGES.txt` states the release workflow verified them against
  `release-images.yml@refs/heads/main` and commit `1a743e59…`, and the
  run's `publish / images / sign` job passed.
- Listing the archives (no extraction run): the Linux archive holds
  `Metro`, `metro-server`, `metro-tui`, `ui/Metro`, `install-desktop`,
  `START.txt` and `RELEASE.json`; the Windows ZIP holds `Metro.exe`,
  `metro-server.exe`, `metro-tui.exe`, `ui/Metro.exe`, `START.txt` and
  `RELEASE.json`; the runner archive holds `metro-runner`, `run-runner`,
  `START.txt`, `RELEASE.json` and its licence notices.

**Windows in the release run.** `release / windows-distribution / preview`
ran on a GitHub Windows runner and passed every step: it assembled and
verified the preview, installed the unsigned MSI into a chosen folder,
checked the payload, ran the server and uninstalled it, checked that the
legacy script refuses to install next to the MSI, and ran the MSI lane's
native tests (fixtures, upgrade, repair, downgrade, uninstall). All
unattended.

**Not verified:** the desktop app or the MSI wizard on screen on native
Windows, by a person; any start of the released binaries or images (not
run here); `cosign` verification of the file or image signatures by
anyone outside the release workflow; macOS (not built); real vendor
accounts and sign-ins; pretrained `google/embeddinggemma-2` ranking.
Windows files carry no Authenticode signature.

## Source only: updater re-review (2026-11-25, local)

Verified locally on `lane/updates-backend` at `92979ca`, after `916f8da`.
None of this is in Metro 0.4.0. Nothing was deployed, pushed, or merged.

**Migration and recovery limits.** Linux desktop in-app updates and Linux
runner self-update require an explicit migration. With the app or runner
stopped and no installer running, use the installer and verified prebuilt
archive for the build that includes these changes:
`update --archive FILE --migrate-update-lock`. Retire old installer copies
before restarting. The new lock excludes 0.4.0 operations that take `.lock`;
published runner `configure` and `uninstall` and Windows script `Uninstall`
bypass it. The migration does not make those old commands safe to run
concurrently.

Windows script and MSI updates are manual-only. The launcher won't start an
MSI helper. If an old journal leaves completion uncertain, Metro stops
before opening the workspace and preserves both the journal and the backup.
Confirm the Windows Installer transaction has ended and resolve the
installation and data manually before restarting. A dead helper PID is
not proof. Unowned backups from older Metro builds are not consumed.

**Local checks.**

- Locked all-features tests for `metro-release`, `metro-runner`,
  `metro-connector`, `metro-server` and `metro-desktop`: 740 passed,
  2 ignored. Formatting and strict Clippy on those crates passed.
  Regressions cover interruptions before and after backup publication and
  during cleanup, a modified staged menu script, legacy MSI journals,
  cached TUF expiry, and a shared release floor raised by another checker.
  The desktop and runner update locks were also exercised in both
  directions against the published Linux scripts.
- `tests/e2e/updates.py` passed with separate test-release binaries. When
  rollback was due and the previous executable was unavailable, the runner
  saved rollback intent, exited with status 75, and sent no claim. The same
  test exercised the in-place update and the rollback after three restarts.
- Packaging, release-signature and Windows-signing suites: 286 passed,
  11 skipped. Published installer fixtures matched the signed 0.4.0
  `SHA256SUMS` entries. The Linux installers ran in throwaway homes. The
  Windows fixture exercised the published and current lock paths in
  PowerShell on Linux, stopping before extraction; it was not an MSI run.
- The admin browser suite passed 22/22 in
  `mcr.microsoft.com/playwright:v1.58.2-noble`. The release, connector and
  desktop crates checked for `x86_64-pc-windows-gnu`, with pre-existing
  connector warnings. This was cross-compilation only.

Native Windows MSI transactions, helper death and shortcuts, live TUF
refresh, and updates between published Metro releases were not tested.

## Source only: updater review 2 (2026-11-25, local)

Tested on `lane/updates-backend` at `3de67c1`. This work was not part of
Metro 0.4.0 and was not published or merged.

- The locked Rust workspace suite ran with `METRO_SERVER_BIN` pointed at
  the local server binary: 803 passed, 2 ignored. Formatting and workspace
  Clippy (`--all-targets --all-features -- -D warnings`) passed.
- Locked all-features tests for `metro-release`, `metro-server`,
  `metro-runner`, `metro-connector`, and `metro-desktop`: 730 passed,
  2 ignored. This run omitted debug symbols to fit on disk.
- The eight plain end-to-end scripts passed against builds without
  test-release hooks: `smoke`, `threads`, `device_connector`, `settings`,
  `schedules`, `intelligence`, `attachments`, and `library`.
- `tests/e2e/updates.py` passed with separate test-release fixture
  binaries. It verified the update notice and owner-only request, updated
  the runner from 1.0.0 to 1.0.1 without restarting the process, then
  rolled a crashing 1.0.2 back after three restarts and reported
  `start_failed`.
- A build check with the `e2e` feature and `METRO_RELEASE_CHANNEL=release`
  was rejected by the compile-time guard as expected.
- The packaging, release-signature, and Windows-signing suites ran with
  PyYAML 6.0.4: 282 passed, 11 skipped. The PowerShell fixtures ran on Linux;
  they are not native Windows install or signing tests.
  Both Linux installers failed the new live-lock regressions when the same
  tests were run against the scripts from `b04dd43`.
- The admin browser suite passed 22/22 in
  `mcr.microsoft.com/playwright:v1.58.2-noble`. `actionlint` 1.7.12 passed.
- `metro-release`, `metro-connector`, and `metro-desktop` checked for
  `x86_64-pc-windows-gnu` in `metro-package-builder:windows-gnu`. This was
  a cross-check only. Native Windows MSI execution, shortcuts, and helper
  handoff were not exercised.

The trust tests replay CDN-captured signed TUF metadata locally and verify
real Metro 0.4.0 Sigstore fixtures offline. They do not demonstrate a live
TUF refresh or an update from one published Metro release to another.

## Staged: Spaces with several agents, per-connection model lists and the desktop redesign at 4175475 (2026-11-25, project manager, local)

A **gate on unreleased source**, run on Linux and reported by the project
manager. It isn't a release, and nothing was built for distribution or
published. Its source came after the published
[Metro 0.3.2](#published-metro-032-2026-11-24-github-release-v032) release
(tag on `d943797`). It isn't in 0.3.0, 0.3.1 or 0.3.2; it was later
released in [Metro 0.4.0](#published-metro-040-2026-11-25-github-release-v040).
Packaging tests weren't rerun.

**Source.** Commits `4d72b28` through `4175475`:

- **ChatGPT connector** (`4d72b28`, `80b82cf`): reads JSON and untyped `200`
  answers, skips unknown items, reports an answer it can't use as
  `chatgpt_unusable_response`, sends the `media_type`, passes reasoning
  summaries through, sends the catalog's display names, and forwards custom
  thinking for OpenAI-compatible endpoints.
- **Per-connection model lists** (`85b579e`): listed models are opt-out,
  manual models carry owner-set thinking options, the first connection of a
  capability with a model becomes its default automatically, and the
  operator's model is shown only as described in [settings.md](settings.md).
- **Spaces** (`e0f9124`): edit, delete and leave; several agents per Space
  with `@handle` mentions; the classifier picks an agent or none; per-Space
  preferences; `reasoning_summary` events.
- **Desktop** (`5abdb06` through `4175475`): Model providers by capability
  with a details view and one grouped model picker, Space settings (General,
  agents, your preferences), the Ask-agent chooser and each run's agent,
  the activity block, the sidebar rail and resizable panels, one header
  height, per-place drafts, and **Test runner** for every member in Space
  settings › Members and runner.

| Check | Result |
| --- | --- |
| `cargo build --workspace --locked`, then `cargo test --workspace --locked --no-fail-fast`, at `8dc3f16` (no Rust changes after it) | **686 passed, 0 failed**. The Docker sandbox suite isn't claimed: those tests ran or skipped as the environment allowed. |
| e2e scripts at `8dc3f16` | 8/8 passed: smoke, threads, settings, device_connector, intelligence, attachments (`--timeout 60`), schedules (`--timeout 150`), library. `library_reference.py` wasn't run (it needs the media service's Python environment). |
| Desktop at `4175475`, NSH 0.6.1: `nsh check` | Clean |
| Desktop at `4175475`, NSH 0.6.1: `nsh test` | **400/400** |

The desktop screenshots reviewed for this gate were rendered against the
in-memory fake server. They're UI previews, not a run against a real
server, and they aren't stored in the repository.

**Not covered by this gate:**

- The connector fix with a real ChatGPT or OpenAI account: it needs a
  confirmation on the user's own device.
- Native Windows and macOS.
- Real agent picture images.
- Resetting a panel's width with a double-click.
- Packaging tests, release archives, and the Docker sandbox suite.

## Published: Metro 0.3.2 (2026-11-24, GitHub release v0.3.2)

**What was published.** The GitHub release
[Metro 0.3.2](https://github.com/wnzn/metro/releases/tag/v0.3.2), tag
`v0.3.2`, public (not a draft, not a prerelease, marked latest), published
2026-11-24T15:30:55Z by `github-actions[bot]`. The tag points at commit
`d943797a332f42bd38ca023f63f624e205af14cf` (the merge of pull request #12,
the 0.3.2 bump on `main`). It was made by the push-to-`main` CI run
[45418420759](https://github.com/wnzn/metro/actions/runs/45418420759)
(conclusion `success`), whose jobs `release / *`,
`release / windows-distribution / seed` and `/ preview`,
`publish / authorize`, `/ tag`, `/ assets`, `/ images / guard`,
`/ images / build` (amd64, arm64), `/ images / media`,
`/ images / publish`, `/ images / sign`, `/ sign / authorize`, `/ sign / sign`
and `/ publish` all passed; `publish / windows-sign` was skipped (Windows
unsigned). `v0.3.1` is tagged on `a033e85` but has no release.

It doesn't contain anything merged after `d943797` (for example the
staged desktop redesign on `ui/providers-settings-redesign`).

**Assets** (18):

| File | Signature |
| --- | --- |
| `Metro-0.3.2-linux-x64.tar.gz` + `.sha256` | `Metro-0.3.2-linux-x64.tar.gz.sigstore.json` |
| `Metro-0.3.2-windows-x64.zip` + `.sha256` | none (no Authenticode) |
| `Metro-0.3.2-windows-x64.unsigned-preview.msi` + `.sha256` | none (no Authenticode) |
| `Metro-runner-0.3.2-linux-x64.tar.gz` + `.sha256` | `Metro-runner-0.3.2-linux-x64.tar.gz.sigstore.json` |
| `install-metro-linux.sh`, `install-metro-runner-linux.sh`, `install-metro-windows.ps1`, `metro-runner-container.sh`, `metro-runner-container.ps1` | none of their own; listed in `SHA256SUMS` |
| `IMAGES.txt` | listed in `SHA256SUMS` |
| `SHA256SUMS` | `SHA256SUMS.sigstore.json` |

**Images** (public on GHCR, from `IMAGES.txt`; no `latest` tag):

| Reference | Index digest | Platforms |
| --- | --- | --- |
| `ghcr.io/wnzn/metro-server:v0.3.2` | `sha256:3095c897e1e4659eef902cbd540ab762111a895232db7818a072929bf85112d5` | `linux/amd64`, `linux/arm64` |
| `ghcr.io/wnzn/metro-runner:v0.3.2` | `sha256:1030ed8cf44bc145d33e1e5b03ad34653f8b62179392248acda61bc97462d324` | `linux/amd64`, `linux/arm64` |
| `ghcr.io/wnzn/metro-embeddinggemma2:v0.3.2` | `sha256:5f979be7f017f890e56673442856fa29fbb4dca223ba509446471d9b6128fa68` | `linux/amd64` (no weights) |

**How it was checked** (2026-11-25, docs lane, from a Debian 13 host; no
downloaded file was executed):

```sh
gh release view v0.3.2 --repo wnzn/metro \
  --json tagName,publishedAt,assets,targetCommitish,isDraft,isPrerelease   # 18 assets, isDraft false, isPrerelease false
gh release list --repo wnzn/metro                                          # only "Metro 0.3.2  Latest  v0.3.2"
gh api repos/wnzn/metro/git/ref/tags/v0.3.2                                # commit d943797a…
git ls-remote --tags origin                                                # v0.3.1 a033e85, v0.3.2 d943797
gh run view 45418420759 --repo wnzn/metro --json jobs                      # the jobs above, all success
# Anonymous GHCR pull token, then HEAD on /v2/wnzn/<image>/manifests/v0.3.2:
#   HTTP 200 for all three images; docker-content-digest equals IMAGES.txt
gh release download v0.3.2 --repo wnzn/metro -p … # into a fresh, empty ~/.cache dir
sha256sum -c --ignore-missing SHA256SUMS                                   # IMAGES.txt and the 3 archives + sidecars: OK
sha256sum -c <each>.sha256                                                 # the 3 archives: OK
```

- The `SHA256SUMS.sigstore.json` bundle (Sigstore bundle v0.3, one Rekor
  entry, log index 4081339195) signs a SHA-256 digest equal to
  `SHA256SUMS`'s (`b77097ca…b6d5`). Its Fulcio certificate names
  `https://github.com/wnzn/metro/.github/workflows/sign-release-assets.yml@refs/heads/main`,
  ref `refs/heads/main` and commit `d943797a…`. That was read from the
  certificate with `openssl x509`; **`cosign verify-blob` wasn't run** here
  (cosign isn't installed), so the signature itself and the Rekor
  inclusion weren't checked by this run.
- The image signatures weren't checked with `cosign verify` here either.
  `IMAGES.txt` states the release workflow verified them, and the run's
  `publish / images / sign` job passed.
- Listing the archives (no extraction run): the Linux archive holds
  `Metro`, `metro-server`, `metro-tui`, `ui/Metro`, `install-desktop` and
  `RELEASE.json`; the Windows ZIP holds `Metro.exe`, `metro-server.exe`,
  `metro-tui.exe`, `ui/Metro.exe` and `RELEASE.json`; the runner archive
  holds `metro-runner`, `run-runner`, `START.txt`, `RELEASE.json` and its
  licence notices.
- `gh api /users/wnzn/packages` needs the `read:packages` scope, which this
  host's token lacks; the images were checked through the registry API
  instead.

**Windows in the release run.** `release / windows-distribution / preview`
ran on GitHub's `windows-2025` runner: it assembled and verified the
preview, installed the unsigned MSI into a chosen folder, checked the
payload, ran the server and uninstalled it, checked that the legacy script
refuses to install next to the MSI, and ran the MSI lane's native tests
(fixtures, upgrade, repair, downgrade, uninstall). All unattended.

**Not verified:** the desktop app or the MSI wizard on screen on native
Windows, by a person; any start of the released binaries or images (not
run here); `cosign` verification of the file or image signatures by
anyone outside the release workflow; macOS (not built); real vendor
accounts and sign-ins; pretrained `google/embeddinggemma-2` ranking.
Windows files carry no Authenticode signature.

## Source only: image load on Docker's classic image store, version 0.3.2 (2026-11-24, packaging lane, local)

The 0.3.1 release run failed in `release-images.yml`, job `build`, step
"Load exactly those archives": `docker load -i <name>-<arch>.oci.tar` exited
1. The orchestrator reproduced it against `docker:28.0.4-dind` (the Docker
version of GitHub's ubuntu-24.04 runners, classic overlay2 image store):
`open /var/lib/docker/tmp/docker-import-…/blobs/json: no such file or
directory`. The archives are pure OCI layouts from buildx, without a Docker
`manifest.json`, which the classic store needs and the containerd store
(Docker 29) doesn't.

Fix, in `packaging/image_release.py` `load` only: it writes a temporary
copy of the archive (in a private temporary directory that's removed
afterwards) with an added Docker `manifest.json` whose `Config` and
`Layers` (in manifest order) are the blobs of the same manifest `describe`
resolves through `index.json`, and loads that copy. `index.json` stays in
the copy, so the containerd store still loads by manifest digest. The
built archive, its digests and what `push` publishes don't change. An
archive that already has a `manifest.json` is accepted only if it names
exactly that image. The checks after loading are unchanged (exactly one
image id, equal to the config or manifest digest, then tag and platform),
and a failed `docker load` now reports Docker's error output. The version
moves to `0.3.2` (Cargo workspace, `apps/desktop/nsh.json`, `Cargo.lock`,
the file names in `docs/deploy.md` and `docs/runner.md`), because `v0.3.1`
is already tagged.

Ran locally on Debian 13 (Python 3.13, PyYAML in a throwaway uv
environment; no PowerShell, so its tests skipped). No Docker daemon was
used; `docker` is mocked in the new tests:

| Check | Result |
| --- | --- |
| `python3 -I packaging/release_policy.py version` | `version=0.3.2` |
| `cargo metadata --locked --format-version 1` | passed |
| `python3 -B -m unittest discover -s packaging -v` | 226 tests, 15 skipped, passed in a throwaway clone with the change committed; in the uncommitted worktree, one test fails only because `HEAD` is still 0.3.1 |
| New `LoadTests` (5): `manifest.json` paths and layer order, plain and index-wrapped layouts, source archive unchanged and temporary copy removed, classic and containerd ids, Docker's error shown, wrong id or platform refused, a disagreeing `manifest.json` refused before `docker` runs | passed |

**Not yet shown here:** a real `docker load` of a buildx archive on Docker
28.0.4 (classic store) or Docker 29 (containerd store) with this change,
and the release run on GitHub.

## Source only: releases ship Windows unsigned, version 0.3.1 (2026-11-24, packaging lane, local)

The maintainer reported that the 0.3.0 bump's run on `main` (GitHub Actions
run 45372491546) passed its build and test gates and then stopped in
`publish / windows-sign / guard` with `METRO_WINDOWS_SIGNING_BACKEND must be
azure or kms`: the repository has no Windows code-signing certificate, no
`windows-release-signing` environment and no signing variables. Nothing was
tagged or published. That run wasn't repeated here.

Decision: release with Windows unsigned, labelled so, until a certificate
exists. The new checked-in switch `packaging/windows-release-signing.txt`
says `unsigned`. In that mode `windows-distribution.yml` checks the seed in
official mode, then builds and natively tests the unsigned build as for a
test build, and `release-publish.yml` skips `windows-sign.yml`. The release
ships `Metro-0.3.1-windows-x64.zip`
(`RELEASE.json` `unsigned-preview`, with its commit),
`Metro-0.3.1-windows-x64.unsigned-preview.msi`, their `.sha256` files and
the unsigned `.ps1` scripts. Its release notes say the Windows files aren't
code-signed and how to check them. The version moves to `0.3.1` (Cargo
workspace, `apps/desktop/nsh.json`, `Cargo.lock`), because 0.3.0's bump
push has already happened.

Ran locally on Debian 13 (Python 3.13, PyYAML 6.0.4 and ShellCheck 0.11.0 in
a throwaway virtualenv; no PowerShell, so its tests skipped):

| Check | Result |
| --- | --- |
| `python3 -I packaging/release_policy.py version` | `version=0.3.1` |
| `python3 -I packaging/release_policy.py windows-signing` | `windows_signing=unsigned` |
| `cargo metadata --locked --format-version 1` | passed |
| `shellcheck -s sh packaging/*.sh` | passed |
| actionlint 1.7.12 (checksum as in `ci.yml`), with ShellCheck | passed |
| Every action pinned to a full SHA (the `ci.yml` check) | passed |
| `python3 -B -m unittest discover -s packaging -v` | 220 tests, 15 skipped, passed in a throwaway clone with the change committed; in the uncommitted worktree, one test fails only because `HEAD` is still 0.3.0 |
| `python3 -B -m unittest discover -s packaging/release-signatures -v` | 18 tests, 1 skipped, passed |
| `python3 -B -m unittest discover -s packaging/windows-signing -v` | 41 tests, 6 skipped, passed |

The unsigned (`--preview`) MSI's wording now fits both its uses, test
builds and unsigned releases. Its product name is "Metro (unsigned)", not
"Metro (unsigned preview)". Its first page says: "This Metro build isn't
code-signed, so Windows may warn about an unknown publisher. Before you
install it, check the file against its SHA-256 checksum (SHA256SUMS)." It
no longer says it "is not a Metro release". With the pinned MSI toolchain
(`build_msi.py fetch-tools`, hashes from `wixl-lock.json`),
`METRO_MSI_TOOLS=… python3 -B -m unittest test_windows_msi` ran 26 tests,
with 2 skipped for lack of PowerShell, and passed. That includes the real
`--preview` builds and their inspection. A `--preview` MSI built from the
lane's fake payload showed, through `msiinfo`, `ProductName` "Metro
(unsigned)", the new `PreviewNote` text, the new Comments, and no
"not a Metro release" or "local preview" anywhere in its `Control` table.
`MsiHarness.ps1` and `test-msi-harness.ps1` expect the new name, but
without `pwsh` they weren't run here.

**Not yet shown:** nothing ran on GitHub. The unsigned release path, the
native Windows checks in it (including the PowerShell harness's product-name
check), the wizard on screen, and the publication of 0.3.1 run only when the
bump reaches `main`.

## Source only: the release version moves to 0.3.0 (2026-11-24, packaging lane, local)

A protected `main` push releases only when it changes the checked-in version,
and the version has been `0.2.0` since `89eedf7` (2026-11-22). Every `main`
push since releases were retargeted to `main` therefore decided
`unchanged (before: 0.2.0, now: 0.2.0); CI only, no release` — as run 45361682056
logged — so `release` and `publish` skipped and nothing was ever published.

This bumps the version that the next release will carry: Cargo
`[workspace.package] version`, `apps/desktop/nsh.json` (which must match) and
`Cargo.lock` move to `0.3.0` together, and the three named release-file targets
in [deploy.md](deploy.md#release-files-and-images),
[runner.md](runner.md#separate-remote-runner) and the desktop README's download
names move with them. `0.3.0` rather than another `0.2.x`: `0.2.0` was never
published, and the source is well past it. Provenance prose — the 0.2.0
candidate, the current source after 0.2.0 — is left as written, because those
records still describe exactly what was verified.

**Not yet shown:** no release, file or image exists, and no official run has
been attempted. Merging a version bump into `main` starts the first attempt,
and it publishes nothing until the `windows-release-signing` environment exists
with its one `main` branch rule and `METRO_WINDOWS_SIGNING_BACKEND` and
`METRO_WINDOWS_PUBLISHER_CN` are set, because `windows-sign` runs before
`release_policy.py tag`. A bump to a prerelease such as `0.3.0-rc.1` would
publish as a prerelease instead of `latest`.

Checked locally, `python3 -m unittest discover -s packaging` with PyYAML
installed: 209 tests pass (15 skipped: no PowerShell 7, `METRO_MSI_TOOLS`
unset). `cargo metadata --locked` confirms `Cargo.lock` matches the manifests.

## Source only: CI runs only the segments a change needs (2026-11-24, CI lane, local)

Uncommitted changes on `ci/path-segmentation`, on top of `17b6e3c`, to
`ci.yml` and `packaging/package_smoke_policy.py`, renamed to
`packaging/ci_changes.py` ([deploy.md](deploy.md#ci-and-publishing)):

- The `workflows` job classifies pull requests into `main` (from the merge
  base) and now also pushes to `main` (from `github.event.before`) into five
  segments: `rust`, `desktop`, `containers`, `packaging` and `previews`
  (`package-smoke`). Each of those jobs runs only when its segment isn't
  `false`; an empty answer runs it.
- Documentation, `apps/desktop/dev`, `ops/`, `.gitignore` and
  `.env.example` need no segment. `ci.yml`, `.gitattributes`, `Cargo.toml`,
  `apps/desktop/nsh.json` and unknown paths need all of them. So does a
  push that is forced, deletes the branch, creates it, or whose `before`
  isn't an ancestor, and any empty or unreadable diff.
- `rust`, `containers`, `packaging` and `desktop` also run whenever
  `release-policy` decides to release, so they now also wait for it. The
  `release` job requires each of its gates' result to be `success`.

**Not yet shown:** any GitHub run of these changes. That a skipped job
satisfies the required checks was seen on PR #6 only for pull requests.

Checked locally, with `TMPDIR` off `/tmp`:

```sh
~/.cache/actionlint-1.7.12/actionlint -color   # clean, also with shellcheck enabled
# ci.yml's "Every action is pinned" grep       # no unpinned actions
shellcheck -s sh packaging/*.sh                # clean (koalaman/shellcheck:stable)
python -B -m unittest discover -s packaging    # 209 tests, OK (15 skipped: no pwsh, MSI tools or macOS runtimes, macOS jobs off)
python -B -m unittest discover -s packaging/release-signatures  # 17 tests, OK (1 skipped)
python -B -m unittest discover -s packaging/windows-signing     # 39 tests, OK (6 skipped)
python -B -m unittest discover -s ops/github-runner             # 33 tests, OK
```

The classifier was also run by hand as a push to `main` (`before` = first
parent) on this repository's history:

| Commit | Changes | rust | desktop | containers | packaging | previews |
| --- | --- | --- | --- | --- | --- | --- |
| `17b6e3c` | README, screenshots, docs, `apps/desktop/dev` | skip | skip | skip | skip | skip |
| `eea7221` | `ci.yml`, `release.yml`, desktop tests | run | run | run | run | run |
| `0a84b10` | `crates/connector` tests | run | skip | run | skip | run |
| `7194081` | `AGENTS.md`, `docs/acceptance.md` | skip | skip | skip | skip | skip |
| `c838263` | `packaging`, `release.yml` | skip | skip | skip | run | run |
| `7452f6f` | `ops/github-runner` | skip | skip | skip | skip | skip |

`17b6e3c` with an all-zero `before`, or marked forced, ran everything. The
`17b6e3c` push was also classified in a fresh depth-1 clone of
`github.com/wnzn/metro`, as CI checks it out; the classifier fetched
`before` and gave the same answer.

## Screenshots refreshed from main `eea7221` (2026-11-24, docs lane, local)

Uncommitted changes on `docs/readme-refresh`, on top of `eea7221` (then
`origin/main`). The desktop screenshots in `docs/screenshots/` were retaken
from that source, natively on NSH 0.6.1's Linux (GTK 4) host under Xvfb in
the `metro-desktop-shots:trixie` and `:trixie-media` images, against
debug `metro-server` and `metro-runner` built with `cargo build --locked`
at `eea7221`, on loopback, in the default dark theme. Every model was a
local double: `scripted_model.py` for chat, and keyless loopback fixtures
for System One, embeddings and media embeddings. No real vendor was called.

| Script | Files written to `docs/screenshots/` | Result |
| --- | --- | --- |
| `apps/desktop/dev/screenshots/capture.sh` (all scenarios) | `connection`, `connection-error`, `room-runner`, `space-members`, `agent-thinking`, `agent-activity-expanded`, `chat-runs`, `thread`, `settings-runners`, `settings-providers`, `settings-device-onboarding`, `settings-appearance`, `settings-space-runner` | pass; the compact profile save was checked on the server |
| `capture-routing.sh` | `model-routing`, `model-routing-dataflow`, `model-sharing`, `route-attribution` | pass, after the fixes below |
| `capture-library.sh` | `library-assets`, `library-search`, `library-open-page`, `library-open-image`, `library-revocation-before`, `library-revocation-after` | pass, after the fixes below |
| `capture-discovery.sh` (new) | `semantic-search`, `scheduled-tasks` | pass; System One was called, and the one-time task dispatched |

That's 25 regenerated files, with the same names. Each script's own check
found none of its tokens or keys in the output, the checkout or the staged
app. Every new image was also reviewed by eye against the old one: no token
on screen (the device token is masked), no error banner except the intended
`401` in `connection-error`, and a fully drawn window. They were then
optimized with `optipng -o5 -strip all`. ImageMagick `compare` found them
pixel-identical before and after.

Two referenced files were **kept unchanged**: `local-workspace-relaunch.png`
and `settings-profile.png`. They're the 0.2.0 Linux-candidate QA shots of a
packaged archive across a relaunch, with the native file picker
([below](#staged-020-linux-candidate-local-workspace-and-desktop-qa-2026-11-22-project-manager-local)).
The screenshot scripts can't reproduce that flow, and these shots show the
older UI.

Script fixes, all dev tooling under `apps/desktop/dev/screenshots/`:

- `capture-routing.sh` and `capture-library.sh` use a random private X
  display per run, like `capture.sh`. With the default `:99` and
  `--network host`, two captures at once collided.
- `routing-steps.mjs`:
  - opens a connection's **Manage** before its sharing control;
  - waits for the route badge *Routed: Incident review*;
  - scrolls the Settings window to the use cases, and to the whole sharing
    warning.
- `library-steps.mjs` and `capture-library.sh` follow the current layout:
  - the Library in the sidebar;
  - the media model saved in Settings › Model providers (session
    `alice-model`);
  - Index from each row's **More** menu (session `alice-index`, which also
    takes `library-assets.png`, because index status lives only in that
    session);
  - search from the box at the top, in Search › Documents & media.
- The unpublished `library-open-time.png` was dropped. In the container,
  the player refuses to play because its preview folder isn't private
  (mode 755). That's the app's own check, not a script fault.
- New: `capture-discovery.sh`, `discovery-steps.mjs` and
  `search_fixture.py`, a keyless word-overlap embeddings and System One
  double. It isn't a semantic model. It follows the same rules as
  `capture.sh`: a staged read-only app copy, tokens only in mode-600 files,
  and redacted output.

Older entries in this file that link these file names, such as the routing,
Documents & media, search, scheduled-tasks and settings captures, now show
the current UI rather than what those runs saw. Their text still describes
those runs. In particular, `model-routing.png` now shows the use cases
rather than the classifier and the routing switch, `library-search.png` is
in similarity order (not reranked), and `semantic-search.png` no longer has
per-result score labels.

Not checked here: the desktop `nsh test` suite and the Rust gate (no
application source changed), the light theme, Windows and macOS, and real
vendors.

## Source only: leaner pull request CI (2026-11-24, CI lane, local)

Uncommitted changes on `ci/trim-steps`, on top of `3933776`, to `ci.yml`,
`release.yml` and `packaging/package_smoke_policy.py`
([deploy.md](deploy.md#ci-and-publishing)):

- The `workflows` job's policy step reads one diff of a pull request into
  `main` and gives three answers: `skip_previews` (as before),
  `docs_only` and `containers`. Pushes, other bases, empty or unreadable
  diffs, symlinks and unknown paths answer `skip_previews=false`,
  `docs_only=false`, `containers=true`.
- `docs_only=true` skips `rust`, `containers` and `desktop`. Documentation
  means Markdown and images under `docs/`, top-level Markdown and Markdown
  under `ops/`, and nothing else.
- `containers=false` skips `containers`. Its inputs are the Dockerfile,
  `.dockerignore`, `.gitattributes`, the Cargo files, `crates/`, the
  Compose files, `services/`, the two container runner scripts,
  `tests/e2e/`, `rust-toolchain*` and `ci.yml`. Only paths under `docs/`,
  `apps/`, `examples/`, `ops/`, other `packaging/` and `tests/` files,
  other `.github/` files, `.cargo/`, top-level Markdown, `.env.example`
  and `.gitignore` can skip it.
- `rust` and `containers` now wait for `workflows`, and always run on
  pushes, including when `workflows` fails.
- The `desktop` job no longer runs `nsh build --release`; it keeps
  `nsh check` and `nsh test`.
- `release.yml`'s `desktop-linux` no longer has the best-effort Xvfb
  launch (`continue-on-error`) or its screenshot upload.

**Not yet shown:** any GitHub run of these changes. That a job skipped by
`if:` satisfies the required `rust` and `containers` checks, and the job
conditions themselves, were checked only by the contract tests below, not
on GitHub.

Checked locally, with `TMPDIR` off `/tmp`:

```sh
~/.cache/actionlint-1.7.12/actionlint -color   # clean
# ci.yml's "Every action is pinned" grep       # no unpinned actions
python -B -m unittest discover -s packaging    # 200 tests, OK (15 skipped: no pwsh, MSI tools or macOS runtimes, macOS jobs off)
python -B -m unittest discover -s packaging/release-signatures  # 17 tests, OK (1 skipped)
python -B -m unittest discover -s packaging/windows-signing     # 39 tests, OK (6 skipped)
python3 -B -m unittest discover -s ops/github-runner            # 33 tests, OK
```

The policy was also run by hand on this repository's history, as a pull
request into `main` from each commit's parent:

| Commit | Changes | skip_previews | docs_only | containers |
| --- | --- | --- | --- | --- |
| `ff787b7` | `docs/acceptance.md` | true | true | false |
| `1719366` | `tests/e2e/schedules.py` | true | false | true |
| `0a84b10` | `crates/connector` tests | false | false | true |
| `057284a` | `apps/desktop` and docs | false | false | false |
| `82ff919` | `compose.prebuilt.yaml`, a packaging test | false | false | true |

`ff787b7` as a push or as a pull request into `master`, and an invalid SHA,
answer `false`, `false`, `true`. In a throwaway clone, one-file commits on
`3933776` gave: `Dockerfile` false, false, true; `.github/workflows/release.yml`
false, false, false; `docs/screenshots/new.png` true, true, false.

**Desktop tests and the wall clock (`ad8f994`).** Three schedule-card
tests compared fixed November 2026 firings with the app's `Date.now()`
and broke as the real date moved on. They now run on NSH's manual clock
from Mon, Nov 23, 2026 12:00 UTC. Desktop `nsh test`: 353/353, and
`nsh check` passed (NSH 0.6.1).

**`desktop-linux` installer check without a download.** The installer
step's real `--install-deps` run downloaded about 58.5 MB from GitHub's
Ubuntu mirror. That took 10–16 minutes, and in run 45320397101 more than
57 minutes before the run was cancelled. A cache of the `.deb` files was
tried (`2decebd`) and removed again: only a `main` run that suffered the
same download could fill it, and it added five steps. The step now needs no
network:

- Where `ldd` finds libraries missing, it checks that the installer
  without `--install-deps` refuses, prints the exact
  `sudo apt-get install --no-install-recommends libgtk-4-1 libwebkitgtk-6.0-4 gstreamer1.0-plugins-base`
  line and installs nothing.
- Where the libraries are already present, it runs the real install,
  `check-deps`, both updates and the uninstall.

`--install-deps` itself, and install, update and uninstall without the
libraries, are covered by `packaging/test_installers.py` with fake tools.
`desktop-linux`, `desktop-windows` and `runner-linux` now have job
timeouts of 30, 40 and 20 minutes. Recent runs took about 5–7, 7–15 and
4 minutes. Checked locally:

```sh
~/.cache/actionlint-1.7.12/actionlint -color   # clean, also with shellcheck (koalaman/shellcheck:stable)
# ci.yml's "Every action is pinned" grep       # no unpinned actions
python -B -m unittest discover -s packaging    # 202 tests, OK (15 skipped, as above)
python -B -m unittest discover -s packaging/release-signatures  # OK (1 skipped)
python -B -m unittest discover -s packaging/windows-signing     # OK (6 skipped)
python3 -B -m unittest discover -s ops/github-runner            # 33 tests, OK
```

**Not yet shown:** this step on GitHub.

## Source only: releases from `main` on a version bump (2026-11-24, CI lane, local)

Uncommitted changes on `fix/ci-cleanup`, on top of `106e5da`
([deploy.md](deploy.md#releases-from-main-on-a-version-bump)).
`main` is the only long-lived branch. Earlier entries that name `master`
describe the source as it was then.

- Every release guard, signer identity and provenance check now names
  `refs/heads/main` and `ci.yml@refs/heads/main`: `release_policy.py`,
  `winsign.py`, `release_signatures.py`, `ci.yml`, `release.yml`,
  `release-publish.yml`, `windows-sign.yml`, `windows-distribution.yml`,
  `sign-release-assets.yml` and `release-images.yml`. `ci.yml` no longer
  runs on `master` pushes.
- Only a version bump releases. `release_policy.py decide` compares the
  checked-in version with the one at `github.event.before`. Without a
  change, or without a readable previous version, it answers
  `release=false` before any GitHub call. `authorize` and `tag` recheck the
  bump and fail without one.
- A version bump uses the earlier eligibility table. A re-run of the bump's
  run resumes after a failure.
- Release runs keep the `desktop` job on GitHub-hosted runners. The
  self-hosted queue is used only when release-policy didn't choose a
  release.
- Every push to `main` has its own workflow-level concurrency group and is
  never cancelled or replaced; pull requests keep `ci-pr-<n>` with cancel.
  Only the `publish` job is serialized (`metro-official-release`, never
  cancelled), so only a second release run can wait behind a first.
- The Windows verifier self-test no longer decides by branch. It runs only
  when the preview job sets `METRO_WINDOWS_SELF_TEST=preview` (from
  `!inputs.release`), and refuses tags and jobs that can request an OIDC
  token.
- The `packaging` CI job also runs the `packaging/release-signatures` and
  `packaging/windows-signing` suites.
- No macOS builds for now, for Apple silicon or Intel, in previews or
  releases: GitHub's macOS runners queued for 15+ minutes and jobs were
  cancelled. `desktop-macos`, meta's macOS matrix and `macos-sign` are
  removed, along with the five Apple secrets passed to `release-publish.yml`.
  `release-publish.yml`'s `assets` job now checks the tag step's `proceed`
  itself; before, `macos-sign` did. The official set is Windows and Linux
  only (no macOS archive, no `install-metro-macos.sh`). The macOS packaging
  code and its unit tests stay. The install-step tests skip while
  `desktop-macos` is absent, and the release-assets macOS gate is still
  tested with the targets patched back in.

With version 0.2.0 checked in and nothing published, merging this releases
nothing until a later pull request changes the version.

**Not yet shown:** any GitHub run. The `windows-release-signing`
environment's deployment rule must name `main` before Windows signing can
pass. The runner routing, the `publish` concurrency group and the release
DAG without macOS were checked only by actionlint and the contract tests
below. The self-test guard ran under Linux pwsh 7.7.0 only, not on Windows.

Checked locally, with `TMPDIR` off `/tmp`:

```sh
~/.cache/actionlint-1.7.12/actionlint -color   # clean
# ci.yml's "Every action is pinned" grep       # no unpinned actions
python -B -m unittest discover -s packaging -v # 190 tests, OK (15 skipped: no pwsh, MSI tools, macOS runtimes; 3 desktop-macos step tests)
python3 -B -m unittest discover -s packaging/release-signatures  # 17 tests, OK (1 skipped: no METRO_TEST_COSIGN)
python3 -B -m unittest discover -s packaging/windows-signing     # 39 tests, OK (6 skipped: no pwsh)
python3 -B -m unittest discover -s ops/github-runner             # 33 tests, OK
# Again with pwsh 7.7.0 on PATH, as on ubuntu-24.04:
#   packaging 190 OK (10 skipped: MSI tools, macOS runtimes, desktop-macos step); windows-signing 39 OK (0 skipped)
# shellcheck -s sh packaging/*.sh (koalaman/shellcheck:stable)  # clean
```

The policy tests run `release_policy.py` itself with synthetic GitHub
environments and no network: no-bump pushes (same version, all-zero,
unknown, malformed or missing `before`) answer `release=false`, and a bump
in a throwaway repository reaches the GitHub client. `master` refs are
refused everywhere.

## Source only: faster pull request CI (2026-11-24, CI lane, local)

Uncommitted changes on `fix/ci-cleanup`, on top of `1719366`, to `ci.yml`,
`release.yml` and `packaging/package_smoke_policy.py`
([deploy.md](deploy.md#ci-and-publishing)):

- a new push to a pull request cancels its older run; pushes and the
  official `master` group never cancel;
- the packaging tests run in their own `packaging` job, which releases need;
- a pull request into `main` skips the test builds unless it changes a
  build input; pull requests build only the Apple-silicon Mac;
- a Rust cache, written only by `main` pushes and skipped by release builds;
- the `rust` job builds its debug binaries once and no longer builds a
  release connector, and `desktop-linux` no longer runs a second release
  compile for tests.

**Not yet shown:** any GitHub run of these changes. GitHub's matrix from
`fromJSON`, cancellation and cache scoping were checked only by the
contract tests below, not on GitHub.

Checked locally, with `TMPDIR` off `/tmp`:

```sh
~/.cache/actionlint-1.7.12/actionlint -color   # clean
# ci.yml's "Every action is pinned" grep       # no unpinned actions
python -B -m unittest discover -s packaging -v # 182 tests, OK (12 skipped: no pwsh, MSI tools or macOS runtimes)
python3 -B -m unittest discover -s ops/github-runner  # 33 tests, OK
```

The `Swatinem/rust-cache` pin `d1527c8` is the commit of tag `v2.9.3`
(`git ls-remote`). The preview policy was also run by hand on this
repository's history: the `tests/e2e`-only commit `1719366` skips as a pull
request into `main` and builds as a pull request into `master` or a push; a
`crates` commit and an invalid SHA build.

## Partial: macOS installer previews on hosted Macs at b7f693b (2026-11-24, docs lane, GitHub)

Pull request #3 (`fix/macos-installer-chmod`), run
[45238024508](https://github.com/wnzn/metro/actions/runs/45238024508), at
`b7f693b`. These are **ad hoc signed previews**: not notarized, not an
official release.

- **macOS installer, Intel**
  ([job 137915858965](https://github.com/wnzn/metro/actions/runs/45238024508/job/137915858965))
  **and Apple silicon**
  ([job 137927761044](https://github.com/wnzn/metro/actions/runs/45238024508/job/137927761044),
  attempt 2): each passed install to a path with spaces, a real launch and
  clean shutdown, update and uninstall. On attempt 1 GitHub couldn't
  allocate an ARM64 runner; the retry ran the same revision.
- **Preview downloads verified:** the Apple silicon
  [artifact](https://github.com/wnzn/metro/actions/runs/45238024508/artifacts/14851224042)
  matched GitHub's digest, and its archive's SHA-256 is
  `3ebef6c2a10486686cf0c071f47aaf5cdc2e7d617f6e4bd653fb976536532072`. An
  earlier Intel artifact, from `f3bc1c8`
  ([run 45233193747](https://github.com/wnzn/metro/actions/runs/45233193747/artifacts/14848567927)),
  was verified the same way: SHA-256
  `431281f493df577e6f2a88bc87c3d88486feef42e67e938b42e1f5edebad3419`.
- **Windows** (job `137925913809`): the fixture MSI lifecycle passed,
  through repair, upgrade, refusals, uninstall and a `D:` install. It
  couldn't prove an install by a standard (non-admin) user, so this isn't
  evidence of a final Windows download.
- **Chrome and Rust** (job `137914856904`): passed.

## Partial: self-hosted CI runner stack, hosted preflight and offline image check (2026-11-23, runner CI)

The repository's own CI runner, [ops/github-runner](../ops/github-runner/README.md).
It's unrelated to Metro's Space runners.

**Not yet shown:** the runner registered to `wnzn/metro`, or any job of
`self-hosted-ci.yml` running on the operator's own machine.

**Hosted preflight passed.** Run
[45199814095](https://github.com/wnzn/metro/actions/runs/45199814095), job
`137779964816`, "Runner stack preflight" (`check-runner-stack.yml` →
`ops/github-runner/ci_preflight.py`), was a push to `main` at `7452f6f8d8d2`.
On a disposable GitHub-hosted Linux VM's own Docker, without registering
anything or using any enrollment credential, it checked:

- **Build and start:** both Compose images built, and the stack started
  with the egress proxy healthy. The jobs network is an internal, IPv6-off
  bridge in `isolated` gateway mode.
- **Containment:**
  - both containers run as non-root, with a read-only root filesystem, no
    capabilities and `no-new-privileges`;
  - no published ports, devices or host namespaces;
  - no credential-like environment variables.
- **Mounts:** the effective mounts were only the expected tmpfs and two
  plain Docker-managed local volumes (state and scratch). There were no
  host paths, volume driver options or Docker socket.
- **Network:**
  - GitHub's public Git endpoint was reachable through the proxy;
  - a `CONNECT` to another host was refused (403), and its header didn't
    appear in the logs;
  - direct TCP from the runner to a GitHub address, to a listener on the VM
    itself, and to a LAN-like listener on another bridge was blocked.
- **Tools:** the pinned NSH 0.6.1 and its offline TypeScript 7.0.2 built and
  tested a small fixture app inside the runner, as UID 1001.
- **Persistence:** the runner reported "not registered". A marker file in
  the state volume survived `down` and `up`.

**Earlier, offline, from the runner image:** Metro's desktop app passed
`nsh check`, `nsh build --release` and the focused Shell tests (13/13), with
the network disabled and an empty `HOME`. This was a local run of the
image, not a GitHub job.

## Partial: automatic releases from protected master, branding, and the first GitHub run (2026-11-23, docs lane, local and GitHub)

**No release exists.** Nothing has been signed, notarized or published for
real. The public repository `wnzn/metro` has only `main`, with no `master`
branch and no release.

**Release process in the source:**
- policy and workflow graph: `1b3edab`;
- Windows signing and GHCR: `1cb6094`;
- Sigstore: `2cfdc24`;
- MSI: `f642616` and `6171c75`.

It's described in [deploy.md](deploy.md#ci-and-publishing). Its first real
run needs a protected `master` branch, the macOS secrets and the Windows
signing setup.

**Local checks at `82ff919`** (docs lane, re-run for this record):

| Check | Result |
| --- | --- |
| `python3 -m unittest discover -s packaging` with PyYAML (`CI=1`). Covers the release policy (official master push versus tags, PRs, `main` and manual runs; eligibility states; immutable tag; staged publication with digest checks), the release set, macOS signing with stand-in Apple tools, installers and the workflow graph. | 136 pass. 9 skipped: PowerShell 7 isn't installed here, and the MSI tool tests need `METRO_MSI_TOOLS`. |
| `packaging/release-signatures` tests | 16 pass, 1 skipped |
| `packaging/windows-signing` tests | 33 pass, 1 skipped |
| actionlint 1.7.12 with shellcheck on every workflow | Clean |
| `cargo-notices.py --offline` for both macOS targets with `metro-tui` | Pass (the `block2` notice gap was removed in `1feba10`) |

**Reported by other lanes** (each at its own commit; not re-run here):

| What | Where | Result |
| --- | --- | --- |
| Desktop at the train identity commit (frozen) | `72f60c1` | 352 tests pass |
| Desktop search centering | `f94656b` | 13 focused tests pass; centering checked in the native GTK app |
| Terminal client F7 sidebar toggle, real PTY | `88a058b` | Pass |
| Admin panel | — | 21 Chromium checks and 8 Rust tests pass |
| Backend | `1feba10` | 662 tests pass, 2 ignored; 8 end-to-end scripts pass |

**Design facts these record:**
- Metro's train identity, with the lowercase `metro` wordmark.
- The desktop bundles the Inter font, with its license.
- The MSI's setup window uses Windows' Segoe UI.
- The desktop's Settings, Navigation and Runs panels collapse independently,
  with **Mod+B** and **Mod+Shift+B**.
- The terminal client's **F7** hides or shows its sidebar.

**First GitHub run: `45182697863`.** It was the first hosted run of CI. It
exposed test fixtures that didn't hold up on GitHub's runners, which
separate lanes are fixing (for example `e4f1ce2` and `15e51d8`). **Not
every hosted job has passed yet.** No run has signed, tagged, pushed an
image or published anything.

**Not verified:**
- native Windows: the MSI wizard and `msiexec` checks, and any Windows
  start-up beyond Wine;
- Apple Developer ID signing and notarization;
- Windows Authenticode signing by a real publisher;
- any real vendor account or sign-in;
- a public release, and the images on GHCR.

## Source only: terminal client at 142bc85 (2026-11-23, project manager, local)

`metro-tui` ([terminal.md](terminal.md)) is in the source at `142bc85`. It
isn't in any packaged, tested or released download, and the 57409bb setup
cut below doesn't contain it. Until a standalone build of it is verified,
this entry records only what its committed test covers.

**The committed test.** `crates/tui/tests/pty_qa.py` runs the real Linux
`metro-tui` binary in a pseudo-terminal against a real, isolated
`metro-server`, with two synthetic accounts and a scripted loopback model.
It uses no real vendor or credentials. It checks, in order:

- Opening makes no model call. Only the caller's Spaces are listed; another
  account's Space never appears.
- **F1** opens help. In personal Chat, **F3 Save note** posts with no model
  call, and **Enter** on a question asks the agent with the default model.
- A Chat draft survives moving to a room. In the room, `@bob` alone and a
  plain **F3** post call no model, and **F2** asks with the default.
- **F4** chooses another connection, and **F2** uses exactly it. After that
  connection is deleted, sending is refused with a prompt to choose again:
  the draft stays, nothing is posted, and no model is called. Choosing again
  works.
- A synthetic paired device advertises a model with thinking levels; **F4**
  picks `high`, and the relayed job carries exactly that model and level.
- A draft containing the sign-in token is refused. An escape sequence in a
  message and the tokens never appear in the terminal output.
- Resizing to 6×32 and back redraws, and Ctrl-C quits with the terminal
  restored.
- The hidden `Metro token:` prompt signs in; Ctrl-C at the prompt exits
  cleanly.
- A wrong token shows HTTP 401. A token file with mode 644 is refused
  (`chmod 600`). A redirecting server shows HTTP 302 and isn't followed.
  Proxy variables pointed at the model aren't used.

The terminal client's lane reported this test passing at `142bc85`. No run
receipt is stored with this record, and the docs lane didn't re-run it. The built binary's
`--help` output was checked against the committed source.

**Not covered:** a packaged or released `metro-tui`, native Windows or macOS
terminals, a real vendor or account, and real network conditions.

## Staged: installers and setup at 57409bb (2026-11-23, project manager, local)

A frozen local integration cut, **not a release**. Nothing was published or
signed. Later work (an MSI installer, a terminal Metro client, Authenticode,
notarization, signed manifests or images) is **not** in this cut, and this
cut contains no terminal client.

**Source.** `57409bb309ee4169c9a2dbeae9ead5c81533bbc6`, with installer
scripts as of `fb7cb06`. The backend was gated at `6392af4`, whose
`Cargo.toml`, `Cargo.lock`, `crates/` and `tests/e2e/` are identical to this
cut.

| Check | Result |
| --- | --- |
| `cargo fmt`, build, clippy | Pass |
| Rust tests | **657 passed**, 0 failed, 2 ignored on purpose |
| e2e scripts | 8/8: smoke, threads, settings, device_connector, intelligence, schedules, library, attachments |
| Desktop: `nsh check`, release build | Pass |
| Desktop `nsh test` | **351/351**, with 0 failures, errors or skips. The first full run had one isolated timeout (a QuickPreview test whose held read didn't park in time); the recheck passed in full |

**Local files** (not release files), in `dist/setup-testing/57409bb309ee/`
with `SHA256SUMS` (all nine checked OK):

| File | SHA-256 |
| --- | --- |
| `Metro-0.2.0-setup-57409bb309ee-linux-x64.tar.gz` | `3937e9dda2148f0d98dcb90f7b87e40adc9eeb62496b44e52abdabefa8ed32cc` |
| `Metro-0.2.0-setup-57409bb309ee-windows-x64.zip` (unsigned) | `fc9680aacab6679fad7903d78247f05f1481a01fe47175c727c4eef3df20771b` |
| `Metro-runner-0.2.0-setup-57409bb309ee-linux-x64.tar.gz` | `6bb85470ae2c4d4ebefd4559516761b49e275acbc77914ca057ec5d4aed64a2c` |

The six installer scripts sit beside them, each with its own checksum.

**Linux installers, in isolated containers, on Debian 13 and CachyOS:**

- `install-metro-linux.sh` and `install-metro-runner-linux.sh` both
  installed, and upgraded a distinct baseline built from the same source to
  the final archive. Both refused a wrong archive checksum.
- An opted-out runner choice survived the upgrade. A default install left
  the empty `enable-local-runner` marker. Removal kept data and
  configuration.
- The runner's `doctor` refused a token file with the wrong mode, and
  passed once that was fixed. The user service unit was written and
  removed; it wasn't started.
- **CachyOS** (a real CachyOS userland image, real offline `pacman` with a
  trusted signature policy): with WebKitGTK missing, the installer refused
  and changed no packages. With `--install-deps` it installed the resolved
  package transaction, and afterwards `ldd` found no missing libraries.
  **No GUI was run on CachyOS.**

**Native GTK on Debian 13, from the installed archive.** This ran in an
isolated read-only root with loopback-only networking, scripted model
endpoints and synthetic accounts:

- **The desktop's own runner, opted out:** no registered runner existed,
  before or after a restart.
- **The desktop's own runner, opted in** (installed with `--local-runner`):
  - **Settings › Runners** showed it online, with the same runner ID after
    a restart. Two new Spaces started unattached.
  - Attaching it to one Space left the other unattached, until it too was
    attached explicitly. Each Space's Demo and agent `workspace.read` saw
    only that Space's own files.
  - After it was detached from one Space, that Space got no new tool
    result, while the other still did.
  - A second bundled server reached from the same app got no local runner.
  - Another member couldn't attach the owner's personal runner (403 in the
    owner's Space, 404 in their own).
- **Monitoring in Settings:** the This computer, My Chat, Space and
  Organization sources were each selected natively.
  - A failed turn in one Space appeared only in that Space, not in the
    other Space or in My Chat.
  - A failed personal turn appeared only in its owner's My Chat.
  - Member and outsider reads of a Space were refused (403, 404), and
    asking for another person's personal events was rejected.
  - Both kinds of event survived a server restart.
- **Composer:**
  - **Grouping:** the grouped picker showed Default on its own, then two
    providers offering the same model ID. A connection that couldn't be used
    had nothing to pick.
  - **Personal Chat:** the first question showed **Ask agent ↵** with the
    closed model and effort, and Return sent exactly that pair.
  - **Space:** `@bob` alone called no model, and an explicit Ask used the
    Space's own choice.
- **Not covered:** sign-out and reopen, an injected device-local failure,
  screen-reader labels, a real vendor or sign-in, and any CachyOS, Windows or
  macOS GUI.

**Windows, under Wine 10 only:**

- **Launch:** the unsigned packaged launcher started its renderer window,
  and the bundled server answered health and sign-in checks (401 anonymous,
  200 for a synthetic account).
- **Not verified:** the window was blank, and Wine reported no window icon.
  `install-metro-windows.ps1` wasn't run on Windows. Native Windows
  rendering, the Windows installer, and the container scripts
  (`metro-runner-container.sh` / `.ps1`) are **not verified** by this cut.

**Optional: container images, local only.** Server and runner images for
`linux/amd64` were built from `git archive` of this commit with the
repository's Dockerfile, under local test tags:

- server manifest `sha256:97fb387d0cab39324eb63d7ec6fe89794f7ad15db0b22724a5e3115fccc8010b`
- runner manifest `sha256:7ee310575dc5fdf13133181fdf9c799a2008d245e7e7d782ea483ea20a2fa373`

One smoke on those exact digests passed:

- health check, and 401 for an anonymous caller, an unknown bearer and an
  unknown runner;
- 404 for outsiders;
- claim and result for a dedicated runner, and for a registered runner in
  two Spaces;
- non-root users, a read-only root filesystem and no published ports;
- no token in `docker inspect` or the logs.

They're **unsigned and not in GHCR**. The saved bundle hasn't been
smoked again since, arm64 wasn't covered here, and the media image wasn't
included.

**Receipts**, kept locally outside git:
- `dist/ui-review/setup/57409bb309ee/` (`backend.json`, `desktop.json`,
  `nsh.junit.xml`, `installer-qa.txt`, and the native GTK `RECEIPT.md` and
  assertions);
- `dist/docker-testing/`.

Earlier failed QA attempts are kept in those receipts; host temporary-file
exhaustion forced private isolated runtimes, and no frozen binary or
installer script was changed. The results above are the record.

## Staged: composer models and Ask from personal Chat at 28ba4e2 (2026-11-23, project manager, local)

A local test cut, **not a release**. Nothing was published, and the
[Metro 0.2.0 local release](#final-metro-020-local-release-prepublication-2026-11-23-project-manager-local)
is still the current final local result. Later commits (`797eac1` server
monitoring, `322dc9b` desktop operational errors) aren't part of this cut
and have no acceptance run recorded yet.

**Source.** `28ba4e2257612bc759b4ebff1d01ecce3a473833`. It adds, since the
`ba07516` cut below: a model and thinking-level picker for agent turns
(`004dd2b`), approved models advertised by device connections (`0c4dcfd`),
owner-fetched model lists (`82fd49e`, `67194e7`), the ChatGPT effort and
recovery fixes (`4824d8a`), packaged icons (`32a46a0`, `b8ac423`), **Ask
agent** from personal Chat with the chosen model and effort (`c836eee`),
catalog invalidation on credential changes (`2352b86`), server validation
of per-turn choices in personal Chat and Spaces (`e97c850`), and opening
provider setup when the composer has no models (`28ba4e2`).

| Check | Result |
| --- | --- |
| `cargo fmt`, clippy, build | Pass |
| Rust tests | **610 passed**, 0 failed, 2 ignored on purpose |
| e2e scripts, original and unmodified | 8/8: smoke, threads, settings, device_connector, intelligence, schedules, library, attachments |
| Desktop: `nsh check`, release build, `nsh test` | Pass; **323/323**, with 0 failures or skips |
| Packaging tests | 10/10 |

**Test archives** (local only, not release files):
`Metro-0.2.0-composer-28ba4e225761-linux-x64.tar.gz`, SHA-256
`1badb0ab278c4f2a70bf86a205fa40d50996638c36db12ac9cca6fbb1d9a9f65`, and
`Metro-0.2.0-composer-28ba4e225761-windows-x64.zip`, SHA-256
`89ef0dbfdc5821b999bbc055aa23f41fe5012252d373308e17be27971bc2c585`.
The package audit found the Windows launcher's icon group with all seven
frames (16 to 256 px), and on Linux the optional `./install-desktop
--install` wrote a menu entry with the exact launcher, icon and window
class, plus a 256×256 PNG and a scalable SVG; `--uninstall` removed them.

**Native Linux GTK, the packaged archive (Xvfb, read-only install).** The
archive's own `metro-server` ran with a fresh database and disposable
synthetic accounts. All 70 `RELEASE.json` digests matched. The chat model
was a scripted loopback `device-openai-oauth` worker that advertised two
approved model IDs; no vendor endpoint or account was used.

| What the tester did | What was checked |
| --- | --- |
| Opened **Model** in an empty personal Chat | The picker showed **Default** and the two approved models; five thinking levels for the chosen one |
| Typed a question with no mention, pressed Return | **Ask agent ↵** was the visible primary action. Exactly one model call, with the chosen model and level; the reply appeared in Chat |
| Pressed **Save note** | The note was saved; no model call |
| Attached a file through the native GTK chooser, sent with an empty message | One attachment saved; no model call |
| In a Space lobby, chose another model and `low`, sent `@bob` only | Bob was the recipient; no model call |
| Pressed **Ask agent** on an unmentioned Space question | Exactly one model call, with that Space's chosen model and level; the reply appeared in the room |

Exactly two model calls were seen over the whole run. Screenshots of the
picker, the levels, the Ask default and both replies were reviewed and
contain only synthetic content.

**Windows, under Wine 10 on Linux only.**

- The packaged launcher started its UI child, the window was titled Metro,
  and the bundled server answered health checks (`/v1/me` 401 without a
  token, 200 with a fixture token).
- The window rendered **blank** under Wine, and Wine reported no live window
  icon. Native Windows rendering, input and icons are **not verified**.
- The Windows connector's ChatGPT sign-in tests (Windows x64 executables run
  under Wine against scripted loopback services, no network): 10 passed, 0
  failed. They covered first chat, history, tool-call continuation,
  `high`/`max` effort, 16 vendor error cases with bounded messages,
  token refresh under eight concurrent callers, and plan-scope loss. No real
  account, live entitlement or effort support was tested.

**Not covered:** native Windows, macOS, a real vendor account or sign-in,
the real connector's HTTP translation in the native GUI run (covered only by
`crates/connector/tests/openai_oauth.rs`), and publishing.

**Receipts** are kept locally, outside git, under
`dist/ui-review/composer-models/28ba4e225761/` (`verification.json`,
`backend-receipt.json`, `package-audit.log`, `windows-inference.md`,
`native/RECEIPT.md`, JUnit report). The results above are the record; the
files are supporting evidence only.

## Staged: startup server, attachments and device sign-in at ba07516 (2026-11-23, project manager, local)

A local test cut, **not a release**. Nothing was published.

**Source.** `ba07516bec5c0a02bb046be9d061d64f14693e4e` (desktop), with the
server identical to `f82b606`.

| Check | Result |
| --- | --- |
| Rust tests | **596 passed**, 2 ignored on purpose |
| e2e scripts | 8/8, as above |
| Packaging tests | 8/8; startup integration checks 6/6 |
| Desktop: `nsh check`, release build, `nsh test` | Pass; **311/311**, with 0 failures or skips |

Test archives (local only): Windows
`Metro-0.2.0-dev-ba07516bec5c-windows-x64.zip`, SHA-256
`2260ba0b0e9b1b7fc5f2dcd241edba7934f0d0009eb28100c25b57cab2d0f904`; Linux
`Metro-0.2.0-dev-ba07516bec5c-linux-x64.tar.gz`, SHA-256
`a3d7923df029c3fd79c104f49d7ba6bdc00ba14bbbaecd215d43c6442d3a8be5`.

**Native Linux GTK, the packaged archive:**

- **Startup server.** Thirteen steps were each confirmed on screen: a clean
  start, adding a server, ordinary use, setting a remote server as the
  startup server, a cold start into it, a refused token, signing in, reset,
  reset to local, forgetting a server, after forgetting, signing out, and a
  stable restart. *Observation:* after a server is forgotten, its address can
  still prefill the generic connection form. It's gone from the saved list and
  the startup choice, and no request went to it.
- **Attachments** (three synthetic accounts: owner, member, outsider; real
  GTK file chooser, real file bytes):
  - Cancelling the chooser, or removing a staged file, uploaded nothing.
  - In personal Chat and in a Space room, a staged file uploaded only on
    **Send**, then opened from the transcript in the Library with the right
    badge and the exact bytes.
  - The Library listed exactly those two files. The private file returned 200
    to its owner and 404 to the others; the Space file 200 to both members
    and 404 to the outsider.
- **ChatGPT sign-in with no network** (Docker `--network none`, no vendor
  account): after consent and pairing, **Sign in with ChatGPT** showed an
  actionable error that no browser was opened and the sign-in service was
  unreachable, with **Retry ChatGPT sign-in**. Retry returned the same error.
  No link or token appeared. Minor copy issue: the banner repeats that no
  browser opened. Waiting, cancel and a successful sign-in weren't
  exercised.

**Windows, under Wine 10 only:** fixture sign-in retry flows (25 completed)
and concurrent credential writes (50 successes). Native Windows and a live
vendor sign-in are **not verified**.

**Receipts** are kept locally, outside git, under
`dist/ui-review/ux-round2/ba07516bec5c/`.

## Staged: Windows test build at 5035545 (2026-11-23, project manager, local)

A local, unreleased Windows x64 test build, for handing off to Windows
testing. It isn't the 0.2.0 release or a replacement for it, and nothing
was published.

- **Source:** `5035545809ab7fb4edfb7444ca12e3c8c02ae949`, with the UI as of
  `8fcc32f`.
- **Build:** the Windows UI from the official, pinned NSH 0.6.1. The
  launcher and server were cross-built locally for Windows GNU x64.
- **File:** `Metro-0.2.0-dev-5035545-windows-x64.zip`, SHA-256
  `1317054d81c9cec245f58ca8445969554e425a93c16eac40c1c6c8be339625c2`, in
  `dist/windows-testing/` (local, not in git).
- **Checked:**
  - All 66 hashes in `RELEASE.json` matched.
  - The archive's integrity was checked.
  - All three executables are PE32+ x64.
- **Under Wine:**
  - The launcher started the renderer and opened a window.
  - The server's health check and test sign-in worked.
  - The window was blank, so real Windows rendering, sign-in and **Use
    locally** are **not** verified.
- **Packager fix `0467371`:** it lists the four production icon modules
  explicitly. 8/8 packaging tests passed, and the ZIP it emitted was
  byte-identical.

## Staged: quick search as you type (2026-11-23, project manager, local)

Local and unreleased: nothing was packaged or published. It doesn't change
the [UI gate below](#staged-desktop-ui-gate-after-020-2026-11-23-project-manager-local)
or the 0.2.0 local release.

**Source.** App `95a3d2b9064a18818681b2a6bf55d932a52b6679`. Server `d6513c6`.

| Check | Result |
| --- | --- |
| Desktop, from a clean archive of `95a3d2b`: NSH 0.6.1 `nsh check`, `nsh build --release` and `nsh test` | Check and build passed. Tests: **272/272**, with 0 failures, errors or skips. JUnit report `verified-95a3d2b90.junit.xml` |
| Follow-up `05fbfb6` (test only) | 17/17 targeted tests |
| `cargo test --locked -p metro-server` at `d6513c6`, 19 nonempty test binaries | **342/342** |

**Native Linux GTK, dark and light, at 1200×1000 and a compact 1200×560.**
Chat, System One, embeddings and media connections were all configured.

- **No model while typing:**
  - Typing made zero model requests. The app sent only
    `POST /v1/search/quick`, and authenticated reads of the preview bytes.
  - The only model calls came from the full search, after **More results**.
- **Previews:**
  - A text file showed an excerpt, and a PNG showed the image itself.
  - A WAV showed its real waveform and duration.
  - Video first frames were checked against the frame's own pixels: VP8
    WebM, and H.264 MP4 on a host with the H.264 codec.
  - Without that codec, H.264 MP4 showed its fallback message instead.
  - An unindexed Markdown file was found by its name.
- **Keyboard and pointer:**
  - In the 560 px window, the arrow keys scrolled all the way down to **More
    results**.
  - Return opened the selected row, or full Search from More.
  - Ctrl+K focused the box. Escape closed the list and kept the query.
  - Clicking a file row opened it. Clicking More opened full Search.
- **Scope and access:**
  - The Library's **Show files from** scope carried into quick results and
    More.
  - Only current memberships were searched. Bob's private file never
    appeared.
- **Not a blocker:** after a file is opened, the top box shows the
  Library's own search field. The quick query isn't carried over.

**Automated tests only:**

- MP3 title and artist tags, shown with no waveform.
- Bounded DOCX text, shown where it can be read.
- The PDF fallback.

**Not verified:**

- Windows and macOS.
- Video on a host without the codec, beyond its fallback.
- General audio or video playback and seeking.

**Receipt:** stored outside the gallery.

- `~/.cache/metro-release/quick-search/final-95a3d2b/RESULTS.txt`, with 32
  delivered PNGs listed in `deliver/SHA256SUMS`.
- The app and server gate logs are in the sibling `checks/` directory.

**Follow-up: compact result rows, `70587cb`.** This followed feedback that
the rows had too much text and nothing to tell them apart. The results
above are for `95a3d2b`. At `70587cb`, the project manager checked the
following natively in NSH/GTK:

- **1200×1000, both themes, the same `mixer` query:**
  - All six file rows fit together. At `95a3d2b`, four were fully visible.
  - Each row has one line under its name and an extension badge, instead
    of repeating the location.
  - Files and messages are listed separately.
- **780×560:** Down reached **More results** in the footer, and Return
  opened full Search.
- **Model calls:** both typing runs with mixed results made zero model
  requests.
- **Tests:** `nsh check` passed. QuickSearch 19/19, QuickPreview 5/5 and the
  preview-data tests 11/11.
- **Evidence:**
  - `dist/ui-review/quick-search/rows/` (`before-after-dark.png`,
    `before-after-light.png`, `compact-more.png`);
  - `~/.cache/metro-release/quick-search/row-review/70587cb9b0bf/`.
- Only the targeted tests above are recorded for `70587cb`. It isn't a
  release, and nothing was packaged.

**Follow-up: extension badges, `8fcc32f`** (after `031d20c`; a visual
change only). Extension badges now use semantic colors, and the rows have
even gutters. The project manager checked this natively in dark and light
at 1200×1000, and **More results** could still be reached from the
keyboard at 780×560. At 720×560 a long `.MARKDOWN` badge stayed whole on
one line, next to a shortened title. The eight new badge text/fill pairs
per theme measured at least 5.60:1 in light and 9.78:1 in dark. `nsh
check` and the release build passed, and QuickSearch passed 20/20.
Evidence: `dist/ui-review/quick-search/extensions/` (`dark.png`,
`light.png`). Nothing was released or packaged.

## Staged: desktop UI gate after 0.2.0 (2026-11-23, project manager, local)

A **UI gate** on unreleased source after tag `v0.2.0`, reported by the
project manager. It isn't a release, and nothing was published.
The [Metro 0.2.0 local release](#final-metro-020-local-release-prepublication-2026-11-23-project-manager-local)
below is still the current final local result. Packaging tests and the e2e
scripts weren't rerun for this gate.

**Source.** The final application gate ran on commit
`13c80b4f682243ead3e13fc68d8a0db17e18fcf5`, 50 commits after `v0.2.0`. Its
last two app changes were:

- `ffd8978`: below 1100 points, Chat's agent runs open in an **Agent runs**
  dialog from a **Runs** button, instead of as a side panel.
- `13c80b4`: the composer hint stays beside **Ask agent** and **Send**, and
  keeps its full wording as its accessible label.

The narrow-Chat and composer checks below ran natively on this source
(renderer `3e3b6c7d85c5`). The other native reviews ran on the earlier
renderer builds named there.

| Check | Result |
| --- | --- |
| Desktop, from a clean `git archive` of `13c80b4`: NSH 0.6.1 `nsh test` | **234/234**, with zero errors, failures or skips. JUnit report `verified-13c80b4f6.junit.xml` |
| NSH 0.6.1 `nsh check` and `nsh build --release` at `13c80b4` | Pass |
| Focused tests in that run | An intact Chat draft, dispatch, and closing the runs dialog (Close, Escape) across widths. The composer hint at narrow widths. |
| Earlier desktop gates | `df5b8fa`: 229/229 from a clean `git archive`, with JUnit `final-nsh-test-df5b8fa.junit.xml`, plus check and release compile. `58a1b7d`: 229/229, plus check and release build. |
| `cargo test --locked -p metro-server`, all 18 nonempty server test binaries | 331/331 |
| Admin web page: browser checks, and color-contrast pairs | 19/19 checks, 68 pairs (the receipt doesn't break the pairs down further) |

**Native Linux GTK review, dark and light themes.** Renderer build ids are
recorded as reported.

- **Discovery states**, renderer `22cc1af11473`:
  - Search runs only on **Return**, with nothing sent while typing.
  - The scope and file-type chips work.
  - Zero-similarity results are folded under **Less relevant**.
  - The routing badge shows its percentage, with the egress warning,
    including that the connection's owner can see the requests and may pay
    for them.
  - Scheduled-task cards work, with **History** in the **More** menu.
- **Library home and its row menu**, recaptured at renderer `0d658fc75fcb`:
  - Library is a standalone page, and the media model is chosen in
    **Settings › Model providers**.
  - Each row shows **Open**. **More** holds **Index** or **Index again**,
    and **Delete…** for the uploader or the Space owner. Delete still asks
    first (`234c1fa`).
- **Audio**, renderer `9dfcee25955e` (`952bcee`):
  - The match band is shown, with one play/pause control. Its first play
    asks to start at the match unless the timeline was moved.
  - The host loaded the file's real duration (0:10).
  - In headless GTK it reported no playback times or progress. Native audio
    and video playback and seeking are **not** verified.
- **Mentions.** Typing `@` with the caret, then Down and Return, selected a
  mention without sending. The plain message then posted to Bob.
- **Scheduled task form**, an extra check at 1200×1000 in both themes:
  - New and Edit were each scrolled down to the enabled **Active**,
    **Cancel**, and **Schedule** or **Save** controls.
  - Pressing **Cancel** saved no task.

**Not covered by this gate:**

- Native Windows and macOS.
- Real vendor accounts or vendor sign-ins: none were used or recorded.
- Pretrained `google/embeddinggemma-2` ranking quality.
- The native file picker for Library uploads.
- Packaging and the e2e scripts.

**Core and compact windows: native GTK, before and after, dark and light.**

- **What was compared:** native `v0.2.0` (`3787f01`) against renderer
  `22cc1af11473`, which is the frozen app at `58a1b7d`. Both ran against a
  real local `metro-server` and `metro-runner`, with a scripted model.
- **The receipt:** stored outside the gallery, under
  `~/.cache/metro-release/desktop-refresh/core/`, with `HANDOFF.md` and
  `PROVENANCE.md`.
  - The final `SHA256SUMS.png.txt` lists 170 PNGs.
  - The capture tooling was at commit `d5258b3` for the first run, and at
    `3289ff8` and `6bd2d3b` for the narrow rerun.
  - The handoff archive is separate from the 0.2.0 artifacts.
- **Checked:** personal Chat with populated agent runs, a populated room with
  agent activity, a thread, **Model providers** with its setup flows,
  **Appearance**, and the account chrome.
  - Chat's **New task**: the dispatch form open, and the narrow layout with
    the runs hidden.
- **Profile Save at 780×810, the same geometry in both themes:**
  - Before, Save showed 23 px of its 36 px and was clipped by the modal.
  - After, all 36 px show, with 12 px of clearance.
  - The fixture was edited and Save pressed without scrolling.
  - In all four runs the server returned the intended display name and bio.
  - Afterwards, *Profile saved* showed in the fixed footer.
- **Admin page:** 19/19 browser checks and 68/68 measured contrast pairs
  (text, controls and focus), with the admin CSS at `2869c6b`. No auth or
  script changes.
- **Narrow Chat and composer: found, fixed, rechecked.**
  - Renderer `22cc1af11473` showed Chat's composer and header overlapping
    the runs at 860 px wide, with the runs shown.
  - The rerun used source `13c80b4`, renderer `3e3b6c7d85c5`, with 126/126
    runtime inputs matched, the same local server and runner, and both
    themes.
    - At 860, **Runs** opened in its own dialog. **Escape** and **Close**
      each closed it, and returned the typed draft unchanged.
    - At 1200, Chat's runs stayed inline.
    - The focused composer hint cleared **Ask agent** and **Send** in Chat
      at 1200 and 860, and in a room at 860 and 720.
    - At 720 the room hint wraps to two lines and ends with an ellipsis.
      Its accessible label keeps the full wording.
- **Still open, minor:**
    - The provider dropdown opens upward over the Documents & media heading.
      0.2.0 does the same.
    - After **Make default**, the media **Save** sits under the fixed footer
      notice until you scroll.

This gate makes no release, packaging or e2e claims.

## Final: Metro 0.2.0 local release, prepublication (2026-11-23, project manager, local)

This is the **current final local result** for Metro 0.2.0. It's local and
prepublication only: no public release or published image is recorded here.
The staged entries below it are historical.

**Source.** Tag `v0.2.0`, commit `1397150b8b0af43b4cc2cf26f25d8604d40bf6d3`,
tree `b9d5a2ea396b872df6b52374d5d040e66580e5f3`.

**Artifacts.** These are in `dist/release/`, with `SHA256SUMS` and a `.sha256`
beside each archive. `sha256sum -c SHA256SUMS` passed.

| File | Bytes | SHA-256 |
| --- | --- | --- |
| `Metro-0.2.0-windows-x64.zip` | 43927434 | `26ffbb2ecc0db1181ba8d46e43356723098a61824b3a983cda02f4d3303e6b22` |
| `Metro-0.2.0-linux-x64.tar.gz` | 37535065 | `6501380b6d7d7505ad842052f118f0f192c7907461f29e74f709807ae943a075` |
| `Metro-runner-0.2.0-linux-x64.tar.gz` | 1424758 | `192f0767e0b8011ca629a3df8aaac3eaf5254ecb8cfae9c709034932f6724fb3` |

| Check | Result |
| --- | --- |
| Embedded UI bundle | SHA-256 `c51c546b69782c12277855e481e0223dc0d58daceca0e39c9392f3cb959e8f9a`, reproduced independently |
| Archives | Architecture, notices, manifests and sidecar checksums all checked |
| `cargo fmt` and strict clippy | Clean |
| Rust tests (default parallel run) | 570 passed, 0 failed, 2 ignored on purpose |
| Earlier intermittent instance-lock assertion | Resolved: it was isolated in a dedicated test, with no production code changed |
| Desktop: pinned NSH `nsh check`, `nsh test` (`TZ=Asia/Manila`) and release build | Check passed, 159/159 tests passed, build green |
| End-to-end scripts (all 7) and packaging tests (6) | Passed |

**Fixed in this source, and covered by automated tests:**

- **Status badges.** The device connection and local tools now move from
  **Starting…** to *Serving from here* or *On* in the open view, with no
  need to reopen it. Pending updates are cancelled on sign-out and when the
  view closes.
- **Sidebar footer.** It shows the hosted local workspace by checking
  `onLocalWorkspace()`, never by guessing from a loopback address, so a
  manual port forward shows its actual URL.

**Linux GUI, from earlier receipts.** The packaged Linux GUI checks are
recorded in the
[UI validation](#staged-linux-ui-validation-of-the-in-app-device-flow-and-local-tools-2026-11-22-qa-local)
and [candidate QA](#staged-020-linux-candidate-local-workspace-and-desktop-qa-2026-11-22-project-manager-local)
entries below. They covered local persistence, avatars, timestamps, in-app
device setup with consent and an approved-model list, three **Ask agent**
replies from a fixture, and per-Space local tools. They weren't all run
again on these final archives. The *Dedicated token* and *Runner attached*
controls seen there are recorded as an observation only, not as something
this release changes.

**Windows, under Wine on Linux.** The polished ZIP (`26ff…`) isn't
byte-identical to the earlier one (`d836…`), because the renderer bytecode
was refreshed. Its native launcher, renderer host and `metro-server.exe`
are, though: they're the same verified bytes. Those checks were run with
the earlier ZIP. Its packaged launcher opened a Metro window, and its
server answered the health check. Over the API of that byte-identical
`metro-server.exe`:

- a managed user was created, and their token got `401` after it was
  revoked;
- the user's display name and bio were saved;
- a PNG avatar was uploaded and read back;
- a personal Chat message's UTC timestamp came back unchanged.

**Not covered:** the native Windows GUI, macOS, any real vendor account or
sign-in, and publishing on GitHub Releases or GHCR.

## Staged: Linux UI validation of the in-app device flow and local tools (2026-11-22, QA, local)

> **Historical.** Superseded by the [Metro 0.2.0 local release](#final-metro-020-local-release-prepublication-2026-11-23-project-manager-local).

A separate **internal UI-validation build** of the Linux desktop archive
(SHA-256 `a8d951da5cc280c8be0a288dbc1bd4aa6fe41cae1d133f453e8691a2922b019a`).
It isn't the 0.2.0 artifact recorded below, which is unchanged, and it isn't
a release. QA ran the archive's own launcher, packaged app and bundled
server in Debian 13 under GTK and Xvfb, with no network. Nothing was built
or swapped in, and neither the connector CLI nor a separate runner was
used. The only model endpoint was a local test fixture, never an external
provider.

**Device connection, in the app.**

1. **Use locally** opened a private workspace with no account or token.
2. **Settings → Model providers** showed vendor marks, and a single OpenAI
   row with its three ways to connect.
3. Choosing a self-hosted endpoint on this computer, then **Connect this
   device**, used the built-in wizard. Pairing stayed disabled until
   consent was given. No device token was shown, entered or copied, and
   the old `metro-connector` hand-off didn't appear.
4. The endpoint listed two models, and only one was approved. **Test
   connection** succeeded and listed only that model.
5. With it as the default chat model, **Ask agent** replied in a Space, in
   the Space again after signing out and back in (with no key re-entered),
   and in personal Chat after a full exit and relaunch. The fixture saw 2
   list requests, 3 completions, no rejected auth and no unexpected
   requests. Plain messages didn't call the model.
6. After sign-out there was no traffic to the fixture. On close, every
   Metro process and listener ended.
7. In the places sampled (process arguments, launcher and app output, the
   server log), no fixture key or recognizable token appeared. This was a
   sampled check, not a proof.

**Local tools.** QA turned them on from **Manage** for Space A, then
separately for Space B, with no runner installed or attached. Each Space's
`workspace.list` demo showed only its own marker file, with neither the
sibling Space's marker nor one outside the folder, and no model was called.
Turning B off returned it to *Off* and *No runner*, and A stayed *On*.
`workspace.read` and arbitrary paths weren't exercised.

**Still working after a relaunch:** the Settings window, avatar upload
through the GTK file picker, and workspace restore (room, timestamped
message, display name, avatar and demo history). Search asked for an
embeddings connection, and retrieval wasn't tested.

**What QA observed in this build.**

- A **Starting…** badge could stay shown after the device connection or
  local tools were already working, until **Manage connection** or the
  Space's Manage page was reopened. No workflow was blocked. Fixes are
  under way.
- In local mode, the sidebar showed the plain loopback address instead of
  an *On this computer* label. This build's source still placed the label
  without checking for the hosted local workspace. The final source
  (`b3ef2da…`, below) checks for it, and a test covers a manual localhost
  port forward. The final build hasn't been re-checked in the GUI.
- Under the local tools card, the Manage page also showed the general
  *Dedicated token* and *Rotate token* controls, and the room said *Runner
  attached*. No token was revealed. This is recorded as an observation
  only.

Out of scope: Windows, real vendor sign-in (OAuth), abnormal child-process
exit, and socket activation.

## Staged: final local 0.2.0 test artifacts (2026-11-22, project manager, local)

> **Historical.** Superseded by the [Metro 0.2.0 local release](#final-metro-020-local-release-prepublication-2026-11-23-project-manager-local).

These are **local test artifacts, not a release.** No binaries have been
published on GitHub, and pushing the source is still waiting on the
maintainer. At the time, the final gate stayed open: one parallel test was
intermittent (below), and full GUI QA was still under way. Both were
resolved for the final release.

**Source.** Commit `b3ef2dae69208af91a0f1ddfb2056207141c6b31`, tree
`b6499d58a0ca28e2f7548a1a4ab1f190bfea3b86`.

| Archive | SHA-256 |
| --- | --- |
| Windows desktop ZIP | `d836d6d0983121389a92e70641f4f4b2cb3fc24dedb3002b2d0844224b3df440` |
| Linux desktop archive | `7ab0b0f128848d2409669c736bd1643883fa9a94f35e719ef3e91eaf97449bc1` |
| Linux runner archive | `192f0767e0b8011ca629a3df8aaac3eaf5254ecb8cfae9c709034932f6724fb3` |

| Check | Result |
| --- | --- |
| Archive contents | Every manifest file verified, including the 21 vendor marks and their notices |
| Release JS bytecode | Exactly matches an independent build from source |
| Desktop `nsh test` (`TZ=Asia/Manila`) | 155/155 passed |
| Packaging unit tests | 6 passed |
| Linux launcher | Socket ownership, restart after a crash, current-bearer handling and shutdown verified |
| Windows archive, under Wine on Linux | The window opened and the bundled server answered `/healthz`. No native Windows run. |
| Windows server executable from this ZIP (`d836…`), under Wine, over its API | Passed: an admin created a managed account; that account set its display name and bio, uploaded a PNG avatar and read it back, and posted a personal message whose stored UTC time came back intact; after the admin revoked the token, it got `401`. No Windows GUI interaction and no production vendor. |
| Linux runner archive, on Debian 12 | A real confined workspace read passed |
| `cargo fmt` and strict clippy | Passed |
| Full parallel Rust gate | **Not clean yet:** one intermittent assertion (an immediate re-acquisition in the exclusive-lock test). Run alone, the affected connector library passed 61/61. Still being investigated. |
| End-to-end scripts on the integrated code | All 7 passed |
| Shared dev service, updated | `/admin`, `/v1/me`, `/v1/me/profile` and `/v1/admin/users` verified; an anonymous profile request got `401` |

Still under way: full GUI QA of the in-app device and model flow, and of
local tools, without any CLI.

## Staged: 0.2.0 Linux candidate, local workspace and desktop QA (2026-11-22, project manager, local)

> **Historical.** Superseded by the [Metro 0.2.0 local release](#final-metro-020-local-release-prepublication-2026-11-23-project-manager-local).

Staged evidence for a **locally built 0.2.0 candidate**, reported by the
project manager. It isn't a release: nothing has been published. It isn't
the final gate either, which stays pending until the integration receipts,
with the latest full test counts, are in.

**Linux candidate GUI QA.** The real packaged launcher and bundled server
ran in Debian 13 under Xvfb, as a non-root user, from a read-only install,
with Docker `--network none`.

1. **Open local workspace** opened a local workspace with no token and no
   remote service.
2. QA created a Space and posted a message, which showed its time.
3. In **Settings** (a window over the app) → **Profile**, QA saved a
   display name and a synthetic PNG avatar chosen through the **native
   Linux file picker**. The avatar showed next to the message.
4. QA saw the centered search box at the top, and the vendor-grouped
   provider dropdown: OpenAI is one row, with three ways to connect.
5. After quitting, every listener and child process had ended.
6. On reopening, the same local room, the message, and the saved profile
   and avatar were all still there
   ([screenshot](screenshots/local-workspace-relaunch.png),
   [profile](screenshots/settings-profile.png)). Both screenshots were
   checked for sanitization.

| Item | Result |
| --- | --- |
| In-app device bridge with the bundled server, in a separate packaged smoke | Passed |
| Device-connection setup screens in the candidate | Still show older `metro-connector` wording; being fixed. In-app device setup wasn't verified end to end in this candidate; it later was, against a local test endpoint ([above](#staged-linux-ui-validation-of-the-in-app-device-flow-and-local-tools-2026-11-22-qa-local)). |
| Windows x64 candidate, under Wine on Linux | A Metro window started and the bundled server answered `/healthz` with `200`. No check of drawing or input, and no native Windows run. |
| Live dev service | The new profile and timestamp read endpoints were checked |
| Rust integration tests | 566 passed, 0 failed, 2 ignored on purpose |
| Strict clippy | Passed |
| `tests/e2e/smoke.py` and `tests/e2e/threads.py` against the server built with timestamps and profiles | Passed |

**Later receipts, same day.** The final local button label is **Use
locally** (accessible id `open-local`), replacing the candidate's *Open
local workspace*, which the steps above record as seen. Provider marks are
21 bundled monochrome SVGs (`@lobehub/icons-static-svg`, MIT), with no
network fetch, and names and sign-in options are grouped by vendor. The
conversion of message times to the local time zone is verified by tests
with `Intl` time zones. The packaged pairing buttons are now **Connect this
device** and **Manage connection**. An end-to-end proof of pairing was
pending at this point; it was later recorded against a local test endpoint
([above](#staged-linux-ui-validation-of-the-in-app-device-flow-and-local-tools-2026-11-22-qa-local)). All seven current end-to-end scripts
(`smoke.py`, `threads.py`, `settings.py`, `device_connector.py`,
`intelligence.py`, `schedules.py`, `library.py`) passed in an independent
run. Local tools (read-only
`workspace.list` and `workspace.read`, per Space) are covered by the app's
and server's tests but aren't in a recorded packaged GUI run.

Not covered: native Windows, macOS, any real vendor account, and Documents
& media in the candidate. The native file picker was verified for the
profile picture only, not for library uploads, and audio and video playback
and seeking weren't tested.

## Staged: admin panel, local accounts, runner package and release targets (2026-11-22, project manager, local and dev service)

> **Historical.** Superseded by the [Metro 0.2.0 local release](#final-metro-020-local-release-prepublication-2026-11-23-project-manager-local).

Staged evidence, reported by the project manager. It isn't a final gate.
The next final gate is still pending integration receipts, and the gate
below stays the current one until then. **Nothing has been released.**

| Item | Result |
| --- | --- |
| Backend tests covering the current admin routes (`/admin`, `/` redirect, `GET\|POST /v1/admin/users`, `PATCH /v1/admin/users/{id}`, `POST\|DELETE /v1/admin/users/{id}/token`) | 302 passed |
| Admin panel browser suite, against a mock server | 19/19 passed |
| Admin panel in headless Chromium against a real `metro-server` with a throwaway, isolated database | Passed: see below |
| Dev service deployment | `/admin` and its assets served; an authenticated admin JSON request succeeded; an anonymous one got `401`. Checked through the service's own bound dev address, which these docs don't name. |
| Prebuilt Linux runner package | Assembled locally, checksum validated, and a real `workspace.read` smoke against a fixture server passed |
| Windows server build | Cross-built, started under Wine, and `/healthz` returned `200`. Not a GUI check, and not native Windows. |
| Release target names | Verified as targets: `ghcr.io/wnzn/metro-server:<tag>`, `ghcr.io/wnzn/metro-runner:<tag>`, `Metro-0.2.0-windows-x64.zip`, `Metro-0.2.0-linux-x64.tar.gz`, `Metro-runner-0.2.0-linux-x64.tar.gz`, each with `.sha256` |

**The headless browser run, step by step:**

1. Signed in to the panel as the environment bootstrap admin.
2. Added a managed user in the panel.
3. With that user's returned bearer token, `GET /v1/me` returned `200` and
   the admin user list returned `403`.
4. Rotated the token in the panel. The old token returned `401` at once.
5. Revoked the token in the panel. The new token returned `401`.
6. Signed out.

No secret was logged, and no screenshot was taken.

Not yet recorded:

- Publishing any release file or image. Nothing has been released.
- The runner package as a published release download, and the runner
  image pulled from GHCR.
- The desktop app on native Windows, or any Windows GUI run.
- The desktop changes that were planned at the time. Most of them were
  later checked in the 0.2.0 Linux candidate
  ([below](#staged-020-linux-candidate-local-workspace-and-desktop-qa-2026-11-22-project-manager-local)).

## Staged: profiles and message times API (2026-11-22, project manager, local)

> **Historical.** Superseded by the [Metro 0.2.0 local release](#final-metro-020-local-release-prepublication-2026-11-23-project-manager-local).

Server-side receipts. Not a final gate, and nothing has been released. The
desktop interface for both features is pending and not verified.

| Item | Result |
| --- | --- |
| Server tests | 323 passed. The lanes' suites overlap, so don't add this to other counts. |
| Profile routes | `GET` and `PATCH /v1/me/profile` with `{id, display_name, bio: null\|string (≤ 280), avatar_version: null\|string}`. Avatars: PNG or JPEG, raw ≤ 1 MiB and ≤ 4096 px a side, re-encoded to a PNG ≤ 256 px a side with no metadata. Reads limited to yourself and current Space co-members, with no admin bypass. |
| Event times | Every HTTP event has an additive `created_at`: RFC 3339 UTC with milliseconds, or `null`. It's immutable server time on posted and returned messages, events stay sorted by id, and no time is invented. |
| Agent context | `sent_at` on current and earlier user-context messages and on a parent thread's task, while earlier assistant replies stay plain assistant text; plus the server's UTC time at run start. Verified with a scripted provider and a device connection across Chat, rooms, threads and schedules. |

Not recorded in this entry: the desktop app. Profile editing and avatars
were later verified in the 0.2.0 Linux candidate, and the live dev
service's profile and timestamp reads were checked
([below](#staged-020-linux-candidate-local-workspace-and-desktop-qa-2026-11-22-project-manager-local)).

## Demonstrated: final local gate with Documents & media (2026-11-22, primary workspace, local)

This was the current gate for the Documents & media build, and the 0.2.0
local release above supersedes it overall. It ran on the primary workspace, with every
full-process script using the same verified final binaries. The [System One
milestone gate](#historical-final-gate-for-the-system-one-milestone-2026-11-22-independent-reviewer-local)
below is kept for the record and is superseded by this one.

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check`, strict `cargo clippy --locked --workspace --all-targets -- -D warnings`, `git diff --check` | pass |
| `cargo test --locked --workspace --no-fail-fast`: **497 passed, 0 failed, 1 ignored** (a doctest). All 7 Docker sandbox tests ran, against a pinned image. | pass |
| Locked debug builds of `metro-server`, `metro-runner` and `metro-connector`, and the locked release `metro-connector` | pass |
| All **seven** full-process e2e scripts: [smoke](../tests/e2e/smoke.py), [threads](../tests/e2e/threads.py), [settings](../tests/e2e/settings.py), [device_connector](../tests/e2e/device_connector.py), [schedules](../tests/e2e/schedules.py), [intelligence](../tests/e2e/intelligence.py) and [library](../tests/e2e/library.py). `library.py` ran twice. | pass |
| Media reference service: the 83 hardened worker tests, and `tests/e2e/library_reference.py` (Metro through the real reference HTTP app, with the test-only encoder) | pass, as [below](#demonstrated-documents--media-with-synthetic-fixtures-2026-11-22-qa-local) |
| Desktop: `nsh check`, `nsh test .` **106/106**, and the build. There are 6 native Linux captures. | pass |

**`library.py`'s expectation was changed once, for correct behaviour.**
When a model share is revoked, the borrower's later search now gets `200`
with **empty results**. There are no file identifiers, no hits, and no
model call. Indexing again with that connection is refused. The script used
to expect an error there.

Every model in this gate was a local fixture, a scripted endpoint or a
test-only encoder. **No pretrained `google/embeddinggemma-2` weights were
downloaded, and no ranking quality was tested.** No commercial vendor was
validated. Native audio and video playback, seeking, and the native file
picker (portal) weren't verified. **Nothing was published or released.**

## Historical: final gate for the System One milestone (2026-11-22, independent reviewer, local)

Superseded by the [Documents & media gate](#demonstrated-final-local-gate-with-documents--media-2026-11-22-primary-workspace-local), and then by the [0.2.0 local release](#final-metro-020-local-release-prepublication-2026-11-23-project-manager-local). It's kept as a record of that milestone.

An independent reviewer ran this on the primary checkout with Rust 1.98.1.
It supersedes the earlier backend gate below. The full report and the test
log were local files (`/tmp/opencode/metro-intelligence-primary-gates.md`
and `metro-intelligence-workspace-test.log`) and aren't in the repo.

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | pass |
| `cargo test --locked --workspace --no-fail-fast`: **403 passed, 0 failed, 1 ignored** (a connector doctest). By crate: auth 37, connector 76, model-protocol 13, runner 64, server 213. All 7 Docker sandbox tests ran, against a pinned Alpine image. | pass |
| Locked debug builds of `metro-server`, `metro-runner` and `metro-connector`, and `cargo build --locked --release --bin metro-connector` | pass |
| `docker build --target server` and `--target runner`. Both images run their binaries as non-root, with networking disabled. | pass |
| Workspace example check, `actionlint`, and Compose service selection (default and registered runner) | pass |
| All six e2e scripts, run separately against `target/debug` within CI deadlines: [smoke](../tests/e2e/smoke.py), [threads](../tests/e2e/threads.py), [settings](../tests/e2e/settings.py), [device_connector](../tests/e2e/device_connector.py) and [intelligence](../tests/e2e/intelligence.py) (60 s each), and [schedules](../tests/e2e/schedules.py) (150 s; it took 57 s) | pass |
| Supplemental: `schedules.py --live-recurring`, a real scheduled occurrence on the live clock ([details](#demonstrated-scheduled-prompts-end-to-end-2026-11-22-scheduler-lane-local)) | pass |
| Desktop: `nsh check`, `nsh test` 99/99 and `nsh build App.tsx`, with the native screenshots checked ([details](#demonstrated-desktop-model-routing-sharing-and-route-attribution-2026-11-22-frontend-lead-local)) | pass |

Every vendor-facing check used local fixtures or scripted models.
**Commercial vendors haven't been validated**: no real TypeSafe, OpenAI,
Anthropic, Gemini or embeddings account, no live ChatGPT sign-in, and no
production OIDC. Nothing was published.

## Earlier backend gate (2026-11-22, integrator, local)

This is superseded by the final gate above. It ran on the primary workspace,
with registered runners, model connections and the device connector. It covers the Rust workspace and the
end-to-end scripts. The desktop results are listed separately
([below](#demonstrated-native-desktop-2026-11-22-frontend-local)).

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | pass |
| `cargo test --locked --workspace`: 298 passed, 0 failed, 1 ignored (a doctest). This includes the Docker sandbox tests, run against a pinned Alpine image (`METRO_TEST_DOCKER_IMAGE` set). | pass |
| `cargo build --locked --release --bin metro-connector` | pass |
| `python3 tests/e2e/smoke.py --timeout 60`, against rebuilt binaries | pass |
| `python3 tests/e2e/threads.py --timeout 60`, against rebuilt binaries | pass |
| `python3 tests/e2e/settings.py --timeout 60`, against rebuilt binaries | pass |
| `python3 tests/e2e/device_connector.py --timeout 60`, against rebuilt binaries | pass |
| Compose with a shared organization runner ([below](#demonstrated-compose-with-a-shared-organization-runner-2026-11-22-integrator-local)) | pass |
| `actionlint` on `.github/workflows/ci.yml` | pass |

The workspace tests include:

- **The server's tests:**
  - Native Anthropic and Gemini fixtures.
  - Keyless local endpoints, which send no auth header.
  - The base URL allowlist, and no redirects.
  - Encrypted keys bound to their owner and connection.
- **The connector's tests:**
  - The library tests, the Claude adapter's tests among them, run against a
    stub CLI and a stub Anthropic API.
  - `--test companion`: the real connector binary against loopback Metro
    fixtures.
  - `--test openai_oauth`: Sign in with ChatGPT against an inert fixture with
    fake OIDC, JWKS, token, revocation, model and SSE endpoints, plus a
    scripted browser.
- **The runner's tests:** registered runner and per-Space sandbox tests.

All of these use fixtures. None used a vendor account, a real CLI or a paid
model. Nothing was signed or published.

## Demonstrated: first Spaces build (2026-11-22, integrator, local)

These are earlier results for commit `d3a435a`, from before registered
runners, model connections and the device connector. For Rust and the e2e
scripts, the gate above supersedes them, and the
[desktop section](#demonstrated-native-desktop-2026-11-22-frontend-local)
supersedes the desktop rows. They're kept for the Compose dedicated-runner
check.

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --locked -- -D warnings` | pass |
| `cargo test --workspace --locked`: 155 tests, including the Docker sandbox tests (`METRO_TEST_DOCKER_IMAGE` set) | pass |
| `python3 tests/e2e/smoke.py --timeout 60`: real server and runner binaries | pass |
| `python3 tests/e2e/threads.py --timeout 60`: real server binary | pass |
| Compose from the current `Dockerfile`, as an isolated project (details below) | pass |
| `actionlint` on `.github/workflows/ci.yml` | pass |
| Desktop: `nsh check` clean, and `nsh test` 22/22 (headless, fake server) | pass |
| Desktop, native Linux GTK capture against the real server and runner on a dedicated Xvfb display: six screenshots, listed below | pass |

**Both e2e scripts use a local scripted model.** It is an OpenAI-compatible
stand-in that returns fixed replies and tool calls. The server, Rig, the
runner and the database are real. These scripts prove the wiring, not the
behaviour of any real model.

What the two scripts cover:

- **`smoke.py`:** Space privacy and membership, runner token rotation, and two
  runners, each listing only its own workspace's marker file.
- **`threads.py`:**
  - A parent agent creates a child thread, sends it a follow-up, and the
    child's results are mirrored to the parent.
  - Access to personal Chat and to runs.
  - An agent turn answered with a reaction alone.

**Compose (isolated project, no model):**

- The server became healthy with no runner and no model.
- A member's Demo in an unpaired Space returned `409`, and an outsider got
  `404`.
- The owner paired the bundled runner.
- Bob's Demo returned `202`, and the `tool_result` listed that runner's own
  fixture workspace.

The six screenshots from this run have since been replaced by the
captures listed under the native desktop section.

## Demonstrated: settings, registered runners and device connections (2026-11-22, integrator, local)

This section shows what the two newer e2e scripts check. Their final run is
part of the backend gate above.

The scripts run real locked builds of `metro-server`, `metro-runner` and
`metro-connector`. The only models are local scripted providers that you
supply yourself (BYOK). No paid or vendor sign-in is involved.

What the two new scripts cover:

- **`settings.py`: providers.**
  - The catalog lists `openai`, `anthropic`, `gcp.gemini` and `custom`, plus
    the allowed base URLs, without calling any provider.
  - Each user's connections are private.
  - A keyed connection and a keyless manual one work, with live model
    listing.
  - The default persists.
  - A two-turn Rig tool exchange runs on the initiator's own connection, and
    no other user's prompt or answer leaks in.
  - A personal Chat message can pick a different, non-default connection for
    that one reply, and the saved default stays unchanged.
  - Keys appear in neither API responses nor the SQLite file.
  - Deleted connections are gone.
- **`settings.py`: runners.**
  - Personal and organization runners get distinct credentials, with the
    right visibility and `can_manage`.
  - The dedicated view works, and a dedicated token stops working once it's
    revoked.
  - A registered credential without protocol 2 gets `426`.
  - The shared runner's claim names the right Space, and results from a
    runner that doesn't own the job get `404`.
  - Deleting a runner detaches it everywhere, and its credential gets `401`.
  - Demo never calls the model.
- **`device_connector.py`.**
  - A `device-openai-compatible` connection pairs with a `metro_pc_` token.
    `GET …/device-token` reports `paired`, `online` and `last_seen_secs`.
  - A personal Chat turn runs through the connector to a loopback vendor,
    including a tool round trip.
  - Model listing is relayed as `{}`.
  - Rotation leaves the old credential with no stale work, and another
    user's device never claims Alice's job.
  - The profile directory is mode 700.
  - The vendor key never appears in Metro's SQLite file or server output.

These prove the wiring with scripted models. They don't show the behaviour
of a real model or vendor account.

## Demonstrated: Compose with a shared organization runner (2026-11-22, integrator, local)

This used the real `reusable-runner` Compose profile, the runner image, and a
named workspace volume.

| Check | Result |
| --- | --- |
| Alice and Bob each own a Space, and both attach the same organization runner | pass |
| The runner creates a separate `spaces/<sid>/` for each Space in the named volume, owned by UID 1000 | pass |
| Each Space's Demo lists only its own marker file, never the other Space's | pass |
| The same isolation holds after the runner container restarts | pass |
| The legacy dedicated `runner` profile still lists the sample workspace | pass |
| `compose.provider-key-file.yaml` mounts the key file exactly as documented | pass |
| `actionlint` on `.github/workflows/ci.yml` | pass |

## Runner image and workspace volume (2026-11-22, docs writer, local)

Run against the working tree while registered runners were being built. It
checks only the Docker steps in [runners.md](runners.md). It doesn't involve a
server, a registered credential or a claimed job.

| Check | Result |
| --- | --- |
| `docker build --target runner -t metro-runner:docs-check .` from the repo root (Docker 29.9.1) | pass; repo tree unchanged |
| A new named volume mounted at `/workspace` in that image is owned by `metro` (1000:1000), and the image's user can create `spaces/<sid>` in it | pass |
| `alpine/git:2.47.2` as `--user 1000:1000` with `HOME=/tmp` clones into an existing, empty `/workspace/spaces/<sid>` on that volume, and the runner image lists it | pass (local source repo) |

## Demonstrated: native search and scheduled tasks screens (2026-11-22, frontend lane, local)

These checks ran natively in NSH against the real backend. Embeddings and
System One were keyless loopback fixtures, and the chat model was the
scripted stand-in. The full desktop gate later passed with these screens
integrated (see *desktop model routing, sharing and route attribution*).

| Check | Result |
| --- | --- |
| 11 focused desktop tests for search, scheduled tasks and routing | pass |
| [semantic-search](screenshots/semantic-search.png): one search across everything the user can read. The results came from Chat and Space rooms, with the *Reranked by System One* badge and per-result labels such as *System One relevance 0.95*. Both appear only because the fixture's System One endpoint scored every result. | pass |
| [scheduled-tasks](screenshots/scheduled-tasks.png): daily, weekday, one-time and paused tasks. The next firing is shown in the task's IANA zone and in UTC. A one-time task actually fired: it now reads *No more firings*, and its history shows *Dispatched*. | pass |

These are the revised captures. The fixture's scores and order show that the
wiring works. They say nothing about ranking quality with a real model.

## Demonstrated: desktop model routing, sharing and route attribution (2026-11-22, frontend lead, local)

Desktop gate on the shared checkout, from `apps/desktop` with NSH 0.6.1,
after the Search and Scheduled tasks screens were integrated:

| Check | Result |
| --- | --- |
| `nsh check` | pass |
| `nsh test`: 99 tests in 14 files, including `src/routing.test.ts` (7), `src/features/RoutingSettings.test.tsx` (11), `src/features/RouteAttribution.test.tsx` (4) and the discovery lane's 11 | pass, 0 failed |
| `nsh build App.tsx` | pass |

The tests run against the in-memory fake server. They cover pickers that
offer only models of their own capability that the user can use, opt-ins
that are off by default, the classifier needed for routing and
classification, the server's bounds (at most 16 use cases, keys
`[a-z][a-z0-9_]{0,31}`, a minimum of 0.5 for "asking the agent"), a revoked
share flagged and refused on save, and a late routing answer that never
lands after sign-out. For capabilities and sharing they cover separate
defaults per capability, sharing with chosen Spaces then revoking it, a
borrower who can pick the model but never sees its key, an outsider who
can't see it, and a ChatGPT sign-in shared only as a preset. For route
attribution they cover one row with a badge, the reason shown only when
opened, no connection IDs, and the composer never sending `addressed_by`.

Native screenshots came from `apps/desktop/dev/screenshots/capture-routing.sh`.
It ran the real `metro-server` (`target/debug`, SHA-256 `3725ad18…9173`, a
newer build than the one in the binaries note) and four loopback fixture
vendors from `tests/e2e/intelligence.py`, with throwaway keys.
`model-routing.png` was then recaptured with the corrected fallback text
against `target/debug/metro-server` SHA-256 `8e468d96…c469`. After that
change, `nsh check` passed and `nsh test` passed 99/99.

| Shot | What it shows | Result |
| --- | --- | --- |
| [model-routing](screenshots/model-routing.png) | Classifier, routing on, use cases each with its answering chat model; one is Bob's connection shared with Acme | pass |
| [model-routing-dataflow](screenshots/model-routing-dataflow.png) | Classification opt-in, embeddings model, and *Where your text goes* naming each endpoint | pass |
| [model-sharing](screenshots/model-sharing.png) | A self-hosted local model shared with one Space, with the owner-visibility and billing warning | pass |
| [route-attribution](screenshots/route-attribution.png) | A real turn: Alice's unaddressed message judged by the System One fixture under her opt-in (*agent · detected by System One*), routed to *Incident review* and answered by Bob's shared fixture endpoint; the group opened to its route | pass |

The script's secret scan found no token or key under the output or the
checkout. The app ran from a disposable copy, so no step file was written
into the checkout. The `p 1.00` and the replies come from the fixtures and
say nothing about a real model.

**Not demonstrated:** a real TypeSafe, OpenAI or embeddings vendor, and a
device (`metro-connector`) connection in these shots.

## Demonstrated: classification, routing, sharing and search end to end (2026-11-22, integrator, local)

`python3 tests/e2e/intelligence.py --timeout 60` passed against the
**primary workspace's locked binaries**, not a snapshot. It ran real
`metro-server` and `metro-connector` processes, with loopback inference
fixtures for chat, embeddings and the System One wire. Each fixture counts
the calls it receives. An earlier run of the full workflow on an integration
snapshot, which included the `no_match` branch, is superseded by this one.

**Exactly one typed question per System One request.**

| Message | System One calls | Chat calls |
| --- | --- | --- |
| `@bob` only (people only) | none | none |
| Unmentioned, classification on, uncertain answer | one `noul`, no `choice` | none; the agent didn't start |
| Unmentioned, classification on, confident answer | one `noul`, then a separate `choice` | one turn, on the routed connection |
| Explicit `@agent` | one `choice`, no `noul` | one turn |

**Failures and fallbacks.**

- A valid explicit `no_match` was answered by the configured default
  connection only (`source: routing_default`). No other model was called.
- When the routed answering model failed, the error was visible in the room,
  and only that model was called. No other model was billed.
- When the addressing decision failed, only System One was called. No
  routing question was asked, no answering model was called, and no agent
  run started.
- A routing failure, or a revoked classifier, stopped the turn visibly,
  without calling the default model.

**Sharing and search.**

- Chat and embeddings connections were shared across users, both on a
  local endpoint and on a device. They worked, and stopped when revoked.
- The semantic-search shortlist was reordered by pointwise System One
  calls over the real System One wire, to the fixture.
- No credential was exposed to the borrower.
- **Server-stored borrowed connections** are rechecked for System One
  decisions and embeddings too. When the share is revoked or changed, an
  in-flight answer or vector is withheld. A request already sent upstream
  can't be unsent.

**Not demonstrated:** a real TypeSafe account, a cloud embeddings vendor,
production authentication (OIDC), or a real chat vendor. This run is
included in the [final gate](#historical-final-gate-for-the-system-one-milestone-2026-11-22-independent-reviewer-local). The per-lane test
counts in the sections below apply only to their own lanes and runs.

## Demonstrated: scheduled prompts end to end (2026-11-22, scheduler lane, local)

The scheduler lane reported these runs. Both black-box runs use the real
`metro-server` binary, dev-token users, a temporary SQLite database, the
real clock, and local scripted OpenAI-compatible model stubs that count
every call. No real vendor model was involved.

| Check | Result |
| --- | --- |
| `python3 tests/e2e/schedules.py --timeout 150` | pass |
| `python3 tests/e2e/schedules.py --live-recurring`: a real hourly cron task. Its first occurrence, due 06:53Z, fired once, and `next_run_at` moved to 07:53Z. | pass |

What the black-box run showed:

- **Accepted once, as the creator.** A once task in personal Chat and one in
  a Space room each posted their prompt once, under the creator's identity,
  and the agent answered once.
- **`dispatched` means accepted, in Chat too.** The stub held the personal
  Chat inference open. History already had exactly one `dispatched` entry,
  and the Chat had no final answer yet. Personal Chat now behaves the same as
  a Space room.
- **Revocation.** After the creator left the Space, nothing was posted and
  no model was called. The task was paused, and the failure was recorded,
  with a notice in the creator's personal Chat.
- **No fallback.** When the creator's default connection was a paired but
  offline device, neither a model stub nor the `METRO_MODEL` fallback was
  called, and the failure was visible.
- **Pause, resume, edit and delete** behaved as documented.
- **Restart.** After a SIGKILL, finished occurrences weren't replayed. An
  occurrence that fell due while the server was down, still inside the
  5-minute grace, ran exactly once after the restart.

**Not demonstrated:** a real vendor model, or a long-running deployment. The
desktop screen is covered [above](#demonstrated-native-search-and-scheduled-tasks-screens-2026-11-22-frontend-lane-local).

## Scheduled prompts: unit and component tests (2026-11-22, scheduler lane, local)

These are the focused tests for the code in `crates/server/src/schedules.rs`,
`schedules/cron.rs` and `crates/server/tests/schedules.rs`. The end-to-end
runs above build on them.

| Check | Result |
| --- | --- |
| 30 unit tests (`src/schedules/tests.rs`, `src/schedules/cron.rs`), with an injected clock and a fake submitter. They cover cron parsing, `Asia/Manila` daily and DST zones, validation, the 50-task cap, owner-only `404`, Space membership and room scope, revocation that pauses and notifies, cancel on edit or pause after claim, no catch-up on resume, a single `missed` entry, recorded errors with no retry, a hung submission becoming `unknown`, concurrent ticks submitting once, a unique-occurrence guard, a restart marking `interrupted`, bounded history, and moving a task to personal Chat | pass |
| 2 HTTP component tests (`tests/schedules.rs`): API shapes, owner-only access, validation and Space access, over real HTTP. They don't start the scheduler loop. | pass |

## Model capabilities and sharing: component tests (2026-11-22, providers lane, local)

The providers lane reported these component tests green, including
`crates/server/tests/provider_sharing.rs` over real HTTP. They ran against a
loopback fixture that speaks System One, embeddings and Chat Completions,
and records every request it receives.

They covered:

- per-connection private defaults, and one default per fixed capability;
- the provider ids `systemone`, `custom-systemone`, `openai-embeddings`,
  `custom-embeddings`, `device-systemone` and `device-embeddings`;
- share management by the owner only, with `personal`, `spaces` and
  `organization` visibility;
- use in a Space only while both people are members;
- borrowed use on the owner's endpoint, with no key or URL in any response;
- revocation stopping the next request, with server and device work
  rechecked;
- subscription presets never running on someone else's account. In
  `presets_never_bill_an_api_key_and_stop_when_revoked`:
  - a borrower with only a working OpenAI API-key connection got a clear
    error, `ready: false`, and no upstream call;
  - with their own `device-openai-oauth`, the preset ran on their device;
  - revoking stopped the next request before it reached the device, and
    withheld an answer the device was still producing;
  - no request ever went anywhere else;
- fingerprints following model edits.

**Not demonstrated:**

- Any real commercial provider: TypeSafe, OpenAI embeddings, or a shared
  real account.
- A live Sign in with ChatGPT preset. Sharing end to end with fixtures is
  covered by `tests/e2e/intelligence.py` ([above](#demonstrated-classification-routing-sharing-and-search-end-to-end-2026-11-22-integrator-local)).

## Semantic search: unit and HTTP tests (2026-11-22, search lane, local)

The search lane reported these results for `crates/server/src/search.rs`,
`search/` and `crates/server/tests/search.rs`. Every model call went to a
loopback fixture that speaks the OpenAI embeddings wire and the System One
wire, and records what reached it.

| Check | Result |
| --- | --- |
| 14 unit tests (`src/search/tests.rs`). They cover: normalized vectors ranking by meaning, not length; System One reordering the shortlist; a rerank failure falling back visibly; malformed System One answers not being used as scores; other people's Chats and other Spaces never reaching the model; leaving a Space hiding its results and deleting its vectors; access revoked between retrieval and results being honoured; a destination-restricted connection skipping its Space; a fingerprint change reindexing and a dimension change being refused; incremental indexing; only messages and agent replies being indexed; deterministic ties; request validation; and vector checks with top-k ordering | pass |
| 4 HTTP tests (`tests/search.rs`): real HTTP through the real provider layer, against the loopback fixture | pass |

**Not demonstrated:** a real embeddings vendor or cloud account, Ollama, or
a real TypeSafe account. The end-to-end run and the desktop screen are
covered by the sections above.

## Demonstrated: grouped agent activity (2026-11-22, frontend, local)

This run is newer than the native desktop run below. It supersedes that
run's test count and its two agent captures. It used NSH 0.6.x on native
Linux GTK, against the real `metro-server` and `metro-runner`.

| Check | Result |
| --- | --- |
| `nsh test`: 66/66, headless, against a fake server. This includes activity tests where people's messages arrive in the middle of a live agent turn. | pass |
| A native capture of a live turn, collapsed as **Metro is thinking…** with 2 steps: [agent-thinking](screenshots/agent-thinking.png) | pass |
| A native capture of the finished turn, **Metro’s activity**, opened to its `workspace.list` call and the runner's listing: [agent-activity-expanded](screenshots/agent-activity-expanded.png) | pass |
| Demo shows as one collapsed **Runner activity** group, never as "thinking" or under a person's name | pass |

What these show, and what they don't:

- Each agent turn is one collapsed group, identified by the server's
  activity metadata.
- The only "thinking" text is the live label. Metro shows no chain of thought
  of its own, and none was invented. A step counts as thinking only if the
  server or provider labels it that way.
- The turn ran through Rig with a real tool call on the runner, but the model
  was the local scripted stand-in, whose reply says "Scripted test model".
  No real model was used.
- This run's other captures were refreshed too: `room-runner`, `chat-runs`
  and `thread`.

## Demonstrated: native desktop (2026-11-22, frontend, local)

This run used NSH v0.6.1, on native Linux GTK. Its test count was superseded
by the [grouped activity run](#demonstrated-grouped-agent-activity-2026-11-22-frontend-local).

| Check | Result |
| --- | --- |
| `nsh check` | pass |
| `nsh test`: 52/52 at the time, headless, against a fake server | pass |
| `nsh build App.tsx` | pass |
| Dark by default, with Light and System applied live | pass |
| Agent activity (tool calls, tool output, and thinking and progress summaries) starts collapsed, while errors and final messages stay visible | pass |
| Six screenshot runs against the assembled real server and runner, 1200×780 in the default dark theme | pass |
| A scan of the captures found no tokens or keys | pass |

The captures are in `docs/screenshots/`:

- **Settings:**
  [settings-runners](screenshots/settings-runners.png),
  [settings-providers](screenshots/settings-providers.png),
  [settings-device-onboarding](screenshots/settings-device-onboarding.png),
  [settings-appearance](screenshots/settings-appearance.png),
  [settings-space-runner](screenshots/settings-space-runner.png).
- **Refreshed in the same run:**
  [room-runner](screenshots/room-runner.png),
  [chat-runs](screenshots/chat-runs.png),
  [thread](screenshots/thread.png),
  [space-members](screenshots/space-members.png),
  [connection](screenshots/connection.png),
  [connection-error](screenshots/connection-error.png).

What they show, and what they don't:

- `room-runner` shows the attached runner's Demo output, collapsed. The
  current capture, from the grouped activity run, shows it as one *Runner
  activity* group.
- `settings-providers` shows a **keyless, locally scripted** model listing.
  The device connection shows as offline.
- `settings-device-onboarding` shows the terminal-wizard guide, with the token
  masked.
- None of them shows a real vendor sign-in or a real model.

The native Linux app needs glibc ≥ 2.39, GTK ≥ 4.14 and WebKitGTK 6.0.

## Demonstrated: Documents & media with synthetic fixtures (2026-11-22, QA, local)

These results come from QA and the service lane. This writer didn't rerun
them. Every model in them was synthetic.

**`tests/e2e/library.py`.** It ran against the real `metro-server` and
`metro-connector` from the primary workspace, and passed three times. The
media vendor was a loopback **scripted** service that extracts text and
returns vectors deterministically. It showed the following:

- **PDF.** A valid PDF went over the wire as its raw bytes, and the search
  hit was located on **page 2**.
- **DOCX.** A valid DOCX gave a hit in **paragraph 2**, with text offsets and
  an extracted excerpt.
- **Device relay.** A real 3 MiB PNG and a payload of **exactly 8 MiB**
  went through the real `metro-connector` companion unchanged. The relay
  enforces per-caller and per-connection budgets. The server allows 4 media
  calls at once, 2 per person and 3 per connection, and it doesn't
  serialize a large body just to measure its size.
- **Content beats file names.** With scripted vectors, the matching content
  outranked a file whose name was misleading.
- **Access.**
  - Out-of-scope reads were blocked, and nothing was sent anywhere for
    them. That includes `Range` and `HEAD` requests.
  - Results were withheld when the member was removed during a search.
  - Removing a member and adding them back dropped only that person's
    index. The Space's files and other members' indexes stayed.
  - An uploader's own access to an original still depends on current
    membership. Adding someone back restores their access to the originals
    but not their old index. Nothing already sent is clawed back.
  - Deleting a connection, changing its model or revision, or narrowing a
    grant purged the affected index rows straight away. A stale write that
    was still in flight couldn't bring them back.

**Reported separately by the final owners.**

- **Upload settings.** All eight `METRO_LIBRARY_*` settings were checked.
- **Compose media stack.** The runtime has a limit of 256 processes and the
  default 8 GiB `METRO_EG2_MEMORY_LIMIT` cap, which is a ceiling, not a
  measured need. The import step is capped at 256 MiB. Landlock started
  under Docker's default profile.

**Reference service (`services/embeddinggemma2/`), after hardening.**
These results come from the service lane. They supersede the earlier 54
tests.

| Check | Result |
| --- | --- |
| `uv run --frozen pytest -q` on the host | **83 passed**, including 9 isolation, 17 wire and 4 admission and deadline tests, plus a fail-closed `503` test |
| `docker build --target test` (BuildKit, UID 10001) | 83 passed |
| The test image under `--read-only --tmpfs /tmp --cap-drop ALL --security-opt no-new-privileges --network none` | 83 passed |
| The runtime image bound to `0.0.0.0` without a key | refused to start, as designed |
| The hardened runtime image: non-root, `--read-only`, noexec tmpfs, `--cap-drop ALL`, `no-new-privileges`, `--pids-limit 256`, `--memory 6g`, the key file mounted read-only, with a random-init model | The sandbox self-test passed at startup. `/v1/models` without the key returned `401`. A PDF with one text page and one scanned page decoded through the worker. PID 1 had `CapEff=0`, `NoNewPrivs=1` and `Seccomp=2`. |
| A probe with harmless test files, on the host (Linux 6.12, Landlock ABI 6) | **Denied:** a sibling job's files, `/proc/<parent>/environ`, `/etc/hostname`, loopback TCP, a UNIX socket, signals, `ptrace` and `process_vm_readv` against the parent, and fork and exec. **Allowed:** threads, a signal to itself, its own workdir, importing numpy. The child environment held only allowlisted keys. |
| `scripts/plumbing_check.py` (before hardening) | The real Sentence Transformers and Transformers code and the pinned processor ran on a **random-init 2-layer** copy of the model. Every modality gave finite, unit-length 768-dimensional vectors, and the prefixes matched `prompt_name` exactly. |
| `tests/e2e/library_reference.py` | Metro uploaded, indexed and searched synthetic TXT and PNG through the service's real HTTP app with its **test-only** encoder. The real locators came back, the TXT had an excerpt, and the PNG had none. |

The isolation was checked only with Docker 29.9.1's default seccomp profile
on that Linux 6.12 host. Podman, Kubernetes and gVisor weren't checked.
**No pretrained weights were used in any of these runs.**

**Desktop (frontend lane).**

- With NSH 0.6.1, `nsh check` was clean. `nsh test .` passed **106/106**
  twice: 7 new library tests and the 99 earlier ones. The first build
  passed.
- There were three native Linux runs against the real server binary
  (sha256 `99865f44…d7ca04b`), with a **scripted** media endpoint. They
  produced six screenshots: `docs/screenshots/library-{assets,search,open-page,open-image,revocation-before,revocation-after}.png`.
  See [library.md](library.md#in-the-desktop-app).
- **Removal.** When Bob was removed while viewing a Space file, his viewer
  closed and only his personal files stayed listed. The server answered
  `404` to his next read.
- **Uploads.** The files in the captures were uploaded through the real
  authorized HTTP upload. The native file picker was tested headless only.
- **PDF and DOCX** open as their indexed page or section text, with the
  location and a save of the authenticated original. There's no
  full native renderer.
- **Images** previewed natively.
- **Audio and video:** a WAV's duration loaded. Native playback and
  seeking were **not** verified, because the capture machine had no sound
  device.
- **Temporary files.** The Linux temporary file used for audio and video is
  removed on close, sign-out, loss of membership and startup. Copies saved
  to Downloads can't be clawed back.

**Not shown:**

- any ranking by the pretrained `google/embeddinggemma-2`, because no
  weights were downloaded and the real-weight smoke wasn't run,
- retrieval quality on real files,
- decoder isolation on any other kernel or container runtime,
- any real remote endpoint or vendor.

## Verified: Windows x64 portable package, built on Linux (2026-11-22, packaging, local)

This is a local, **unpublished** desktop client package built from the
working tree. It isn't signed and isn't a release. It wasn't run on native
Windows.

| Item | Value |
| --- | --- |
| File | `dist/windows/Metro-windows-x64.zip`, with `Metro-windows-x64.zip.sha256` beside it |
| Version | 0.1.0 |
| Size | 26,288,229 bytes |
| SHA-256 | `5d42a80326485f484cd8addf330129984d0a80db073501e92c90fd78ce07dfed` |
| Contents | `Metro/Metro.exe`, `Metro/nsh/app.json`, `Metro/nsh/bundle.nshc`, `Metro/LICENSE-APACHE` |

**How it was built.** On a copy of the current `apps/desktop`, the Linux NSH
CLI ran `nsh package App.tsx --target windows --out <output>`, with
`NSH_RUNTIME` pointing at the `nsh/runtime` directory of the pinned official
Windows distribution. Both the CLI and the Windows distribution are official
[longbridge/nsh v0.6.1](https://github.com/longbridge/nsh/releases/tag/v0.6.1)
release assets.

| Check | Result |
| --- | --- |
| The ZIP's CRCs | pass |
| The Windows distribution matches the official v0.6.1 release digest | pass |
| `Metro.exe` is a PE32+ x86-64 GUI executable | pass |
| `Metro.exe` matches the official v0.6.1 Windows release runner | pass |
| An independent `nsh build App.tsx --release` on the live working tree gave a bundle identical byte for byte to the shipped `Metro/nsh/bundle.nshc` | pass |
| `nsh test .` on that snapshot, headless: **106/106** | pass |

**What the package needs and holds.**

- 64-bit Windows 10 1809 or later, or Windows 11.
- The Microsoft Visual C++ v14 x64 runtime (`VCRUNTIME140.dll` and
  `VCRUNTIME140_1.dll`). Windows N editions may also need the Media Feature
  Pack, because the executable imports `mfplat.dll` and `mfreadwrite.dll`.
- It contains no credentials and no server. People connect it to a Metro
  server, over HTTPS when the server is remote.

**Wine smoke test (QA).** On Linux, under Wine 10.0 on Xvfb with software
graphics, QA launched the shipped `Metro.exe` from an unrelated working
directory. It opened a window titled Metro, 1100×720, and stayed running for
the whole 20–25 second observation. The window showed only a dark
background, and no Connect form was visible. No token was entered, and the
server connection and sign-in weren't tested.

**Not shown:** launching, rendering, input or sign-in on native Windows.

## Not demonstrated

- Deployment behind a real OIDC provider (Ory Hydra, Authelia or authentik).
  Auth is covered by `crates/auth`'s tests against a local stand-in provider.
- Inference with a real or paid model.
- Any server-side catalog adapter against its real vendor with a real
  account. Only fixtures have been used.
- `device-openai-oauth` with a live ChatGPT sign-in or real inference, or any
  plan's limits.
- `device-claude-code` with a real Claude Code CLI or a real Console key.
- Publishing images to GHCR from a `v*` tag.
- A full scan of logs and the database for leaked tokens. The code is written
  never to log tokens, but no exhaustive scan was done.
- The desktop app on macOS, or on native Windows. Windows has run only
  under Wine, using the launcher and server executables that are byte-identical
  in the final ZIP ([0.2.0 local release](#final-metro-020-local-release-prepublication-2026-11-23-project-manager-local)).
- A recorded desktop GUI run on a Linux distribution other than Debian 13
  (for example Arch or CachyOS). Their runtime package names are documented
  as troubleshooting only.
- Monitoring and the app's installer-requested personal runner with a real
  account, or outside the native Debian GTK run of the
  [57409bb setup cut](#staged-installers-and-setup-at-57409bb-2026-11-23-project-manager-local). A device-local failure (including the
  ChatGPT diagnostics of `9144e1c` and `38749f3`) wasn't injected there.
- The desktop GUI after a CachyOS install: the CachyOS run covered the
  installer and `--install-deps` only.
- `install-metro-windows.ps1`, `install-metro-macos.sh` and the
  `metro-runner-container` scripts in any recorded run, and the desktop on
  native Windows or macOS.
- Any tagged CI release, signed build or published GHCR image.
- Automatic vendor or CLI sign-in from the app, or a real vendor account
  through the in-app device flow. That flow was verified only against a
  local test endpoint.
- Signing or publishing a release.
- Documents & media with the pretrained `google/embeddinggemma-2`: no
  ranking or retrieval quality on real files, no measured memory or timings,
  and decoder isolation checked only on one Linux 6.12 host with Docker
  29.9.1 ([library.md](library.md#not-yet-recorded)).
- Commercial vendor validation for the System One milestone
  ([system-one.md](system-one.md)): any real TypeSafe, cloud embeddings
  (OpenAI) or Ollama call from Metro, or a shared real account. Its local
  gates all passed ([above](#historical-final-gate-for-the-system-one-milestone-2026-11-22-independent-reviewer-local)).

## Reproduce

Run from the repo root.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --locked --bin metro-server --bin metro-runner --bin metro-connector
cargo test --workspace --locked --no-fail-fast
cargo build --locked --release --bin metro-connector
python3 tests/e2e/smoke.py --timeout 60
python3 tests/e2e/threads.py --timeout 60
python3 tests/e2e/settings.py --timeout 60
python3 tests/e2e/device_connector.py --timeout 60
python3 tests/e2e/schedules.py --timeout 150
python3 tests/e2e/schedules.py --live-recurring   # optional: waits for a real hourly cron firing
python3 tests/e2e/intelligence.py --timeout 60
python3 tests/e2e/library.py              # default --timeout 120
```

The media reference service's own checks are weight-free, and run from
`services/embeddinggemma2`: `uv run --frozen pytest -q`, or `docker build
--target test .`. On a new host or container runtime, run the test image
with the runtime's restrictions, using the command in
[its README](../services/embeddinggemma2/README.md#tests-and-checks).
`tests/e2e/library_reference.py` needs the service's Python environment.
CI runs it inside the service's test image with `--network none`. See
`.github/workflows/ci.yml`.

The Docker sandbox tests in `cargo test` are skipped without Docker. To run
them in full, first load a local image pinned by digest that contains
`/bin/sh`, `id`, `ls`, `grep` and `sleep`. The final gate used a pinned Alpine
image. BusyBox works too:

```sh
docker pull busybox:1.37
export METRO_TEST_DOCKER_IMAGE="$(docker image inspect --format '{{index .RepoDigests 0}}' busybox:1.37)"
cargo test -p metro-runner --locked --test docker_sandbox
docker ps -a --filter label=dev.metro.runner=1    # expect nothing left behind
```

Desktop, from `apps/desktop`: `nsh check`, `nsh test` and `nsh build App.tsx`.
`nsh check` and `nsh test` are headless, and the tests use an in-memory fake
server. To recreate the native
screenshots against real binaries, see
[development.md](development.md#screenshots-native-linux-isolated).

To check Compose by hand, follow the [README quickstart](../README.md). In a
throwaway checkout, set `COMPOSE_PROJECT_NAME` in `.env` to keep the run
isolated.
