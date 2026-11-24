# Metro release signatures

Only a nondeleted push of the pinned release commit to protected
`refs/heads/main` in `wnzn/metro` (repository ID `1467654868`) may sign.
The top-level workflow must be `.github/workflows/ci.yml`. A release tag
is a public version-to-commit lookup, not a workflow trigger. The preceding
release gate must pin `v<version>` to the triggering commit and pass that
commit as `revision` to `sign-release-assets.yml` along with `version`
(without `v`). The signer independently checks the exact public tag,
including the peeled commit for an annotated tag. Protect `main` and
configure an immutable `v*` tag ruleset (restrict tag creation; prevent
updates and deletion). Looking up a tag alone does not prove it is immutable.

The Fulcio certificate must have the GitHub OIDC issuer and the exact
reusable signer URI:

`https://github.com/wnzn/metro/.github/workflows/sign-release-assets.yml@refs/heads/main`

The verifier additionally checks the source repository URI and ID, the
source ref, the push trigger, and the top-level CI workflow URI. The
source commit, reusable signing workflow commit, and top-level CI workflow
commit must **all** equal the independently trusted SHA for the release.
Checking just the issuer and signer URI with cosign does not enforce this
last requirement. No tag-push signature is accepted.

From a trusted copy of the verifier, with Python 3.11+, OpenSSL, Git, and
cosign 3.1.4 on `PATH`, verify the complete download directory:

```sh
set -eu
version=0.2.0  # Replace with the exact release version; omit the leading v.
verifier=/path/to/trusted/metro/packaging/release-signatures/release_signatures.py
revision="$(python3 -I "$verifier" resolve-tag --version "$version")"
python3 -I "$verifier" verify --version "$version" --revision "$revision" \
  --directory /path/to/downloads
```

For just one Linux tarball, use `verify-file` with the same `--version` and
`--revision`, followed by the archive path. Keep its `.sigstore.json` bundle
next to it. The verifier checks the embedded release version as well as the
certificate. For other assets, use `verify-file` on `SHA256SUMS` and check
the downloaded asset against its row in that authenticated manifest; the
full-directory `verify` command does both checks for all assets.

Without a network connection, supply a previously recorded, independently
trusted full release commit SHA instead of `resolve-tag`. Do not use the
current tip of `main`, or a SHA read from the bundle or `IMAGES.txt`, as the
expected commit. Offline cosign also needs already provisioned Sigstore trust
material; bundled log evidence is not a trust root.
