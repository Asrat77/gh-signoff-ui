//! Opening links in the system browser.
//!
//! eframe is built without its `web_browser` feature, so `Context::open_url`
//! does nothing here; every link goes through the `open` crate instead.

use egui::Context;
use signoff_ui::Status;
use signoff_ui::widgets::toast;

/// Opens `url` in the default browser, with a toast when that fails.
pub fn open(ctx: &Context, url: &str) {
    if !is_web_url(url) {
        toast(ctx, format!("Not a web link: {url}"), Status::Failing);
    } else if let Err(err) = open::that_detached(url) {
        toast(ctx, format!("Could not open the browser: {err}"), Status::Failing);
    }
}

/// Only http(s) links are handed to the system, never file paths or other schemes.
fn is_web_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

#[cfg(test)]
mod tests {
    use super::is_web_url;

    #[test]
    fn only_web_links_pass() {
        assert!(is_web_url("https://github.com/o/r/pull/1"));
        assert!(!is_web_url("file:///etc/passwd"));
        assert!(!is_web_url("/usr/bin/true"));
    }
}
