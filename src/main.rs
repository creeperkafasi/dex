//! dex: your Android phone as a desktop.
//!
//!   dex [-s SERIAL] [--windowed] [--density 0.5-2.0]

use std::sync::Arc;
use std::time::{Duration, Instant};

use dex::backend::AdbBackend;
use dex::shell::{self, Shell};
use egui::{Align2, FontId, vec2};

enum State {
    Waiting { checked: Instant, message: String },
    Running(Shell),
}

struct App {
    serial: Option<String>,
    density: f32,
    state: State,
}

impl App {
    fn try_connect(&mut self, ctx: &egui::Context) {
        let State::Waiting { checked, message } = &mut self.state else { return };
        if checked.elapsed() < Duration::from_secs(2) {
            ctx.request_repaint_after(Duration::from_millis(500));
            return;
        }
        *checked = Instant::now();
        let serial = match (&self.serial, dex::adb::devices()) {
            (Some(s), _) => s.clone(),
            (None, Ok(d)) if d.len() == 1 => d[0].clone(),
            (None, Ok(d)) if d.len() > 1 => {
                *message = format!("Several devices connected; start with -s SERIAL ({})", d.join(", "));
                return;
            }
            (None, Ok(_)) => {
                *message = "Connect your phone over USB and allow USB debugging".into();
                return;
            }
            (None, Err(e)) => {
                *message = format!("adb not available: {e}");
                return;
            }
        };
        match AdbBackend::new(serial) {
            Ok(backend) => {
                let mut shell = Shell::new(Arc::new(backend), ctx);
                shell.density = self.density;
                self.state = State::Running(shell);
            }
            Err(e) => *message = e,
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.key_pressed(egui::Key::F11)) {
            let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
        }
        self.try_connect(&ctx);
        match &mut self.state {
            State::Running(shell) => shell.ui(ui),
            State::Waiting { message, .. } => {
                let screen = ui.max_rect();
                shell::paint_wallpaper(ui.painter(), screen);
                let card = egui::Rect::from_center_size(screen.center(), vec2(460.0, 150.0));
                let pal = shell::palette(&ctx);
                ui.painter().rect_filled(card, 18, pal.panel);
                ui.painter().text(card.center() - vec2(0.0, 22.0), Align2::CENTER_CENTER,
                    "Waiting for your phone", FontId::proportional(20.0), pal.text);
                ui.painter().text(card.center() + vec2(0.0, 16.0), Align2::CENTER_CENTER,
                    message.as_str(), FontId::proportional(13.0), pal.text_dim);
            }
        }
    }
}

fn main() -> eframe::Result {
    let mut serial = None;
    let mut windowed = false;
    let mut density = std::env::var("DEX_DENSITY").ok().and_then(|d| d.parse().ok()).unwrap_or(1.0f32);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-s" => serial = args.next(),
            "--windowed" => windowed = true,
            "--density" => match args.next().and_then(|d| d.parse::<f32>().ok()) {
                Some(d) if (0.5..=2.0).contains(&d) => density = d,
                _ => {
                    eprintln!("dex: --density takes a number from 0.5 to 2.0");
                    std::process::exit(2);
                }
            },
            "-h" | "--help" => {
                println!("usage: dex [-s SERIAL] [--windowed] [--density 0.5-2.0]\n\n\
                    F11 toggles fullscreen, Ctrl+Space opens the app launcher, right-click ⊞ for settings.\n\
                    --density: Android UI scale for app windows (1.0 = 160 dpi; lower = smaller, more content).");
                return Ok(());
            }
            other => {
                eprintln!("dex: unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    let viewport = egui::ViewportBuilder::default()
        .with_title("dex")
        .with_app_id("dex")
        .with_inner_size([1440.0, 900.0])
        .with_fullscreen(!windowed);
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("dex", options, Box::new(move |cc| {
        // Follow the desktop's light/dark preference, live.
        let ctx = cc.egui_ctx.clone();
        if let Some(theme) = dex::desktop_theme::current() {
            ctx.set_theme(theme);
        }
        dex::desktop_theme::watch(move |theme| {
            ctx.set_theme(theme);
            ctx.request_repaint();
        });
        Ok(Box::new(App {
            serial,
            density,
            state: State::Waiting { checked: Instant::now() - Duration::from_secs(10), message: String::new() },
        }))
    }))
}
