//! End-to-end against a real phone: the shell UI, rendered offscreen, driving
//! real scrcpy-server sessions. Opt-in, since it opens apps on the phone:
//!
//!   DEX_DEVICE_TEST=1 [DEX_SHOTS=dir] cargo test --release --test device

use std::sync::Arc;
use std::time::{Duration, Instant};

use dex::backend::AdbBackend;
use dex::shell::Shell;
use egui::{Event, Modifiers, PointerButton, pos2, vec2};
use egui_kittest::{Harness, kittest::Queryable};

struct State {
    backend: Arc<AdbBackend>,
    shell: Option<Shell>,
}

fn shot(h: &mut Harness<'static, State>, name: &str) {
    let Some(dir) = std::env::var_os("DEX_SHOTS") else { return };
    h.render().unwrap().save(std::path::Path::new(&dir).join(format!("device-{name}.png"))).unwrap();
}

/// Steps the UI until `done` holds, or panics after `timeout`.
fn wait_for(h: &mut Harness<'static, State>, what: &str, timeout: Duration, done: impl Fn(&Harness<'static, State>) -> bool) {
    let start = Instant::now();
    while !done(h) {
        assert!(start.elapsed() < timeout, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
        h.run_steps(1);
    }
}

#[test]
fn real_phone() {
    if std::env::var_os("DEX_DEVICE_TEST").is_none() {
        eprintln!("skipped: set DEX_DEVICE_TEST=1 to run against the connected phone");
        return;
    }
    let devices = dex::adb::devices().unwrap();
    assert_eq!(devices.len(), 1, "need exactly one device, got {devices:?}");
    let backend = Arc::new(AdbBackend::new(devices[0].clone()).unwrap());

    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 900.0))
        .with_max_steps(5)
        .wgpu()
        .build_ui_state(
            |ui, s: &mut State| {
                let backend = s.backend.clone();
                s.shell.get_or_insert_with(|| Shell::new(backend, ui.ctx())).ui(ui);
            },
            State { backend, shell: None },
        );

    wait_for(&mut h, "app list", Duration::from_secs(20), |h| h.query_by_label("Ayarlar").is_some());
    // Real icons, if the companion app is installed (first fetch takes ~10 s).
    let has_companion = dex::adb::shell(&devices[0], "pm list packages dev.dex.companion")
        .unwrap()
        .contains("dev.dex.companion");
    if has_companion {
        wait_for(&mut h, "icons", Duration::from_secs(40), |h| h.state().shell.as_ref().unwrap().icon_count() > 50);
        h.run_steps(2);
    }
    shot(&mut h, "1-launcher");

    h.get_by_label("Ayarlar").click();
    wait_for(&mut h, "first frame", Duration::from_secs(15), |h| {
        h.state().shell.as_ref().unwrap().window_frames().first().is_some_and(|&n| n > 3)
    });
    std::thread::sleep(Duration::from_millis(800)); // let Settings finish drawing
    h.run_steps(2);
    shot(&mut h, "2-settings");

    // Click in the window and see the app react (frames keep coming).
    let r = h.state().shell.as_ref().unwrap().window_rects()[0];
    let p = pos2(r.center().x, r.min.y + 36.0 + 260.0);
    h.event(Event::PointerMoved(p));
    h.run_steps(1);
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        h.run_steps(1);
    }
    let before = h.state().shell.as_ref().unwrap().window_frames()[0];
    wait_for(&mut h, "a reaction to the click", Duration::from_secs(5), |h| {
        h.state().shell.as_ref().unwrap().window_frames()[0] > before + 2
    });
    std::thread::sleep(Duration::from_millis(1000));
    h.run_steps(2);
    shot(&mut h, "3-after-click");

    // Close: the virtual display must go away on the phone.
    h.get_by_label("Close").click();
    h.run_steps(2);
    assert_eq!(h.state().shell.as_ref().unwrap().window_count(), 0);
    std::thread::sleep(Duration::from_millis(1500));
    let displays = dex::adb::shell(&devices[0], "dumpsys SurfaceFlinger --display-id").unwrap();
    assert!(!displays.contains("Virtual display"), "virtual display left behind:\n{displays}");
}
