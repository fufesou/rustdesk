import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).parents[1]
EXPECTED_VERSION = "1.4.5"
EXPECTED_TEST_RELEASE = "https://github.com/fufesou/rustdesk/releases/tag/fix-update-metadata"
VERSION_MARKERS = (
    ("Cargo.toml", f'version = "{EXPECTED_VERSION}"'),
    ("libs/portable/Cargo.toml", f'version = "{EXPECTED_VERSION}"'),
    ("flutter/pubspec.yaml", f"version: {EXPECTED_VERSION}+63"),
    (".github/workflows/flutter-build.yml", f'VERSION: "{EXPECTED_VERSION}"'),
    (".github/workflows/playground.yml", f'VERSION: "{EXPECTED_VERSION}"'),
    ("appimage/AppImageBuilder-aarch64.yml", f"version: {EXPECTED_VERSION}"),
    ("appimage/AppImageBuilder-x86_64.yml", f"version: {EXPECTED_VERSION}"),
    ("res/PKGBUILD", f"pkgver={EXPECTED_VERSION}"),
    ("res/rpm-flutter-suse.spec", f"Version:    {EXPECTED_VERSION}"),
    ("res/rpm-flutter.spec", f"Version:    {EXPECTED_VERSION}"),
    ("res/rpm.spec", f"Version:    {EXPECTED_VERSION}"),
)


class UpdateBranchOverridesTest(unittest.TestCase):
    def test_all_release_manifests_keep_test_version(self):
        for relative_path, marker in VERSION_MARKERS:
            with self.subTest(path=relative_path):
                content = (REPOSITORY_ROOT / relative_path).read_text(encoding="utf-8")
                self.assertIn(marker, content)

    def test_software_update_check_uses_fixed_test_release(self):
        source = (REPOSITORY_ROOT / "src/common.rs").read_text(encoding="utf-8")

        self.assertIn(EXPECTED_TEST_RELEASE, source)
        check_body = source.split("pub fn check_software_update()", 1)[1].split(
            "pub(crate) fn release_id_from_update_url", 1
        )[0]
        self.assertIn("set_fixed_test_software_update_url", check_body)

    def test_windows_update_omits_os_signature_sanity_checks(self):
        cargo_source = (REPOSITORY_ROOT / "Cargo.toml").read_text(encoding="utf-8")
        updater_source = (REPOSITORY_ROOT / "src/updater.rs").read_text(
            encoding="utf-8"
        )
        verified_update_source = (
            REPOSITORY_ROOT / "src/platform/windows/verified_update.rs"
        ).read_text(encoding="utf-8")
        signature_source = (
            REPOSITORY_ROOT / "src/platform/windows/verified_update/signature.rs"
        )

        self.assertNotIn("verify_authenticode", updater_source)
        self.assertNotIn("mod signature;", verified_update_source)
        self.assertNotIn("verify_authenticode", verified_update_source)
        self.assertFalse(signature_source.exists())
        self.assertNotIn('"Win32_Security_Cryptography"', cargo_source)
        self.assertNotIn('"Win32_Security_WinTrust"', cargo_source)

    def test_published_metadata_is_never_deleted_or_replaced(self):
        workflow = (
            REPOSITORY_ROOT / ".github/workflows/flutter-build.yml"
        ).read_text(encoding="utf-8")

        self.assertNotIn("- name: Remove published update metadata", workflow)
        guard_position = workflow.index(
            "- name: Refuse to replace published update metadata"
        )
        checkout_position = workflow.index(
            "- name: Checkout source code", guard_position
        )
        self.assertLess(guard_position, checkout_position)
        for step_name in (
            "Verify Windows Flutter version",
            "Verify Windows Sciter version",
            'reported_version="$(./flutter/build/macos/Build/Products/Release/'
            'RustDesk.app/Contents/MacOS/RustDesk --version)"',
        ):
            with self.subTest(step=step_name):
                self.assertIn(step_name, workflow)

    def test_update_response_caches_successful_tls_handshake(self):
        source = (REPOSITORY_ROOT / "src/common.rs").read_text(encoding="utf-8")
        response_branch = source.split(
            "Ok((used_tls_type, response)) => {", 1
        )[1].split("Err(err) => Err(err)", 1)[0]

        self.assertLess(
            response_branch.index("upsert_tls_cache"),
            response_branch.index("let status = response.status()"),
        )

    def test_update_dialog_buttons_are_translated_once(self):
        source = (
            REPOSITORY_ROOT / "flutter/lib/desktop/widgets/update_progress.dart"
        ).read_text(encoding="utf-8")

        self.assertNotIn("dialogButton(translate(", source)

    def test_obsolete_update_entry_points_are_removed(self):
        ffi_source = (REPOSITORY_ROOT / "src/flutter_ffi.rs").read_text(
            encoding="utf-8"
        )
        macos_source = (REPOSITORY_ROOT / "src/platform/macos.rs").read_text(
            encoding="utf-8"
        )
        windows_source = (REPOSITORY_ROOT / "src/platform/windows.rs").read_text(
            encoding="utf-8"
        )

        self.assertNotIn('key.starts_with("download-file-")', ffi_source)
        self.assertNotIn("pub fn update_to(_file:", macos_source)
        self.assertNotIn("pub fn extract_update_dmg", macos_source)
        self.assertNotIn("pub fn update_to(file:", windows_source)


if __name__ == "__main__":
    unittest.main()
