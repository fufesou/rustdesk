pub(super) fn elevation_script(executable: &str, args: &[&str], prompt: &str) -> String {
    let command = std::iter::once(executable)
        .chain(args.iter().copied())
        .map(|arg| format!("'{}'", arg.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        r#"do shell script "{}" with prompt "{}" with administrator privileges"#,
        escape_applescript(&command),
        escape_applescript(prompt)
    )
}

fn escape_applescript(text: &str) -> String {
    // Shell quoting and AppleScript string escaping apply at separate layers.
    text.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

#[cfg(test)]
mod tests {
    use super::elevation_script;

    #[test]
    fn elevation_script_preserves_spaces() {
        assert_eq!(
            elevation_script(
                "/Applications/RustDesk Client.app/Contents/MacOS/RustDesk Client",
                &["--approve-main-ipc", "request id"],
                "Unlock RustDesk settings",
            ),
            r#"do shell script "'/Applications/RustDesk Client.app/Contents/MacOS/RustDesk Client' '--approve-main-ipc' 'request id'" with prompt "Unlock RustDesk settings" with administrator privileges"#
        );
    }

    #[test]
    fn elevation_script_escapes_shell_and_applescript_text() {
        assert_eq!(
            elevation_script(
                "/Applications/RustDesk's \"Client\"\\Preview.app/Contents/MacOS/RustDesk",
                &["--approve-main-ipc", "$TOKEN;`command`\n\r\t", ""],
                "Unlock \"RustDesk\"\\settings\n\r\t",
            ),
            r#"do shell script "'/Applications/RustDesk'\\''s \"Client\"\\Preview.app/Contents/MacOS/RustDesk' '--approve-main-ipc' '$TOKEN;`command`\n\r\t' ''" with prompt "Unlock \"RustDesk\"\\settings\n\r\t" with administrator privileges"#
        );
    }
}
