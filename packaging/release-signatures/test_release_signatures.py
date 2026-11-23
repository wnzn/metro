"""Release signing contract tests (no Metro release, GitHub credentials or test CA)."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
from urllib.request import urlopen

import release_signatures as release


class ReleaseInputs(unittest.TestCase):
    version = "0.2.0-rc.1"
    revision = "a" * 40

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.assets = root / "release-assets"
        self.images = root / "release-image-digests"
        self.out = root / "signed-release-assets"
        self.assets.mkdir()
        self.images.mkdir()
        for name in release.binaries(self.version) | release.INSTALLERS:
            (self.assets / name).write_bytes(name.encode())
        for name in release.binaries(self.version):
            archive = self.assets / name
            archive.with_name(name + ".sha256").write_bytes(
                f"{release.checksum(archive)}  {name}\n".encode()
            )
        self.recompute_aggregate()
        lines = [f"# Metro container images v{self.version}", f"# Source commit: {self.revision}"]
        for image in ("metro-runner", "metro-server"):
            repo = f"ghcr.io/wnzn/{image}"
            lines.append(f"{repo}:v{self.version}@sha256:{'1' * 64}  linux/amd64,linux/arm64")
            lines.append(f"{repo}@sha256:{'2' * 64}  linux/amd64")
            lines.append(f"{repo}@sha256:{'3' * 64}  linux/arm64")
        (self.images / "IMAGES.txt").write_text("\n".join(lines))

    def recompute_aggregate(self):
        # Exercise the aggregate workflow's format, excluding sidecar rows.
        names = sorted(release.binaries(self.version) | release.INSTALLERS)
        (self.assets / release.MANIFEST).write_text("".join(
            f"{release.checksum(self.assets / name)}  {name}\n" for name in names
        ))

    def prepare(self):
        release.prepare(self.assets, self.images, self.out, self.version, self.revision)

    def test_prepare_covers_all_payload_files_and_preserves_sidecars(self):
        self.prepare()
        release.check_directory(self.out, self.version, self.revision, signed=False)
        manifest = (self.out / release.MANIFEST).read_text()
        self.assertEqual({line.split("  ")[1] for line in manifest.splitlines()},
                         {p.name for p in self.out.iterdir()} - {release.MANIFEST})
        self.assertNotEqual((self.assets / release.MANIFEST).read_text(), manifest)
        for name in release.binaries(self.version):
            sidecar = name + ".sha256"
            self.assertEqual((self.assets / sidecar).read_bytes(), (self.out / sidecar).read_bytes())

    def test_missing_or_extra_platform_or_msi_is_rejected(self):
        msi = self.assets / f"Metro-{self.version}-windows-x64.msi"
        msi.unlink()
        with self.assertRaisesRegex(release.ReleaseError, "missing.*windows-x64.msi"):
            self.prepare()
        msi.write_bytes(msi.name.encode())
        (self.assets / f"Metro-{self.version}-windows-arm64.msi").touch()
        with self.assertRaisesRegex(release.ReleaseError, "unexpected.*windows-arm64.msi"):
            self.prepare()
        self.assertFalse(self.out.exists())

    def test_changed_pre_signing_archive_even_with_recomputed_manifest_is_rejected(self):
        (self.assets / release.direct_archives(self.version)[0]).write_bytes(b"modified")
        self.recompute_aggregate()
        with self.assertRaisesRegex(release.ReleaseError, "pre-signing checksum mismatch"):
            self.prepare()

    def test_changed_installer_is_rejected(self):
        (self.assets / "install-metro-linux.sh").write_bytes(b"modified")
        with self.assertRaisesRegex(release.ReleaseError, "aggregate SHA256SUMS"):
            self.prepare()

    def test_changed_staged_file_is_not_signed(self):
        self.prepare()
        (self.out / "install-metro-linux.sh").write_bytes(b"modified")
        with patch.object(release, "run") as signer:
            with self.assertRaisesRegex(release.ReleaseError, "final SHA256SUMS"):
                release.sign("cosign", self.out, self.version, self.revision)
            signer.assert_not_called()

    def test_wrong_image_revision_or_missing_platform_is_rejected(self):
        images = self.images / "IMAGES.txt"
        original = images.read_text()
        images.write_text(original.replace(self.revision, "b" * 40))
        with self.assertRaisesRegex(release.ReleaseError, "source commit"):
            self.prepare()
        images.write_text(original.rsplit("\n", 1)[0])
        with self.assertRaisesRegex(release.ReleaseError, "image platforms"):
            self.prepare()

    def test_sidecar_symlink_is_rejected(self):
        sidecar = self.assets / (release.direct_archives(self.version)[0] + ".sha256")
        sidecar.unlink()
        sidecar.symlink_to(self.assets / release.MANIFEST)
        with self.assertRaisesRegex(release.ReleaseError, "not a plain file"):
            self.prepare()

    def test_policy_passes_exact_issuer_and_reusable_job_workflow(self):
        self.prepare()
        manifest = self.out / release.MANIFEST
        manifest.with_name(manifest.name + release.BUNDLE).write_bytes(b"bundle")
        with patch.object(release, "run") as verifier, patch.object(release, "check_cert_extensions") as claims:
            release.verify_file("cosign", manifest, self.version)
        self.assertEqual(verifier.call_args.args[0], [
            "cosign", "verify-blob", "--bundle", str(manifest) + release.BUNDLE,
            "--certificate-identity",
            "https://github.com/wnzn/metro/.github/workflows/sign-release-assets.yml@refs/tags/v0.2.0-rc.1",
            "--certificate-oidc-issuer", "https://token.actions.githubusercontent.com", str(manifest),
        ])
        claims.assert_called_once_with(b"bundle", self.version, None)

    def test_tool_failure_does_not_leak_diagnostics(self):
        failure = subprocess.CalledProcessError(1, ["cosign"], b"fake-secret", b"fake-secret")
        with patch.object(subprocess, "run", side_effect=failure):
            with self.assertRaisesRegex(release.ReleaseError, r"cosign sign-blob failed \(exit 1\)$"):
                release.run(["cosign", "sign-blob"])


@unittest.skipUnless(os.getenv("METRO_TEST_COSIGN"), "set METRO_TEST_COSIGN for public Sigstore fixture tests")
class PublicSigstoreBundles(unittest.TestCase):
    """Real public Fulcio/Rekor evidence, never generated with a local test key."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.cosign = os.environ["METRO_TEST_COSIGN"]
        self.version = "2.20.3"
        self.artifact = root / release.MANIFEST
        self.bundle = root / (release.MANIFEST + release.BUNDLE)
        self.good = self.fetch("v2.20.3", "checksums.txt.sigstore.json",
                               "4ce93f9492bc677d56a0fddbea0a1d25fb22509f52ed74e4778c5d055bdefffc")
        self.artifact.write_bytes(self.fetch("v2.20.3", "checksums.txt",
                                           "0cb9bc491aed89a6bed88d89a61cd0bbf97f30d0906ac6e99ecb5b2d9d4d6aaf"))
        self.bundle.write_bytes(self.good)
        policy = patch.multiple(release, REPOSITORY="https://github.com/goreleaser/goreleaser",
                                REPOSITORY_ID="77071454", CALLER_WORKFLOW="release.yml",
                                SIGNER_WORKFLOW="release.yml")
        policy.start()
        self.addCleanup(policy.stop)

    def fetch(self, tag, name, expected_hash):
        url = f"https://github.com/goreleaser/goreleaser/releases/download/{tag}/{name}"
        with urlopen(url, timeout=20) as response:
            raw = response.read(65537)
        self.assertEqual(hashlib.sha256(raw).hexdigest(), expected_hash)
        return raw

    def test_real_cosign_accepts_only_correct_payload_bundle_and_provenance(self):
        release.verify_file(self.cosign, self.artifact, self.version)
        with self.subTest("modified payload"):
            good_artifact = self.artifact.read_bytes()
            self.artifact.write_bytes(good_artifact + b"modified\n")
            with self.assertRaisesRegex(release.ReleaseError, "verify-blob failed"):
                release.verify_file(self.cosign, self.artifact, self.version)
            self.artifact.write_bytes(good_artifact)
        with self.subTest("signature substituted from another real bundle"):
            good = json.loads(self.good)
            other = json.loads(self.fetch("v2.20.2", "checksums.txt.sigstore.json",
                                          "3a10d83f1c2f78ff4691d01c05a831b07d3261f8e0851632454c5619eeef74f8"))
            good["messageSignature"]["signature"] = other["messageSignature"]["signature"]
            self.bundle.write_text(json.dumps(good))
            with self.assertRaisesRegex(release.ReleaseError, "verify-blob failed"):
                release.verify_file(self.cosign, self.artifact, self.version)
            self.bundle.write_bytes(self.good)
        with self.subTest("wrong tag"), self.assertRaisesRegex(release.ReleaseError, "verify-blob failed"):
            release.verify_file(self.cosign, self.artifact, "2.20.2")
        with self.subTest("wrong issuer"), patch.object(release, "ISSUER", "https://oauth2.sigstore.dev/auth"):
            with self.assertRaisesRegex(release.ReleaseError, "verify-blob failed"):
                release.verify_file(self.cosign, self.artifact, self.version)
        with self.subTest("wrong caller"), patch.object(release, "CALLER_WORKFLOW", "ci.yml"):
            with self.assertRaisesRegex(release.ReleaseError, r"wrong value for Fulcio extension .*\.18"):
                release.verify_file(self.cosign, self.artifact, self.version)
        with self.subTest("wrong repository ID"), patch.object(release, "REPOSITORY_ID", "0"):
            with self.assertRaisesRegex(release.ReleaseError, r"wrong value for Fulcio extension .*\.15"):
                release.verify_file(self.cosign, self.artifact, self.version)


if __name__ == "__main__":
    unittest.main()
