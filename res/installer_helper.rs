#![windows_subsystem = "windows"]

// Built independently with a static CRT: privileged verification must not load
// RustDesk or Flutter DLLs from the portable application's directory.
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::Path,
    process::ExitCode,
    ptr,
};

const HASH_BYTES: usize = 32;
const READ_BUFFER_SIZE: usize = 64 * 1024;
const PROV_RSA_AES: u32 = 24;
const CRYPT_VERIFYCONTEXT: u32 = 0xf000_0000;
const CALG_SHA_256: u32 = 0x800c;
const HP_HASHVAL: u32 = 2;
const CRYPT_STRING_BASE64: u32 = 1;
const CRYPT_STRING_STRICT: u32 = 0x2000_0000;
const BASE64_GROUP_SIZE: usize = 4;
const BASE64_MAX_PADDING: usize = 2;

#[link(name = "advapi32")]
extern "system" {
    fn CryptAcquireContextW(
        provider: *mut usize,
        container: *const u16,
        name: *const u16,
        kind: u32,
        flags: u32,
    ) -> i32;
    fn CryptReleaseContext(provider: usize, flags: u32) -> i32;
    fn CryptCreateHash(
        provider: usize,
        algorithm: u32,
        key: usize,
        flags: u32,
        hash: *mut usize,
    ) -> i32;
    fn CryptDestroyHash(hash: usize) -> i32;
    fn CryptHashData(hash: usize, data: *const u8, length: u32, flags: u32) -> i32;
    fn CryptGetHashParam(
        hash: usize,
        parameter: u32,
        data: *mut u8,
        length: *mut u32,
        flags: u32,
    ) -> i32;
}

#[link(name = "crypt32")]
extern "system" {
    fn CryptStringToBinaryA(
        input: *const u8,
        length: u32,
        flags: u32,
        output: *mut u8,
        output_length: *mut u32,
        skipped: *mut u32,
        format: *mut u32,
    ) -> i32;
}

struct Provider(usize);
impl Drop for Provider {
    fn drop(&mut self) {
        if let Err(error) = check(unsafe { CryptReleaseContext(self.0, 0) }) {
            eprintln!("Failed to release hash provider: {error}");
        }
    }
}

struct Hash(usize);
impl Drop for Hash {
    fn drop(&mut self) {
        if let Err(error) = check(unsafe { CryptDestroyHash(self.0) }) {
            eprintln!("Failed to release hash: {error}");
        }
    }
}

fn check(result: i32) -> io::Result<()> {
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn sha256(path: &Path) -> io::Result<[u8; HASH_BYTES]> {
    let mut provider = 0;
    check(unsafe {
        CryptAcquireContextW(
            &mut provider,
            ptr::null(),
            ptr::null(),
            PROV_RSA_AES,
            CRYPT_VERIFYCONTEXT,
        )
    })?;
    let provider = Provider(provider);
    let mut hash = 0;
    check(unsafe { CryptCreateHash(provider.0, CALG_SHA_256, 0, 0, &mut hash) })?;
    let hash = Hash(hash);
    let mut file = File::open(path)?;
    let mut buffer = [0u8; READ_BUFFER_SIZE];
    loop {
        let length = file.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        check(unsafe { CryptHashData(hash.0, buffer.as_ptr(), length as u32, 0) })?;
    }
    let mut digest = [0u8; HASH_BYTES];
    let mut length = digest.len() as u32;
    check(unsafe { CryptGetHashParam(hash.0, HP_HASHVAL, digest.as_mut_ptr(), &mut length, 0) })?;
    Ok(digest)
}

fn verify(path: &Path, expected: &str, receipt: &Path) -> io::Result<()> {
    let digest = sha256(path)?;
    let actual: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(invalid("Installer file SHA-256 mismatch"));
    }
    // A receipt prevents a successful early process exit from authorizing a BAT
    // or update package without actually completing verification.
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(receipt)?
        .write_all(b"verified\r\n")?;
    Ok(())
}

fn decode(input: &Path, output: &Path) -> io::Result<()> {
    let text = fs::read_to_string(input)?;
    let encoded = text.trim();
    let unpadded = encoded.trim_end_matches('=');
    // CryptStringToBinary accepts incomplete groups; installer shortcuts are
    // generated as complete, padded Base64 and malformed inputs must fail.
    if encoded.is_empty()
        || encoded.len() % BASE64_GROUP_SIZE != 0
        || encoded.len() - unpadded.len() > BASE64_MAX_PADDING
        || !unpadded
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/')
    {
        return Err(invalid("Invalid installer shortcut Base64"));
    }
    let length =
        u32::try_from(encoded.len()).map_err(|_| invalid("Shortcut input is too large"))?;
    let mut output_length = 0;
    let flags = CRYPT_STRING_BASE64 | CRYPT_STRING_STRICT;
    check(unsafe {
        CryptStringToBinaryA(
            encoded.as_ptr(),
            length,
            flags,
            ptr::null_mut(),
            &mut output_length,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    })?;
    let mut bytes = vec![0u8; output_length as usize];
    check(unsafe {
        CryptStringToBinaryA(
            encoded.as_ptr(),
            length,
            flags,
            bytes.as_mut_ptr(),
            &mut output_length,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    })?;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?
        .write_all(&bytes[..output_length as usize])?;
    Ok(())
}

fn run() -> io::Result<()> {
    let arguments: Vec<OsString> = std::env::args_os().collect();
    match arguments.as_slice() {
        [_, command, path, expected, receipt] if command == "--verify-install-file" => {
            let expected = expected
                .to_str()
                .ok_or_else(|| invalid("Invalid SHA-256 argument"))?;
            verify(Path::new(path), expected, Path::new(receipt))
        }
        [_, command, input, output] if command == "--decode-install-shortcut" => {
            decode(Path::new(input), Path::new(output))
        }
        _ => Err(invalid("Invalid installer helper arguments")),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Installer helper failed: {error}");
            ExitCode::FAILURE
        }
    }
}
