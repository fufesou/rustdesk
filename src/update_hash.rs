use sha2::{Digest, Sha256};
use std::io::{self, Read, Seek, SeekFrom};

const SHA256_HEX_LENGTH: usize = 64;
const UPDATE_HASH_BUFFER_SIZE: usize = 8192;

#[derive(Debug)]
pub(crate) enum Sha256VerificationError {
    InvalidExpected,
    Mismatch {
        expected_sha256: String,
        actual_sha256: String,
    },
    Io(io::Error),
}

impl From<io::Error> for Sha256VerificationError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub(crate) fn sha256_reader_hex<R: Read>(reader: &mut R) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; UPDATE_HASH_BUFFER_SIZE];
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub(crate) fn verify_sha256_reader<R: Read + Seek>(
    reader: &mut R,
    expected_sha256: &str,
) -> Result<(), Sha256VerificationError> {
    let expected_sha256 = expected_sha256.trim().to_ascii_lowercase();
    if expected_sha256.len() != SHA256_HEX_LENGTH
        || !expected_sha256.chars().all(|c| c.is_ascii_hexdigit())
    {
        return Err(Sha256VerificationError::InvalidExpected);
    }
    reader.seek(SeekFrom::Start(0))?;
    let actual_sha256 = match sha256_reader_hex(reader) {
        Ok(actual_sha256) => actual_sha256,
        Err(error) => {
            reader.seek(SeekFrom::Start(0))?;
            return Err(error.into());
        }
    };
    reader.seek(SeekFrom::Start(0))?;
    if actual_sha256 != expected_sha256 {
        return Err(Sha256VerificationError::Mismatch {
            expected_sha256,
            actual_sha256,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, ErrorKind};

    const RUSTDESK_SHA256: &str =
        "304ca1638c5effa6832e0e15b958a8f74847efe4df9c3f3187216e921c168fed";

    struct InterruptedOnceReader {
        inner: Cursor<Vec<u8>>,
        interrupted: bool,
    }

    impl Read for InterruptedOnceReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::Error::from(ErrorKind::Interrupted));
            }
            self.inner.read(buffer)
        }
    }

    struct FailsAfterPartialRead {
        inner: Cursor<Vec<u8>>,
        read_once: bool,
    }

    impl Read for FailsAfterPartialRead {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.read_once {
                return Err(io::Error::from(ErrorKind::Other));
            }
            self.read_once = true;
            let partial_len = buffer.len().min(4);
            self.inner.read(&mut buffer[..partial_len])
        }
    }

    impl Seek for FailsAfterPartialRead {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.inner.seek(position)
        }
    }

    #[test]
    fn retries_interrupted_reads() {
        let mut reader = InterruptedOnceReader {
            inner: Cursor::new(b"rustdesk".to_vec()),
            interrupted: false,
        };

        assert_eq!(sha256_reader_hex(&mut reader).unwrap(), RUSTDESK_SHA256);
    }

    #[test]
    fn rewinds_reader_after_read_error() {
        let mut reader = FailsAfterPartialRead {
            inner: Cursor::new(b"rustdesk".to_vec()),
            read_once: false,
        };

        let result = verify_sha256_reader(&mut reader, RUSTDESK_SHA256);

        assert!(matches!(result, Err(Sha256VerificationError::Io(_))));
        assert_eq!(reader.inner.position(), 0);
    }

    #[test]
    fn verifies_sha256_and_rewinds_reader() {
        let mut reader = Cursor::new(b"rustdesk".to_vec());
        reader.set_position(reader.get_ref().len() as u64);

        verify_sha256_reader(
            &mut reader,
            &format!("  {}  ", RUSTDESK_SHA256.to_uppercase()),
        )
        .unwrap();

        assert_eq!(reader.position(), 0);
    }

    #[test]
    fn rejects_malformed_or_mismatched_sha256() {
        let mut reader = Cursor::new(b"rustdesk".to_vec());
        assert!(matches!(
            verify_sha256_reader(&mut reader, "invalid"),
            Err(Sha256VerificationError::InvalidExpected)
        ));
        assert!(matches!(
            verify_sha256_reader(&mut reader, &"0".repeat(64)),
            Err(Sha256VerificationError::Mismatch { .. })
        ));
        assert_eq!(reader.position(), 0);
    }
}
