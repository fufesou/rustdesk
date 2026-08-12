import base64
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("generate_update_metadata.py")


class GenerateUpdateMetadataTest(unittest.TestCase):
    def setUp(self):
        from cryptography.hazmat.primitives import serialization
        from cryptography.hazmat.primitives.asymmetric import ed25519

        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = Path(self.temp_dir.name)
        private_key = ed25519.Ed25519PrivateKey.generate()
        seed = private_key.private_bytes(
            serialization.Encoding.Raw,
            serialization.PrivateFormat.Raw,
            serialization.NoEncryption(),
        )
        public_key = private_key.public_key().public_bytes(
            serialization.Encoding.Raw,
            serialization.PublicFormat.Raw,
        )
        self.seed = base64.b64encode(seed).decode("ascii")
        self.public_key = base64.b64encode(public_key).decode("ascii")

    def tearDown(self):
        self.temp_dir.cleanup()

    def run_script(self, *args, seed=None, public_key=None, repository=None):
        env = os.environ.copy()
        env.pop("RUSTDESK_UPDATE_ED25519_SEED", None)
        env.pop("RUSTDESK_UPDATE_ED25519_PUBLIC_KEY", None)
        env.pop("RUSTDESK_UPDATE_GITHUB_REPOSITORY", None)
        if seed is not None:
            env["RUSTDESK_UPDATE_ED25519_SEED"] = seed
        if public_key is not None:
            env["RUSTDESK_UPDATE_ED25519_PUBLIC_KEY"] = public_key
        if repository is not None:
            env["RUSTDESK_UPDATE_GITHUB_REPOSITORY"] = repository
        return subprocess.run(
            [sys.executable, str(SCRIPT), *args],
            cwd=SCRIPT.parents[1],
            env=env,
            text=True,
            capture_output=True,
        )

    def artifact(self, name="rustdesk-1.4.6-x86_64.exe", data=b"rustdesk"):
        path = self.root / name
        path.write_bytes(data)
        return path

    def sign(
        self,
        artifacts,
        *,
        version="1.4.6",
        release_id="v1.4.6",
        seed=None,
        repository=None,
    ):
        metadata = self.root / "rustdesk-update.json"
        signature = self.root / "rustdesk-update.json.sig"
        args = ["sign"]
        for platform, arch, file_format, path in artifacts:
            args.extend(["--artifact", platform, arch, file_format, str(path)])
        args.extend(
            [
                "--version",
                version,
                "--release-id",
                release_id,
                "--published-at",
                "2026-05-14T00:00:00Z",
                "--metadata-out",
                str(metadata),
                "--signature-out",
                str(signature),
            ]
        )
        result = self.run_script(
            *args,
            seed=self.seed if seed is None else seed,
            repository=repository,
        )
        return metadata, signature, result

    def verify(self, metadata, signature, artifacts, public_key=None):
        args = [
            "verify",
            "--metadata",
            str(metadata),
            "--signature",
            str(signature),
            "--version",
            "1.4.6",
            "--release-id",
            "v1.4.6",
        ]
        for artifact in artifacts:
            args.extend(["--artifact", str(artifact)])
        return self.run_script(
            *args,
            public_key=public_key or self.public_key,
        )

    def test_signs_and_verifies_release_artifacts(self):
        exe = self.artifact()
        dmg = self.artifact("rustdesk-1.4.6-aarch64.dmg", b"dmg")
        specs = [("windows", "x86_64", "exe", exe), ("macos", "aarch64", "dmg", dmg)]

        metadata, signature, signed = self.sign(specs)
        verified = self.verify(metadata, signature, [exe, dmg])

        self.assertEqual(signed.returncode, 0, signed.stderr)
        self.assertEqual(verified.returncode, 0, verified.stderr)
        data = json.loads(metadata.read_text(encoding="utf-8"))
        self.assertEqual(data["release_id"], "v1.4.6")
        self.assertEqual({item["file_name"] for item in data["artifacts"]}, {exe.name, dmg.name})

    def test_sign_uses_configured_github_repository(self):
        artifact = self.artifact()
        metadata, _, signed = self.sign(
            [("windows", "x86_64", "exe", artifact)],
            release_id="fix-update-metadata",
            repository="fufesou/rustdesk",
        )

        self.assertEqual(signed.returncode, 0, signed.stderr)
        data = json.loads(metadata.read_text(encoding="utf-8"))
        self.assertEqual(
            data["artifacts"][0]["url"],
            "https://github.com/fufesou/rustdesk/releases/download/"
            "fix-update-metadata/rustdesk-1.4.6-x86_64.exe",
        )

    def test_sign_rejects_invalid_github_repository(self):
        artifact = self.artifact()
        _, _, signed = self.sign(
            [("windows", "x86_64", "exe", artifact)],
            repository="../rustdesk",
        )

        self.assertNotEqual(signed.returncode, 0)
        self.assertIn("invalid GitHub repository", signed.stderr)

    def test_verification_rejects_tampering(self):
        artifact = self.artifact()
        metadata, signature, _ = self.sign([("windows", "x86_64", "exe", artifact)])
        artifact.write_bytes(b"tampered")
        self.assertNotEqual(self.verify(metadata, signature, [artifact]).returncode, 0)

        artifact.write_bytes(b"rustdesk")
        data = json.loads(metadata.read_text(encoding="utf-8"))
        data["published_at"] = "2026-05-15T00:00:00Z"
        metadata.write_text(json.dumps(data), encoding="utf-8")
        self.assertNotEqual(self.verify(metadata, signature, [artifact]).returncode, 0)

    def test_verification_rejects_wrong_public_key(self):
        artifact = self.artifact()
        metadata, signature, _ = self.sign([("windows", "x86_64", "exe", artifact)])
        wrong_key = base64.b64encode(b"x" * 32).decode("ascii")
        self.assertNotEqual(
            self.verify(metadata, signature, [artifact], wrong_key).returncode,
            0,
        )

    def test_sign_rejects_invalid_release_inputs(self):
        artifact = self.artifact()
        spec = [("windows", "x86_64", "exe", artifact)]
        self.assertNotEqual(self.sign(spec, version="1.4.7")[2].returncode, 0)
        self.assertNotEqual(self.sign(spec, release_id="bad/tag")[2].returncode, 0)
        self.assertNotEqual(self.sign(spec, seed="invalid")[2].returncode, 0)
        duplicate = spec + [("windows", "x86_64", "exe", artifact)]
        self.assertNotEqual(self.sign(duplicate)[2].returncode, 0)

    def test_checks_embedded_public_key(self):
        key_bytes = base64.b64decode(self.public_key)
        source = self.root / "update_metadata.rs"
        source.write_text(
            'TrustedUpdateKey { key_id: "2026-ed25519-main", public_key: ['
            + ",".join(str(byte) for byte in key_bytes)
            + "] }",
            encoding="utf-8",
        )
        result = self.run_script(
            "check-key",
            "--rust-source",
            str(source),
            public_key=self.public_key,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_release_workflows_use_current_metadata_cli(self):
        workflows = [
            SCRIPT.parents[1] / ".github/workflows/flutter-build.yml",
            SCRIPT.parents[1]
            / ".github/workflows/complete-test-update-release.yml",
        ]
        for workflow in workflows:
            content = workflow.read_text(encoding="utf-8")
            with self.subTest(workflow=workflow.name):
                self.assertNotIn("generate_update_metadata.py fragment", content)
                self.assertNotIn("--fragment", content)
                self.assertIn("generate_update_metadata.py sign", content)
                self.assertIn("generate_update_metadata.py verify", content)


class FlutterBuildWorkflowTest(unittest.TestCase):
    def test_uploads_artifacts_from_merged_download_directory(self):
        workflow = SCRIPT.parents[1] / ".github/workflows/flutter-build.yml"
        content = workflow.read_text(encoding="utf-8")
        upload_step = content.split(
            "      - name: Upload signed update artifacts", 1
        )[1].split("      - name: Upload signed update metadata signature", 1)[0]
        expected_files = (
            "./artifacts/rustdesk-${{ env.VERSION }}-x86_64.exe",
            "./artifacts/rustdesk-${{ env.VERSION }}-x86_64.msi",
            "./artifacts/rustdesk-${{ env.VERSION }}-aarch64.exe",
            "./artifacts/rustdesk-${{ env.VERSION }}-aarch64.msi",
            "./artifacts/rustdesk-${{ env.VERSION }}-x86-sciter.exe",
            "./artifacts/rustdesk-${{ env.VERSION }}-aarch64.dmg",
            "./artifacts/rustdesk-${{ env.VERSION }}-x86_64.dmg",
        )

        for artifact in expected_files:
            with self.subTest(artifact=artifact):
                self.assertIn(artifact, upload_step)
        self.assertNotIn("./artifacts/windows-", upload_step)
        self.assertNotIn("./artifacts/macos-", upload_step)


if __name__ == "__main__":
    unittest.main()
