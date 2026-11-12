//! One scrcpy-server session: a virtual display on the phone, streamed to us.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::adb;
use crate::control::Msg;
use crate::decode::{Decoder, Frame};

const CODEC_H264: u32 = 0x6832_3634;

/// The scrcpy-server jar to push, and the version string it must be started with.
pub struct Server {
    pub jar: PathBuf,
    pub version: String,
}

impl Server {
    /// Uses `$DEX_SCRCPY_SERVER` (+ `$DEX_SCRCPY_VERSION`) or the installed scrcpy.
    pub fn find() -> io::Result<Server> {
        if let Some(jar) = std::env::var_os("DEX_SCRCPY_SERVER") {
            let version = std::env::var("DEX_SCRCPY_VERSION")
                .map_err(|_| io::Error::other("DEX_SCRCPY_SERVER needs DEX_SCRCPY_VERSION too"))?;
            return Ok(Server { jar: jar.into(), version });
        }
        let out = Command::new("scrcpy").arg("--version").stdin(Stdio::null()).output()
            .map_err(|e| io::Error::other(format!("scrcpy not found ({e}); install it or set DEX_SCRCPY_SERVER")))?;
        let text = String::from_utf8_lossy(&out.stdout);
        let version = text.split_whitespace().nth(1)
            .ok_or_else(|| io::Error::other("could not parse `scrcpy --version`"))?
            .to_owned();
        let jar = ["/usr/share/scrcpy/scrcpy-server", "/usr/local/share/scrcpy/scrcpy-server"]
            .iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
            .ok_or_else(|| io::Error::other("scrcpy-server jar not found; set DEX_SCRCPY_SERVER"))?;
        Ok(Server { jar, version })
    }

    /// Copies the jar to a path only this server process uses: the server
    /// deletes its own jar when it exits, so a shared path would race.
    fn push(&self, serial: &str, scid: u32) -> io::Result<String> {
        let remote = format!("/data/local/tmp/dex-server-{scid:08x}.jar");
        adb::push(serial, &self.jar.to_string_lossy(), &remote)?;
        Ok(remote)
    }

    /// Lists launchable apps as (package, label), sorted by label.
    pub fn list_apps(&self, serial: &str) -> io::Result<Vec<(String, String)>> {
        let jar = self.push(serial, random_scid())?;
        let cmd = format!(
            "CLASSPATH={jar} app_process / com.genymobile.scrcpy.Server {} list_apps=true log_level=info",
            self.version
        );
        Ok(parse_app_list(&adb::shell(serial, &cmd)?))
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    pub width: u32,
    pub height: u32,
    pub dpi: u32,
    /// Frame cap; 0 = as fast as the display renders.
    pub max_fps: u32,
    /// Video bitrate in bits/s; 0 = the server's default (8 Mbps).
    pub bit_rate: u32,
    /// Let the client resize the virtual display (scrcpy >= 5).
    pub flex: bool,
    /// System decorations: launcher, status/nav bars on the virtual display.
    pub decorations: bool,
    /// Play the phone's audio. Android captures all apps' audio at once, so
    /// one session per phone is enough.
    pub audio: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { width: 1280, height: 800, dpi: 200, max_fps: 60, bit_rate: 0, flex: true, decorations: false, audio: false }
    }
}

/// Shared between the video thread and whoever draws the frames.
#[derive(Default)]
pub struct Video {
    pub latest: Mutex<Option<Frame>>,
    /// Incremented on every decoded frame.
    pub frames: AtomicU64,
    pub packets: AtomicU64,
    /// Cumulative ns in decode (incl. conversion) and in conversion alone.
    pub decode_ns: AtomicU64,
    pub convert_ns: AtomicU64,
    pub decode_errors: AtomicU64,
    /// Encoder size from the latest session packet, as (width << 32 | height).
    pub session_size: AtomicU64,
    pub error: Mutex<Option<String>>,
    /// Text the phone put on its clipboard, until someone takes it.
    pub clipboard: Mutex<Option<String>>,
}

impl Video {
    pub fn session_size(&self) -> (u32, u32) {
        let v = self.session_size.load(Ordering::Relaxed);
        ((v >> 32) as u32, v as u32)
    }
}

pub struct Session {
    pub device_name: String,
    pub video: Arc<Video>,
    control: Mutex<TcpStream>,
    alive: Arc<AtomicBool>,
    server: Child,
    serial: String,
    port: u16,
}

impl Session {
    /// Starts a server on a new virtual display. `on_frame` runs on the video
    /// thread after each decoded frame (e.g. to request a repaint).
    pub fn start(
        serial: &str,
        server: &Server,
        opts: &Options,
        on_frame: impl Fn() + Send + 'static,
    ) -> io::Result<Session> {
        let scid = random_scid();
        let jar = server.push(serial, scid)?;
        let port = adb::forward(serial, &format!("scrcpy_{scid:08x}"))?;
        let mut args = vec![
            format!("CLASSPATH={jar}"),
            "app_process".into(),
            "/".into(),
            "com.genymobile.scrcpy.Server".into(),
            server.version.clone(),
            format!("scid={scid:08x}"),
            format!("log_level={}", if std::env::var_os("DEX_DEBUG").is_some() { "verbose" } else { "warn" }),
            format!("audio={}", opts.audio),
            "tunnel_forward=true".into(),
            "video_codec=h264".into(),
            format!("new_display={}x{}/{}", opts.width, opts.height, opts.dpi),
        ];
        if opts.max_fps > 0 {
            args.push(format!("max_fps={}", opts.max_fps));
        }
        if opts.bit_rate > 0 {
            args.push(format!("video_bit_rate={}", opts.bit_rate));
        }
        if opts.flex {
            args.push("flex_display=true".into());
        }
        if !opts.decorations {
            args.push("vd_system_decorations=false".into());
        }
        let mut child = adb::spawn_shell(serial, &args)?;

        let connected = connect(port, &mut child, opts.audio);
        let (mut video_sock, audio_sock, control) = match connected {
            Ok(s) => s,
            Err(e) => {
                let _ = child.kill();
                adb::remove_forward(serial, port);
                return Err(e);
            }
        };

        let mut meta = [0u8; 64];
        video_sock.read_exact(&mut meta)?;
        let end = meta.iter().position(|&b| b == 0).unwrap_or(meta.len());
        let device_name = String::from_utf8_lossy(&meta[..end]).into_owned();

        let video = Arc::new(Video::default());
        let alive = Arc::new(AtomicBool::new(true));
        {
            let (video, alive) = (video.clone(), alive.clone());
            thread::Builder::new().name("dex-video".into()).spawn(move || {
                if let Err(e) = video_loop(video_sock, &video, on_frame)
                    && alive.load(Ordering::Relaxed)
                {
                    *video.error.lock().unwrap() = Some(e.to_string());
                }
                alive.store(false, Ordering::Relaxed);
            })?;
        }

        if let Some(sock) = audio_sock {
            thread::Builder::new().name("dex-audio".into()).spawn(move || {
                if let Err(e) = crate::audio::run(sock) {
                    eprintln!("dex: audio stopped: {e}");
                }
            })?;
        }
        {
            let (reader, video) = (control.try_clone()?, video.clone());
            thread::Builder::new()
                .name("dex-device-msgs".into())
                .spawn(move || {
                    // Ends when the socket closes; nothing else to report.
                    let _ = device_messages(reader, &video);
                })?;
        }

        Ok(Session {
            device_name,
            video,
            control: Mutex::new(control),
            alive,
            server: child,
            serial: serial.to_owned(),
            port,
        })
    }

    pub fn send(&self, msg: &Msg) -> io::Result<()> {
        self.control.lock().unwrap().write_all(&msg.encode())
    }

    /// Shared flag that turns false when this session ends.
    pub fn alive_flag(&self) -> Arc<AtomicBool> {
        self.alive.clone()
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
        // Closing the control socket makes the server exit and free the display.
        let _ = self.control.lock().unwrap().shutdown(Shutdown::Both);
        let _ = self.server.kill();
        let _ = self.server.wait();
        adb::remove_forward(&self.serial, self.port);
    }
}

/// Opens the video, audio (if enabled) and control sockets, in the order the
/// server accepts them. Until the server listens, adb accepts the connection
/// and closes it, so we retry until the dummy byte arrives.
fn connect(port: u16, child: &mut Child, audio: bool) -> io::Result<(TcpStream, Option<TcpStream>, TcpStream)> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait()? {
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                let _ = e.read_to_string(&mut err);
            }
            return Err(io::Error::other(format!("server exited ({status}): {}", err.trim())));
        }
        if let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) {
            s.set_read_timeout(Some(Duration::from_secs(2)))?;
            let mut dummy = [0u8; 1];
            if matches!(s.read(&mut dummy), Ok(1)) {
                s.set_read_timeout(None)?;
                s.set_nodelay(true)?;
                let audio = if audio { Some(TcpStream::connect(("127.0.0.1", port))?) } else { None };
                let control = TcpStream::connect(("127.0.0.1", port))?;
                control.set_nodelay(true)?;
                return Ok((s, audio, control));
            }
        }
        if Instant::now() > deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "server did not come up"));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn video_loop(mut s: TcpStream, video: &Video, on_frame: impl Fn()) -> io::Result<()> {
    let mut codec = [0u8; 4];
    s.read_exact(&mut codec)?;
    let codec = u32::from_be_bytes(codec);
    if codec != CODEC_H264 {
        return Err(io::Error::other(format!("unexpected codec id {codec:#x}")));
    }
    let mut dec = Decoder::h264().map_err(io::Error::other)?;
    let mut config: Vec<u8> = Vec::new();
    let mut header = [0u8; 12];
    let mut payload = Vec::new();
    loop {
        s.read_exact(&mut header)?;
        if header[0] & 0x80 != 0 {
            // Session packet: the encoder restarts at a new size (rotation, resize).
            let w = u32::from_be_bytes(header[4..8].try_into().unwrap());
            let h = u32::from_be_bytes(header[8..12].try_into().unwrap());
            video.session_size.store(u64::from(w) << 32 | u64::from(h), Ordering::Relaxed);
            // A new session is a new H.264 stream; the old decoder state rejects it.
            dec = Decoder::h264().map_err(io::Error::other)?;
            config.clear();
            continue;
        }
        let pts_flags = u64::from_be_bytes(header[..8].try_into().unwrap());
        let is_config = pts_flags & (1 << 62) != 0;
        let len = u32::from_be_bytes(header[8..].try_into().unwrap()) as usize;
        payload.resize(len, 0);
        s.read_exact(&mut payload)?;
        if is_config {
            // SPS/PPS: hand them to the decoder together with the next frame.
            config = payload.clone();
            continue;
        }
        let packet: &[u8] = if config.is_empty() {
            &payload
        } else {
            config.extend_from_slice(&payload);
            &config
        };
        video.packets.fetch_add(1, Ordering::Relaxed);
        let (d0, c0) = (dec.decode_ns, dec.convert_ns);
        let frame = dec.decode(packet);
        config.clear();
        video.decode_ns.fetch_add(dec.decode_ns - d0, Ordering::Relaxed);
        video.convert_ns.fetch_add(dec.convert_ns - c0, Ordering::Relaxed);
        match frame {
            Ok(Some(frame)) => {
                *video.latest.lock().unwrap() = Some(frame);
                video.frames.fetch_add(1, Ordering::Relaxed);
                on_frame();
            }
            Ok(None) => {}
            // One corrupt packet shouldn't kill the stream; the next keyframe recovers.
            Err(_) => {
                video.decode_errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Reads device-to-client messages from the control socket.
fn device_messages(mut s: TcpStream, video: &Video) -> io::Result<()> {
    loop {
        let mut kind = [0u8; 1];
        s.read_exact(&mut kind)?;
        match kind[0] {
            0 => {
                // Clipboard: u32 length + UTF-8 text
                let mut len = [0u8; 4];
                s.read_exact(&mut len)?;
                let mut text = vec![0u8; u32::from_be_bytes(len) as usize];
                s.read_exact(&mut text)?;
                *video.clipboard.lock().unwrap() = Some(String::from_utf8_lossy(&text).into_owned());
            }
            1 => {
                // Clipboard ack: u64 sequence (we never ask for acks)
                s.read_exact(&mut [0u8; 8])?;
            }
            2 => {
                // UHID output: u16 id, u16 length, data (we create no UHID devices)
                let mut head = [0u8; 4];
                s.read_exact(&mut head)?;
                let mut data = vec![0u8; u16::from_be_bytes([head[2], head[3]]) as usize];
                s.read_exact(&mut data)?;
            }
            other => return Err(io::Error::other(format!("unknown device message {other}"))),
        }
    }
}

fn random_scid() -> u32 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(Instant::now().elapsed().as_nanos() ^ u128::from(std::process::id()));
    (h.finish() as u32) & 0x7fff_ffff
}

/// Parses the server's app list lines: ` * Label   com.example.pkg`
/// (`*` system, `-` user app; label and package separated by 2+ spaces).
fn parse_app_list(text: &str) -> Vec<(String, String)> {
    let mut apps: Vec<(String, String)> = text
        .lines()
        .filter_map(|l| {
            let rest = l.trim_start().strip_prefix(['*', '-'])?.trim();
            let (label, pkg) = rest.rsplit_once("  ")?;
            let (label, pkg) = (label.trim(), pkg.trim());
            (!label.is_empty() && pkg.contains('.') && !pkg.contains(' '))
                .then(|| (pkg.to_owned(), label.to_owned()))
        })
        .collect();
    apps.sort_by_key(|(_, label)| label.to_lowercase());
    apps
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_app_list() {
        let apps = super::parse_app_list(
            "[server] INFO: List of apps:\n * Ayarlar      com.android.settings\n - My  App     org.x.y\n",
        );
        let expected = [("com.android.settings", "Ayarlar"), ("org.x.y", "My  App")];
        assert_eq!(apps, expected.map(|(p, l)| (p.to_owned(), l.to_owned())));
    }
}
