use librustdesk::*;

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "--write-plists" {
        hbb_common::init_log(false, "service");
        if let Err(e) = librustdesk::platform::write_plists() {
            eprintln!("Failed to write plists: {}", e);
        }
        return;
    }
    crate::common::load_custom_client();
    hbb_common::init_log(false, "service");
    crate::start_os_service();
}
