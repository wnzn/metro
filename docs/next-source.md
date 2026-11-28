# Metro 0.6.5 (in the source, not released)

- Windows script and MSI installations can download and verify a published
  update in **Settings → About & updates** and install it when you choose
  **Restart now**. Older script installs need one manual update to turn
  this on. Portable ZIP copies still update manually. See
  [Updates](updates.md) for migration, backups and recovery.
  No native Windows run of these update paths is recorded yet.
- An Apple-silicon Mac download, `Metro-X.Y.Z-macos-arm64.tar.gz`, in every
  test build and release, installed with `install-metro-macos.sh`. It's ad
  hoc signed and not notarized, so macOS may refuse to open it, and it
  updates only through the installer. There's no Intel Mac. See
  [macOS](install.md#desktop-on-macos). No CI run of it is recorded yet.
- CI runs its Linux and macOS jobs, release builds included, on two
  self-hosted runners for trusted pushes and pull requests, and on GitHub's
  machines for everything else. Tagging, images, signing and publishing
  stay on GitHub's machines. See
  [Which machines run CI](deploy.md#which-machines-run-ci).
