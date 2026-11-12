//! Headless end-to-end check: opens an app on a virtual display, decodes the
//! stream, resizes the display, and saves frames as PNG. Never opens a window.
//!
//!   dex-probe [-s SERIAL] [APP_PACKAGE] [OUT_DIR]

use std::fs::File;
use std::io::BufWriter;
use std::sync::atomic::Ordering;
use std::thread::sleep;
use std::time::{Duration, Instant};

use dex::control::Msg;
use dex::decode::Frame;
use dex::session::{Options, Server, Session};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let serial = match args.iter().position(|a| a == "-s") {
        Some(i) => {
            args.remove(i);
            args.remove(i)
        }
        None => match dex::adb::devices()?.as_slice() {
            [one] => one.clone(),
            d => return Err(format!("need exactly one device or -s, found {d:?}").into()),
        },
    };
    let app = args.first().cloned().unwrap_or_else(|| "com.android.settings".into());
    let out = std::path::PathBuf::from(args.get(1).cloned().unwrap_or_else(|| ".".into()));

    let server = Server::find()?;
    println!("server {} ({})", server.version, server.jar.display());
    let apps = server.list_apps(&serial)?;
    println!("{} launchable apps", apps.len());

    let t = Instant::now();
    // DEX_PROBE_FPS / DEX_PROBE_BITRATE (Mbps) try other stream settings.
    let env_num = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<u32>().ok());
    let opts = Options {
        audio: true,
        max_fps: env_num("DEX_PROBE_FPS").unwrap_or(60),
        bit_rate: env_num("DEX_PROBE_BITRATE").map_or(0, |m| m * 1_000_000),
        ..Options::default()
    };
    let session = Session::start(&serial, &server, &opts, || {})?;
    println!("connected to {:?} in {:?}", session.device_name, t.elapsed());
    session.send(&Msg::StartApp(app.clone()))?;

    let first = wait_frames(&session, 30, Duration::from_secs(8))?;
    let fps = measure_fps(&session, Duration::from_secs(2));
    save(&first, &out.join("probe-1.png"))?;
    println!("frame {}x{}, ~{fps:.0} fps while idle-ish -> probe-1.png", first.width, first.height);

    session.send(&Msg::ResizeDisplay { w: 800, h: 1000 })?;
    let deadline = Instant::now() + Duration::from_secs(6);
    let resized = loop {
        let f = session.video.latest.lock().unwrap().clone();
        if let Some(f) = f.filter(|f| (f.width, f.height) != (first.width, first.height)) {
            break f;
        }
        if Instant::now() > deadline {
            let v = &session.video;
            return Err(format!(
                "display did not resize: session size {:?}, {} packets, {} frames, {} decode errors, error {:?}",
                v.session_size(),
                v.packets.load(Ordering::Relaxed),
                v.frames.load(Ordering::Relaxed),
                v.decode_errors.load(Ordering::Relaxed),
                v.error.lock().unwrap(),
            ).into());
        }
        sleep(Duration::from_millis(50));
    };
    sleep(Duration::from_millis(800)); // let the app re-layout
    let resized = session.video.latest.lock().unwrap().clone().unwrap_or(resized);
    save(&resized, &out.join("probe-2.png"))?;
    println!("resized to {}x{} -> probe-2.png", resized.width, resized.height);

    if std::env::var_os("DEX_PROBE_MOTION").is_some() {
        motion_test(&session, &resized);
    }

    // DEX_PROBE_LISTEN=N keeps streaming N more seconds (e.g. to hear audio).
    if let Some(secs) = std::env::var("DEX_PROBE_LISTEN").ok().and_then(|s| s.parse().ok()) {
        println!("listening for {secs} s...");
        sleep(Duration::from_secs(secs));
    }
    let samples = dex::audio::DECODED_SAMPLES.load(Ordering::Relaxed);
    println!("audio: {samples} samples decoded ({:.1} s), peak {:.3}", samples as f64 / 48_000.0, dex::audio::peak());
    drop(session);
    println!("closed cleanly");
    Ok(())
}

fn wait_frames(s: &Session, n: u64, timeout: Duration) -> Result<Frame, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + timeout;
    while s.video.frames.load(Ordering::Relaxed) < n {
        if let Some(e) = s.video.error.lock().unwrap().clone() {
            return Err(e.into());
        }
        if Instant::now() > deadline {
            // Static screens produce few frames; one is enough to prove decoding.
            if s.video.frames.load(Ordering::Relaxed) > 0 {
                break;
            }
            return Err("no frames decoded".into());
        }
        sleep(Duration::from_millis(20));
    }
    Ok(s.video.latest.lock().unwrap().clone().unwrap())
}

fn measure_fps(s: &Session, over: Duration) -> f64 {
    let start = s.video.frames.load(Ordering::Relaxed);
    sleep(over);
    (s.video.frames.load(Ordering::Relaxed) - start) as f64 / over.as_secs_f64()
}

fn save(f: &Frame, path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut enc = png::Encoder::new(BufWriter::new(File::create(path)?), f.width, f.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&f.rgba)?;
    Ok(())
}

/// Scrolls the app through our own control channel and measures how the
/// stream keeps up: input-to-decoded-frame latency, fps under motion, and
/// where the PC spends time per frame.
fn motion_test(s: &Session, frame: &Frame) {
    let v = &s.video;
    let frames = || v.frames.load(Ordering::Relaxed);
    let (w, h) = (frame.width as u16, frame.height as u16);
    let scroll = |dir: f32| {
        let _ = s.send(&Msg::Scroll {
            x: i32::from(w) / 2, y: i32::from(h) / 2, w, h, hscroll: 0.0, vscroll: dir, buttons: 0,
        });
    };
    let wait_idle = || {
        // Idle = no new frame for 300 ms.
        let mut last = frames();
        let mut since = Instant::now();
        while since.elapsed() < Duration::from_millis(300) {
            sleep(Duration::from_millis(10));
            if frames() != last {
                last = frames();
                since = Instant::now();
            }
        }
    };

    let mut lat = Vec::new();
    for i in 0..12 {
        wait_idle();
        let n = frames();
        let t = Instant::now();
        scroll(if i % 2 == 0 { -1.0 } else { 1.0 });
        while frames() == n && t.elapsed() < Duration::from_secs(1) {
            std::hint::spin_loop();
        }
        if frames() != n {
            lat.push(t.elapsed().as_secs_f64() * 1000.0);
        }
    }
    lat.sort_by(f64::total_cmp);
    if !lat.is_empty() {
        println!("input -> decoded frame: min {:.1} ms, median {:.1} ms, max {:.1} ms ({} samples)",
            lat[0], lat[lat.len() / 2], lat[lat.len() - 1], lat.len());
    }

    // Continuous motion: a scroll tick every 8 ms, down then up.
    wait_idle();
    let (n0, d0, c0) = (frames(), v.decode_ns.load(Ordering::Relaxed), v.convert_ns.load(Ordering::Relaxed));
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(3) {
        scroll(if t.elapsed() < Duration::from_millis(1500) { -1.0 } else { 1.0 });
        sleep(Duration::from_millis(8));
    }
    let n = (frames() - n0).max(1);
    let per = |ns: u64| ns as f64 / n as f64 / 1e6;
    println!("under motion: {:.0} fps; per frame on the PC: decode+convert {:.2} ms, of which convert {:.2} ms",
        n as f64 / t.elapsed().as_secs_f64(),
        per(v.decode_ns.load(Ordering::Relaxed) - d0),
        per(v.convert_ns.load(Ordering::Relaxed) - c0));
}
