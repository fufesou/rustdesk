#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use std::process::Command;

fn main() {
    let current_exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("Failed to get current executable path: {}", e);
            std::process::exit(1);
        }
    };

    let exe_dir = match current_exe.parent() {
        Some(dir) => dir,
        None => {
            eprintln!("Failed to get executable directory");
            std::process::exit(1);
        }
    };

    let exe_name = match current_exe.file_name() {
        Some(name) => name,
        None => {
            eprintln!("Failed to get executable name");
            std::process::exit(1);
        }
    };

    let exe_name_str = exe_name.to_string_lossy();
    let exe_name_lower = exe_name_str.to_lowercase();
    
    let target_name = if exe_name_lower.ends_with("runner.exe") {
        let base = exe_name_str[..exe_name_str.len() - 10].to_string();
        format!("{}.exe", base)
    } else if exe_name_lower.ends_with("runner") {
        exe_name_str[..exe_name_str.len() - 6].to_string()
    } else {
        eprintln!("Executable name does not end with 'runner'");
        std::process::exit(1);
    };

    let target_path = exe_dir.join(&target_name);

    if !target_path.exists() {
        eprintln!("Target executable not found: {}", target_path.display());
        std::process::exit(1);
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    
    let mut cmd = Command::new(&target_path);
    cmd.args(&args);
    
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0);
    }

    match cmd.spawn() {
        Ok(_) => {
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("Failed to spawn process: {}", e);
            std::process::exit(1);
        }
    }
}
