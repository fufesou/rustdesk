#!/usr/bin/env python3
import argparse
import base64
import hashlib
import json
import os
import re
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit
from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric import ed25519
APP_NAME = "rustdesk"
SCHEMA_VERSION = 1
SIGNATURE_ALGORITHM = "ed25519"
SIGNATURE_CONTEXT = b"RustDesk update metadata v1\n"
GITHUB_RELEASE_PREFIX = "https://github.com/rustdesk/rustdesk/releases/download"
def fail(message):
    raise SystemExit(message)
def read_json(path):
    with Path(path).open("r", encoding="utf-8") as handle:
        return json.load(handle)
def write_stable_json(path, value):
    output_path = Path(path)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    raw = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    output_path.write_bytes(raw)
    return raw
def sha256_hex(path):
    hasher = hashlib.sha256()
    with Path(path).open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()
def validate_release_id(release_id):
    if not release_id:
        fail("release id must not be empty")
    if any(char in release_id for char in (" ", "/", "\\", "?", "#")):
        fail(f"invalid release id: {release_id}")
    if unquote(release_id) != release_id:
        fail(f"release id must not require URL decoding: {release_id}")
def display_version_from_release_id(release_id):
    match = re.fullmatch(r"v?(\d+\.\d+\.\d+)", release_id)
    if not match:
        return None
    return match.group(1)
def parse_artifact_url(url):
    parsed = urlsplit(url)
    if parsed.scheme != "https" or parsed.netloc != "github.com":
        fail(f"artifact URL must use GitHub HTTPS: {url}")
    if parsed.query or parsed.fragment:
        fail(f"artifact URL must not contain query or fragment: {url}")
    parts = [part for part in parsed.path.split("/") if part]
    expected_prefix = ["rustdesk", "rustdesk", "releases", "download"]
    if len(parts) != 6 or parts[:4] != expected_prefix:
        fail(f"artifact URL must be a RustDesk release asset URL: {url}")
    release_id = parts[4]
    file_name = parts[5]
    validate_release_id(release_id)
    if not file_name:
        fail(f"artifact URL has no file name: {url}")
    if unquote(file_name) != file_name:
        fail(f"artifact URL file name must not require URL decoding: {url}")
    return release_id, file_name
def expected_artifact_url(release_id, file_name):
    return f"{GITHUB_RELEASE_PREFIX}/{release_id}/{file_name}"
def load_seed_from_env(env_name):
    encoded = os.environ.get(env_name)
    if not encoded:
        fail(f"missing private key seed environment variable: {env_name}")
    try:
        seed = base64.b64decode(encoded, validate=True)
    except ValueError as error:
        fail(f"invalid private key seed base64 in {env_name}: {error}")
    if len(seed) != 32:
        fail(f"private key seed in {env_name} must decode to 32 bytes")
    return seed
def load_public_key(encoded):
    try:
        key_bytes = base64.b64decode(encoded, validate=True)
    except ValueError as error:
        fail(f"invalid trusted public key base64: {error}")
    if len(key_bytes) != 32:
        fail("trusted public key must decode to 32 bytes")
    return ed25519.Ed25519PublicKey.from_public_bytes(key_bytes)
def decode_signature(encoded):
    try:
        signature = base64.b64decode(encoded, validate=True)
    except ValueError as error:
        fail(f"invalid metadata signature base64: {error}")
    if len(signature) != 64:
        fail("metadata signature must decode to 64 bytes")
    if base64.b64encode(signature).decode("ascii") != encoded:
        fail("metadata signature must use standard base64 with padding")
    return signature
def artifact_identity(artifact):
    return (
        artifact.get("platform"),
        artifact.get("arch"),
        artifact.get("format"),
        artifact.get("file_name"),
    )
def validate_fragment(fragment):
    required = ["platform", "arch", "format", "url", "file_name", "size", "sha256"]
    for key in required:
        if key not in fragment:
            fail(f"fragment missing field: {key}")
    if not isinstance(fragment["size"], int) or fragment["size"] < 0:
        fail("fragment size must be a non-negative integer")
    if not re.fullmatch(r"[0-9a-f]{64}", fragment["sha256"]):
        fail("fragment sha256 must be 64 lowercase hex characters")
def validate_fragment_url(fragment):
    release_id, file_name = parse_artifact_url(fragment["url"])
    if fragment["file_name"] != file_name:
        fail("fragment URL basename does not match file_name")
    return release_id
def command_fragment(args):
    artifact_path = Path(args.artifact)
    artifact_bytes = artifact_path.read_bytes()
    release_id, url_file_name = parse_artifact_url(args.artifact_url)
    if artifact_path.name != url_file_name:
        fail("artifact URL basename must match artifact file name")
    validate_release_id(release_id)
    fragment = {
        "platform": args.platform,
        "arch": args.arch,
        "format": args.format,
        "url": args.artifact_url,
        "file_name": artifact_path.name,
        "size": len(artifact_bytes),
        "sha256": hashlib.sha256(artifact_bytes).hexdigest(),
    }
    write_stable_json(args.fragment_out, fragment)
def command_sign(args):
    validate_release_id(args.release_id)
    artifacts = [read_json(path) for path in args.fragment]
    seen = set()
    for artifact in artifacts:
        validate_fragment(artifact)
        identity = artifact_identity(artifact)
        if identity in seen:
            fail(f"duplicate artifact identity: {identity}")
        seen.add(identity)
    artifacts.sort(key=artifact_identity)
    metadata = {
        "schema_version": SCHEMA_VERSION,
        "app": APP_NAME,
        "package_id": args.package_id,
        "version": args.version,
        "release_id": args.release_id,
        "published_at": args.published_at,
        "signature_key_id": args.key_id,
        "artifacts": artifacts,
    }
    metadata_bytes = write_stable_json(args.metadata_out, metadata)
    seed = load_seed_from_env(args.private_key_seed_env)
    private_key = ed25519.Ed25519PrivateKey.from_private_bytes(seed)
    signature = private_key.sign(SIGNATURE_CONTEXT + metadata_bytes)
    signature_json = {
        "schema_version": SCHEMA_VERSION,
        "algorithm": SIGNATURE_ALGORITHM,
        "key_id": args.key_id,
        "signature": base64.b64encode(signature).decode("ascii"),
    }
    write_stable_json(args.signature_out, signature_json)
def validate_metadata_and_signature(metadata, signature, args):
    if signature.get("schema_version") != SCHEMA_VERSION:
        fail("unsupported signature schema version")
    if signature.get("algorithm") != SIGNATURE_ALGORITHM:
        fail("unsupported signature algorithm")
    if signature.get("key_id") != args.trusted_public_key_id:
        fail("signature key id mismatch")
    if metadata.get("schema_version") != SCHEMA_VERSION:
        fail("unsupported metadata schema version")
    if metadata.get("app") != APP_NAME:
        fail("metadata app mismatch")
    if metadata.get("package_id") != args.package_id:
        fail("metadata package id mismatch")
    if metadata.get("version") != args.version:
        fail("metadata version mismatch")
    if metadata.get("release_id") != args.release_id:
        fail("metadata release id mismatch")
    if metadata.get("signature_key_id") != signature.get("key_id"):
        fail("metadata signature key id mismatch")
def command_verify(args):
    metadata_path = Path(args.metadata)
    signature_path = Path(args.signature)
    metadata_bytes = metadata_path.read_bytes()
    metadata = json.loads(metadata_bytes.decode("utf-8"))
    signature_json = read_json(signature_path)
    validate_metadata_and_signature(metadata, signature_json, args)
    signature = decode_signature(signature_json["signature"])
    public_key = load_public_key(args.trusted_public_key_base64)
    try:
        public_key.verify(signature, SIGNATURE_CONTEXT + metadata_bytes)
    except InvalidSignature:
        fail("invalid metadata signature")
    artifacts = metadata.get("artifacts")
    if not isinstance(artifacts, list) or not artifacts:
        fail("metadata must contain artifacts")
    local_artifacts = {Path(path).name: Path(path) for path in args.artifact}
    metadata_names = {artifact.get("file_name") for artifact in artifacts}
    if metadata_names != set(local_artifacts):
        fail("local artifact set does not match metadata artifact set")
    for artifact in artifacts:
        validate_fragment(artifact)
        validate_fragment_url(artifact)
        file_name = artifact["file_name"]
        expected_url = expected_artifact_url(args.release_id, file_name)
        if artifact["url"] != expected_url:
            fail(f"artifact URL mismatch for {file_name}")
        local_path = local_artifacts[file_name]
        if local_path.stat().st_size != artifact["size"]:
            fail(f"artifact size mismatch for {file_name}")
        if sha256_hex(local_path) != artifact["sha256"]:
            fail(f"artifact sha256 mismatch for {file_name}")
def command_check_version(args):
    validate_release_id(args.release_id)
    display_version = display_version_from_release_id(args.release_id)
    if display_version is None:
        print(
            f"release id {args.release_id} is not an official client update source; "
            "it is not parseable as an official client update source; "
            "skipping display-version consistency check"
        )
        return
    if display_version != args.version:
        fail(f"release id {args.release_id} maps to {display_version}, not {args.version}")
def build_parser():
    parser = argparse.ArgumentParser(description="Generate and verify RustDesk update metadata")
    subparsers = parser.add_subparsers(dest="command", required=True)
    fragment = subparsers.add_parser("fragment", help="Generate one artifact metadata fragment")
    fragment.add_argument("--artifact", required=True)
    fragment.add_argument("--artifact-url", required=True)
    fragment.add_argument("--platform", required=True)
    fragment.add_argument("--arch", required=True)
    fragment.add_argument("--format", required=True)
    fragment.add_argument("--fragment-out", required=True)
    fragment.set_defaults(func=command_fragment)
    sign = subparsers.add_parser("sign", help="Sign release metadata")
    sign.add_argument("--fragment", action="append", required=True)
    sign.add_argument("--package-id", required=True)
    sign.add_argument("--version", required=True)
    sign.add_argument("--release-id", required=True)
    sign.add_argument("--published-at", required=True)
    sign.add_argument("--key-id", required=True)
    sign.add_argument("--private-key-seed-env", required=True)
    sign.add_argument("--metadata-out", required=True)
    sign.add_argument("--signature-out", required=True)
    sign.set_defaults(func=command_sign)
    verify = subparsers.add_parser("verify", help="Verify release metadata and artifacts")
    verify.add_argument("--metadata", required=True)
    verify.add_argument("--signature", required=True)
    verify.add_argument("--artifact", action="append", default=[])
    verify.add_argument("--package-id", required=True)
    verify.add_argument("--version", required=True)
    verify.add_argument("--release-id", required=True)
    verify.add_argument("--trusted-public-key-base64", required=True)
    verify.add_argument("--trusted-public-key-id", required=True)
    verify.set_defaults(func=command_verify)
    check_version = subparsers.add_parser("check-version", help="Validate tag/version consistency")
    check_version.add_argument("--version", required=True)
    check_version.add_argument("--release-id", required=True)
    check_version.set_defaults(func=command_check_version)
    return parser
def main(argv=None):
    parser = build_parser()
    args = parser.parse_args(argv)
    args.func(args)
if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except Exception as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(1)
