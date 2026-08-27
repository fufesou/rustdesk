import unittest
from pathlib import Path

REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
WORKFLOW_TEXT = (REPOSITORY_ROOT / ".github/workflows/flutter-build.yml").read_text(
    encoding="utf-8"
)
COMPLETE_TEST_WORKFLOW_TEXT = (
    REPOSITORY_ROOT / ".github/workflows/complete-test-update-release.yml"
).read_text(encoding="utf-8")


def workflow_step_body(workflow_text, step_name):
    start = workflow_text.index(f"      - name: {step_name}")
    remainder = workflow_text[start + 1 :]
    next_step = remainder.find("\n      - name:")
    return (
        workflow_text[start:]
        if next_step < 0
        else workflow_text[start : start + next_step + 1]
    )


def step_body(step_name):
    return workflow_step_body(WORKFLOW_TEXT, step_name)


class FlutterBuildWorkflowTests(unittest.TestCase):
    def test_update_validation_job_runs_all_update_tests(self):
        test_step = step_body("Test update metadata signer")

        for test_file in (
            "res/test_generate_update_metadata.py",
            "res/test_update_metadata_cli.py",
            "res/test_flutter_build_workflow.py",
            "res/test_macos_update_scripts.py",
        ):
            self.assertIn(test_file, test_step)

    def test_update_branch_overrides_run_for_all_builds(self):
        checkout_step = step_body("Checkout source code")
        override_step = step_body("Test update branch overrides")

        self.assertNotIn("if:", checkout_step)
        self.assertNotIn("if:", override_step)
        self.assertIn(
            "python -m unittest res/test_update_branch_overrides.py", override_step
        )

    def test_complete_test_release_publishes_metadata_after_signature(self):
        signature_step = workflow_step_body(
            COMPLETE_TEST_WORKFLOW_TEXT, "Publish signed metadata signature"
        )
        metadata_step = workflow_step_body(
            COMPLETE_TEST_WORKFLOW_TEXT, "Publish signed update metadata"
        )

        self.assertIn("rustdesk-update.json.sig", signature_step)
        self.assertNotIn("rustdesk-update.json.sig", metadata_step)
        self.assertIn("rustdesk-update.json", metadata_step)
        self.assertLess(
            COMPLETE_TEST_WORKFLOW_TEXT.index(signature_step),
            COMPLETE_TEST_WORKFLOW_TEXT.index(metadata_step),
        )

    def test_published_metadata_is_never_removed_or_replaced(self):
        publish_job = WORKFLOW_TEXT.split("  publish-signed-update-metadata:", 1)[1].split(
            "\n  publish_unsigned:", 1
        )[0]
        guard_step = step_body("Refuse to replace published update metadata")

        self.assertIn("Signed update metadata is already published", guard_step)
        self.assertIn("exit 1", guard_step)
        self.assertNotIn("--method DELETE", publish_job)

    def test_macos_update_tests_use_matrix_features(self):
        test_step = step_body("Test verified updates")

        self.assertIn("${{ matrix.job.extra-cargo-features }}", test_step)
        self.assertNotIn(
            "--features flutter,hwcodec,unix-file-copy-paste,screencapturekit",
            test_step,
        )

    def test_uploads_artifacts_from_merged_download_directory(self):
        upload_step = step_body("Upload signed update artifacts")
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
