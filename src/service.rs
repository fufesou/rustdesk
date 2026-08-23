use librustdesk::*;

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
#[derive(Debug, Eq, PartialEq)]
enum ServiceCommand {
    Run,
    WritePlists,
    CompleteHelperMigration,
    Invalid,
}

#[cfg(target_os = "macos")]
fn service_command(args: &[std::ffi::OsString]) -> ServiceCommand {
    match args {
        [_] => ServiceCommand::Run,
        [_, command] if command == std::ffi::OsStr::new("--write-plists") => {
            ServiceCommand::WritePlists
        }
        [_, command] if command == std::ffi::OsStr::new("--complete-helper-migration") => {
            ServiceCommand::CompleteHelperMigration
        }
        _ => ServiceCommand::Invalid,
    }
}

#[cfg(target_os = "macos")]
fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    crate::common::load_custom_client();
    hbb_common::init_log(false, "service");
    match service_command(&args) {
        ServiceCommand::WritePlists => {
            if let Err(e) = librustdesk::platform::write_plists() {
                eprintln!("Failed to write plists: {}", e);
                std::process::exit(1);
            }
            std::process::exit(0);
        }
        ServiceCommand::CompleteHelperMigration => {
            if let Err(e) = librustdesk::platform::complete_helper_migration() {
                eprintln!("Failed to complete privileged helper migration: {}", e);
                std::process::exit(1);
            }
            std::process::exit(0);
        }
        ServiceCommand::Invalid => {
            eprintln!("Invalid service command");
            std::process::exit(2);
        }
        ServiceCommand::Run => {}
    }
    if let Err(e) = crate::start_os_service() {
        eprintln!("Failed to start macOS service: {}", e);
        std::process::exit(1);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #[test]
    fn service_commands_require_exact_os_arguments() {
        let source = include_str!("service.rs");
        assert!(source.find("init_log").unwrap() < source.find("match service_command").unwrap());
        for (args, expected) in [
            (
                vec!["service".into(), "--complete-helper-migration".into()],
                super::ServiceCommand::CompleteHelperMigration,
            ),
            (
                vec![
                    "service".into(),
                    "--complete-helper-migration".into(),
                    "unexpected".into(),
                ],
                super::ServiceCommand::Invalid,
            ),
        ] {
            assert_eq!(super::service_command(&args), expected);
        }
    }
}
