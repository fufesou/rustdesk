use crate::update_metadata::{UpdateArtifactQuery, VerifiedUpdateArtifact};
use hbb_common::{bail, ResultType};

pub(crate) const VERSION: &str = "1.5.1";
pub(crate) const RELEASE_PAGE_URL: &str =
    "https://github.com/fufesou/rustdesk/releases/tag/test-windows-ipc-server-auth";
const EXE_URL: &str = "https://github.com/fufesou/rustdesk/releases/download/test-windows-ipc-server-auth/rustdesk-1.5.1-x86_64.exe";
const MSI_URL: &str = "https://github.com/fufesou/rustdesk/releases/download/test-windows-ipc-server-auth/rustdesk-1.5.1-x86_64.msi";
const EXE_SIZE: u64 = 26_052_096;
const MSI_SIZE: u64 = 26_608_117;
const EXE_SHA256: &str = "6034d5c61df7e42b84553a075ca1e6694ac956ce4549cb0788a652e3530daa4d";
const MSI_SHA256: &str = "202d1bc7af8c857adc3586b5a33e2d0238a8009b66d386ff357393ce805aad58";

// This test release has no signed metadata. Pin the existing GitHub assets here
// so the normal download and installer SHA-256 checks still verify their bytes.
pub(super) fn artifact(query: &UpdateArtifactQuery<'_>) -> ResultType<VerifiedUpdateArtifact> {
    let (url, size, sha256) = match (query.platform, query.arch, query.format) {
        ("windows", "x86_64", "exe") => (EXE_URL, EXE_SIZE, EXE_SHA256),
        ("windows", "x86_64", "msi") => (MSI_URL, MSI_SIZE, MSI_SHA256),
        _ => bail!("The test update release only supports Windows x86_64 EXE and MSI"),
    };
    Ok(VerifiedUpdateArtifact {
        version: VERSION.to_owned(),
        url: url.to_owned(),
        #[cfg(target_os = "macos")]
        file_name: format!("rustdesk-{VERSION}-x86_64.{}", query.format),
        size,
        sha256: sha256.to_owned(),
    })
}

pub(super) fn is_download_url(url: &str) -> bool {
    matches!(url, EXE_URL | MSI_URL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updater::artifact::{
        get_update_download_file_from_url, verified_update_artifact_from_release_page_url,
    };

    #[test]
    fn pinned_windows_updates_use_requested_assets() {
        #[cfg(target_os = "windows")]
        {
            crate::common::do_check_software_update().unwrap();
            assert_eq!(
                crate::common::SOFTWARE_UPDATE_URL.lock().unwrap().as_str(),
                RELEASE_PAGE_URL
            );
        }
        for format in ["exe", "msi"] {
            let query = UpdateArtifactQuery {
                platform: "windows",
                arch: "x86_64",
                format,
            };
            let artifact =
                verified_update_artifact_from_release_page_url(RELEASE_PAGE_URL, &query).unwrap();
            let filename = format!("rustdesk-1.5.1-x86_64.{format}");
            assert_eq!(artifact.version, "1.5.1");
            assert_eq!(
                artifact.url,
                format!("https://github.com/fufesou/rustdesk/releases/download/test-windows-ipc-server-auth/{filename}")
            );
            assert_eq!(
                get_update_download_file_from_url(&artifact.url).unwrap(),
                std::env::temp_dir().join(filename)
            );
        }
    }

    #[test]
    fn pinned_windows_updates_reject_other_artifacts() {
        for (platform, arch, format) in [
            ("windows", "aarch64", "exe"),
            ("windows", "x86_64", "zip"),
            ("linux", "x86_64", "exe"),
        ] {
            assert!(artifact(&UpdateArtifactQuery {
                platform,
                arch,
                format
            })
            .is_err());
        }
        for url in [
            EXE_URL.replace("test-windows-ipc-server-auth", "another-tag"),
            EXE_URL.replace("rustdesk-1.5.1-x86_64.exe", "another.exe"),
            format!("{EXE_URL}?download=1"),
        ] {
            assert!(get_update_download_file_from_url(&url).is_none(), "{url}");
        }
    }
}
