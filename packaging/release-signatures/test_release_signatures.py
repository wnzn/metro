"""Release signing contract tests (no Metro release, GitHub credentials or test CA)."""

import base64
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import textwrap
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

    def test_release_ships_windows_and_linux_only_while_macos_is_off(self):
        self.assertEqual(release.MACOS_TARGETS, ())
        self.assertEqual(release.binaries(self.version), {
            f"Metro-{self.version}-linux-x64.tar.gz", f"Metro-runner-{self.version}-linux-x64.tar.gz",
            f"Metro-{self.version}-windows-x64.zip", f"Metro-{self.version}-windows-x64.msi"})
        self.assertFalse(any("macos" in name for name in release.INSTALLERS))
        (self.assets / f"Metro-{self.version}-macos-x64.tar.gz").touch()
        with self.assertRaisesRegex(release.ReleaseError, "unexpected.*macos-x64"):
            self.prepare()

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
            release.verify_file("cosign", manifest, self.version, self.revision)
        self.assertEqual(verifier.call_args.args[0], [
            "cosign", "verify-blob", "--bundle", str(manifest) + release.BUNDLE,
            "--certificate-identity",
            "https://github.com/wnzn/metro/.github/workflows/sign-release-assets.yml@refs/heads/main",
            "--certificate-oidc-issuer", "https://token.actions.githubusercontent.com", str(manifest),
        ])
        claims.assert_called_once_with(b"bundle", self.revision)

    def test_verifier_requires_independently_supplied_full_commit(self):
        for revision in (None, "", self.revision[:12], "refs/heads/main"):
            with self.subTest(revision=revision), self.assertRaisesRegex(release.ReleaseError, "full release commit"):
                release.verify_file("cosign", self.out / release.MANIFEST, self.version, revision)

    def test_signed_manifest_version_cannot_be_relabelled(self):
        self.prepare()
        manifest = self.out / release.MANIFEST
        manifest.with_name(manifest.name + release.BUNDLE).write_bytes(b"bundle")
        with patch.object(release, "run"), patch.object(release, "check_cert_extensions"):
            with self.assertRaisesRegex(release.ReleaseError, "another version"):
                release.verify_file("cosign", manifest, "0.3.0", self.revision)

    def test_signed_linux_archive_cannot_be_relabelled(self):
        name = release.direct_archives(self.version)[0]
        artifact = self.assets / name
        archive_metadata = self.assets / "RELEASE.json"
        archive_metadata.write_text(json.dumps({"version": "0.3.0"}))
        with tarfile.open(artifact, "w:gz") as archive:
            archive.add(archive_metadata, arcname=f"{name.removesuffix('.tar.gz')}/RELEASE.json")
        artifact.with_name(name + release.BUNDLE).write_bytes(b"bundle")
        with patch.object(release, "run"), patch.object(release, "check_cert_extensions"):
            with self.assertRaisesRegex(release.ReleaseError, "another version"):
                release.verify_file("cosign", artifact, self.version, self.revision)
            archive_metadata.write_text(json.dumps({"version": self.version}))
            with tarfile.open(artifact, "w:gz") as archive:
                archive.add(archive_metadata, arcname=f"{name.removesuffix('.tar.gz')}/RELEASE.json")
            release.verify_file("cosign", artifact, self.version, self.revision)

    def test_workflow_guard_requires_non_deleted_protected_main_push(self):
        root = Path(__file__).resolve().parents[2]
        workflow = (root / ".github/workflows/sign-release-assets.yml").read_text()
        step = workflow.split("- name: Require Metro CI on a protected main push\n", 1)[1]
        script = textwrap.dedent(step.split("run: |\n", 1)[1].split("      - uses:", 1)[0])
        env = {
            **os.environ, "GITHUB_REPOSITORY": "wnzn/metro", "REPOSITORY_ID": "1467654868",
            "GITHUB_EVENT_NAME": "push", "REF_TYPE": "branch", "GITHUB_REF": "refs/heads/main",
            "WORKFLOW_REF": "wnzn/metro/.github/workflows/ci.yml@refs/heads/main", "REF_PROTECTED": "true",
            "PUSH_DELETED": "false", "VERSION": self.version, "REVISION": self.revision,
            "GITHUB_SHA": self.revision, "PUSH_AFTER": self.revision,
        }
        self.assertEqual(subprocess.run(["bash", "-c", script], env=env, capture_output=True).returncode, 0)
        for changes in (
            {"REF_TYPE": "tag", "GITHUB_REF": "refs/tags/v" + self.version},
            {"GITHUB_REF": "refs/heads/master"}, {"GITHUB_REF": "refs/heads/feature/main"}, {"GITHUB_REF": "refs/heads/Main"},
            {"GITHUB_REF": "refs/heads/master",
             "WORKFLOW_REF": "wnzn/metro/.github/workflows/ci.yml@refs/heads/master"},
            {"WORKFLOW_REF": "wnzn/metro/.github/workflows/ci.yml@refs/heads/master"},
            {"GITHUB_EVENT_NAME": "pull_request"}, {"GITHUB_EVENT_NAME": "workflow_dispatch"},
            {"REF_PROTECTED": "false"}, {"PUSH_DELETED": "true"}, {"REPOSITORY_ID": "0"},
            {"GITHUB_REPOSITORY": "somebody/metro"}, {"REVISION": "b" * 40}, {"PUSH_AFTER": "b" * 40},
            {"WORKFLOW_REF": "wnzn/metro/.github/workflows/other.yml@refs/heads/main"},
        ):
            with self.subTest(changes=changes):
                result = subprocess.run(["bash", "-c", script], env=env | changes, capture_output=True)
                self.assertNotEqual(result.returncode, 0)

    def test_resolve_exact_public_tag_and_peel_annotated_tag(self):
        ref = "refs/tags/v0.2.0-rc.1"
        object_id = "b" * 40
        output = f"{object_id}\t{ref}\n{self.revision}\t{ref}^{{}}\n".encode()
        with patch.object(subprocess, "run", return_value=subprocess.CompletedProcess([], 0, output)) as git:
            self.assertEqual(release.resolve_tag(self.version), self.revision)
        self.assertEqual(git.call_args.args[0],
                         ["git", "ls-remote", "--exit-code", "https://github.com/wnzn/metro.git", ref, ref + "^{}"])
        with patch.object(subprocess, "run", return_value=subprocess.CompletedProcess([], 0, output.splitlines()[0])):
            self.assertEqual(release.resolve_tag(self.version), object_id)
        with patch.object(subprocess, "run", return_value=subprocess.CompletedProcess([], 2, b"")):
            with self.assertRaisesRegex(release.ReleaseError, "unable to resolve"):
                release.resolve_tag(self.version)
        output += f"{object_id}\t{ref}-wrong\n".encode()
        with patch.object(subprocess, "run", return_value=subprocess.CompletedProcess([], 0, output)):
            with self.assertRaisesRegex(release.ReleaseError, "unexpected response"):
                release.resolve_tag(self.version)

    def test_main_claims_require_the_pinned_commit_in_all_three_digest_fields(self):
        # Only exercise the parser here: an OpenSSL test certificate is never
        # passed through the cosign verifier or trusted as production evidence.
        claims = {
            9: "https://github.com/wnzn/metro/.github/workflows/sign-release-assets.yml@refs/heads/main",
            10: self.revision, 12: "https://github.com/wnzn/metro", 13: self.revision,
            14: "refs/heads/main", 15: "1467654868",
            18: "https://github.com/wnzn/metro/.github/workflows/ci.yml@refs/heads/main",
            19: self.revision, 20: "push",
        }
        for bad_claim, bad_value in ((None, None), (10, "b" * 40), (13, "b" * 40), (19, "b" * 40),
                                     (14, "refs/heads/master"), (9, claims[9].replace("refs/heads/main", "refs/heads/master")),
                                     (18, claims[18].replace("refs/heads/main", "refs/heads/master")),
                                     (18, claims[18].replace("ci.yml", "other.yml"))):
            with self.subTest(claim=bad_claim):
                raw = claims if bad_claim is None else claims | {bad_claim: bad_value}
                cmd = ["openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256",
                       "-nodes", "-keyout", os.devnull, "-outform", "DER", "-subj", "/CN=test-only", "-days", "1"]
                for oid, value in raw.items():
                    cmd.extend(["-addext", f"1.3.6.1.4.1.57264.1.{oid}=ASN1:UTF8String:{value}"])
                cert = subprocess.run(cmd, capture_output=True, check=True).stdout
                bundle = json.dumps({"mediaType": "application/vnd.dev.sigstore.bundle.v0.3+json",
                                     "verificationMaterial": {"certificate": {"rawBytes":
                                         base64.b64encode(cert).decode("ascii")}}})
                if bad_claim is None:
                    release.check_cert_extensions(bundle, self.revision)
                else:
                    with self.assertRaisesRegex(release.ReleaseError, rf"wrong value for Fulcio extension .*\.{bad_claim}$"):
                        release.check_cert_extensions(bundle, self.revision)

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

    def fetch(self, tag, name, expected_hash):
        url = f"https://github.com/goreleaser/goreleaser/releases/download/{tag}/{name}"
        with urlopen(url, timeout=20) as response:
            raw = response.read(65537)
        self.assertEqual(hashlib.sha256(raw).hexdigest(), expected_hash)
        return raw

    def test_valid_third_party_tag_bundle_does_not_pass_metro_main_policy(self):
        identity = "https://github.com/goreleaser/goreleaser/.github/workflows/release.yml@refs/tags/v2.20.3"
        issuer = "https://token.actions.githubusercontent.com"

        def verify_public(expected_identity=identity, expected_issuer=issuer):
            release.run([self.cosign, "verify-blob", "--bundle", str(self.bundle),
                         "--certificate-identity", expected_identity, "--certificate-oidc-issuer", expected_issuer,
                         str(self.artifact)])

        verify_public()
        with self.assertRaisesRegex(release.ReleaseError, "verify-blob failed"):
            release.verify_file(self.cosign, self.artifact, self.version, "a6edafaafe0fd15a4725e917beac26305ba67738")
        with self.subTest("modified payload"):
            good_artifact = self.artifact.read_bytes()
            self.artifact.write_bytes(good_artifact + b"modified\n")
            with self.assertRaisesRegex(release.ReleaseError, "verify-blob failed"):
                verify_public()
            self.artifact.write_bytes(good_artifact)
        with self.subTest("signature substituted from another real bundle"):
            good = json.loads(self.good)
            other = json.loads(self.fetch("v2.20.2", "checksums.txt.sigstore.json",
                                          "3a10d83f1c2f78ff4691d01c05a831b07d3261f8e0851632454c5619eeef74f8"))
            good["messageSignature"]["signature"] = other["messageSignature"]["signature"]
            self.bundle.write_text(json.dumps(good))
            with self.assertRaisesRegex(release.ReleaseError, "verify-blob failed"):
                verify_public()
            self.bundle.write_bytes(self.good)
        with self.subTest("wrong tag"), self.assertRaisesRegex(release.ReleaseError, "verify-blob failed"):
            verify_public(expected_identity=identity.replace("v2.20.3", "v2.20.2"))
        with self.subTest("wrong issuer"):
            with self.assertRaisesRegex(release.ReleaseError, "verify-blob failed"):
                verify_public(expected_issuer="https://oauth2.sigstore.dev/auth")


if __name__ == "__main__":
    unittest.main()
