use std::{error::Error, fmt};

const INVALID_COMPONENT_CHARACTERS: &str = "<>:\"|?*";
const DEVICE_NAMES: &[&str] = &["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];
const DEVICE_SUFFIXES: &[&str] = &["1", "2", "3", "4", "5", "6", "7", "8", "9", "¹", "²", "³"];

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RelativePath(String);

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct PathError {
    path: String,
    reason: &'static str,
}

impl fmt::Display for PathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Invalid manifest path {:?}: {}",
            self.path, self.reason
        )
    }
}

impl Error for PathError {}

impl RelativePath {
    // This only checks spelling; filesystem identity and reparse checks are still required.
    pub(crate) fn parse(value: &str) -> Result<Self, PathError> {
        let normalized = value.replace('\\', "/");
        for component in normalized.split('/') {
            validate_component(component).map_err(|reason| PathError {
                path: value.to_owned(),
                reason,
            })?;
        }
        Ok(Self(normalized))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn validate_payload_file(&self) -> Result<(), PathError> {
        let path = self.0.to_ascii_lowercase();
        if path == "meta.toml"
            || path.starts_with("meta.toml/")
            || path == "data"
            || path == "data/meta"
            || path.starts_with("data/meta/")
        {
            return Err(PathError {
                path: self.0.clone(),
                reason: "path conflicts with installer control metadata",
            });
        }
        Ok(())
    }
}

fn validate_component(component: &str) -> Result<(), &'static str> {
    if component.is_empty() || matches!(component, "." | "..") {
        return Err("empty, current-directory and parent-directory components are forbidden");
    }
    if component.ends_with(['.', ' ']) {
        return Err("trailing dots or spaces are ambiguous on Windows");
    }
    if component
        .chars()
        .any(|character| character < ' ' || INVALID_COMPONENT_CHARACTERS.contains(character))
    {
        return Err("Windows path prefixes, streams and invalid characters are forbidden");
    }
    if is_device_name(component) {
        return Err("Windows device names are forbidden");
    }
    Ok(())
}

fn is_device_name(component: &str) -> bool {
    let stem = component
        .split_once('.')
        .map_or(component, |(stem, _)| stem)
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    if DEVICE_NAMES.contains(&stem.as_str()) {
        return true;
    }
    stem.strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|suffix| DEVICE_SUFFIXES.contains(&suffix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_relative_file_names_and_normalizes_separators() {
        for (input, expected) in [
            ("RustDesk.exe", "RustDesk.exe"),
            (
                r"data\flutter_assets\AssetManifest.bin",
                "data/flutter_assets/AssetManifest.bin",
            ),
            ("assets/中文 logo.png", "assets/中文 logo.png"),
            ("assets/com10.txt", "assets/com10.txt"),
        ] {
            let path = RelativePath::parse(input).unwrap();
            assert_eq!(path.as_str(), expected);
            assert!(path.validate_payload_file().is_ok());
        }
    }

    #[test]
    fn rejects_escaping_and_ambiguous_windows_paths() {
        for input in [
            "",
            ".",
            "..",
            "./app.exe",
            "data/../app.exe",
            "data/./file",
            "data//file",
            "data/",
            "/app.exe",
            r"\app.exe",
            r"C:\app.exe",
            "C:app.exe",
            r"\\server\share\app.exe",
            r"\\?\C:\app.exe",
            "app.exe:stream",
            "data./file",
            "data /file",
            "data/file.",
            "data/file ",
            "data/a\0b",
            "data/a\nb",
            "data/*.dll",
            "data/a?b",
            "data/a\"b",
            "data/a<b",
            "data/a>b",
            "data/a|b",
            "CON",
            "data/nul.txt",
            "data/COM1.log",
            "data/lpt9",
            "data/COM¹.txt",
            "data/LPT²",
            "data/CON .txt",
            "CONOUT$",
        ] {
            assert!(RelativePath::parse(input).is_err(), "accepted {input:?}");
        }
    }

    #[test]
    fn reserves_control_metadata_without_blocking_flutter_assets() {
        for input in [
            "meta.toml",
            "META.TOML/child",
            "data",
            "DATA/META",
            r"Data\Meta\install-manifest.json",
            "data/meta/meta.toml",
        ] {
            let path = RelativePath::parse(input).unwrap();
            assert!(path.validate_payload_file().is_err(), "accepted {input:?}");
        }
        for input in [
            "data/meta.txt",
            "data/metadata/file",
            "data/flutter_assets/meta.toml",
        ] {
            assert!(RelativePath::parse(input)
                .unwrap()
                .validate_payload_file()
                .is_ok());
        }
    }
}
