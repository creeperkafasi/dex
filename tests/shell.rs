//! Drives the real shell UI against a fake phone, rendered offscreen with wgpu.
//! Set DEX_SHOTS=<dir> to save screenshots of each step.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dex::backend::{AppInfo, Backend, Status, Surface};
use dex::control::{self, Msg};
use dex::decode::Frame;
use dex::session::Video;
use dex::shell::Shell;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, pos2, vec2};
use egui_kittest::{Harness, kittest::Queryable};

const APPS: &[(&str, &str)] = &[
    ("com.android.settings", "Ayarlar"),
    ("com.android.chrome", "Chrome"),
    ("com.google.android.apps.maps", "Maps"),
    ("com.spotify.music", "Spotify"),
    ("org.telegram.messenger", "Telegram"),
    ("com.google.android.youtube", "YouTube"),
    ("com.google.android.gm", "Gmail"),
    ("com.whatsapp", "WhatsApp"),
    ("com.google.android.calendar", "Calendar"),
    ("com.android.camera", "Camera"),
    ("com.google.android.keep", "Keep Notes"),
    ("org.mozilla.firefox", "Firefox"),
];

/// Every message sent to the fake phone, with the package of its window.
type Sent = Arc<Mutex<Vec<(String, Msg)>>>;

#[derive(Default)]
struct Fake {
    sent: Sent,
    /// Video state of every opened window, to play the phone's side.
    videos: Mutex<Vec<Arc<Video>>>,
    /// (package, dpi) of every display opened.
    opened: Mutex<Vec<(String, u32)>>,
}

struct FakeSurface {
    package: String,
    video: Arc<Video>,
    sent: Sent,
}

fn frame(w: u32, h: u32, seed: u8) -> Frame {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let band = ((y * 8 / h.max(1)) % 2) as u8;
            rgba.extend([seed.wrapping_add((x * 120 / w) as u8), 60 + band * 30, 160 - (y * 100 / h) as u8, 255]);
        }
    }
    Frame { width: w, height: h, rgba }
}

fn publish(video: &Video, f: Frame) {
    *video.latest.lock().unwrap() = Some(f);
    video.frames.fetch_add(1, Ordering::Relaxed);
}

impl Surface for FakeSurface {
    fn video(&self) -> &Arc<Video> {
        &self.video
    }
    fn send(&self, msg: Msg) {
        // Flex display: the "phone" follows resize requests like the real server.
        if let Msg::ResizeDisplay { w, h } = msg {
            publish(&self.video, frame(w.into(), h.into(), 90));
        }
        self.sent.lock().unwrap().push((self.package.clone(), msg));
    }
    fn alive(&self) -> bool {
        true
    }
}

impl Backend for Fake {
    fn apps(&self) -> Result<Vec<AppInfo>, String> {
        Ok(APPS.iter().map(|(p, l)| AppInfo { package: p.to_string(), label: l.to_string() }).collect())
    }
    fn open(&self, package: &str, size: [u32; 2], dpi: u32, repaint: Box<dyn Fn() + Send>)
        -> Result<Box<dyn Surface>, String> {
        self.opened.lock().unwrap().push((package.to_owned(), dpi));
        let video = Arc::new(Video::default());
        publish(&video, frame(size[0], size[1], package.len() as u8 * 20));
        self.videos.lock().unwrap().push(video.clone());
        repaint();
        Ok(Box::new(FakeSurface { package: package.into(), video, sent: self.sent.clone() }))
    }
    fn status(&self) -> Status {
        Status { device_name: "Xiaomi 24116RACCG".into(), battery: Some(64), charging: true }
    }
}

struct State {
    backend: Arc<Fake>,
    shell: Option<Shell>,
}

fn harness() -> (Harness<'static, State>, Sent) {
    harness_with(egui::Theme::Dark)
}

fn harness_with(theme: egui::Theme) -> (Harness<'static, State>, Sent) {
    let backend = Arc::new(Fake::default());
    let sent = backend.sent.clone();
    let h = Harness::builder()
        .with_size(vec2(1440.0, 900.0))
        .with_max_steps(20)
        .with_theme(theme)
        .wgpu()
        .build_ui_state(
            |ui, s: &mut State| {
                let backend = s.backend.clone();
                s.shell.get_or_insert_with(|| Shell::new(backend, ui.ctx())).ui(ui);
            },
            State { backend, shell: None },
        );
    (h, sent)
}

/// Lets background threads (app list, window start) finish, then settles the UI.
fn settle(h: &mut Harness<'static, State>) {
    for _ in 0..4 {
        std::thread::sleep(Duration::from_millis(30));
        h.run_steps(2);
    }
}

fn shot(h: &mut Harness<'static, State>, name: &str) {
    let Some(dir) = std::env::var_os("DEX_SHOTS") else { return };
    let img = h.render().expect("render");
    img.save(std::path::Path::new(&dir).join(format!("{name}.png"))).unwrap();
}

fn shell<'a>(h: &'a Harness<'static, State>) -> &'a Shell {
    h.state().shell.as_ref().unwrap()
}

#[test]
fn desktop_flow() {
    let (mut h, sent) = harness();
    settle(&mut h);
    assert!(shell(&h).launcher_open(), "launcher opens on start");
    shot(&mut h, "1-launcher");

    // Click a tile: opens a window and closes the launcher.
    h.get_by_label("Ayarlar").click();
    settle(&mut h);
    assert_eq!(shell(&h).window_apps(), ["com.android.settings"]);
    assert!(!shell(&h).launcher_open());
    let start = sent.lock().unwrap().len();

    // Ctrl+Space, type, Enter: second app via search.
    h.key_press_modifiers(Modifiers::CTRL, Key::Space);
    settle(&mut h);
    assert!(shell(&h).launcher_open());
    h.event(Event::Text("chro".into()));
    settle(&mut h);
    shot(&mut h, "2-search");
    h.key_press(Key::Enter);
    settle(&mut h);
    assert_eq!(shell(&h).window_apps(), ["com.android.settings", "com.android.chrome"]);
    shot(&mut h, "3-two-windows");

    // Typing goes to the focused (Chrome) window, not to egui.
    h.event(Event::Text("hi".into()));
    h.key_press(Key::Enter);
    settle(&mut h);
    {
        let sent = sent.lock().unwrap();
        let chrome: Vec<&Msg> = sent[start..].iter().filter(|(p, _)| p == "com.android.chrome").map(|(_, m)| m).collect();
        assert!(chrome.contains(&&Msg::Text("hi".into())), "{chrome:?}");
        assert!(chrome.iter().any(|m| matches!(m, Msg::Key { keycode: 66, action: control::ACTION_DOWN, .. })));
    }

    // A click in the content becomes a mouse down/up at the matching device pixel.
    let content_point = window_point(&h, 0.5, 0.5);
    click(&mut h, content_point);
    settle(&mut h);
    {
        let sent = sent.lock().unwrap();
        let down = sent.iter().rev().find_map(|(p, m)| match m {
            Msg::Touch { action: control::ACTION_DOWN, x, y, w, h, .. } if p == "com.android.chrome" => Some((*x, *y, *w, *h)),
            _ => None,
        });
        let (x, y, w, hh) = down.expect("touch down sent");
        assert!((x - i32::from(w) / 2).abs() <= 2 && (y - i32::from(hh) / 2).abs() <= 2, "{x},{y} of {w}x{hh}");
    }

    // Close Chrome with its own close button (the topmost one).
    let closes = h.get_all_by_label("Close").count();
    assert_eq!(closes, 2);
    h.get_all_by_label("Close").last().unwrap().click();
    settle(&mut h);
    assert_eq!(shell(&h).window_apps(), ["com.android.settings"]);
    shot(&mut h, "4-closed");
}

#[test]
fn resizing_a_window_resizes_the_phone_display() {
    let (mut h, sent) = harness();
    settle(&mut h);
    h.get_by_label("Maps").click();
    settle(&mut h);

    // Drag the bottom-right corner 200px left and 100px up.
    let before = shell(&h).window_rects()[0];
    let corner = before.max - vec2(1.0, 1.0);
    drag(&mut h, corner, corner - vec2(200.0, 100.0));
    let after = shell(&h).window_rects()[0];
    assert_eq!(after.size(), before.size() - vec2(200.0, 100.0), "window {before:?} -> {after:?}");
    std::thread::sleep(Duration::from_millis(200)); // past the settle delay
    settle(&mut h);

    let resizes: Vec<(u16, u16)> = sent.lock().unwrap().iter().filter_map(|(_, m)| match m {
        Msg::ResizeDisplay { w, h } => Some((*w, *h)),
        _ => None,
    }).collect();
    let &(w, hh) = resizes.last().expect("a resize was sent");
    // Window content started at 1000x624 (660 minus the title bar).
    assert_eq!((w, hh), (800, 524), "all resizes: {resizes:?}");
    shot(&mut h, "5-resized");
}

/// A point in the topmost window's content, as fractions of it.
fn window_point(h: &Harness<'static, State>, fx: f32, fy: f32) -> Pos2 {
    // The shell cascades windows from the desk center; find the window by its
    // close button instead of duplicating that math.
    let close = h.get_all_by_label("Close").last().unwrap().rect();
    let right = close.max.x + 6.0;
    let top = close.min.y - 4.0;
    let size = vec2(1000.0, 660.0);
    let left = right - size.x;
    let content_top = top + 36.0;
    pos2(left + fx * size.x, content_top + fy * (size.y - 36.0))
}

fn click(h: &mut Harness<'static, State>, p: Pos2) {
    h.event(Event::PointerMoved(p));
    h.run_steps(1);
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        h.run_steps(1);
    }
}

fn drag(h: &mut Harness<'static, State>, from: Pos2, to: Pos2) {
    h.event(Event::PointerMoved(from));
    h.run_steps(1);
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(1);
    for i in 1..=10 {
        h.event(Event::PointerMoved(from + (to - from) * (i as f32 / 10.0)));
        h.run_steps(1);
    }
    h.event(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(1);
}


#[test]
fn phone_clipboard_reaches_the_pc() {
    let (mut h, _) = harness();
    settle(&mut h);
    h.get_by_label("Keep Notes").click();
    settle(&mut h);

    let video = h.state().backend.videos.lock().unwrap()[0].clone();
    *video.clipboard.lock().unwrap() = Some("copied on the phone".into());
    h.step();
    let copied = h.output().platform_output.commands.iter().any(|c| {
        matches!(c, egui::OutputCommand::CopyText(t) if t == "copied on the phone")
    });
    assert!(copied, "{:?}", h.output().platform_output.commands);
}

#[test]
fn light_theme_follows_the_desktop() {
    let (mut h, _) = harness_with(egui::Theme::Light);
    settle(&mut h);
    shot(&mut h, "6-light-launcher");
    h.get_by_label("Maps").click();
    settle(&mut h);
    shot(&mut h, "7-light-window");
    // The shell paints with the light palette: sample the taskbar background.
    let img = h.render().unwrap();
    let px = img.get_pixel(700, 870);
    assert!(px[0] > 180 && px[1] > 180 && px[2] > 180, "taskbar should be light, got {px:?}");
}

#[test]
fn pop_out_turns_a_window_native() {
    let (mut h, sent) = harness();
    settle(&mut h);
    h.get_by_label("Spotify").click();
    settle(&mut h);
    assert_eq!(h.state().shell.as_ref().unwrap().window_native(), [false]);

    h.get_by_label("Pop out").click();
    settle(&mut h);
    assert_eq!(h.state().shell.as_ref().unwrap().window_native(), [true]);
    // Without multi-window support (as here) egui embeds the viewport; the
    // shell's own title bar is gone either way.
    assert!(h.query_by_label("Pop out").is_none());
    shot(&mut h, "8-popped-out");

    // The app keeps streaming and taking input in its new window.
    // egui places the embedded stand-in itself (top right here), so aim inside it.
    let before = sent.lock().unwrap().len();
    let p = pos2(900.0, 300.0);
    h.event(Event::PointerMoved(p));
    h.run_steps(1);
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        h.run_steps(1);
    }
    let touched = sent.lock().unwrap()[before..].iter().any(|(_, m)| matches!(m, Msg::Touch { action: control::ACTION_DOWN, .. }));
    assert!(touched, "no touch reached the app after popping out");

    // Right-click its taskbar button: "Pop back in" returns it to the shell.
    taskbar_button(&h, "Spotify").click_secondary();
    settle(&mut h);
    h.get_by_label("Pop back in").click();
    settle(&mut h);
    assert_eq!(h.state().shell.as_ref().unwrap().window_native(), [false]);
    assert!(h.query_by_label("Pop out").is_some(), "title bar is back");

    // And "Close" from the same menu ends it.
    taskbar_button(&h, "Spotify").click_secondary();
    settle(&mut h);
    h.get_by_label("Close window").click();
    settle(&mut h);
    assert_eq!(h.state().shell.as_ref().unwrap().window_count(), 0);
}

/// The taskbar button for an app (its label is also a window title).
fn taskbar_button<'a>(h: &'a Harness<'static, State>, label: &'a str) -> egui_kittest::Node<'a> {
    h.get_all_by_label(label).find(|n| n.rect().min.y > 830.0).expect("taskbar button")
}

#[test]
fn density_applies_to_new_and_reopened_windows() {
    let (mut h, _) = harness();
    settle(&mut h);
    h.state_mut().shell.as_mut().unwrap().density = 0.75;
    h.get_by_label("Maps").click();
    settle(&mut h);
    assert_eq!(*h.state().backend.opened.lock().unwrap(), [("com.google.android.apps.maps".to_owned(), 120)]);

    // Same density: nothing to reopen.
    taskbar_button(&h, "Maps").click_secondary();
    settle(&mut h);
    assert!(h.query_by_label("Reopen at new density").is_none());
    h.key_press(Key::Escape);
    settle(&mut h);

    // New density: the window offers to restart its display with it.
    h.state_mut().shell.as_mut().unwrap().density = 1.5;
    taskbar_button(&h, "Maps").click_secondary();
    settle(&mut h);
    h.get_by_label("Reopen at new density").click();
    settle(&mut h);
    let opened = h.state().backend.opened.lock().unwrap().clone();
    assert_eq!(opened.last(), Some(&("com.google.android.apps.maps".to_owned(), 240)));
    assert_eq!(h.state().shell.as_ref().unwrap().window_count(), 1);
}

#[test]
fn density_slider_lives_on_the_apps_button() {
    let (mut h, _) = harness();
    settle(&mut h);
    h.key_press(Key::Escape); // close the launcher
    settle(&mut h);
    h.get_by_label("Apps").click_secondary();
    settle(&mut h);
    assert!(h.query_by_label("Display density").is_some());
    shot(&mut h, "9-density-menu");
}
