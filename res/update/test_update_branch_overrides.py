import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
EXPECTED_VERSION = "1.4.5"
EXPECTED_TEST_RELEASE = (
    "https://github.com/fufesou/rustdesk/releases/tag/fix-update-metadata"
)
EXPECTED_TEST_DOWNLOAD = (
    "https://github.com/fufesou/rustdesk/releases/download/fix-update-metadata/"
)
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

    def test_update_checks_use_fixed_test_release(self):
        common_source = (REPOSITORY_ROOT / "src/common.rs").read_text(
            encoding="utf-8"
        )
        updater_source = (REPOSITORY_ROOT / "src/updater.rs").read_text(
            encoding="utf-8"
        )
        check_body = common_source.split("pub fn check_software_update()", 1)[1].split(
            "pub(crate) fn release_id_from_update_url", 1
        )[0]

        self.assertIn(f'"{EXPECTED_TEST_RELEASE}"', common_source)
        self.assertIn(
            "std::thread::spawn(set_fixed_test_software_update_url);", check_body
        )
        self.assertEqual(
            2, updater_source.count("set_fixed_test_software_update_url();")
        )

    def test_update_artifacts_use_fixed_test_release(self):
        common_source = (REPOSITORY_ROOT / "src/common.rs").read_text(
            encoding="utf-8"
        )
        artifact_source = (REPOSITORY_ROOT / "src/updater/artifact.rs").read_text(
            encoding="utf-8"
        )

        self.assertIn(f'"{EXPECTED_TEST_DOWNLOAD}"', common_source)
        self.assertIn(
            "if update_url == FIXED_TEST_UPDATE_RELEASE_PAGE_URL", artifact_source
        )
        self.assertIn('owner == "fufesou"', artifact_source)
        self.assertIn("tag == FIXED_TEST_UPDATE_RELEASE_ID", artifact_source)

    def test_update_ui_uses_test_release_page(self):
        rust_source = (REPOSITORY_ROOT / "src/ui_interface.rs").read_text(
            encoding="utf-8"
        )
        flutter_source = (
            REPOSITORY_ROOT / "flutter/lib/desktop/pages/desktop_home_page.dart"
        ).read_text(encoding="utf-8")

        self.assertIn("FIXED_TEST_UPDATE_RELEASE_ID", rust_source)
        self.assertIn("display_version_from_release_id", rust_source)
        self.assertIn("link: isToUpdate ? updateUrl : null", flutter_source)


if __name__ == "__main__":
    unittest.main()
