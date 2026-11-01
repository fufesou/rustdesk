use crate::bin_reader::BinaryReader;
use std::io::{self, Cursor};

#[path = "../../../src/install_manifest/package.rs"]
pub(crate) mod context;
#[path = "../../../src/update_hash.rs"]
mod update_hash;

const DECOMPRESS_BUFFER_SIZE: usize = 8192;

pub(crate) fn describe(reader: &BinaryReader) -> io::Result<context::Package> {
    let files = reader
        .files
        .iter()
        .map(|file| {
            let mut content =
                brotli::Decompressor::new(Cursor::new(file.raw), DECOMPRESS_BUFFER_SIZE);
            Ok(context::PayloadFile {
                path: package_path(&file.path),
                sha256: update_hash::sha256_reader_hex(&mut content)?,
            })
        })
        .collect::<io::Result<_>>()?;
    Ok(context::Package {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        executable: package_path(&reader.exe),
        files,
    })
}

fn package_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    path.strip_prefix("./").unwrap_or(&path).to_owned()
}
