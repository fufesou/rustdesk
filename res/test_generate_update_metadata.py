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
        merged_env.pop("RUSTDESK_UPDATE_ED25519_SEED", None)
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
    def run_sign(
        self,
        fragments,
        *,
        release_id="v1.4.6",
        seed_b64=...,
        output_path=None,
        check=True,
    ):
        metadata = output_path or self.root / "rustdesk-update.json"
        signature = output_path or self.root / "rustdesk-update.json.sig"
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
        seed_env = {} if seed_b64 is None else {"RUSTDESK_UPDATE_ED25519_SEED": self.seed_b64 if seed_b64 is ... else seed_b64}
        result = self.run_script(
            *args,
            env=seed_env,
            check=check,
        )
        return metadata, signature, result
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
    def test_verify_fails_after_artifact_tamper_or_missing_local_artifact(self):
        artifact, fragment = self.make_fragment()
        metadata, signature, _ = self.run_sign([fragment])
        ok = self.verify_release(metadata, signature, [artifact])
        self.assertEqual(ok.returncode, 0)
        artifact.write_bytes(b"tampered")
        tampered = self.verify_release(metadata, signature, [artifact], check=False)
        self.assertNotEqual(tampered.returncode, 0)
        self.assertIn("artifact sha256 mismatch", tampered.stderr)
        missing = self.verify_release(metadata, signature, [], check=False)
        self.assertNotEqual(missing.returncode, 0)
        self.assertIn("local artifact set does not match metadata artifact set", missing.stderr)
    def test_verify_rejects_metadata_tampering_and_trust_mismatches(self):
        artifact, fragment = self.make_fragment()
        metadata, signature, _ = self.run_sign([fragment])
        wrong_version = self.verify_release(metadata, signature, [artifact], version="1.4.7", check=False)
        self.assertNotEqual(wrong_version.returncode, 0)
        self.assertIn("metadata version mismatch", wrong_version.stderr)
        wrong_release = self.verify_release(metadata, signature, [artifact], release_id="v1.4.7", check=False)
        self.assertNotEqual(wrong_release.returncode, 0)
        self.assertIn("metadata release id mismatch", wrong_release.stderr)
        _, wrong_public = self.generate_keypair()
        wrong_signature = self.verify_release(metadata, signature, [artifact], public_key=wrong_public, check=False)
        self.assertNotEqual(wrong_signature.returncode, 0)
        self.assertIn("invalid metadata signature", wrong_signature.stderr)
        wrong_key_id = self.verify_release(metadata, signature, [artifact], key_id="other-key", check=False)
        self.assertNotEqual(wrong_key_id.returncode, 0)
        self.assertIn("signature key id mismatch", wrong_key_id.stderr)
        data = json.loads(metadata.read_text(encoding="utf-8"))
        data["published_at"] = "2026-05-15T00:00:00Z"
        metadata.write_text(json.dumps(data, separators=(",", ":"), sort_keys=True), encoding="utf-8")
        tampered = self.verify_release(metadata, signature, [artifact], check=False)
        self.assertNotEqual(tampered.returncode, 0)
        self.assertIn("invalid metadata signature", tampered.stderr)
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
            _, _, result = self.run_sign([bad_fragment], check=False)
            self.assertNotEqual(result.returncode, 0, bad_url)
        data["file_name"] = "rustdesk\\update.exe"
        data["url"] = f"{GITHUB_RELEASE}/v1.4.6/{data['file_name']}"
        bad_fragment = self.fragment_path("bad-path-separator.json")
        bad_fragment.write_text(json.dumps(data), encoding="utf-8")
        _, _, result = self.run_sign([bad_fragment], check=False)
        self.assertNotEqual(result.returncode, 0)
    def test_sign_rejects_non_string_fragment_fields(self):
        _, fragment = self.make_fragment()
        data = json.loads(fragment.read_text(encoding="utf-8"))
        for key in ["platform", "arch", "format", "url", "file_name", "sha256"]:
            mutated = dict(data, **{key: []})
            fragment.write_text(json.dumps(mutated), encoding="utf-8")
            _, _, result = self.run_sign([fragment], check=False)
            self.assertIn(f"fragment {key} must be a string", result.stderr)

    def test_sign_rejects_blank_artifact_selector_fields(self):
        _, fragment = self.make_fragment()
        data = json.loads(fragment.read_text(encoding="utf-8"))
        for key in ["platform", "arch", "format"]:
            for value in ["", " \t"]:
                with self.subTest(key=key, value=value):
                    fragment.write_text(
                        json.dumps(dict(data, **{key: value})), encoding="utf-8"
                    )
                    _, _, result = self.run_sign([fragment], check=False)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(f"fragment {key} must not be empty", result.stderr)

    def test_sign_rejects_invalid_seed_without_outputs(self):
        _, fragment = self.make_fragment()
        for seed in [None, "not-base64", base64.b64encode(b"short").decode("ascii")]:
            metadata, signature, result = self.run_sign([fragment], seed_b64=seed, check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(metadata.exists())
            self.assertFalse(signature.exists())
    def test_sign_rejects_same_output_path_without_overwrite(self):
        _, fragment = self.make_fragment()
        output = self.root / "existing.json"
        original = b"existing content"
        output.write_bytes(original)
        _, _, result = self.run_sign([fragment], output_path=output, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--metadata-out and --signature-out must be different files", result.stderr)
        self.assertEqual(output.read_bytes(), original)
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
        _, _, result = self.run_sign(
            [fragment_one, fragment_two], check=False
        )
        self.assertNotEqual(result.returncode, 0)
    def test_sign_rejects_stable_tag_version_mismatch(self):
        _, fragment = self.make_fragment(release_id="v1.4.7", out_name="mismatch-version.json")
        _, _, result = self.run_sign(
            [fragment],
            release_id="v1.4.7",
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("maps to 1.4.7", result.stderr)
    def test_check_version_accepts_matching_stable_tag_only(self):
        self.run_script("check-version", "--release-id", "v1.4.6", "--version", "1.4.6")
        self.run_script("check-version", "--release-id", "1.4.6", "--version", "1.4.6")
        mismatch = self.run_script("check-version", "--release-id", "v1.4.7", "--version", "1.4.6", check=False)
        self.assertNotEqual(mismatch.returncode, 0)
        for release_id in ["0.0.0", "v0.0.0"]:
            result = self.run_script(
                "check-version",
                "--release-id",
                release_id,
                "--version",
                "0.0.0",
                check=False,
            )
            self.assertNotEqual(result.returncode, 0, release_id)
        oversized = self.run_script(
            "check-version",
            "--release-id",
            "v9223372036854775807.1.1",
            "--version",
            "9223372036854775807.1.1",
            check=False,
        )
        self.assertNotEqual(oversized.returncode, 0)
        nightly = self.run_script(
            "check-version", "--release-id", "nightly", "--version", "1.4.6"
        )
        self.assertIn("skip", nightly.stdout.lower())
    def test_release_id_validation_rejects_illegal_segments(self):
        for bad_release_id in ["release/v1.4.6", "bad tag", "bad?tag", "bad#tag"]:
            check = self.run_script("check-version", "--release-id", bad_release_id, "--version", "1.4.6", check=False)
            self.assertNotEqual(check.returncode, 0, bad_release_id)
            _, fragment = self.make_fragment(out_name=f"sign-source-{hashlib.sha1(bad_release_id.encode()).hexdigest()}.json")
            _, _, sign = self.run_sign(
                [fragment],
                release_id=bad_release_id,
                check=False,
            )
            self.assertNotEqual(sign.returncode, 0, bad_release_id)
if __name__ == "__main__":
    unittest.main()
