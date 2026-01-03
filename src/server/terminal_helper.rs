//! Terminal Helper Process
//!
//! This module implements a helper process that runs as the logged-in user and creates
//! the ConPTY + Shell. This is necessary because ConPTY has compatibility issues with
//! CreateProcessAsUserW when the ConPTY is created by a different user (SYSTEM service).
//!
//! Architecture:
//! ```
//! SYSTEM Service (terminal_service.rs)
//!     |
//!     +-- CreateProcessAsUserW --> Terminal Helper (this module, runs as user)
//!     |                                |
//!     |                                +-- CreateProcessW + ConPTY --> Shell
//!     |                                |
//!     +-- Named Pipes <----------------+
//! ```

use hbb_common::{
    anyhow::{anyhow, Context, Result},
    log,
};
use portable_pty::{CommandBuilder, MasterPty, PtySize};
use std::{
    ffi::OsStr,
    fs::File,
    io::{Read, Write},
    os::windows::{ffi::OsStrExt, io::FromRawHandle},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};
use windows::{
    core::PCWSTR,
    Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    },
};

/// Message type constants for helper protocol.
/// Used to distinguish between terminal data and control commands.
pub const MSG_TYPE_DATA: u8 = 0x00;
pub const MSG_TYPE_RESIZE: u8 = 0x01;

/// Message header size: 1 byte type + 4 bytes length
pub const MSG_HEADER_SIZE: usize = 5;

/// Maximum payload size to prevent denial of service from malicious messages.
/// 16MB should be more than enough for any legitimate terminal data.
const MAX_PAYLOAD_SIZE: usize = 16 * 1024 * 1024;

/// Get the default shell for Windows.
/// Shared implementation used by both terminal_service and terminal_helper.
pub fn get_default_shell() -> String {
    // Try PowerShell Core first
    let pwsh_paths = [
        "pwsh.exe",
        r"C:\Program Files\PowerShell\7\pwsh.exe",
        r"C:\Program Files\PowerShell\6\pwsh.exe",
    ];

    for path in &pwsh_paths {
        if std::path::Path::new(path).exists() {
            return path.to_string();
        }
    }

    // Try Windows PowerShell
    let powershell_path = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
    if std::path::Path::new(powershell_path).exists() {
        return powershell_path.to_string();
    }

    // Fallback to cmd.exe
    std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
}

/// Run terminal helper process
/// Args: --terminal-helper <input_pipe_name> <output_pipe_name> <rows> <cols> [terminal_id]
pub fn run_terminal_helper(args: &[String]) -> Result<()> {
    if args.len() < 4 {
        return Err(anyhow!(
            "Usage: --terminal-helper <input_pipe> <output_pipe> <rows> <cols> [terminal_id]"
        ));
    }

    let input_pipe_name = &args[0];
    let output_pipe_name = &args[1];
    let rows: u16 = args[2].parse().unwrap_or(24);
    let cols: u16 = args[3].parse().unwrap_or(80);
    let terminal_id: i32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);

    log::debug!(
        "Terminal helper starting: terminal_id={}, size={}x{}",
        terminal_id,
        cols,
        rows
    );

    // Open named pipes (created by the service)
    let input_pipe = open_pipe(input_pipe_name, true)?;
    let output_pipe = open_pipe(output_pipe_name, false)?;

    // Create ConPTY and shell
    let pty_size = PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    };

    let pty_system = portable_pty::native_pty_system();
    let pty_pair = pty_system.openpty(pty_size).context("Failed to open PTY")?;

    let shell = get_default_shell();
    log::debug!("Using shell: {}", shell);

    let cmd = CommandBuilder::new(&shell);
    let mut child = pty_pair
        .slave
        .spawn_command(cmd)
        .context("Failed to spawn shell")?;

    let pid = child.process_id().unwrap_or(0);
    log::debug!("Shell started with PID: {}", pid);

    let mut pty_writer = pty_pair
        .master
        .take_writer()
        .context("Failed to get PTY writer")?;

    let mut pty_reader = pty_pair
        .master
        .try_clone_reader()
        .context("Failed to get PTY reader")?;

    // Wrap pty_pair.master in Arc<Mutex> for sharing with input thread (for resize)
    let pty_master: Arc<Mutex<Box<dyn MasterPty + Send>>> = Arc::new(Mutex::new(pty_pair.master));

    let exiting = Arc::new(AtomicBool::new(false));

    // Thread: Read from input pipe, parse messages, write data to PTY or handle control commands
    let exiting_clone = exiting.clone();
    let pty_master_clone = pty_master.clone();
    let input_thread = thread::spawn(move || {
        let mut input_pipe = input_pipe;
        let mut header_buf = [0u8; MSG_HEADER_SIZE];
        let mut payload_buf = vec![0u8; 4096];

        loop {
            if exiting_clone.load(Ordering::SeqCst) {
                break;
            }

            // Read message header
            match read_exact_or_eof(&mut input_pipe, &mut header_buf) {
                Ok(false) => {
                    log::debug!("Input pipe EOF");
                    break;
                }
                Ok(true) => {}
                Err(e) => {
                    log::error!("Input pipe header read error: {}", e);
                    break;
                }
            }

            let msg_type = header_buf[0];
            let payload_len =
                u32::from_le_bytes([header_buf[1], header_buf[2], header_buf[3], header_buf[4]])
                    as usize;

            // Validate payload length to prevent denial of service
            if payload_len > MAX_PAYLOAD_SIZE {
                log::error!(
                    "Payload too large: {} bytes (max {})",
                    payload_len,
                    MAX_PAYLOAD_SIZE
                );
                break;
            }

            // Ensure payload buffer is large enough
            if payload_buf.len() < payload_len {
                payload_buf.resize(payload_len, 0);
            }

            // Read payload
            if payload_len > 0 {
                match read_exact_or_eof(&mut input_pipe, &mut payload_buf[..payload_len]) {
                    Ok(false) => {
                        log::debug!("Input pipe EOF during payload read");
                        break;
                    }
                    Ok(true) => {}
                    Err(e) => {
                        log::error!("Input pipe payload read error: {}", e);
                        break;
                    }
                }
            }

            match msg_type {
                MSG_TYPE_DATA => {
                    // Write terminal data to PTY
                    if let Err(e) = pty_writer.write_all(&payload_buf[..payload_len]) {
                        log::error!("PTY write error: {}", e);
                        break;
                    }
                    if let Err(e) = pty_writer.flush() {
                        log::error!("PTY flush error: {}", e);
                        break;
                    }
                }
                MSG_TYPE_RESIZE => {
                    // Parse resize command: rows (u16 LE) + cols (u16 LE)
                    if payload_len >= 4 {
                        let rows = u16::from_le_bytes([payload_buf[0], payload_buf[1]]);
                        let cols = u16::from_le_bytes([payload_buf[2], payload_buf[3]]);
                        log::debug!("Resize command received: {}x{}", cols, rows);

                        if let Ok(master) = pty_master_clone.lock() {
                            if let Err(e) = master.resize(PtySize {
                                rows,
                                cols,
                                pixel_width: 0,
                                pixel_height: 0,
                            }) {
                                log::error!("PTY resize error: {}", e);
                            }
                        }
                    } else {
                        log::warn!("Invalid resize payload length: {}", payload_len);
                    }
                }
                _ => {
                    log::warn!("Unknown message type: {}", msg_type);
                }
            }
        }
        log::debug!("Input thread exiting");
    });

    // Thread: Read from PTY, write to output pipe
    let exiting_clone = exiting.clone();
    let output_thread = thread::spawn(move || {
        let mut output_pipe = output_pipe;
        let mut buf = vec![0u8; 4096];
        loop {
            if exiting_clone.load(Ordering::SeqCst) {
                break;
            }
            match pty_reader.read(&mut buf) {
                Ok(0) => {
                    log::debug!("PTY EOF");
                    break;
                }
                Ok(n) => {
                    if let Err(e) = output_pipe.write_all(&buf[..n]) {
                        log::error!("Output pipe write error: {}", e);
                        break;
                    }
                    if let Err(e) = output_pipe.flush() {
                        log::error!("Output pipe flush error: {}", e);
                        break;
                    }
                }
                Err(e) => {
                    if e.kind() != std::io::ErrorKind::WouldBlock {
                        log::error!("PTY read error: {}", e);
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            }
        }
        log::debug!("Output thread exiting");
    });

    // Wait for child process to exit
    let exit_status = child.wait();
    log::info!("Shell exited: {:?}", exit_status);

    exiting.store(true, Ordering::SeqCst);

    // Wait for threads
    let _ = input_thread.join();
    let _ = output_thread.join();

    log::info!("Terminal helper exiting");
    Ok(())
}

/// Read exactly `buf.len()` bytes from reader.
/// Returns Ok(true) if successful, Ok(false) on EOF, Err on error.
fn read_exact_or_eof<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<bool> {
    let mut pos = 0;
    while pos < buf.len() {
        match reader.read(&mut buf[pos..]) {
            Ok(0) => return Ok(false), // EOF
            Ok(n) => pos += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// Open a named pipe as a client.
/// `for_read`: true for reading (input pipe), false for writing (output pipe).
fn open_pipe(pipe_name: &str, for_read: bool) -> Result<File> {
    let wide_name: Vec<u16> = OsStr::new(pipe_name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let access = if for_read {
        FILE_GENERIC_READ.0
    } else {
        FILE_GENERIC_WRITE.0
    };

    let handle = unsafe {
        CreateFileW(
            PCWSTR::from_raw(wide_name.as_ptr()),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    };

    match handle {
        Ok(h) => Ok(unsafe { File::from_raw_handle(h.0 as _) }),
        Err(e) => Err(anyhow!(
            "Failed to open {} pipe '{}': {}",
            if for_read { "input" } else { "output" },
            pipe_name,
            e
        )),
    }
}
