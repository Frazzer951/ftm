//! Opening a link clicked in a terminal in the default browser or mail app. Only web and mail URLs reach the OS
//! opener: a pane's output is untrusted, and `file:` or another app's scheme could launch anything.

use std::process::{Command, Stdio};

const SCHEMES: &[&str] = &["http", "https", "mailto"];

fn openable(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once(':') else {
        return false;
    };
    let scheme = scheme.to_ascii_lowercase();
    SCHEMES.contains(&scheme.as_str())
        && (scheme == "mailto" || rest.starts_with("//"))
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

pub fn open(url: &str) -> Result<(), String> {
    if !openable(url) {
        return Err(format!("not opening {url:?}: only http, https and mailto links open"));
    }
    let mut child = command(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not open {url}: {e}"))?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

fn command(url: &str) -> Command {
    let mut command = Command::new(if cfg!(target_os = "macos") { "open" } else { "xdg-open" });
    command.arg(url);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_only_web_and_mail_links() {
        for url in [
            "https://example.com/a?b#c",
            "http://localhost:3000/",
            "HTTPS://EXAMPLE.COM/",
            "mailto:me@example.com",
        ] {
            assert!(openable(url), "{url}");
        }
        for url in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "vscode://file/tmp/x",
            "smb://host/share",
            "-a Calculator",
            "https:example.com",
            "https://example.com/a b",
            "https://example.com/\n",
            "/tmp/x",
            "",
        ] {
            assert!(!openable(url), "{url}");
        }
        assert!(open("file:///etc/passwd").unwrap_err().contains("only http"));
    }

    #[test]
    fn passes_the_url_as_one_argument() {
        let c = command("https://example.com/?a=1&b=2");
        assert_eq!(c.get_args().collect::<Vec<_>>(), ["https://example.com/?a=1&b=2"]);
    }
}
