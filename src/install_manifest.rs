use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::io;

#[path = "install_manifest/collection.rs"]
pub(crate) mod collection;
#[path = "install_manifest/migration.rs"]
pub(crate) mod migration;
#[cfg(windows)]
#[path = "install_manifest/package.rs"]
pub(crate) mod package;
#[path = "install_manifest/path.rs"]
mod path;
#[path = "install_manifest/request.rs"]
pub(crate) mod request;
#[cfg(windows)]
#[path = "platform/windows/manifest_files.rs"]
pub(crate) mod windows;

pub(crate) use path::RelativePath;

pub(crate) const SCHEMA_VERSION: u32 = 1;
const SHA256_HEX_LENGTH: usize = 64;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileIdentity {
    pub volume_serial_number: u32,
    pub file_index: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstallIdentity {
    pub product: String,
    pub architecture: String,
    pub root: FileIdentity,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InstallState {
    Ready,
    Installing,
    Updating,
    Uninstalling,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum PreviousTarget {
    Absent,
    Owned,
    Legacy { identity: FileIdentity },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingFile {
    pub path: RelativePath,
    pub identity: FileIdentity,
    pub sha256: String,
    pub previous_target: PreviousTarget,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstallManifest {
    pub schema_version: u32,
    pub identity: InstallIdentity,
    pub package_version: String,
    pub state: InstallState,
    pub files: Vec<RelativePath>,
    pub created_dirs: Vec<RelativePath>,
    pub pending_file: Option<PendingFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub migration_targets: Vec<migration::MigrationTarget>,
}

impl InstallManifest {
    // The expected identity must come from trusted product metadata and an opened root.
    pub(crate) fn from_bytes(bytes: &[u8], expected: &InstallIdentity) -> io::Result<Self> {
        let manifest: Self = serde_json::from_slice(bytes).map_err(invalid_data)?;
        manifest.validate()?;
        if &manifest.identity != expected {
            return Err(invalid_data(
                "Manifest installation identity does not match",
            ));
        }
        Ok(manifest)
    }

    pub(crate) fn to_bytes(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        serde_json::to_vec(self).map_err(invalid_data)
    }

    fn validate(&self) -> io::Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(invalid_data("Unsupported installation manifest schema"));
        }
        if self.identity.product.trim().is_empty()
            || self.identity.architecture.trim().is_empty()
            || self.package_version.trim().is_empty()
        {
            return Err(invalid_data(
                "Manifest product, architecture and version are required",
            ));
        }
        for file in &self.files {
            file.validate_payload_file().map_err(invalid_data)?;
        }
        self.validate_pending()?;
        migration::validate(self)?;
        collection::validate_manifest_sets(self)
    }

    fn validate_pending(&self) -> io::Result<()> {
        let Some(pending) = &self.pending_file else {
            return Ok(());
        };
        if !matches!(
            self.state,
            InstallState::Installing | InstallState::Updating
        ) {
            return Err(invalid_data(
                "Pending publication requires an installation or update",
            ));
        }
        pending.path.validate_payload_file().map_err(invalid_data)?;
        if pending.sha256.len() != SHA256_HEX_LENGTH
            || !pending.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(invalid_data(
                "Pending publication has an invalid SHA-256 digest",
            ));
        }
        Ok(())
    }
}

fn invalid_data(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

impl Serialize for RelativePath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RelativePath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let path = String::deserialize(deserializer)?;
        Self::parse(&path).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest() -> InstallManifest {
        InstallManifest {
            schema_version: SCHEMA_VERSION,
            identity: InstallIdentity {
                product: "RustDesk".into(),
                architecture: "x86_64".into(),
                root: FileIdentity {
                    volume_serial_number: 1,
                    file_index: 2,
                },
            },
            package_version: "2.0.0".into(),
            state: InstallState::Installing,
            files: vec![RelativePath::parse("uninstall.exe").unwrap()],
            created_dirs: vec![RelativePath::parse("data/meta").unwrap()],
            migration_targets: Vec::new(),
            pending_file: Some(PendingFile {
                path: RelativePath::parse("RustDesk.exe").unwrap(),
                identity: FileIdentity {
                    volume_serial_number: 1,
                    file_index: 3,
                },
                sha256: "a".repeat(SHA256_HEX_LENGTH),
                previous_target: PreviousTarget::Absent,
            }),
        }
    }

    #[test]
    fn preserves_migration_permission_without_claiming_legacy_files() {
        let record = manifest();
        let mut value = serde_json::to_value(&record).unwrap();
        value["migration_targets"] = json!([{
            "path": "legacy.dll", "sha256": "a".repeat(SHA256_HEX_LENGTH),
            "identity": { "volume_serial_number": 1, "file_index": 30 }
        }]);
        let decoded =
            InstallManifest::from_bytes(&serde_json::to_vec(&value).unwrap(), &record.identity)
                .unwrap();
        let saved = serde_json::to_value(&decoded).unwrap();
        assert_eq!(saved["migration_targets"], value["migration_targets"]);
        assert!(!decoded
            .files
            .iter()
            .any(|path| path.as_str() == "legacy.dll"));
    }

    #[test]
    fn round_trip_preserves_owned_and_pending_files_separately() {
        let original = manifest();
        let bytes = original.to_bytes().unwrap();
        let decoded = InstallManifest::from_bytes(&bytes, &original.identity).unwrap();
        assert_eq!(decoded, original);
        assert_eq!(decoded.files.len(), 1);
        assert_eq!(decoded.pending_file.unwrap().path.as_str(), "RustDesk.exe");
    }

    #[test]
    fn rejects_invalid_receipts_instead_of_treating_them_as_legacy() {
        let original = manifest();
        for (field, value) in [
            ("schema_version", json!(SCHEMA_VERSION + 1)),
            ("files", json!(["../outside.exe"])),
            ("files", json!(["data/meta/install-manifest.json"])),
            ("state", json!("ready")),
            ("unknown_field", json!(true)),
        ] {
            let mut invalid = serde_json::to_value(&original).unwrap();
            invalid[field] = value;
            let bytes = serde_json::to_vec(&invalid).unwrap();
            assert!(InstallManifest::from_bytes(&bytes, &original.identity).is_err());
        }
        let mut wrong_root = original.identity.clone();
        wrong_root.root.file_index += 1;
        assert!(InstallManifest::from_bytes(&original.to_bytes().unwrap(), &wrong_root).is_err());
        assert!(InstallManifest::from_bytes(b"{", &original.identity).is_err());
    }

    #[test]
    fn accepts_lower_package_versions_for_the_same_installation() {
        let original = manifest();
        let lower = InstallManifest {
            package_version: "1.0.0".into(),
            ..original.clone()
        };
        let decoded =
            InstallManifest::from_bytes(&lower.to_bytes().unwrap(), &original.identity).unwrap();
        assert_eq!(decoded.package_version, "1.0.0");
    }

    #[test]
    fn rejects_conflicting_file_sets_and_pending_ownership() {
        let original = manifest();
        for (field, value) in [
            ("files", json!(["App.dll", "app.dll"])),
            ("files", json!(["assets", "assets/logo.png"])),
            ("created_dirs", json!(["data", "DATA"])),
            ("created_dirs", json!(["uninstall.exe/logs"])),
            ("files", json!(["RustDesk.exe"])),
        ] {
            let mut invalid = serde_json::to_value(&original).unwrap();
            invalid[field] = value;
            assert!(
                InstallManifest::from_bytes(
                    &serde_json::to_vec(&invalid).unwrap(),
                    &original.identity,
                )
                .is_err(),
                "accepted conflicting {field}: {invalid}"
            );
        }
        let mut invalid = original.clone();
        invalid.pending_file.as_mut().unwrap().previous_target = PreviousTarget::Owned;
        assert!(InstallManifest::from_bytes(
            &serde_json::to_vec(&invalid).unwrap(),
            &original.identity,
        )
        .is_err());
    }
}
