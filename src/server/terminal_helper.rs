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

#[cfg(target_os = "windows")]
use {
    hbb_common::{
        anyhow::{anyhow, Context, Result},
        log,
    },
    portable_pty::{CommandBuilder, PtySize},
    std::{
        fs::File,
        io::{Read, Write},
        os::windows::io::FromRawHandle,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        thread,
        time::Duration,
    },
    windows::{
        core::PCWSTR,
        Win32::Storage::FileSystem::{
            CreateFileW, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE,
            OPEN_EXISTING,
        },
    },
};

#[cfg(target_os = "windows")]
fn get_default_shell() -> String {
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
/// Args: --terminal-helper <input_pipe_name> <output_pipe_name> <rows> <cols>
#[cfg(target_os = "windows")]
pub fn run_terminal_helper(args: &[String]) -> Result<()> {
    if args.len() < 4 {
        return Err(anyhow!(
            "Usage: --terminal-helper <input_pipe> <output_pipe> <rows> <cols>"
        ));
    }

    let input_pipe_name = &args[0];
    let output_pipe_name = &args[1];
    let rows: u16 = args[2].parse().unwrap_or(24);
    let cols: u16 = args[3].parse().unwrap_or(80);

    log::debug!("Terminal helper starting: size={}x{}", cols, rows);

    // Open named pipes (created by the service)
    let input_pipe = open_pipe_for_read(input_pipe_name)?;
    let output_pipe = open_pipe_for_write(output_pipe_name)?;

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

    let exiting = Arc::new(AtomicBool::new(false));

    // Thread: Read from input pipe, write to PTY
    let exiting_clone = exiting.clone();
    let input_thread = thread::spawn(move || {
        let mut input_pipe = input_pipe;
        let mut buf = vec![0u8; 4096];
        loop {
            if exiting_clone.load(Ordering::SeqCst) {
                break;
            }
            match input_pipe.read(&mut buf) {
                Ok(0) => {
                    log::debug!("Input pipe EOF");
                    break;
                }
                Ok(n) => {
                    if let Err(e) = pty_writer.write_all(&buf[..n]) {
                        log::error!("PTY write error: {}", e);
                        break;
                    }
                    if let Err(e) = pty_writer.flush() {
                        log::error!("PTY flush error: {}", e);
                        break;
                    }
                }
                Err(e) => {
                    if e.kind() != std::io::ErrorKind::WouldBlock {
                        log::error!("Input pipe read error: {}", e);
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
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

#[cfg(target_os = "windows")]
fn open_pipe_for_read(pipe_name: &str) -> Result<File> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;

    let wide_name: Vec<u16> = OsStr::new(pipe_name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let handle = unsafe {
        CreateFileW(
            PCWSTR::from_raw(wide_name.as_ptr()),
            FILE_GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    };

    match handle {
        Ok(h) => Ok(unsafe { File::from_raw_handle(h.0 as _) }),
        Err(e) => Err(anyhow!("Failed to open input pipe: {}", e)),
    }
}

#[cfg(target_os = "windows")]
fn open_pipe_for_write(pipe_name: &str) -> Result<File> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;

    let wide_name: Vec<u16> = OsStr::new(pipe_name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let handle = unsafe {
        CreateFileW(
            PCWSTR::from_raw(wide_name.as_ptr()),
            FILE_GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    };

    match handle {
        Ok(h) => Ok(unsafe { File::from_raw_handle(h.0 as _) }),
        Err(e) => Err(anyhow!("Failed to open output pipe: {}", e)),
    }
}

#[cfg(not(target_os = "windows"))]
pub fn run_terminal_helper(_args: &[String]) -> hbb_common::anyhow::Result<()> {
    Err(hbb_common::anyhow::anyhow!(
        "Terminal helper is only supported on Windows"
    ))
}
