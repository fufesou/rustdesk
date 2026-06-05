import base64
import hashlib
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
GITHUB_RELEASE = "https://github.com/rustdesk/rustdesk/releases/download"
class GenerateUpdateMetadataTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.seed_b64, self.public_b64 = self.generate_keypair()
    def tearDown(self):
        self.tmp.cleanup()
    def generate_keypair(self):
        private_key = ed25519.Ed25519PrivateKey.generate()
        seed = private_key.private_bytes(
            encoding=serialization.Encoding.Raw,
            format=serialization.PrivateFormat.Raw,
            encryption_algorithm=serialization.NoEncryption(),
        )
        public_key = private_key.public_key().public_bytes(
            encoding=serialization.Encoding.Raw,
            format=serialization.PublicFormat.Raw,
        )
        return base64.b64encode(seed).decode("ascii"), base64.b64encode(public_key).decode("ascii")
    def run_script(self, *args, env=None, check=True):
        merged_env = os.environ.copy()
        if env:
            merged_env.update(env)
        result = subprocess.run(
            [sys.executable, str(SCRIPT), *args],
            cwd=Path(__file__).parents[1],
            env=merged_env,
            text=True,
            capture_output=True,
        )
        if check and result.returncode != 0:
            self.fail(f"command failed: {result.args}\nstdout={result.stdout}\nstderr={result.stderr}")
        return result
    def write_artifact(self, name, content=b"rustdesk"):
        path = self.root / name
        path.write_bytes(content)
        return path
    def fragment_path(self, name):
        return self.root / "fragments" / name
    def make_fragment(
        self,
        artifact_name="rustdesk-1.4.6-x86_64.exe",
        content=b"rustdesk",
        platform="windows",
        arch="x86_64",
        file_format="exe",
        release_id="v1.4.6",
        out_name="fragment.json",
    ):
        artifact = self.write_artifact(artifact_name, content)
        out = self.fragment_path(out_name)
        args = [
            "fragment",
            "--artifact",
            str(artifact),
            "--artifact-url",
            f"{GITHUB_RELEASE}/{release_id}/{artifact.name}",
            "--platform",
            platform,
            "--arch",
            arch,
            "--format",
            file_format,
            "--fragment-out",
            str(out),
        ]
        self.run_script(*args)
        return artifact, out
    def sign_fragments(self, fragments, metadata=None, signature=None, release_id="v1.4.6"):
        metadata = metadata or (self.root / "rustdesk-update.json")
        signature = signature or (self.root / "rustdesk-update.json.sig")
        args = ["sign"]
        for fragment in fragments:
            args.extend(["--fragment", str(fragment)])
        args.extend(
            [
                "--package-id",
                "rustdesk",
                "--version",
                "1.4.6",
                "--release-id",
                release_id,
                "--published-at",
                "2026-05-14T00:00:00Z",
                "--key-id",
                "test-ed25519-main",
                "--private-key-seed-env",
                "RUSTDESK_UPDATE_ED25519_SEED",
                "--metadata-out",
                str(metadata),
                "--signature-out",
                str(signature),
            ]
        )
        self.run_script(*args, env={"RUSTDESK_UPDATE_ED25519_SEED": self.seed_b64})
        return metadata, signature
    def verify_release(self, metadata, signature, artifacts, version="1.4.6", release_id="v1.4.6", public_key=None, key_id="test-ed25519-main", check=True):
        args = [
            "verify",
            "--metadata",
            str(metadata),
            "--signature",
            str(signature),
            "--package-id",
            "rustdesk",
            "--version",
            version,
            "--release-id",
            release_id,
            "--trusted-public-key-base64",
            public_key or self.public_b64,
            "--trusted-public-key-id",
            key_id,
        ]
        for artifact in artifacts:
            args.extend(["--artifact", str(artifact)])
        return self.run_script(*args, check=check)
    def test_fragment_writes_file_name_size_sha256_and_creates_parent(self):
        artifact, fragment = self.make_fragment(content=b"artifact bytes", out_name="nested/fragment.json")
        data = json.loads(fragment.read_text(encoding="utf-8"))
        self.assertEqual(data["file_name"], artifact.name)
        self.assertEqual(data["size"], len(b"artifact bytes"))
        self.assertEqual(data["sha256"], hashlib.sha256(b"artifact bytes").hexdigest())
        self.assertEqual(data["platform"], "windows")
        self.assertEqual(data["arch"], "x86_64")
        self.assertEqual(data["format"], "exe")
    def test_sign_sorts_fragments_and_outputs_base64_signature(self):
        _, mac_fragment = self.make_fragment(
            "rustdesk-1.4.6-aarch64.dmg",
            platform="macos",
            arch="aarch64",
            file_format="dmg",
            out_name="mac.json",
        )
        _, win_fragment = self.make_fragment(out_name="win.json")
        metadata, signature = self.sign_fragments([win_fragment, mac_fragment])
        metadata_json = json.loads(metadata.read_text(encoding="utf-8"))
        signature_json = json.loads(signature.read_text(encoding="utf-8"))
        artifact_keys = [
            (artifact["platform"], artifact["arch"], artifact["format"])
            for artifact in metadata_json["artifacts"]
        ]
        self.assertEqual(artifact_keys, sorted(artifact_keys))
        decoded_signature = base64.b64decode(signature_json["signature"], validate=True)
        self.assertEqual(len(decoded_signature), 64)
        self.assertEqual(base64.b64encode(decoded_signature).decode("ascii"), signature_json["signature"])
    def test_verify_fails_after_artifact_tamper_or_missing_local_artifact(self):
        artifact, fragment = self.make_fragment()
        metadata, signature = self.sign_fragments([fragment])
        ok = self.verify_release(metadata, signature, [artifact])
        self.assertEqual(ok.returncode, 0)
        artifact.write_bytes(b"tampered")
        tampered = self.verify_release(metadata, signature, [artifact], check=False)
        self.assertNotEqual(tampered.returncode, 0)
        missing = self.verify_release(metadata, signature, [], check=False)
        self.assertNotEqual(missing.returncode, 0)
    def test_verify_rejects_metadata_version_release_key_and_public_key_mismatch(self):
        artifact, fragment = self.make_fragment()
        metadata, signature = self.sign_fragments([fragment])
        self.assertNotEqual(self.verify_release(metadata, signature, [artifact], version="1.4.7", check=False).returncode, 0)
        self.assertNotEqual(self.verify_release(metadata, signature, [artifact], release_id="v1.4.7", check=False).returncode, 0)
        wrong_seed, wrong_public = self.generate_keypair()
        self.assertNotEqual(self.verify_release(metadata, signature, [artifact], public_key=wrong_public, check=False).returncode, 0)
        self.assertNotEqual(self.verify_release(metadata, signature, [artifact], key_id="other-key", check=False).returncode, 0)
        self.assertNotEqual(wrong_seed, self.seed_b64)
    def test_sign_rejects_cross_release_or_basename_mismatch_urls(self):
        artifact, fragment = self.make_fragment()
        data = json.loads(fragment.read_text(encoding="utf-8"))
        for bad_url in [
            f"{GITHUB_RELEASE}/v1.4.7/{artifact.name}",
            f"https://github.com/other/rustdesk/releases/download/v1.4.6/{artifact.name}",
            f"{GITHUB_RELEASE}/v1.4.6/other.exe",
            f"{GITHUB_RELEASE}/v1.4.6//{artifact.name}",
        ]:
            bad_fragment = self.fragment_path(f"bad-{hashlib.sha1(bad_url.encode()).hexdigest()}.json")
            mutated = dict(data)
            mutated["url"] = bad_url
            bad_fragment.write_text(json.dumps(mutated), encoding="utf-8")
            result = self.run_script(
                "sign",
                "--fragment",
                str(bad_fragment),
                "--package-id",
                "rustdesk",
                "--version",
                "1.4.6",
                "--release-id",
                "v1.4.6",
                "--published-at",
                "2026-05-14T00:00:00Z",
                "--key-id",
                "test-ed25519-main",
                "--private-key-seed-env",
                "RUSTDESK_UPDATE_ED25519_SEED",
                "--metadata-out",
                str(self.root / "metadata.json"),
                "--signature-out",
                str(self.root / "signature.json"),
                env={"RUSTDESK_UPDATE_ED25519_SEED": self.seed_b64},
                check=False,
            )
            self.assertNotEqual(result.returncode, 0, bad_url)
    def test_fragment_rejects_dot_segment_release_or_file_name(self):
        artifact = self.write_artifact("rustdesk.exe")
        for bad_url in [
            f"{GITHUB_RELEASE}/./{artifact.name}",
            f"{GITHUB_RELEASE}/../{artifact.name}",
            f"{GITHUB_RELEASE}/v1.4.6/.",
            f"{GITHUB_RELEASE}/v1.4.6/..",
        ]:
            result = self.run_script(
                "fragment",
                "--artifact",
                str(artifact),
                "--artifact-url",
                bad_url,
                "--platform",
                "windows",
                "--arch",
                "x86_64",
                "--format",
                "exe",
                "--fragment-out",
                str(self.fragment_path(f"bad-dot-{hashlib.sha1(bad_url.encode()).hexdigest()}.json")),
                check=False,
            )
            self.assertNotEqual(result.returncode, 0, bad_url)
    def test_sign_rejects_duplicate_artifact_selector(self):
        _, fragment_one = self.make_fragment(
            artifact_name="rustdesk-1.4.6-x86_64.exe",
            out_name="one.json",
        )
        _, fragment_two = self.make_fragment(
            artifact_name="rustdesk-1.4.6-x86_64-alt.exe",
            out_name="two.json",
        )
        result = self.run_script(
            "sign",
            "--fragment",
            str(fragment_one),
            "--fragment",
            str(fragment_two),
            "--package-id",
            "rustdesk",
            "--version",
            "1.4.6",
            "--release-id",
            "v1.4.6",
            "--published-at",
            "2026-05-14T00:00:00Z",
            "--key-id",
            "test-ed25519-main",
            "--private-key-seed-env",
            "RUSTDESK_UPDATE_ED25519_SEED",
            "--metadata-out",
            str(self.root / "metadata.json"),
            "--signature-out",
            str(self.root / "signature.json"),
            env={"RUSTDESK_UPDATE_ED25519_SEED": self.seed_b64},
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)
    def test_check_version_accepts_matching_stable_tag_only(self):
        self.run_script("check-version", "--release-id", "v1.4.6", "--version", "1.4.6")
        self.run_script("check-version", "--release-id", "1.4.6", "--version", "1.4.6")
        mismatch = self.run_script("check-version", "--release-id", "v1.4.7", "--version", "1.4.6", check=False)
        self.assertNotEqual(mismatch.returncode, 0)
        for release_id in ["v1.4.7-1", "nightly"]:
            result = self.run_script(
                "check-version",
                "--release-id",
                release_id,
                "--version",
                "1.4.6",
            )
            self.assertEqual(result.returncode, 0)
            self.assertIn("skip", result.stdout.lower())
    def test_release_id_validation_rejects_illegal_segments(self):
        for bad_release_id in ["release/v1.4.6", "bad tag", "bad?tag", "bad#tag"]:
            check = self.run_script("check-version", "--release-id", bad_release_id, "--version", "1.4.6", check=False)
            self.assertNotEqual(check.returncode, 0, bad_release_id)
            _, fragment = self.make_fragment(out_name=f"sign-source-{hashlib.sha1(bad_release_id.encode()).hexdigest()}.json")
            sign = self.run_script(
                "sign",
                "--fragment",
                str(fragment),
                "--package-id",
                "rustdesk",
                "--version",
                "1.4.6",
                "--release-id",
                bad_release_id,
                "--published-at",
                "2026-05-14T00:00:00Z",
                "--key-id",
                "test-ed25519-main",
                "--private-key-seed-env",
                "RUSTDESK_UPDATE_ED25519_SEED",
                "--metadata-out",
                str(self.root / f"metadata-{hashlib.sha1(bad_release_id.encode()).hexdigest()}.json"),
                "--signature-out",
                str(self.root / f"signature-{hashlib.sha1(bad_release_id.encode()).hexdigest()}.json"),
                env={"RUSTDESK_UPDATE_ED25519_SEED": self.seed_b64},
                check=False,
            )
            self.assertNotEqual(sign.returncode, 0, bad_release_id)
    def test_fragment_rejects_invalid_artifact_url_release_segment(self):
        artifact = self.write_artifact("rustdesk-1.4.6-x86_64.exe")
        for index, bad_url in enumerate(
            [
                f"{GITHUB_RELEASE}/release/v1.4.6/{artifact.name}",
                f"{GITHUB_RELEASE}/bad tag/{artifact.name}",
                f"{GITHUB_RELEASE}/bad?tag/{artifact.name}",
                f"{GITHUB_RELEASE}/bad#tag/{artifact.name}",
            ]
        ):
            result = self.run_script(
                "fragment",
                "--artifact",
                str(artifact),
                "--artifact-url",
                bad_url,
                "--platform",
                "windows",
                "--arch",
                "x86_64",
                "--format",
                "exe",
                "--fragment-out",
                str(self.fragment_path(f"bad-url-{index}.json")),
                check=False,
            )
            self.assertNotEqual(result.returncode, 0, bad_url)
if __name__ == "__main__":
    unittest.main()
