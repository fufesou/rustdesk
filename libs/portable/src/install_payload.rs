use crate::bin_reader::{normalize_path, BinaryReader};
use std::{
    collections::{HashMap, HashSet},
    io::{self, Cursor},
};

#[path = "../../../src/install_manifest/package.rs"]
pub(crate) mod context;
#[path = "../../../src/update_hash.rs"]
mod update_hash;

const DECOMPRESS_BUFFER_SIZE: usize = 8192;
const BASE_METADATA: &str = "data/meta/package-base.json";
const CUSTOM_METADATA: &str = "data/meta/package-custom.json";

#[derive(serde::Deserialize)]
struct EmbeddedHashes {
    executable: String,
    files: HashMap<String, String>,
}

pub(crate) fn describe(reader: &mut BinaryReader) -> io::Result<context::Package> {
    let base = take_metadata(reader, BASE_METADATA)?;
    let custom = take_metadata(reader, CUSTOM_METADATA)?;
    let custom_paths: HashSet<_> = reader
        .package_paths
        .iter()
        .map(|path| normalize_path(path))
        .collect();
    let executable = normalize_path(&reader.exe);
    let mut legacy = false;
    let files = reader
        .files
        .iter()
        .map(|file| {
            let path = normalize_path(&file.path);
            let is_custom = custom_paths.contains(&path);
            let metadata = if is_custom { &custom } else { &base };
            let sha256 = if let Some(metadata) = metadata {
                let key = if !is_custom && path == executable { &metadata.executable } else { &path };
                metadata.files.get(key).cloned().ok_or_else(|| io::Error::new(
                    io::ErrorKind::InvalidData, format!("Missing embedded digest for {path}"),
                ))?
            } else {
                if !legacy {
                    eprintln!("Legacy package has no embedded SHA-256 metadata; computing payload digests");
                    legacy = true;
                }
                let mut content = brotli::Decompressor::new(Cursor::new(file.raw), DECOMPRESS_BUFFER_SIZE);
                update_hash::sha256_reader_hex(&mut content)?
            };
            Ok(context::PayloadFile {
                path: package_path(&file.path),
                sha256,
            })
        })
        .collect::<io::Result<_>>()?;
    Ok(context::Package {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        executable: package_path(&reader.exe),
        files,
    })
}

fn take_metadata(reader: &mut BinaryReader, path: &str) -> io::Result<Option<EmbeddedHashes>> {
    let Some(index) = reader
        .files
        .iter()
        .position(|file| normalize_path(&file.path) == path)
    else {
        return Ok(None);
    };
    let file = reader.files.remove(index);
    reader
        .package_paths
        .retain(|entry| normalize_path(entry) != path);
    let content = brotli::Decompressor::new(Cursor::new(file.raw), DECOMPRESS_BUFFER_SIZE);
    serde_json::from_reader(content)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn package_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    path.strip_prefix("./").unwrap_or(&path).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bin_reader::BinaryData;
    use std::io::Write;

    fn entry(path: &str, content: &[u8]) -> BinaryData {
        let mut compressed = Vec::new();
        brotli::CompressorWriter::new(&mut compressed, DECOMPRESS_BUFFER_SIZE, 1, 20)
            .write_all(content)
            .unwrap();
        BinaryData {
            path: path.to_owned(),
            raw: Box::leak(compressed.into_boxed_slice()),
            md5_code: b"",
        }
    }

    #[test]
    fn embedded_hashes_do_not_decompress_application_files() {
        let digest = "ab".repeat(32);
        let metadata = serde_json::json!({
            "executable": "rustdesk.exe", "files": {"rustdesk.exe": digest}
        });
        let mut reader = BinaryReader {
            files: vec![
                BinaryData {
                    path: "./rustdesk.exe".into(),
                    raw: b"not brotli",
                    md5_code: b"",
                },
                entry(
                    "data/meta/package-base.json",
                    metadata.to_string().as_bytes(),
                ),
            ],
            exe: "./rustdesk.exe".into(),
            package_paths: Vec::new(),
        };
        let package = describe(&mut reader).unwrap();
        assert_eq!(package.files.len(), 1);
        assert_eq!(package.files[0].sha256, digest);
        assert_eq!(reader.files.len(), 1);
    }

    #[test]
    fn custom_hashes_override_base_and_legacy_packages_still_work() {
        let digest = update_hash::sha256_reader_hex(&mut &b"custom"[..]).unwrap();
        for embedded in [false, true] {
            let base = serde_json::json!({
                "executable": "rustdesk.exe", "files": {"rustdesk.exe": "a".repeat(64), "custom.txt": "b".repeat(64)}
            });
            let custom =
                serde_json::json!({"executable": "acme.exe", "files": {"custom.txt": digest}});
            let mut reader = BinaryReader {
                files: vec![
                    entry("./ACME.exe", b"app"),
                    entry("./custom.txt", b"custom"),
                    entry(BASE_METADATA, base.to_string().as_bytes()),
                ],
                exe: "./ACME.exe".into(),
                package_paths: vec!["./custom.txt".into()],
            };
            if embedded {
                reader
                    .files
                    .push(entry(CUSTOM_METADATA, custom.to_string().as_bytes()));
                reader.package_paths.push(CUSTOM_METADATA.into());
            }
            let package = describe(&mut reader).unwrap();
            assert_eq!(package.files[0].sha256, "a".repeat(64));
            assert_eq!(package.files[1].sha256, digest);
            assert_eq!(reader.package_paths, ["./custom.txt"]);
        }
    }
}
