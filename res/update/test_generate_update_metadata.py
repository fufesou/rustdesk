import base64
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ed25519

SCRIPT = Path(__file__).with_name("generate_update_metadata.py")
OFFICIAL_RELEASE_BASE_URL = "https://github.com/rustdesk/rustdesk/releases/download"


class GenerateUpdateMetadataTest(unittest.TestCase):
    def setUp(self):
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
        self.trusted_keys = [
            {"key_id": "previous", "algorithm": "ed25519",
             "public_key": base64.b64encode(bytes(32)).decode("ascii")},
            {"key_id": "test-rotated-key", "algorithm": "ed25519",
             "public_key": self.public_key},
        ]

    def tearDown(self):
        self.temp_dir.cleanup()

    def run_script(self, *args, seed=None, trusted_keys=None):
        env = os.environ.copy()
        env.pop("RUSTDESK_UPDATE_ED25519_SEED", None)
        if seed is not None:
            env["RUSTDESK_UPDATE_ED25519_SEED"] = seed
        env["RUSTDESK_TRUSTED_UPDATE_KEYS"] = (
            json.dumps(self.trusted_keys) if trusted_keys is None else trusted_keys
        )
        return subprocess.run(
            [sys.executable, str(SCRIPT), *args],
            cwd=SCRIPT.parents[2],
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
        package_id="rustdesk",
        release_base_url=OFFICIAL_RELEASE_BASE_URL,
        seed=None,
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
                "--package-id",
                package_id,
                "--release-base-url",
                release_base_url,
                "--published-at",
                "2026-05-14T00:00:00Z",
                "--metadata-out",
                str(metadata),
                "--signature-out",
                str(signature),
            ]
        )
        result = self.run_script(*args, seed=self.seed if seed is None else seed)
        return metadata, signature, result

    def verify(
        self,
        metadata,
        signature,
        artifacts,
        *,
        public_key=None,
        package_id="rustdesk",
        release_base_url=OFFICIAL_RELEASE_BASE_URL,
    ):
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
            "--package-id",
            package_id,
            "--release-base-url",
            release_base_url,
        ]
        for artifact in artifacts:
            args.extend(["--artifact", str(artifact)])
        return self.run_script(
            *args,
            trusted_keys=json.dumps([
                {**self.trusted_keys[-1], "public_key": public_key or self.public_key}
            ]),
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
        signed_data = json.loads(signature.read_text(encoding="utf-8"))
        self.assertEqual(data["signature_key_id"], "test-rotated-key")
        self.assertEqual(signed_data["key_id"], "test-rotated-key")
        self.assertEqual(signed_data["algorithm"], "ed25519")

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
            self.verify(
                metadata,
                signature,
                [artifact],
                public_key=wrong_key,
            ).returncode,
            0,
        )

    def test_sign_rejects_invalid_release_inputs(self):
        artifact = self.artifact()
        spec = [("windows", "x86_64", "exe", artifact)]
        self.assertNotEqual(self.sign(spec, version="1.4.7")[2].returncode, 0)
        for release_id in ("bad/tag", "bad\tid", "bad%zz"):
            self.assertNotEqual(self.sign(spec, release_id=release_id)[2].returncode, 0)
        self.assertNotEqual(self.sign(spec, seed="invalid")[2].returncode, 0)
        duplicate = spec + [("windows", "x86_64", "exe", artifact)]
        self.assertNotEqual(self.sign(duplicate)[2].returncode, 0)

    def test_signs_and_verifies_custom_release_identity(self):
        package_id = "com.example.rustdesk-custom"
        release_base_url = "https://updates.example.com/releases/download"
        artifact = self.artifact("rustdesk-custom-1.4.6-aarch64.dmg", b"dmg")

        metadata, signature, signed = self.sign(
            [("macos", "aarch64", "dmg", artifact)],
            package_id=package_id,
            release_base_url=release_base_url,
        )
        verified = self.verify(
            metadata,
            signature,
            [artifact],
            package_id=package_id,
            release_base_url=release_base_url,
        )

        self.assertEqual(signed.returncode, 0, signed.stderr)
        self.assertEqual(verified.returncode, 0, verified.stderr)
        data = json.loads(metadata.read_text(encoding="utf-8"))
        self.assertEqual(data["package_id"], package_id)
        self.assertEqual(
            data["artifacts"][0]["url"],
            f"{release_base_url}/v1.4.6/{artifact.name}",
        )

    def test_rejects_unsafe_custom_release_policy(self):
        artifact = self.artifact("rustdesk-custom-1.4.6-aarch64.dmg", b"dmg")
        spec = [("macos", "aarch64", "dmg", artifact)]

        for package_id in ("", "com.example/custom", "com.example custom"):
            with self.subTest(package_id=package_id):
                self.assertNotEqual(
                    self.sign(spec, package_id=package_id)[2].returncode,
                    0,
                )
        for release_base_url in (
            "http://updates.example.com/releases",
            "https://user@updates.example.com/releases",
            "https://updates.example.com/releases/",
            "https://updates.example.com/releases?channel=stable",
            "https://updates.example.com/relea\nses",
        ):
            with self.subTest(release_base_url=release_base_url):
                self.assertNotEqual(
                    self.sign(spec, release_base_url=release_base_url)[2].returncode,
                    0,
                )

    def test_checks_selected_public_key(self):
        result = self.run_script("check-key", seed=self.seed)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_check_key_rejects_invalid_or_mismatched_seed(self):
        invalid_seeds = [
            None,
            "invalid",
            base64.b64encode(b"x" * 31).decode("ascii"),
            base64.b64encode(b"x" * 32).decode("ascii"),
        ]

        for seed in invalid_seeds:
            with self.subTest(seed=seed):
                result = self.run_script("check-key", seed=seed)
                self.assertNotEqual(result.returncode, 0)

    def test_check_key_rejects_invalid_or_ambiguous_key_lists(self):
        key = self.trusted_keys[-1]
        invalid_lists = [
            [], None, {}, [{}],
            [{**key, "key_id": ""}], [{**key, "algorithm": "rsa"}],
            [{**key, "public_key": "invalid"}],
            [{**key, "public_key": base64.b64encode(bytes(31)).decode("ascii")}],
            [key, {**key, "key_id": "ambiguous"}],
            [key, {**self.trusted_keys[0], "key_id": key["key_id"]}],
        ]
        for encoded in ["", "invalid"] + [json.dumps(keys) for keys in invalid_lists]:
            with self.subTest(trusted_keys=encoded):
                result = self.run_script(
                    "check-key", seed=self.seed, trusted_keys=encoded,
                )
                self.assertNotEqual(result.returncode, 0)

    def test_verification_rejects_unknown_key_identity(self):
        artifact = self.artifact()
        metadata, signature, signed = self.sign([("windows", "x86_64", "exe", artifact)])
        self.assertEqual(signed.returncode, 0, signed.stderr)
        original = json.loads(signature.read_text(encoding="utf-8"))
        for field, value in [("key_id", "unknown"), ("algorithm", "rsa")]:
            with self.subTest(field=field):
                signature.write_text(json.dumps({**original, field: value}), encoding="utf-8")
                self.assertNotEqual(self.verify(metadata, signature, [artifact]).returncode, 0)

if __name__ == "__main__":
    unittest.main()
