# Windows signing and distribution (CI interface)

This folder holds the helpers behind two reusable workflows:

- `.github/workflows/windows-distribution.yml`, which runs on every build and
  never signs anything;
- `.github/workflows/windows-sign.yml`, which runs only for an official
  release from `main` (a version bump) while
  `packaging/windows-release-signing.txt` says `signed`, and signs.

## Release mode: signed or unsigned

`packaging/windows-release-signing.txt` holds exactly one value line,
`signed` or `unsigned`. `release_signatures.windows_signing()` reads it for
`release_policy.py` (`windows-signing`, and the `windows_signing` output of
`decide` and `authorize`), `release_assets.py` and `release_signatures.py`;
any other content fails. No variable, input or environment can change it.

**It says `unsigned` now**, because there's no Windows code-signing
certificate yet. An official release then:

- verifies the seed in official mode (all four EXEs, `metro-tui.exe`
  included) and builds the same unsigned distribution as a preview, from the
  same seed matched by hash, with the MSI built with
  `--require-file metro-tui.exe`;
- runs the `preview` job's native checks and self-test on it, and uploads
  `dist-desktop-windows-x64` from there;
- never calls `windows-sign.yml`: no signing variable, no
  `windows-release-signing` environment and no OIDC token for Windows.

To sign again, do the [repository setup](#repository-setup-for-official-releases)
(a CA certificate in an HSM or KSP fits the `kms` backend), then change the
value line to `signed` in a pull request. Signed mode keeps failing closed:
`guard` refuses unless the file says `signed` and
`METRO_WINDOWS_SIGNING_BACKEND` is `azure` or `kms`, and nothing falls back to
unsigned.

## Official release context

Official mode is allowed only for a `push` event to the protected literal
`refs/heads/main` branch of `wnzn/metro`. The push must not be a branch
deletion, the top-level workflow must be exactly
`wnzn/metro/.github/workflows/ci.yml@refs/heads/main`, and the checked-out
commit must equal `github.sha`. The push event payload must agree on the
`ref`, the `after` commit and the repository.

- Tags, pull requests, other branches, forks and manual dispatches are
  never official. A push to `main` reaches this workflow only when
  `release_policy.py` found a version bump.
- The version comes from CI as the `version` input. Nothing reads it from a
  ref name. Output names use that version, `Metro-<version>-windows-x64.*`.
- `RELEASE.json` → `signing.commit` records `github.sha`, so every output can
  be traced to its commit. `verify-dist` and `extract-final` check it with
  `--expect-commit`.

The rules are in `winsign.py release-context`, together with their negative
tests. The following places check them:

- the `seed` job of `windows-distribution.yml`, before the hand-off or the
  unsigned release build
- the `guard` job of `windows-sign.yml`
- a job-level `if:` on `sign` and `sign-msi`, so no runner (self-hosted
  included) is assigned outside this context
- `Assert-MetroReleaseContext`, before any credential is fetched
- `winsign.py finalize --mode official`

Nothing here generates, imports or stores a key or certificate. Nothing has
been signed for real yet: the official path has only been exercised with the
synthetic fixtures in `test_winsign.py` and a local dry run that used
synthetic signatures on real 0.2.0 binaries.

| File | Role |
| --- | --- |
| `winsign.py` | Standard-library Python (`python -I`). Seed verification, staging, signature-only delta proof, Authenticode PKCS#7 inspection, ZIP repack, final distribution checks, MSI admin-image and installed-payload checks |
| `MetroWindowsSigning.psm1` | PowerShell 7 on Windows. SignTool signing (two backends), Windows trust checks, MSI table rules, install/uninstall, tamper checks, preview self-test |
| `test_winsign.py` | Unit tests plus static guards on both workflows. Run with `python3 -B -m unittest discover -s packaging/windows-signing -v` |

## Workflow interface

### `windows-distribution.yml`

- **Inputs:** `release` (boolean) and `version` (string, without the leading
  `v`).
- **Permissions:** `contents: read` only. The caller grants just that, and
  every job declares just that.
- **Input artifact:** always `seed-desktop-windows-x64`, from the same run. It
  must contain exactly `Metro-<version>-windows-x64.zip` and its `.sha256`.
- **`release: false`** (PR and main): uploads `dist-desktop-windows-x64` with
  exactly these six files:
  - `Metro-<v>-windows-x64.zip` and its `.sha256`. The archive is unsigned,
    and its `RELEASE.json` has `"signing": {"mode": "unsigned-preview", …}`.
  - `Metro-<v>-windows-x64.unsigned-preview.msi` (built with `--preview`) and
    its `.sha256`.
  - `install-metro-windows.ps1` and `metro-runner-container.ps1`, as
    unchanged source copies.

  The `seed` job (Ubuntu) repacks the seed and builds the MSI and the MSI
  lane's test fixtures. The `preview` job (`windows-2025`) checks, installs and
  self-tests them. No environment, variable, OIDC token or self-hosted runner
  is used.
- **`release: true`**: allowed only in the official release context above.
  The seed is verified in official mode, which requires `metro-tui.exe`.
  Then, as `packaging/windows-release-signing.txt` says:
  - `signed` (output `mode` `official`): exactly the seed ZIP and its
    `.sha256` are uploaded as `unsigned-desktop-windows-x64` for
    `windows-sign.yml`.
  - `unsigned` (output `mode` `unsigned-release`): the same seed, checked
    again in preview mode with the same hash, goes through the
    `release: false` path above, and the `preview` job uploads the same six
    files as `dist-desktop-windows-x64`. The MSI always requires
    `metro-tui.exe`.

  This mode is still read-only.
- **Outputs:** `mode` (`preview`, `official` or `unsigned-release`) and
  `seed-sha256`.

### `windows-sign.yml`

- **Inputs:** `version` only.
- **Caller:** called only by `release-publish.yml`, only in the official
  release context and only when `packaging/windows-release-signing.txt` says
  `signed`, with `permissions: { contents: read, id-token: write }`.
  It must run in the same workflow run as `windows-distribution.yml`
  (`release: true`), because it downloads `unsigned-desktop-windows-x64`
  without a run id.
- **Jobs:**
  - `guard`: runs on hosted Ubuntu with no credentials. It checks the release
    context, that the switch says `signed`, and the backend, then verifies
    the hand-off again.
  - `sign`: runs in the `windows-release-signing` environment with
    `id-token: write`. It signs `Metro.exe`, `metro-server.exe`,
    `metro-tui.exe`, `ui/Metro.exe`, `install-metro-windows.ps1` and
    `metro-runner-container.ps1`, proves that only signature bytes changed,
    rewrites `RELEASE.json` and repacks the ZIP.
  - `msi`: runs on hosted Ubuntu with no credentials. The MSI lane's
    `build_msi.py` builds the MSI from the newly signed tree with
    `--require-file metro-tui.exe`, using its pinned msitools. Its `inspect`
    step checks the tables and the extracted bytes of every file. This job also
    builds the lane's test fixtures.
  - `sign-msi`: runs in the environment and signs exactly that MSI, matched by
    hash.
  - `verify`: runs on a fresh hosted `windows-2025` VM with no certificate
    added. It runs every check listed below, including the MSI lane's
    `test-msi-windows.ps1` on the signed MSI, then uploads
    `dist-desktop-windows-x64`.
- **Output artifact:** `dist-desktop-windows-x64` with exactly
  `Metro-<v>-windows-x64.zip`, `Metro-<v>-windows-x64.msi`, their two
  `.sha256` files and the two signed `.ps1` files. The scripts are the raw
  signed bytes; nothing re-signs them later. The aggregate list is a separate
  artifact, `windows-sha256sums`, and also appears in the job summary.
- **Outputs:** `zip-sha256` and `msi-sha256`.

Internal artifacts, kept for three days and never named `dist-*`:
`windows-signed-stage`, `windows-msi-unsigned`, `windows-msi-signed`,
`windows-msi-fixtures` and, for previews, `windows-preview-stage`,
`windows-preview-msi` and `windows-preview-msi-fixtures`. Logs go to
`windows-preview-logs` and `windows-sign-logs`.

## What is verified

### Seed (before signing, in both workflows)

- The artifact holds exactly the ZIP and its `.sha256`, and the hash matches.
- Every ZIP entry is a regular file under `Metro-<v>-windows-x64/`. There are
  no links, no `..` and no encryption. The modes are 0755 for EXEs and 0644
  for everything else.
- The ZIP entries are exactly the files in `RELEASE.json` plus `RELEASE.json`
  itself, and every hash matches.
- `RELEASE.json` has exactly the expected keys, version, target, kind, NSH
  version, connector and `local_runner`. It has no `signing` key.
- The binary allowlist is fixed: `Metro.exe`, `metro-server.exe`,
  `metro-tui.exe` and `ui/Metro.exe`. Official mode requires all four. Preview
  mode makes `metro-tui.exe` optional, so seeds that predate the TUI still
  work. Any other member that looks executable, by name or by an `MZ` header,
  fails the check.
- `ui/Metro.exe` is the pinned stock NSH renderer (`package.NSH_APPS`), and
  none of the EXEs is signed yet.
- The seed matches this checkout's sources:
  - The ICO has 7 frames, and `Metro.exe` carries the same 7-frame icon group.
  - `ui/nsh/icon.ico`, `capabilities.json` (against `apps/desktop/nsh.json`),
    `app.json`, the SVG assets, the NSH license and the brand notice all match.
  - The import tables match `RELEASE.json` `dependencies`.
  - `metro-tui.exe` is a console-subsystem EXE.

### After signing (`winsign.py finalize` and `verify-dist`)

- **EXEs.** Each signed EXE differs from its seed only in the PE checksum, the
  certificate-table directory entry and one trailing `WIN_CERTIFICATE` with
  zero padding. Removing those restores the exact unsigned bytes.
  `RELEASE.json` → `signing.files` records `unsigned_sha256`, `unsigned_size`
  and `unsigned_pe_checksum` for each EXE, so anyone can repeat this check
  from the ZIP or from an installed copy. For `ui/Metro.exe`, the restored
  bytes must equal the pinned stock renderer hash.
- **Scripts.** Each signed script is its exact source bytes followed by one
  PowerShell signature block.
- **Authenticode structure.** Checked for the four EXEs, the two scripts and
  the MSI:
  - SHA-256 is the only digest, with exactly one signer and no nested
    signatures.
  - The signer has the code-signing EKU and isn't self-signed.
  - The subject CN equals `METRO_WINDOWS_PUBLISHER_CN`, and the O equals
    `METRO_WINDOWS_PUBLISHER_ORG` if that's set.
  - There is exactly one RFC 3161 timestamp. It uses SHA-256, its imprint is
    bound to this signature, its TSA has the time-stamping EKU, and it falls
    within the signer certificate's validity.
  - All seven signatures name the same publisher and were timestamped within
    12 hours of each other.
- **Distribution.** The ZIP is re-extracted into a fresh directory and checked
  against `RELEASE.json`. The file set is exact, the `.sha256` files are
  checked, and the aggregate checksum list is written after signing.
- **Windows trust** (fresh VM). For every signed file:
  - `signtool verify /pa /all /tw` must exit 0. Exit code 2, which covers
    warnings such as a missing timestamp, counts as a failure.
  - `Get-AuthenticodeSignature` must report `Valid`.
  - The signer must have the code-signing EKU and the expected CN/O.
  - `TimeStamperCertificate` must be non-null and have the time-stamping EKU.

  Because no certificate is ever added on that VM, a private or test root
  can't pass.
- **Rejection.** These must all fail verification: tampered copies (an EXE,
  the TUI, a script, and an MSI with one added Property row), the unsigned
  seed EXEs and the unsigned source script.
- **MSI.**
  - **Before signing (Linux):** the MSI lane's own inspection runs, and
    `winsign.py msi-state` confirms the MSI has no signature.
  - **On Windows:** the unsigned MSI (test builds and unsigned releases),
    or the official MSI after signing, goes through these checks:
    - The MSI has no rows in `MsiEmbeddedUI`,
      `MsiEmbeddedChainer`, `ServiceInstall`, `ServiceControl`, `SelfReg` or
      `MsiDigitalSignature`.
    - `ProductVersion` equals the MSI lane's mapping
      (`build_msi.py version <semver>`), and the MSI has an `INSTALLDIR`
      directory.
    - Custom actions are limited to types 19, 35 and 51, so no DLL, EXE or
      script custom action can run.
    - Its administrative image holds exactly the release tree. The only extra
      file allowed is the empty `enable-local-runner` marker.
  - **Install:** the MSI is installed with
    `INSTALLDIR="<chosen folder with spaces>"`. The installed folder must
    equal the ZIP tree byte for byte, plus the empty default
    `enable-local-runner` marker. In official mode its EXEs must pass the
    Windows trust check. The installed `metro-server.exe` must answer
    `/healthz` on loopback. Then the MSI is uninstalled, and the product must
    be unregistered with the folder gone or empty.
- **Preview only.** A throwaway self-signed certificate signs scratch copies
  of `Metro.exe`, the renderer, a script and the MSI. This proves that real
  SignTool output changes only signature bytes and that the CFB reader finds
  the MSI signature. Both verifiers must reject the throwaway signature, and
  tampering must show as `HashMismatch`. The certificate is removed
  afterwards, and nothing from the self-test is uploaded. The self-test runs
  only in the preview job, for previews and unsigned releases (never for a
  signed release). It refuses to run unless that job sets
  `METRO_WINDOWS_SELF_TEST=preview`, for tags, and in any job that can
  request a GitHub OIDC token. It doesn't exist in `windows-sign.yml`.

## MSI lane interface (`packaging/windows-msi`)

These workflows use the MSI lane's documented interface as it is (see
`packaging/windows-msi/README.md`):

```sh
python3 -I packaging/windows-msi/build_msi.py fetch-tools --dest "$TOOLS"
python3 -I packaging/windows-msi/build_msi.py build --tools "$TOOLS" --payload "$TREE" --version "$VERSION" \
  --out "Metro-$VERSION-windows-x64.msi" --require-file metro-tui.exe          # official
  # preview: --out "Metro-$VERSION-windows-x64.unsigned-preview.msi" --preview [--require-file metro-tui.exe if present]
python3 -I packaging/windows-msi/build_msi.py fixtures --tools "$TOOLS" --dest "$FIXTURES"
pwsh -File packaging/windows-msi/test-msi-windows.ps1 -FixturesDir "$FIXTURES" -Msi "$MSI" -Version "$VERSION" [-RequireFile metro-tui.exe]
```

- `$TREE` is the verified extraction of the final ZIP. For official releases
  it holds the signed EXEs, and its `RELEASE.json` includes the `signing` record.
- This lane relies on these properties of the MSI:
  - It installs exactly the tree, plus the empty `enable-local-runner` marker
    when the local-runner choice is on.
  - Its custom actions only set properties (type 51).
  - It has no services, self-registration, embedded UI or chainer.
  - `INSTALLDIR` can be chosen, and `msiexec /x` removes what the MSI
    installed.

## Repository setup for official releases

Needed only for signed releases. While `packaging/windows-release-signing.txt`
says `unsigned`, none of this is read.

All of these are **variables**, not secrets. No secret is needed.

### Repository variables

These have to be repository variables because they're read before a job
starts:

- `METRO_WINDOWS_SIGNING_BACKEND`: `azure` or `kms`.
- `METRO_WINDOWS_SIGNING_RUNNER`: for `kms` only. A JSON label array for the
  restricted runner, for example
  `["self-hosted","windows","x64","metro-release-signing"]`.
- `METRO_WINDOWS_PUBLISHER_CN`: the expected signer CN, exactly as the issued
  certificate shows it. Set it once identity validation is done. It's the
  organization's or person's validated name, never hard-coded in this
  repository.
- `METRO_WINDOWS_PUBLISHER_ORG` (optional): the expected O.

### Environment `windows-release-signing`

- **Deployment branches and tags:** selected branches only, exactly `main`,
  with no tag rules. Protect `main` with a ruleset (pull requests required,
  no force pushes, no deletion).
- **Name:** the environment name stays `windows-release-signing`, so the
  Azure federated subject doesn't change.
- **Required reviewers:** recommended. The environment gates both `sign` and
  `sign-msi`, so each release asks for approval twice.
- **Variables for the `azure` backend:**
  - `METRO_ARTIFACT_SIGNING_ENDPOINT`: the regional
    `https://<region>.codesigning.azure.net`, which must match the account's
    region.
  - `METRO_ARTIFACT_SIGNING_ACCOUNT`
  - `METRO_ARTIFACT_SIGNING_PROFILE`: a **Public Trust** certificate profile,
    never Private Trust or Test.
  - `METRO_AZURE_CLIENT_ID` and `METRO_AZURE_TENANT_ID`: the app registration
    or user-assigned identity that holds the *Artifact Signing Certificate
    Profile Signer* role on that profile.
  - `METRO_ARTIFACT_SIGNING_CLIENT_VERSION` and
    `METRO_ARTIFACT_SIGNING_CLIENT_SHA256`: the `Microsoft.ArtifactSigning.Client`
    NuGet version, and the SHA-256 of its `.nupkg`, computed by a maintainer.
- **Variables for the `kms` backend:**
  - `METRO_WINDOWS_SIGNING_CERT_SHA1`: the certificate thumbprint.
  - `METRO_WINDOWS_SIGNING_CERT_STORE`: `CurrentUser` or `LocalMachine`.
  - `METRO_WINDOWS_TIMESTAMP_URL`: the certificate authority's RFC 3161
    SHA-256 timestamp URL.

### Backend `azure`: Azure Artifact Signing, public trust

This backend runs on hosted `windows-2025`. Signing uses Windows SDK
SignTool 10.0.22621.755 or newer with the Artifact Signing dlib
(`/dlib … /dmdf metadata.json`) and Microsoft's timestamp service,
`http://timestamp.acs.microsoft.com`, as Microsoft's
[SignTool integration guide](https://learn.microsoft.com/azure/artifact-signing/how-to-signing-integrations)
describes. Authentication is a GitHub OIDC token exchanged through Entra
workload identity federation. The metadata excludes every credential except
`WorkloadIdentityCredential`, and the token file only exists during the
signing call.

Add a federated credential on the app registration or identity with these
values:

- issuer `https://token.actions.githubusercontent.com`
- audience `api://AzureADTokenExchange`
- subject `repo:<owner>/<repo>:environment:windows-release-signing`

Public Trust needs Microsoft's identity validation, and eligibility depends on
the publisher's country and entity type. **Check whether it's available to
you before you choose this backend.** Nothing here assumes it is.

### Backend `kms`: a standard CA certificate in an HSM or KSP, on a restricted self-hosted runner

This backend works with any publicly trusted code-signing certificate whose
key is non-exportable in an HSM, a token or a cloud KMS that exposes a Windows
CNG KSP. Under the CA/Browser Forum baseline requirements, new code-signing
keys must be in such hardware anyway. SignTool selects the certificate by
thumbprint (`/sha1`, plus `/sm` for the machine store). SHA-256 file digest
and an RFC 3161 SHA-256 timestamp are required.

Runner requirements:

- Windows x64 with PowerShell 7, Python 3.11 or newer, Windows SDK SignTool
  10.0.22621.755 or newer, and the vendor's KSP or CSP.
- The certificate is installed with its private key only in the HSM or KSP.
- The runner is in a runner group that's limited to this repository and to
  the workflow `wnzn/metro/.github/workflows/windows-sign.yml@refs/heads/main`.
  Check how your GitHub plan matches reusable workflows. The label is unique
  to that group.
- Ephemeral or `--ephemeral` registration is recommended. No other jobs run on
  it, it has no inbound access, and it never runs PR jobs.

Self-signed certificates are refused by both backends and by the verifiers.
Test certificates appear only in the preview self-test.

### Which backend to choose

Azure Artifact Signing has no hardware to run and no key that can leave the
service. Choose it when the publisher passes Public Trust identity
validation. If the publisher can't enroll (for example, because of country or
entity-type limits), use `kms` with a certificate from a CA that does issue to
you. The workflow, the verifiers and the release output are the same for both.

## Not covered or not verified yet

- **Releases ship Windows unsigned for now.** The unsigned release path is
  covered by the contract tests in `packaging/` and `test_winsign.py`; it
  runs for real only on a version bump pushed to `main`.
- **No real signing has run.** No release certificate exists in this
  repository. The official path, both backends, SignTool's handling of the LF
  ASCII `.ps1` files, and the trust checks on a hosted VM have never been run.
  The first official `main` release will be the first real run, and the
  preview self-test is the only native SignTool exercise before then.
- **Windows SmartScreen reputation** isn't something CI can check.
- **Native Windows hasn't run yet.** Neither this lane's Windows steps nor
  the MSI lane's `test-msi-windows.ps1` have run on Windows so far.
