//! What the shell needs from a phone, as traits, so the UI can run against
//! a real device or a fake one in tests.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::control::Msg;
use crate::session::{Options, Server, Session, Video};

#[derive(Clone, Debug, PartialEq)]
pub struct AppInfo {
    pub package: String,
    pub label: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub device_name: String,
    pub battery: Option<u8>,
    pub charging: bool,
}

/// A live app window on the phone.
pub trait Surface: Send {
    fn video(&self) -> &Arc<Video>;
    fn send(&self, msg: Msg);
    fn alive(&self) -> bool;
}

pub trait Backend: Send + Sync {
    /// Launchable apps. May block (the shell calls it off the UI thread).
    fn apps(&self) -> Result<Vec<AppInfo>, String>;
    /// Opens `package` on a new display of `size` pixels. May block.
    /// `repaint` is called whenever a new frame is ready.
    fn open(&self, package: &str, size: [u32; 2], dpi: u32, repaint: Box<dyn Fn() + Send>)
        -> Result<Box<dyn Surface>, String>;
    fn status(&self) -> Status;
    /// Launcher icons as (package, PNG bytes). Needs the companion app; may block.
    fn icons(&self) -> Result<Vec<(String, Vec<u8>)>, String> {
        Err("no icon source".into())
    }
}

/// Parses the companion's icon bundle: [u16 len][package][u32 len][png]...
pub fn parse_icon_bundle(mut b: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut icons = Vec::new();
    while b.len() >= 2 {
        let n = u16::from_be_bytes([b[0], b[1]]) as usize;
        let Some(name) = b.get(2..2 + n) else { break };
        let Some(len) = b.get(2 + n..6 + n) else { break };
        let len = u32::from_be_bytes(len.try_into().unwrap()) as usize;
        let Some(png) = b.get(6 + n..6 + n + len) else { break };
        icons.push((String::from_utf8_lossy(name).into_owned(), png.to_vec()));
        b = &b[6 + n + len..];
    }
    icons
}

/// A phone reached through adb, streamed with scrcpy-server.
pub struct AdbBackend {
    serial: String,
    server: Server,
    status: Arc<Mutex<Status>>,
    /// The session currently playing the phone's audio, if any.
    audio_owner: Mutex<Option<Arc<std::sync::atomic::AtomicBool>>>,
}

impl AdbBackend {
    pub fn new(serial: String) -> Result<Self, String> {
        let server = Server::find().map_err(|e| e.to_string())?;
        let status = Arc::new(Mutex::new(Status::default()));
        let (s, st) = (serial.clone(), status.clone());
        thread::Builder::new()
            .name("dex-status".into())
            .spawn(move || poll_status(&s, &st))
            .map_err(|e| e.to_string())?;
        Ok(AdbBackend { serial, server, status, audio_owner: Mutex::new(None) })
    }
}

fn poll_status(serial: &str, status: &Mutex<Status>) {
    let model = crate::adb::shell(serial, "getprop ro.product.model").unwrap_or_default();
    let brand = crate::adb::shell(serial, "getprop ro.product.brand").unwrap_or_default();
    let name = format!("{} {}", capitalize(brand.trim()), model.trim()).trim().to_owned();
    loop {
        let Ok(battery) = crate::adb::shell(serial, "dumpsys battery") else { return };
        let field = |key: &str| {
            battery.lines().find_map(|l| l.trim().strip_prefix(key).map(|v| v.trim().to_owned()))
        };
        *status.lock().unwrap() = Status {
            device_name: name.clone(),
            battery: field("level:").and_then(|v| v.parse().ok()),
            // BatteryManager.BATTERY_STATUS_CHARGING = 2, FULL = 5
            charging: matches!(field("status:").as_deref(), Some("2" | "5")),
        };
        thread::sleep(Duration::from_secs(30));
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

impl Backend for AdbBackend {
    fn apps(&self) -> Result<Vec<AppInfo>, String> {
        let apps = self.server.list_apps(&self.serial).map_err(|e| e.to_string())?;
        Ok(apps.into_iter().map(|(package, label)| AppInfo { package, label }).collect())
    }

    fn open(&self, package: &str, size: [u32; 2], dpi: u32, repaint: Box<dyn Fn() + Send>)
        -> Result<Box<dyn Surface>, String> {
        // Phone audio is global, so only one live window carries it.
        let mut owner = self.audio_owner.lock().unwrap();
        let audio = !owner.as_ref().is_some_and(|a| a.load(std::sync::atomic::Ordering::Relaxed));
        let opts = Options { width: size[0], height: size[1], dpi, audio, ..Options::default() };
        let session = Session::start(&self.serial, &self.server, &opts, repaint)
            .map_err(|e| e.to_string())?;
        if audio {
            *owner = Some(session.alive_flag());
        }
        drop(owner);
        session.send(&Msg::StartApp(package.to_owned())).map_err(|e| e.to_string())?;
        Ok(Box::new(session))
    }

    fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    fn icons(&self) -> Result<Vec<(String, Vec<u8>)>, String> {
        // Fetching all icons takes seconds, so keep the last bundle per device.
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
            .map(|d| d.join("dex").join(format!("icons-{}.bin", self.serial.replace(['/', ':'], "_"))));
        if let Some(bundle) = cache.as_ref().and_then(|c| std::fs::read(c).ok()) {
            return Ok(parse_icon_bundle(&bundle));
        }
        let bundle = crate::adb::exec_out(&self.serial, "content read --uri content://dev.dex.companion.icons/_all?size=128")
            .map_err(|e| e.to_string())?;
        let icons = parse_icon_bundle(&bundle);
        if icons.is_empty() {
            // e.g. "Could not find provider": the companion app isn't installed.
            return Err(String::from_utf8_lossy(&bundle[..bundle.len().min(200)]).trim().to_owned());
        }
        if let Some(c) = cache {
            let _ = c.parent().map(std::fs::create_dir_all);
            let _ = std::fs::write(c, &bundle);
        }
        Ok(icons)
    }
}

impl Surface for Session {
    fn video(&self) -> &Arc<Video> {
        &self.video
    }

    fn send(&self, msg: Msg) {
        // A dead socket shows up as `alive() == false`; the window reports that.
        let _ = Session::send(self, &msg);
    }

    fn alive(&self) -> bool {
        self.is_alive()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn icon_bundle_round_trip_and_truncation() {
        let mut b = Vec::new();
        for (pkg, png) in [("a.b", &b"PNG1"[..]), ("c.d", &b"PNG22"[..])] {
            b.extend((pkg.len() as u16).to_be_bytes());
            b.extend(pkg.as_bytes());
            b.extend((png.len() as u32).to_be_bytes());
            b.extend(png);
        }
        let icons = super::parse_icon_bundle(&b);
        assert_eq!(icons, [("a.b".to_owned(), b"PNG1".to_vec()), ("c.d".to_owned(), b"PNG22".to_vec())]);
        // A cut-off stream keeps the complete records only.
        assert_eq!(super::parse_icon_bundle(&b[..b.len() - 2]).len(), 1);
    }
}
