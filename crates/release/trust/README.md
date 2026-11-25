# Vendored Sigstore trust material

Metro verifies release signatures against these files. It doesn't use the
copies built into the sigstore crates, which can change silently on a crate
upgrade. `tests/sigstore_real.rs` pins both SHA-256 values, so a change
always goes through a reviewed pull request.

| File | What | SHA-256 |
| --- | --- | --- |
| `sigstore-trusted-root.json` | Sigstore public-good `trusted_root.json` (Fulcio CAs, Rekor v1 and v2 logs, CT logs, the TSA) | `6494e21ea73fa7ee769f85f57d5a3e6a08725eae1e38c755fc3517c9e6bc0b66` |
| `sigstore-tuf-root.json` | Sigstore public-good TUF `root.json`, version 16 (expires 2027-04-22). It anchors a trust-root refresh from `https://tuf-repo-cdn.sigstore.dev` | `27aea7e372aa7864065174f0661784fcf8b0e28b1dbfebeada31a0f1fb19167f` |

## Where they came from (2026-11-25)

- Both files are byte-identical to the ones in `sigstore-trust-root` 0.17.0
  (`src/trusted_root.json`, `repository/tuf_root.json`).
- `trusted_root.json`'s SHA-256 and length (6787 bytes) equal the
  `trusted_root.json` target in the live TUF repository at that date
  (`timestamp.json` → snapshot 166 → `14.targets.json`).
- `16.root.json` is the newest root on the CDN (`17.root.json` returned 404).
- The published Metro 0.4.0 `SHA256SUMS` bundle verifies against
  `sigstore-trusted-root.json` (`tests/sigstore_real.rs`).

## Refreshing

1. Fetch the current TUF metadata (`timestamp.json` → `<n>.snapshot.json` →
   `<n>.targets.json`), check the target hash and length, and download
   `targets/<sha256>.trusted_root.json`. Better still, use a TUF client such as
   `sigstore-trust-root`'s `update-embedded-roots` example, which verifies the
   chain of signed roots.
2. Replace the file, update the pinned hash in `tests/sigstore_real.rs` and in
   this table, and run `cargo test -p metro-release`. The 0.4.0 fixture must
   still verify.
