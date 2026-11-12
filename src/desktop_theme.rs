//! The desktop's light/dark preference, read from the XDG desktop portal
//! (GNOME, KDE and most others implement it) and watched for changes.
//!
//! winit can't tell us this on Linux (X11 reports nothing, Wayland only the
//! decoration theme), so we ask the portal ourselves through `gdbus`.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::thread;

use egui::Theme;

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";

/// The preference right now, if the desktop states one.
/// `DEX_THEME=dark|light` overrides it.
pub fn current() -> Option<Theme> {
    if let Ok(forced) = std::env::var("DEX_THEME") {
        return parse_name(&forced);
    }
    portal().or_else(gsettings)
}

/// Calls `on_change` whenever the desktop's preference changes.
/// Does nothing if the portal can't be watched or `DEX_THEME` is set.
pub fn watch(on_change: impl Fn(Theme) + Send + 'static) {
    if std::env::var_os("DEX_THEME").is_some() {
        return;
    }
    let child = Command::new("gdbus")
        .args(["monitor", "--session", "--dest", PORTAL, "--object-path", PORTAL_PATH])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return };
    let Some(out) = child.stdout.take() else { return };
    let _ = thread::Builder::new().name("dex-theme".into()).spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            // ...SettingChanged ('org.freedesktop.appearance', 'color-scheme', <uint32 1>)
            if line.contains("'org.freedesktop.appearance', 'color-scheme'")
                && let Some(theme) = parse_scheme(&line)
            {
                on_change(theme);
            }
        }
        let _ = child.wait();
    });
}

fn portal() -> Option<Theme> {
    let out = Command::new("gdbus")
        .args([
            "call", "--session", "--dest", PORTAL, "--object-path", PORTAL_PATH,
            "--method", "org.freedesktop.portal.Settings.ReadOne",
            "org.freedesktop.appearance", "color-scheme",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    parse_scheme(&String::from_utf8_lossy(&out.stdout))
}

/// GNOME without a portal: `gsettings get org.gnome.desktop.interface color-scheme`.
fn gsettings() -> Option<Theme> {
    let out = Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    match String::from_utf8_lossy(&out.stdout).trim().trim_matches('\'') {
        "prefer-dark" => Some(Theme::Dark),
        "prefer-light" => Some(Theme::Light),
        _ => None,
    }
}

/// Portal values: 0 = no preference, 1 = prefer dark, 2 = prefer light.
fn parse_scheme(text: &str) -> Option<Theme> {
    let value = text.rsplit("uint32 ").next()?;
    match value.trim_start().chars().next()? {
        '1' => Some(Theme::Dark),
        '2' => Some(Theme::Light),
        _ => None,
    }
}

fn parse_name(name: &str) -> Option<Theme> {
    match name.trim().to_ascii_lowercase().as_str() {
        "dark" => Some(Theme::Dark),
        "light" => Some(Theme::Light),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_portal_replies_and_signals() {
        assert_eq!(parse_scheme("(<uint32 1>,)"), Some(Theme::Dark));
        assert_eq!(parse_scheme("(<<uint32 2>>,)"), Some(Theme::Light));
        assert_eq!(parse_scheme("(<uint32 0>,)"), None);
        let signal = "/org/freedesktop/portal/desktop: org.freedesktop.portal.Settings.SettingChanged \
                      ('org.freedesktop.appearance', 'color-scheme', <uint32 2>)";
        assert_eq!(parse_scheme(signal), Some(Theme::Light));
        assert_eq!(parse_name("Dark"), Some(Theme::Dark));
    }
}
