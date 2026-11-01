use super::{
    invalid_data,
    request::{validate_hash, InputFile},
    FileIdentity, InstallManifest, InstallState, PreviousTarget, RelativePath,
};
use serde::{Deserialize, Serialize};
use std::io;

// Replacement permission is separate from ownership of a successfully published file.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MigrationTarget {
    pub path: RelativePath,
    pub identity: FileIdentity,
    pub sha256: String,
}

impl MigrationTarget {
    pub(crate) fn permits(&self, path: &RelativePath, hash: &str, identity: &FileIdentity) -> bool {
        self.path.as_str().eq_ignore_ascii_case(path.as_str())
            && self.sha256.eq_ignore_ascii_case(hash)
            && self.identity == *identity
    }
}

pub(crate) fn from_inputs(inputs: &[InputFile]) -> Vec<MigrationTarget> {
    inputs
        .iter()
        .filter_map(|input| {
            let PreviousTarget::Legacy { identity } = &input.previous else {
                return None;
            };
            Some(MigrationTarget {
                path: input.path.clone(),
                identity: identity.clone(),
                sha256: input.sha256.clone(),
            })
        })
        .collect()
}

pub(super) fn validate(record: &InstallManifest) -> io::Result<()> {
    if record.migration_targets.is_empty() {
        return Ok(());
    }
    if !matches!(
        record.state,
        InstallState::Installing | InstallState::Updating
    ) {
        return Err(invalid_data(
            "Migration permission requires an unfinished installation",
        ));
    }
    for target in &record.migration_targets {
        validate_hash(&target.sha256)?;
        if target.identity.volume_serial_number != record.identity.root.volume_serial_number {
            return Err(invalid_data(
                "Migration target is outside the installation volume",
            ));
        }
    }
    Ok(())
}

pub(crate) fn verify_retry(record: &InstallManifest, inputs: &[InputFile]) -> io::Result<()> {
    for target in &record.migration_targets {
        if !inputs.iter().any(|input| {
            target
                .path
                .as_str()
                .eq_ignore_ascii_case(input.path.as_str())
                && target.sha256.eq_ignore_ascii_case(&input.sha256)
        }) {
            return Err(invalid_data(
                "Retry the originally confirmed migration package",
            ));
        }
    }
    for input in inputs {
        let PreviousTarget::Legacy { identity } = &input.previous else {
            continue;
        };
        if !record
            .migration_targets
            .iter()
            .any(|target| target.permits(&input.path, &input.sha256, identity))
        {
            return Err(invalid_data(
                "Legacy replacement was not previously confirmed",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install_manifest::{InstallIdentity, SCHEMA_VERSION, SHA256_HEX_LENGTH};

    #[test]
    fn retry_requires_the_confirmed_content_and_original_object() {
        let identity = FileIdentity {
            volume_serial_number: 1,
            file_index: 2,
        };
        let input = InputFile {
            path: RelativePath::parse("legacy.dll").unwrap(),
            source: "source.dll".into(),
            sha256: "a".repeat(SHA256_HEX_LENGTH),
            previous: PreviousTarget::Legacy {
                identity: identity.clone(),
            },
        };
        let record = InstallManifest {
            schema_version: SCHEMA_VERSION,
            identity: InstallIdentity {
                product: "RustDesk".into(),
                architecture: "x86_64".into(),
                root: identity,
            },
            package_version: "1.0".into(),
            state: InstallState::Installing,
            files: Vec::new(),
            created_dirs: Vec::new(),
            pending_file: None,
            migration_targets: from_inputs(std::slice::from_ref(&input)),
        };
        verify_retry(&record, std::slice::from_ref(&input)).unwrap();
        assert!(verify_retry(&record, &[]).is_err());
        let changed_content = InputFile {
            sha256: "b".repeat(SHA256_HEX_LENGTH),
            ..input.clone()
        };
        assert!(verify_retry(&record, &[changed_content]).is_err());
        let changed_object = InputFile {
            previous: PreviousTarget::Legacy {
                identity: FileIdentity {
                    volume_serial_number: 1,
                    file_index: 3,
                },
            },
            ..input
        };
        assert!(verify_retry(&record, &[changed_object]).is_err());
    }
}
