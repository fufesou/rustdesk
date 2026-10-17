#[cfg(windows)]
fn build_windows() {
    let file = "src/platform/windows.cc";
    let file2 = "src/platform/windows_delete_test_cert.cc";
    cc::Build::new().file(file).file(file2).compile("windows");
    println!("cargo:rustc-link-lib=WtsApi32");
    println!("cargo:rerun-if-changed={}", file);
    println!("cargo:rerun-if-changed={}", file2);
}

#[cfg(windows)]
fn build_installer_helper() -> Result<(), Box<dyn std::error::Error>> {
    use std::{env, fs, path::PathBuf, process::Command};

    let source = "res/installer_helper.rs";
    let output = PathBuf::from(env::var("OUT_DIR")?).join("installer-helper.exe");
    println!("cargo:rerun-if-changed={source}");
    println!("cargo:rerun-if-env-changed=RUSTDESK_INSTALLER_HELPER");
    // Release builders can sign a target-matching helper before embedding it.
    // Signing the outer application does not sign the embedded executable.
    if let Some(path) = env::var_os("RUSTDESK_INSTALLER_HELPER") {
        println!("cargo:rerun-if-changed={}", PathBuf::from(&path).display());
        fs::copy(path, output)?;
        return Ok(());
    }
    let target = env::var("TARGET")?;
    let mut compiler = Command::new(env::var_os("RUSTC").ok_or("RUSTC is not set")?);
    compiler
        .args([
            "--crate-name",
            "rustdesk_installer_helper",
            "--edition=2021",
            "--target",
        ])
        .arg(&target)
        .args([
            "-C",
            "opt-level=z",
            "-C",
            "target-feature=+crt-static",
            "-C",
            "strip=symbols",
            "-o",
        ])
        .arg(output)
        .arg(source);
    if target.ends_with("windows-msvc") {
        // The default asInvoker manifest prevents x86 installer detection from
        // requesting elevation again. The parent cmd already owns the UAC flow.
        compiler.args(["-C", "link-arg=/MANIFEST:EMBED"]);
    }
    let status = compiler.status()?;
    if !status.success() {
        return Err(format!("Installer helper compilation failed: {status}").into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn build_mac() {
    let file = "src/platform/macos.mm";
    let mut b = cc::Build::new();
    if let Ok(os_version::OsVersion::MacOS(v)) = os_version::detect() {
        let v = v.version;
        if v.contains("10.14") {
            b.flag("-DNO_InputMonitoringAuthStatus=1");
        }
    }
    b.flag("-std=c++17").file(file).compile("macos");
    println!("cargo:rerun-if-changed={}", file);
}

#[cfg(all(windows, feature = "inline"))]
fn build_manifest() {
    use std::io::Write;
    if std::env::var("PROFILE").unwrap() == "release" {
        let mut res = winres::WindowsResource::new();
        res.set_icon("res/icon.ico")
            .set_language(winapi::um::winnt::MAKELANGID(
                winapi::um::winnt::LANG_ENGLISH,
                winapi::um::winnt::SUBLANG_ENGLISH_US,
            ))
            .set_manifest_file("res/manifest.xml");
        match res.compile() {
            Err(e) => {
                write!(std::io::stderr(), "{}", e).unwrap();
                std::process::exit(1);
            }
            Ok(_) => {}
        }
    }
}

// bionic only exports getifaddrs()/freeifaddrs() from API 24, while the jniLibs
// are built against the API 21 sysroot (flutter/ndk_*.sh). webrtc-util calls
// them, so without this the android link fails on undefined symbols.
fn build_android_ifaddrs() {
    let file = "src/platform/android_ifaddrs.c";
    cc::Build::new().file(file).compile("android_ifaddrs");
    println!("cargo:rerun-if-changed={}", file);
}

fn install_android_deps() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    if target_os != "android" {
        return;
    }
    let mut target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    if target_arch == "x86_64" {
        target_arch = "x64".to_owned();
    } else if target_arch == "x86" {
        target_arch = "x86".to_owned();
    } else if target_arch == "aarch64" {
        target_arch = "arm64".to_owned();
    } else {
        target_arch = "arm".to_owned();
    }
    let target = format!("{}-android", target_arch);
    let vcpkg_root = std::env::var("VCPKG_ROOT").unwrap();
    let mut path: std::path::PathBuf = vcpkg_root.into();
    if let Ok(vcpkg_root) = std::env::var("VCPKG_INSTALLED_ROOT") {
        path = vcpkg_root.into();
    } else {
        path.push("installed");
    }
    path.push(target);
    println!(
        "cargo:rustc-link-search={}",
        path.join("lib").to_str().unwrap()
    );
    println!("cargo:rustc-link-lib=ndk_compat");
    println!("cargo:rustc-link-lib=c++");
    println!("cargo:rustc-link-lib=OpenSLES");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    hbb_common::gen_version();
    install_android_deps();
    #[cfg(all(windows, feature = "inline"))]
    build_manifest();
    #[cfg(windows)]
    build_windows();
    #[cfg(windows)]
    build_installer_helper()?;
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    if target_os == "macos" {
        #[cfg(target_os = "macos")]
        build_mac();
        println!("cargo:rustc-link-lib=framework=ApplicationServices");
    }
    if target_os == "android" {
        build_android_ifaddrs();
    }
    println!("cargo:rerun-if-changed=build.rs");
    Ok(())
}
