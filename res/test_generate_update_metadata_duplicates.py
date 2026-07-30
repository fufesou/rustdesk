import base64
import json
import unittest

from cryptography.hazmat.primitives.asymmetric import ed25519

import generate_update_metadata as update_metadata
import test_generate_update_metadata as shared


class VerifyDuplicateArtifactsTest(unittest.TestCase):
    def setUp(self):
        self.helper = shared.GenerateUpdateMetadataTest(
            "test_fragment_writes_file_name_size_sha256_and_creates_parent"
        )
        self.helper.setUp()

    def tearDown(self):
        self.helper.tearDown()

    def _resign_metadata(self, metadata, signature, metadata_data):
        metadata_bytes = update_metadata.write_stable_json(metadata, metadata_data)
        private_key = ed25519.Ed25519PrivateKey.from_private_bytes(
            base64.b64decode(self.helper.seed_b64, validate=True)
        )
        signature_data = json.loads(signature.read_text(encoding="utf-8"))
        signature_data["signature"] = base64.b64encode(
            private_key.sign(update_metadata.SIGNATURE_CONTEXT + metadata_bytes)
        ).decode("ascii")
        update_metadata.write_stable_json(signature, signature_data)

    def test_verify_rejects_duplicate_local_artifact_basenames(self):
        artifact, fragment = self.helper.make_fragment()
        metadata, signature, _ = self.helper.run_sign([fragment])
        duplicate_dir = self.helper.root / "duplicate"
        duplicate_dir.mkdir()
        duplicate = duplicate_dir / artifact.name
        duplicate.write_bytes(artifact.read_bytes())

        result = self.helper.verify_release(
            metadata, signature, [artifact, duplicate], check=False
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("duplicate local artifact basename", result.stderr)

    def test_sign_rejects_duplicate_metadata_file_names(self):
        artifact, fragment = self.helper.make_fragment()
        _, duplicate_fragment = self.helper.make_fragment(
            artifact_name=artifact.name,
            arch="aarch64",
            out_name="duplicate.json",
        )
        _, _, result = self.helper.run_sign(
            [fragment, duplicate_fragment], check=False
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("duplicate metadata artifact file_name", result.stderr)

    def test_verify_rejects_duplicate_metadata_file_names(self):
        artifact, fragment = self.helper.make_fragment()
        metadata, signature, _ = self.helper.run_sign([fragment])
        metadata_data = json.loads(metadata.read_text(encoding="utf-8"))
        metadata_data["artifacts"].append(
            dict(metadata_data["artifacts"][0], arch="aarch64")
        )
        self._resign_metadata(metadata, signature, metadata_data)

        result = self.helper.verify_release(
            metadata, signature, [artifact], check=False
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("duplicate metadata artifact file_name", result.stderr)

    def test_verify_rejects_duplicate_artifact_selectors(self):
        artifact_one, fragment_one = self.helper.make_fragment(out_name="one.json")
        artifact_two, fragment_two = self.helper.make_fragment(
            artifact_name="rustdesk-1.4.6-aarch64.exe",
            arch="aarch64",
            out_name="two.json",
        )
        metadata, signature, _ = self.helper.run_sign([fragment_one, fragment_two])
        metadata_data = json.loads(metadata.read_text(encoding="utf-8"))
        first, second = metadata_data["artifacts"]
        metadata_data["artifacts"] = [
            first,
            dict(
                second,
                platform=first["platform"],
                arch=first["arch"],
                format=first["format"],
            ),
        ]
        self._resign_metadata(metadata, signature, metadata_data)

        result = self.helper.verify_release(
            metadata, signature, [artifact_one, artifact_two], check=False
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("duplicate artifact selector", result.stderr)


if __name__ == "__main__":
    unittest.main()
