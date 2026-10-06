//! Who launched the host (spec 02 section 3.2).
//!
//! | Browser | `argv` |
//! |---|---|
//! | Chrome, Edge, Brave, Chromium | `argv[1]` = `chrome-extension://<id>/`; on Windows `--parent-window=<n>` follows (ignored) |
//! | Firefox | `argv[1]` = path of the host manifest, `argv[2]` = the add-on id |
//!
//! The extension id found here is the **only authoritative identity**; the `extension_id` an extension declares in
//! its own `hello` is informational and the daemon rejects a mismatch (`nm.extension_mismatch`).

use zeus_types::BrowserName;

use crate::error::HostError;

/// How the host was launched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// The browser family derived from the argument shape. Chromium-family browsers cannot be told apart from
    /// `argv` alone and are reported as `chrome`; the installer registers one manifest per browser, so the
    /// daemon may refine this from the registration path later.
    pub browser: BrowserName,
    /// The extension id: 32 characters `a-p` for Chromium-family, the add-on id for Firefox.
    pub extension_id: String,
}

fn valid_chromium_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

fn valid_firefox_id(id: &str) -> bool {
    let len_ok = (3..=128).contains(&id.len());
    let chars_ok = id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'@' | b'.' | b'_' | b'-' | b'{' | b'}'));
    len_ok && chars_ok && (id.contains('@') || (id.starts_with('{') && id.ends_with('}')))
}

/// Parse `argv` (including the program name at index 0).
pub fn parse_launch(args: &[String]) -> Result<Launch, HostError> {
    let first = args.get(1).ok_or(HostError::Launch("no arguments"))?;
    if let Some(rest) = first.strip_prefix("chrome-extension://") {
        let id = rest.trim_end_matches('/');
        if !valid_chromium_id(id) || rest.matches('/').count() > 1 {
            return Err(HostError::Launch("malformed chrome-extension origin"));
        }
        return Ok(Launch {
            browser: BrowserName::Chrome,
            extension_id: id.to_string(),
        });
    }
    if first.ends_with(".json") {
        let id = args
            .get(2)
            .ok_or(HostError::Launch("firefox launch without an add-on id"))?;
        if !valid_firefox_id(id) {
            return Err(HostError::Launch("malformed add-on id"));
        }
        return Ok(Launch {
            browser: BrowserName::Firefox,
            extension_id: id.clone(),
        });
    }
    Err(HostError::Launch("unrecognised launch arguments"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn chromium_launch_with_and_without_parent_window() {
        let id = "abcdefghijklmnopabcdefghijklmnop";
        let origin = format!("chrome-extension://{id}/");
        let launch = parse_launch(&args(&["host", &origin])).unwrap();
        assert_eq!(
            launch,
            Launch {
                browser: BrowserName::Chrome,
                extension_id: id.into()
            }
        );
        assert!(parse_launch(&args(&["host", &origin, "--parent-window=0"])).is_ok());
    }

    #[test]
    fn firefox_launch_uses_the_second_argument() {
        let launch = parse_launch(&args(&[
            "host",
            "C:\\x\\dev.zeus.native_host.json",
            "zeus@zeus.invalid",
        ]))
        .unwrap();
        assert_eq!(launch.browser, BrowserName::Firefox);
        assert_eq!(launch.extension_id, "zeus@zeus.invalid");
        assert!(parse_launch(&args(&["host", "/x/y.json"])).is_err());
    }

    #[test]
    fn anything_else_is_refused() {
        for bad in [
            vec!["host"],
            vec!["host", "chrome-extension://short/"],
            vec![
                "host",
                "chrome-extension://ABCDEFGHIJKLMNOPABCDEFGHIJKLMNOP/",
            ],
            vec![
                "host",
                "chrome-extension://abcdefghijklmnopabcdefghijklmnop/extra/",
            ],
            vec!["host", "https://evil.example/"],
            vec!["host", "--help"],
            vec!["host", "/x/y.json", "not an id"],
        ] {
            assert!(parse_launch(&args(&bad)).is_err(), "{bad:?}");
        }
    }
}
