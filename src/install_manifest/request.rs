use super::{
    collection::{FileChanges, Replacement},
    invalid_data, FileIdentity, PreviousTarget, RelativePath,
};
use serde::{Deserialize, Serialize};
use std::{io, path::PathBuf};

pub(crate) const FILE_WORKER_NAME: &str = "rustdesk-file-ops.exe";
pub(crate) const REQUEST_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Operation {
    Install,
    Update,
    Uninstall,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InputFile {
    pub path: RelativePath,
    pub source: PathBuf,
    pub sha256: String,
    pub previous: PreviousTarget,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Script {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub schema_version: u32,
    pub operation: Operation,
    pub product: String,
    pub architecture: String,
    pub root: PathBuf,
    pub root_identity: Option<FileIdentity>,
    pub registry_subkey: String,
    pub expected_manifest_sha256: Option<String>,
    pub package_version: String,
    pub preserve_unlisted: bool,
    pub files: Vec<InputFile>,
    pub before: Option<Script>,
    pub after: Option<Script>,
    pub show_result: bool,
}

impl Request {
    pub(crate) fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        let request: Self = serde_json::from_slice(bytes).map_err(invalid_data)?;
        request.validate()?;
        Ok(request)
    }

    pub(crate) fn to_bytes(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        serde_json::to_vec(self).map_err(invalid_data)
    }

    fn validate(&self) -> io::Result<()> {
        let executable =
            RelativePath::parse(&format!("{}.exe", self.product)).map_err(invalid_data)?;
        if self.schema_version != REQUEST_VERSION
            || executable.as_str().contains('/')
            || self.product.trim().is_empty()
            || self.package_version.trim().is_empty()
            || !self.root.is_absolute()
            || self.root.file_name().is_none()
            || self.architecture != std::env::consts::ARCH
        {
            return Err(invalid_data("Invalid installer request identity"));
        }
        if self.operation == Operation::Uninstall
            && (!self.files.is_empty() || self.preserve_unlisted)
        {
            return Err(invalid_data(
                "Uninstallation must use the installed ownership record",
            ));
        }
        let paths: Vec<_> = self.files.iter().map(|file| file.path.clone()).collect();
        FileChanges::new(&[], &paths, Replacement::Complete)?;
        self.validate_required_files(&paths)?;
        for file in &self.files {
            if !file.source.is_absolute() {
                return Err(invalid_data("Package source is not absolute"));
            }
            validate_hash(&file.sha256)?;
        }
        for script in [&self.before, &self.after].into_iter().flatten() {
            if !script.path.is_absolute() {
                return Err(invalid_data("Installer script path is not absolute"));
            }
            validate_hash(&script.sha256)?;
        }
        if let Some(hash) = &self.expected_manifest_sha256 {
            validate_hash(hash)?;
        }
        Ok(())
    }

    fn validate_required_files(&self, paths: &[RelativePath]) -> io::Result<()> {
        if self.operation == Operation::Uninstall {
            return Ok(());
        }
        for required in [FILE_WORKER_NAME.to_owned(), format!("{}.exe", self.product)] {
            if !paths
                .iter()
                .any(|path| path.as_str().eq_ignore_ascii_case(&required))
            {
                return Err(invalid_data(format!(
                    "Incomplete application package: {required}"
                )));
            }
        }
        Ok(())
    }
}

pub(crate) fn validate_hash(hash: &str) -> io::Result<()> {
    if hash.len() != super::SHA256_HEX_LENGTH || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid_data("Invalid package SHA-256 digest"));
    }
    Ok(())
}
