//! The desktop: wallpaper, taskbar, app launcher and app windows.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Event, FontId, Id, Key, LayerId, Mesh, Order, Pos2,
    Rect, Sense, Shape, Stroke, StrokeKind, TextureHandle, TextureOptions, Ui, Vec2, pos2, vec2,
};

use crate::backend::{AppInfo, Backend, Surface};
use crate::control::{self, Msg};

const TASKBAR_H: f32 = 52.0;
const TITLE_H: f32 = 36.0;
const RADIUS: u8 = 12;
const EDGE: f32 = 6.0;
const MIN_SIZE: Vec2 = vec2(320.0, 240.0);
/// How long a window size must hold still before the phone display follows it.
const RESIZE_SETTLE: Duration = Duration::from_millis(150);

/// Shell colors. Picked per frame from egui's theme, which follows the
/// desktop's light/dark preference unless the user overrides it.
pub struct Palette {
    pub panel: Color32,
    pub launcher: Color32,
    pub field: Color32,
    pub window: Color32,
    pub title: Color32,
    pub title_focused: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub hover: Color32,
    pub accent: Color32,
    pub close: Color32,
    pub shadow: Color32,
    /// Wallpaper gradient corners (top left, top right, bottom left, bottom right) and glows.
    pub wallpaper: [Color32; 4],
    pub glows: [Color32; 2],
}

// Translucent colors are premultiplied: white at alpha a is (a, a, a, a).
pub const DARK: Palette = Palette {
    panel: Color32::from_rgba_premultiplied(16, 18, 26, 225),
    launcher: Color32::from_rgb(20, 22, 31),
    field: Color32::from_rgb(34, 37, 50),
    window: Color32::from_rgb(24, 26, 34),
    title: Color32::from_rgb(30, 33, 43),
    title_focused: Color32::from_rgb(38, 42, 56),
    border: Color32::from_rgba_premultiplied(22, 22, 22, 22),
    text: Color32::from_rgb(232, 234, 240),
    text_dim: Color32::from_rgb(150, 156, 172),
    hover: Color32::from_rgba_premultiplied(26, 26, 26, 26),
    accent: Color32::from_rgb(122, 162, 255),
    close: Color32::from_rgb(226, 72, 82),
    shadow: Color32::from_rgba_premultiplied(0, 0, 0, 120),
    wallpaper: [
        Color32::from_rgb(20, 24, 52),
        Color32::from_rgb(52, 28, 78),
        Color32::from_rgb(10, 32, 44),
        Color32::from_rgb(38, 22, 48),
    ],
    glows: [Color32::from_rgb(120, 70, 200), Color32::from_rgb(30, 130, 170)],
};

pub const LIGHT: Palette = Palette {
    panel: Color32::from_rgba_premultiplied(230, 232, 238, 235),
    launcher: Color32::from_rgb(248, 249, 252),
    field: Color32::from_rgb(234, 236, 242),
    window: Color32::from_rgb(250, 250, 252),
    title: Color32::from_rgb(236, 238, 243),
    title_focused: Color32::from_rgb(222, 228, 242),
    border: Color32::from_rgba_premultiplied(0, 0, 0, 28),
    text: Color32::from_rgb(24, 26, 32),
    text_dim: Color32::from_rgb(92, 98, 112),
    hover: Color32::from_rgba_premultiplied(0, 0, 0, 18),
    accent: Color32::from_rgb(38, 98, 226),
    close: Color32::from_rgb(214, 56, 68),
    shadow: Color32::from_rgba_premultiplied(0, 0, 0, 50),
    wallpaper: [
        Color32::from_rgb(214, 224, 250),
        Color32::from_rgb(236, 220, 246),
        Color32::from_rgb(208, 236, 240),
        Color32::from_rgb(244, 228, 236),
    ],
    glows: [Color32::from_rgb(190, 160, 250), Color32::from_rgb(140, 210, 230)],
};

pub fn palette(ctx: &egui::Context) -> &'static Palette {
    match ctx.theme() {
        egui::Theme::Dark => &DARK,
        egui::Theme::Light => &LIGHT,
    }
}

#[derive(Clone, Copy)]
enum TaskAction {
    ToggleNative,
    Reopen,
    Close,
}

enum Apps {
    Loading,
    Ready(Vec<AppInfo>),
    Failed(String),
}

enum WinState {
    Starting(Receiver<Result<Box<dyn Surface>, String>>),
    Live(Box<dyn Surface>),
    Failed(String),
}

struct Win {
    id: u64,
    app: AppInfo,
    /// Outer rect, title bar included.
    rect: Rect,
    state: WinState,
    texture: Option<TextureHandle>,
    seen_frames: u64,
    minimized: bool,
    /// Rect to go back to when un-maximizing.
    restore: Option<Rect>,
    /// Display size we want (px), when it last changed, and what the phone has.
    want_px: [u32; 2],
    want_since: Instant,
    sent_px: [u32; 2],
    /// Android buttons held down inside this window, so drags keep going outside it.
    buttons: i32,
    /// Edges being dragged (left, top, right, bottom), while resizing.
    resizing: Option<[bool; 4]>,
    /// Shown as its own OS window instead of inside the shell.
    native: bool,
    /// Density its display was created with (fixed for the display's life).
    dpi: u32,
}

impl Win {
    fn content(&self) -> Rect {
        Rect::from_min_max(self.rect.min + vec2(0.0, TITLE_H), self.rect.max)
    }

    fn layer(&self) -> LayerId {
        LayerId::new(Order::Middle, Id::new(("dex-window", self.id)))
    }

    fn surface(&self) -> Option<&dyn Surface> {
        match &self.state {
            WinState::Live(s) => Some(s.as_ref()),
            _ => None,
        }
    }
}

#[derive(Default)]
struct Launcher {
    open: bool,
    query: String,
    focus_search: bool,
}

pub struct Shell {
    backend: Arc<dyn Backend>,
    apps: Arc<Mutex<Apps>>,
    windows: Vec<Win>,
    next_id: u64,
    focused: Option<u64>,
    launcher: Launcher,
    /// Real app icons, once the companion app has delivered them.
    icons: std::collections::HashMap<String, TextureHandle>,
    decoded_icons: Arc<Mutex<Vec<(String, egui::ColorImage)>>>,
    /// Android density for new windows, per screen point: 1.0 = 160 dpi
    /// (Android's baseline). Lower shows more, smaller UI.
    pub density: f32,
    /// Open new apps as OS windows rather than inside the shell.
    pub native_by_default: bool,
}

impl Shell {
    pub fn new(backend: Arc<dyn Backend>, ctx: &egui::Context) -> Self {
        let apps = Arc::new(Mutex::new(Apps::Loading));
        {
            let (backend, apps, ctx) = (backend.clone(), apps.clone(), ctx.clone());
            thread::spawn(move || {
                *apps.lock().unwrap() = match backend.apps() {
                    Ok(list) => Apps::Ready(list),
                    Err(e) => Apps::Failed(e),
                };
                ctx.request_repaint();
            });
        }
        let decoded_icons = Arc::new(Mutex::new(Vec::new()));
        {
            let (backend, out, ctx) = (backend.clone(), decoded_icons.clone(), ctx.clone());
            thread::spawn(move || {
                // Without the companion app there are no icons; letter tiles stay.
                let Ok(icons) = backend.icons() else { return };
                let decoded: Vec<_> = icons.into_iter().filter_map(|(pkg, png)| Some((pkg, decode_png(&png)?))).collect();
                *out.lock().unwrap() = decoded;
                ctx.request_repaint();
            });
        }
        Shell {
            icons: Default::default(),
            decoded_icons,
            backend,
            apps,
            windows: Vec::new(),
            next_id: 1,
            focused: None,
            launcher: Launcher { open: true, focus_search: true, ..Default::default() },
            density: 1.0,
            native_by_default: false,
        }
    }

    pub fn window_count(&self) -> usize {
        self.windows.len()
    }

    pub fn launcher_open(&self) -> bool {
        self.launcher.open
    }

    /// Which open windows are popped out as OS windows, in creation order.
    pub fn window_native(&self) -> Vec<bool> {
        self.windows.iter().map(|w| w.native).collect()
    }

    /// Number of real app icons loaded.
    pub fn icon_count(&self) -> usize {
        self.icons.len()
    }

    /// Frames decoded so far per open window, in creation order.
    pub fn window_frames(&self) -> Vec<u64> {
        self.windows.iter().map(|w| w.seen_frames).collect()
    }

    /// Outer rects of open windows, in creation order.
    pub fn window_rects(&self) -> Vec<Rect> {
        self.windows.iter().map(|w| w.rect).collect()
    }

    /// Package names of open windows, bottom to top.
    pub fn window_apps(&self) -> Vec<String> {
        self.windows.iter().map(|w| w.app.package.clone()).collect()
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let screen = ui.max_rect();
        let desk = Rect::from_min_max(screen.min, pos2(screen.max.x, screen.max.y - TASKBAR_H));

        for (pkg, image) in std::mem::take(&mut *self.decoded_icons.lock().unwrap()) {
            let tex = ctx.load_texture(format!("dex-icon-{pkg}"), image, TextureOptions::LINEAR);
            self.icons.insert(pkg, tex);
        }
        paint_wallpaper(ui.painter(), screen);
        if self.windows.iter().all(|w| w.minimized) && !self.launcher.open {
            ui.painter().text(
                desk.center(),
                Align2::CENTER_CENTER,
                "Click  ⊞  or press Ctrl+Space to open an app",
                FontId::proportional(16.0),
                palette(&ctx).text_dim,
            );
        }

        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, Key::Space)) {
            self.toggle_launcher();
        }

        self.poll_windows(&ctx);
        let mut closed = Vec::new();
        for i in 0..self.windows.len() {
            let closes = if self.windows[i].native {
                self.native_window_ui(&ctx, i)
            } else {
                !self.windows[i].minimized && self.window_ui(&ctx, i, desk)
            };
            if closes {
                closed.push(self.windows[i].id);
            }
        }
        self.windows.retain(|w| !closed.contains(&w.id));
        if self.focused.is_some_and(|f| closed.contains(&f)) {
            self.focused = self.topmost_visible(&ctx);
        }

        self.forward_keyboard(&ctx);
        self.taskbar_ui(&ctx, screen);
        if self.launcher.open {
            self.launcher_ui(&ctx, desk);
        }
    }

    fn toggle_launcher(&mut self) {
        self.launcher.open = !self.launcher.open;
        self.launcher.query.clear();
        self.launcher.focus_search = self.launcher.open;
    }

    pub fn open_app(&mut self, ctx: &egui::Context, app: &AppInfo, desk: Rect) {
        let id = self.next_id;
        self.next_id += 1;
        // Cascade new windows from the middle of the desk.
        let size = vec2(1000.0, 660.0).min(desk.size() - vec2(160.0, 120.0)).max(MIN_SIZE);
        let step = (self.windows.len() % 6) as f32;
        let min = (desk.min + vec2(40.0 + step * 64.0, 24.0 + step * 40.0)).min(desk.max - size);
        let rect = Rect::from_min_size(min, size);

        let px = content_px(rect, ctx.pixels_per_point());
        let dpi = self.dpi(ctx);
        let rx = self.start_session(ctx, &app.package, px, dpi);
        self.windows.push(Win {
            dpi,
            id,
            app: app.clone(),
            rect,
            state: WinState::Starting(rx),
            texture: None,
            seen_frames: 0,
            minimized: false,
            restore: None,
            want_px: px,
            want_since: Instant::now(),
            sent_px: px,
            buttons: 0,
            resizing: None,
            native: self.native_by_default,
        });
        self.focus(ctx, id);
    }

    /// Density for new displays, in dpi.
    fn dpi(&self, ctx: &egui::Context) -> u32 {
        (160.0 * ctx.pixels_per_point() * self.density).round().clamp(80.0, 640.0) as u32
    }

    /// Starts an app's display in the background; the result arrives on the receiver.
    fn start_session(&self, ctx: &egui::Context, package: &str, px: [u32; 2], dpi: u32)
        -> Receiver<Result<Box<dyn Surface>, String>> {
        let (tx, rx) = mpsc::channel();
        let (backend, pkg, ctx) = (self.backend.clone(), package.to_owned(), ctx.clone());
        thread::spawn(move || {
            let repaint_ctx = ctx.clone();
            let result = backend.open(&pkg, px, dpi, Box::new(move || repaint_ctx.request_repaint()));
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        rx
    }

    fn focus(&mut self, ctx: &egui::Context, id: u64) {
        self.focused = Some(id);
        if let Some(w) = self.windows.iter().find(|w| w.id == id) {
            ctx.move_to_top(w.layer());
        }
    }

    fn topmost_visible(&self, ctx: &egui::Context) -> Option<u64> {
        // egui keeps the real stacking order; walk it from the top.
        let order = ctx.memory(|m| m.layer_ids().collect::<Vec<_>>());
        order.iter().rev().find_map(|layer| {
            self.windows.iter().find(|w| !w.minimized && w.layer() == *layer).map(|w| w.id)
        })
    }

    fn poll_windows(&mut self, ctx: &egui::Context) {
        for w in &mut self.windows {
            if let WinState::Starting(rx) = &w.state {
                w.state = match rx.try_recv() {
                    Ok(Ok(surface)) => WinState::Live(surface),
                    Ok(Err(e)) => WinState::Failed(e),
                    Err(TryRecvError::Empty) => continue,
                    Err(TryRecvError::Disconnected) => WinState::Failed("could not start".into()),
                };
            }
            let WinState::Live(surface) = &w.state else { continue };
            if !surface.alive() {
                let why = surface.video().error.lock().unwrap().clone();
                w.state = WinState::Failed(why.unwrap_or_else(|| "Disconnected".into()));
                continue;
            }
            let video = surface.video();
            if let Some(text) = video.clipboard.lock().unwrap().take() {
                ctx.copy_text(text);
            }
            let n = video.frames.load(std::sync::atomic::Ordering::Relaxed);
            if n != w.seen_frames {
                w.seen_frames = n;
                if let Some(frame) = video.latest.lock().unwrap().take() {
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [frame.width as usize, frame.height as usize],
                        &frame.rgba,
                    );
                    match &mut w.texture {
                        Some(t) => t.set(image, TextureOptions::LINEAR),
                        None => {
                            w.texture = Some(ctx.load_texture(format!("dex-win-{}", w.id), image, TextureOptions::LINEAR));
                        }
                    }
                }
            }
            // Let the phone display follow the window once the size settles.
            if w.want_px != w.sent_px {
                if w.want_since.elapsed() >= RESIZE_SETTLE {
                    let [width, height] = w.want_px.map(|v| v.min(u16::MAX as u32) as u16);
                    surface.send(Msg::ResizeDisplay { w: width, h: height });
                    w.sent_px = w.want_px;
                } else {
                    ctx.request_repaint_after(RESIZE_SETTLE);
                }
            }
        }
    }

    /// Draws window `i`. Returns true if it was closed.
    fn window_ui(&mut self, ctx: &egui::Context, i: usize, desk: Rect) -> bool {
        let pal = palette(ctx);
        let focused = self.focused == Some(self.windows[i].id);
        let icons = &self.icons;
        let w = &mut self.windows[i];
        let id = w.layer().id;
        let mut close = false;
        let mut take_focus = false;

        egui::Area::new(id)
            .order(Order::Middle)
            .fixed_pos(w.rect.min)
            .constrain(false)
            .show(ctx, |ui| {
                ui.set_clip_rect(ctx.content_rect());
                let painter = ui.painter().clone();
                let rect = w.rect;
                // The area must cover the resize handles too, which reach past the edges.
                ui.allocate_rect(rect.expand(EDGE), Sense::hover());

                // Chrome
                let shadow = egui::epaint::Shadow { offset: [0, 10], blur: 32, spread: 0, color: pal.shadow };
                painter.add(shadow.as_shape(rect, CornerRadius::same(RADIUS)));
                painter.rect_filled(rect, CornerRadius::same(RADIUS), pal.window);
                let title = Rect::from_min_size(rect.min, vec2(rect.width(), TITLE_H));
                let top_only = CornerRadius { nw: RADIUS, ne: RADIUS, sw: 0, se: 0 };
                painter.rect_filled(title, top_only, if focused { pal.title_focused } else { pal.title });

                // Any press on the window brings it forward.
                if ui.rect_contains_pointer(rect) && ui.input(|i| i.pointer.any_pressed()) {
                    take_focus = true;
                }

                // Title bar: drag to move, double-click to maximize.
                let buttons_w = 5.0 * 34.0 + 6.0;
                let drag_zone = Rect::from_min_max(title.min + vec2(EDGE, 3.0), pos2(title.max.x - buttons_w, title.max.y));
                let drag = ui.interact(drag_zone, id.with("title"), Sense::click_and_drag());
                if drag.double_clicked() {
                    toggle_maximize(w, desk);
                } else if drag.dragged() {
                    if let Some(restore) = w.restore.take() {
                        // Un-maximize under the cursor, keeping its relative x position.
                        let p = drag.interact_pointer_pos().unwrap_or(title.center());
                        let fx = (p.x - rect.min.x) / rect.width();
                        w.rect = Rect::from_min_size(pos2(p.x - fx * restore.width(), rect.min.y), restore.size());
                    }
                    w.rect = w.rect.translate(drag.drag_delta());
                    // Keep the title bar reachable.
                    let y = w.rect.min.y.clamp(desk.min.y, desk.max.y - TITLE_H);
                    w.rect = w.rect.translate(vec2(0.0, y - w.rect.min.y));
                }
                if drag.dragged() {
                    ctx.set_cursor_icon(CursorIcon::Grabbing);
                }

                draw_app_icon(&painter, Rect::from_center_size(pos2(title.min.x + 22.0, title.center().y), vec2(20.0, 20.0)), &w.app, icons.get(&w.app.package));
                painter.text(
                    pos2(title.min.x + 40.0, title.center().y),
                    Align2::LEFT_CENTER,
                    &w.app.label,
                    FontId::proportional(14.0),
                    if focused { pal.text } else { pal.text_dim },
                );

                // Window controls, right to left.
                let mut x = title.max.x - 6.0;
                let mut button = |glyph: Glyph, label: &str| {
                    let r = Rect::from_min_max(pos2(x - 34.0, title.min.y + 4.0), pos2(x, title.max.y - 4.0));
                    x -= 34.0;
                    let resp = ui.interact(r, id.with(label), Sense::click());
                    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
                    if resp.hovered() {
                        let fill = if glyph == Glyph::Close { pal.close } else { pal.hover };
                        painter.rect_filled(r, CornerRadius::same(8), fill);
                    }
                    draw_glyph(&painter, r.center(), glyph, pal.text);
                    resp.clicked()
                };
                if button(Glyph::Close, "Close") {
                    close = true;
                }
                let maximize = button(if w.restore.is_some() { Glyph::Restore } else { Glyph::Maximize }, "Maximize");
                let minimize = button(Glyph::Minimize, "Minimize");
                let pop_out = button(Glyph::PopOut, "Pop out");
                let back = button(Glyph::Back, "Back");
                if pop_out {
                    w.native = true;
                }
                if maximize {
                    toggle_maximize(w, desk);
                }
                if minimize {
                    w.minimized = true;
                }
                if back && let Some(s) = w.surface() {
                    s.send(Msg::BackOrScreenOn { action: control::ACTION_DOWN });
                    s.send(Msg::BackOrScreenOn { action: control::ACTION_UP });
                }

                // Content
                let content = w.content();
                let bottom_only = CornerRadius { nw: 0, ne: 0, sw: RADIUS, se: RADIUS };
                painter.rect_filled(content, bottom_only, Color32::BLACK);
                let image_rect = match (&w.state, &w.texture) {
                    (WinState::Live(_), Some(tex)) => {
                        let r = fit(tex.size_vec2(), content);
                        let corners = if r == content { bottom_only } else { CornerRadius::ZERO };
                        painter.add(
                            egui::epaint::RectShape::filled(r, corners, Color32::WHITE)
                                .with_texture(tex.id(), Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0))),
                        );
                        Some((r, tex.size()))
                    }
                    (WinState::Failed(e), _) => {
                        painter.text(content.center() - vec2(0.0, 12.0), Align2::CENTER_CENTER,
                            format!("{} stopped", w.app.label), FontId::proportional(16.0), pal.text);
                        painter.text(content.center() + vec2(0.0, 14.0), Align2::CENTER_CENTER,
                            e, FontId::proportional(13.0), pal.text_dim);
                        None
                    }
                    _ => {
                        let spinner = Rect::from_center_size(content.center() - vec2(0.0, 14.0), vec2(28.0, 28.0));
                        ui.put(spinner, egui::Spinner::new().size(28.0).color(pal.accent));
                        painter.text(content.center() + vec2(0.0, 22.0), Align2::CENTER_CENTER,
                            format!("Starting {}…", w.app.label), FontId::proportional(14.0), pal.text_dim);
                        None
                    }
                };
                painter.rect_stroke(rect, CornerRadius::same(RADIUS), Stroke::new(1.0, pal.border), StrokeKind::Inside);

                if let (Some((img, size)), WinState::Live(surface)) = (image_rect, &w.state) {
                    // Leave the edge strips to the resize handles.
                    let input = Rect::from_min_max(
                        pos2(content.min.x + EDGE, content.min.y),
                        content.max - vec2(EDGE, EDGE),
                    );
                    let resp = ui.interact(input, id.with("content"), Sense::click_and_drag());
                    let hovered = ui.rect_contains_pointer(input);
                    forward_pointer(ui, surface.as_ref(), &mut w.buttons, hovered, img, size);
                    if hovered || resp.dragged() {
                        ctx.set_cursor_icon(CursorIcon::Default);
                    }
                }

                // Resize from edges and corners (not while maximized).
                if w.restore.is_none() {
                    resize_handles(ui, &mut w.rect, &mut w.resizing);
                }
            });

        // Ask the phone to follow the window size.
        let px = content_px(w.rect, ctx.pixels_per_point());
        if px != w.want_px {
            w.want_px = px;
            w.want_since = Instant::now();
            ctx.request_repaint_after(RESIZE_SETTLE);
        }

        if take_focus {
            let wid = w.id;
            self.focus(ctx, wid);
        }
        close
    }

    fn forward_keyboard(&mut self, ctx: &egui::Context) {
        if self.launcher.open || ctx.egui_wants_keyboard_input() {
            return;
        }
        // Native windows get their keys from their own viewport.
        let Some(target) = self.focused.and_then(|f| self.windows.iter().find(|w| w.id == f && !w.minimized && !w.native)) else {
            return;
        };
        let Some(surface) = target.surface() else { return };
        Self::forward_keys(ctx, surface);
    }

    /// Draws window `i` as its own OS window. Returns true if it was closed.
    fn native_window_ui(&mut self, ctx: &egui::Context, i: usize) -> bool {
        let pal = palette(ctx);
        let w = &mut self.windows[i];
        let viewport = egui::ViewportId::from_hash_of(("dex-native", w.id));
        // Only used when the OS window is created; after that the user sizes it.
        let builder = egui::ViewportBuilder::default()
            .with_title(&w.app.label)
            .with_app_id("dex")
            .with_inner_size(w.content().size())
            .with_min_inner_size(MIN_SIZE);
        let mut close = false;
        let mut focused = false;
        let mut area_px = None;

        ctx.show_viewport_immediate(viewport, builder, |ui, class| {
            let vctx = ui.ctx().clone();
            close = vctx.input(|i| i.viewport().close_requested());
            focused = match class {
                // Tests and backends without multi-window support embed it instead.
                egui::ViewportClass::EmbeddedWindow => self.focused == Some(w.id),
                _ => vctx.input(|i| i.viewport().focused.unwrap_or(false)),
            };
            let area = ui.max_rect();
            area_px = Some(px_size(area.size(), vctx.pixels_per_point()));
            ui.painter().rect_filled(area, 0, Color32::BLACK);
            match (&w.state, &w.texture) {
                (WinState::Live(surface), Some(tex)) => {
                    let img = fit(tex.size_vec2(), area);
                    ui.painter().image(tex.id(), img, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
                    ui.interact(area, Id::new(("dex-native-content", w.id)), Sense::click_and_drag());
                    // Nothing overlaps an OS window, so position alone decides. (egui's
                    // layer test also misreads the embedded fallback's window.)
                    let hovered = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| img.contains(p));
                    forward_pointer(ui, surface.as_ref(), &mut w.buttons, hovered, img, tex.size());
                    if focused && class != egui::ViewportClass::EmbeddedWindow {
                        Self::forward_keys(&vctx, surface.as_ref());
                    }
                }
                (WinState::Failed(e), _) => {
                    ui.painter().text(area.center(), Align2::CENTER_CENTER,
                        format!("{} stopped: {e}", w.app.label), FontId::proportional(14.0), pal.text_dim);
                }
                _ => {
                    ui.put(Rect::from_center_size(area.center(), vec2(28.0, 28.0)),
                        egui::Spinner::new().size(28.0).color(pal.accent));
                }
            }
        });

        if let Some(px) = area_px
            && px != w.want_px
        {
            w.want_px = px;
            w.want_since = Instant::now();
            ctx.request_repaint_after(RESIZE_SETTLE);
        }
        if focused {
            self.focused = Some(self.windows[i].id);
        }
        close
    }

    /// Sends this frame's keyboard input to a window's app.
    fn forward_keys(ctx: &egui::Context, surface: &dyn Surface) {
        for ev in ctx.input(|i| i.events.clone()) {
            match ev {
                Event::Text(text) => surface.send(Msg::Text(text)),
                Event::Paste(text) => surface.send(Msg::SetClipboard { text, paste: true }),
                Event::Copy => send_combo(surface, Key::C),
                Event::Cut => send_combo(surface, Key::X),
                Event::Key { key, pressed, repeat, modifiers, .. } => {
                    // Plain printable keys arrive as Event::Text; don't type them twice.
                    let printable = is_printable(key) && !modifiers.ctrl && !modifiers.alt;
                    if let (Some(keycode), false) = (android_keycode(key), printable) {
                        surface.send(Msg::Key {
                            action: if pressed { control::ACTION_DOWN } else { control::ACTION_UP },
                            keycode,
                            repeat: repeat as i32,
                            meta: android_meta(modifiers),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    fn taskbar_ui(&mut self, ctx: &egui::Context, screen: Rect) {
        let pal = palette(ctx);
        let bar = Rect::from_min_max(pos2(screen.min.x, screen.max.y - TASKBAR_H), screen.max);
        let mut clicked: Option<u64> = None;
        let mut menu: Option<(u64, TaskAction)> = None;
        let mut toggle_launcher = false;
        egui::Area::new(Id::new("dex-taskbar"))
            .order(Order::Foreground)
            .fixed_pos(bar.min)
            .constrain(false)
            .show(ctx, |ui| {
                let painter = ui.painter().clone();
                ui.allocate_rect(bar, Sense::hover());
                painter.rect_filled(bar, CornerRadius::ZERO, pal.panel);
                painter.hline(bar.x_range(), bar.min.y, Stroke::new(1.0, pal.border));

                let slot = |x: f32| Rect::from_center_size(pos2(x, bar.center().y), vec2(42.0, 42.0));
                let start = slot(bar.min.x + 30.0);
                let resp = ui.interact(start, Id::new("dex-start"), Sense::click());
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Apps"));
                if resp.hovered() || self.launcher.open {
                    painter.rect_filled(start, CornerRadius::same(10), pal.hover);
                }
                draw_glyph(&painter, start.center(), Glyph::Grid, if self.launcher.open { pal.accent } else { pal.text });
                toggle_launcher = resp.clicked();
                resp.context_menu(|ui| {
                    ui.set_min_width(240.0);
                    ui.label("Display density");
                    ui.add(egui::Slider::new(&mut self.density, 0.5..=2.0).step_by(0.05).suffix("×"));
                    ui.weak(format!("{} dpi · applies to apps you open next", (160.0 * ctx.pixels_per_point() * self.density).round()));
                });

                // Running apps
                let mut x = start.max.x + 34.0;
                for w in &self.windows {
                    let r = slot(x);
                    x += 48.0;
                    let resp = ui.interact(r, Id::new(("dex-task", w.id)), Sense::click());
                    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &w.app.label));
                    let focused = self.focused == Some(w.id) && !w.minimized;
                    if resp.hovered() || focused {
                        painter.rect_filled(r, CornerRadius::same(10), pal.hover);
                    }
                    draw_app_icon(&painter, Rect::from_center_size(r.center(), vec2(28.0, 28.0)), &w.app, self.icons.get(&w.app.package));
                    let dot_w = if focused { 16.0 } else { 6.0 };
                    painter.rect_filled(
                        Rect::from_center_size(pos2(r.center().x, r.max.y + 1.0), vec2(dot_w, 3.0)),
                        CornerRadius::same(2),
                        if focused { pal.accent } else { pal.text_dim },
                    );
                    let resp = resp.on_hover_text(&w.app.label);
                    if resp.clicked() {
                        clicked = Some(w.id);
                    }
                    let dpi_now = (160.0 * ctx.pixels_per_point() * self.density).round() as u32;
                    resp.context_menu(|ui| {
                        if w.dpi != dpi_now && ui.button("Reopen at new density").clicked() {
                            menu = Some((w.id, TaskAction::Reopen));
                            ui.close();
                        }
                        let toggle = if w.native { "Pop back in" } else { "Pop out" };
                        if ui.button(toggle).clicked() {
                            menu = Some((w.id, TaskAction::ToggleNative));
                            ui.close();
                        }
                        if ui.button("Close window").clicked() {
                            menu = Some((w.id, TaskAction::Close));
                            ui.close();
                        }
                    });
                }

                // Status: device, battery, clock
                let status = self.backend.status();
                let now = chrono::Local::now();
                let right = bar.max.x - 16.0;
                painter.text(pos2(right, bar.center().y - 8.0), Align2::RIGHT_CENTER,
                    now.format("%H:%M").to_string(), FontId::proportional(14.0), pal.text);
                painter.text(pos2(right, bar.center().y + 10.0), Align2::RIGHT_CENTER,
                    now.format("%a %-d %b").to_string(), FontId::proportional(11.0), pal.text_dim);
                let mut x = right - 86.0;
                if let Some(level) = status.battery {
                    let text = format!("{level}%{}", if status.charging { " ⚡" } else { "" });
                    let galley = painter.layout_no_wrap(text, FontId::proportional(13.0), pal.text);
                    let w = galley.size().x;
                    painter.galley(pos2(x - w, bar.center().y - galley.size().y / 2.0), galley, pal.text);
                    draw_battery(&painter, pos2(x - w - 30.0, bar.center().y), level);
                    x -= w + 48.0;
                }
                if !status.device_name.is_empty() {
                    painter.text(pos2(x, bar.center().y), Align2::RIGHT_CENTER,
                        &status.device_name, FontId::proportional(13.0), pal.text_dim);
                }
            });
        ctx.request_repaint_after(Duration::from_secs(10)); // keep the clock fresh

        if toggle_launcher {
            self.toggle_launcher();
        }
        match menu {
            Some((id, TaskAction::ToggleNative)) => {
                let w = self.windows.iter_mut().find(|w| w.id == id).unwrap();
                // Back in: the OS window closes once we stop showing its viewport.
                w.native = !w.native;
                w.minimized = false;
                self.focus(ctx, id);
            }
            Some((id, TaskAction::Reopen)) => {
                let dpi = self.dpi(ctx);
                let i = self.windows.iter().position(|w| w.id == id).unwrap();
                let (pkg, px) = (self.windows[i].app.package.clone(), self.windows[i].want_px);
                let rx = self.start_session(ctx, &pkg, px, dpi);
                let w = &mut self.windows[i];
                // Replacing the state drops the old session, which frees its display.
                w.state = WinState::Starting(rx);
                w.dpi = dpi;
                w.texture = None;
                w.seen_frames = 0;
                w.sent_px = px;
                w.buttons = 0;
            }
            Some((id, TaskAction::Close)) => {
                self.windows.retain(|w| w.id != id);
                if self.focused == Some(id) {
                    self.focused = self.topmost_visible(ctx);
                }
            }
            None => {}
        }
        if let Some(id) = clicked {
            let topmost = self.topmost_visible(ctx);
            let w = self.windows.iter_mut().find(|w| w.id == id).unwrap();
            if w.native {
                // The window manager owns it now: just bring it forward.
                let viewport = egui::ViewportId::from_hash_of(("dex-native", id));
                ctx.send_viewport_cmd_to(viewport, egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd_to(viewport, egui::ViewportCommand::Focus);
                self.focused = Some(id);
            } else if w.minimized {
                w.minimized = false;
                self.focus(ctx, id);
            } else if topmost == Some(id) {
                w.minimized = true;
                self.focused = self.topmost_visible(ctx).filter(|&f| f != id);
            } else {
                self.focus(ctx, id);
            }
        }
    }

    fn launcher_ui(&mut self, ctx: &egui::Context, desk: Rect) {
        let pal = palette(ctx);
        // Tall enough for the apps, up to most of the desk.
        let rows = match &*self.apps.lock().unwrap() {
            Apps::Ready(list) => list.len().div_ceil(6).max(1),
            _ => 1,
        };
        let height = 40.0 + 42.0 + 14.0 + rows as f32 * 100.0 + 8.0;
        let size = vec2(720.0, height.max(220.0)).min(desk.size() - vec2(32.0, 32.0));
        let rect = Rect::from_min_size(pos2(desk.min.x + 16.0, desk.max.y - size.y - 12.0), size);
        let mut launch: Option<AppInfo> = None;
        let mut close = false;
        let icons = &self.icons;

        egui::Area::new(Id::new("dex-launcher"))
            .order(Order::Foreground)
            .fixed_pos(rect.min)
            .constrain(false)
            .show(ctx, |ui| {
                let painter = ui.painter().clone();
                let shadow = egui::epaint::Shadow { offset: [0, 12], blur: 40, spread: 0, color: pal.shadow };
                painter.add(shadow.as_shape(rect, CornerRadius::same(18)));
                painter.rect(rect, CornerRadius::same(18), pal.launcher,
                    Stroke::new(1.0, pal.border), StrokeKind::Inside);
                ui.allocate_rect(rect, Sense::click()); // swallow clicks on the panel itself

                let inner = rect.shrink(20.0);
                let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(inner));
                ui.style_mut().visuals.extreme_bg_color = pal.field;
                ui.style_mut().visuals.selection.stroke = Stroke::new(1.5, pal.accent);

                let search = ui.add(
                    egui::TextEdit::singleline(&mut self.launcher.query)
                        .hint_text("Search apps")
                        .font(FontId::proportional(16.0))
                        .margin(vec2(12.0, 9.0))
                        .desired_width(f32::INFINITY),
                );
                if std::mem::take(&mut self.launcher.focus_search) {
                    search.request_focus();
                }
                ui.add_space(14.0);

                let apps = self.apps.lock().unwrap();
                match &*apps {
                    Apps::Loading => {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().color(pal.accent));
                            ui.colored_label(pal.text_dim, "Reading apps from your phone…");
                        });
                    }
                    Apps::Failed(e) => {
                        ui.colored_label(pal.close, format!("Could not list apps: {e}"));
                    }
                    Apps::Ready(list) => {
                        let q = self.launcher.query.trim().to_lowercase();
                        let matches: Vec<&AppInfo> = list
                            .iter()
                            .filter(|a| q.is_empty() || a.label.to_lowercase().contains(&q) || a.package.contains(&q))
                            .collect();
                        // A single-line edit drops focus on Enter, so check both.
                        let entered = (search.has_focus() || search.lost_focus())
                            && ui.input(|i| i.key_pressed(Key::Enter));
                        if entered {
                            launch = matches.first().map(|a| (*a).clone());
                        }
                        egui::ScrollArea::vertical().auto_shrink(false).show(&mut ui, |ui| {
                            let tile = vec2(112.0, 100.0);
                            let cols = ((ui.available_width() / tile.x).floor() as usize).max(1);
                            for row in matches.chunks(cols) {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 0.0;
                                    for app in row {
                                        if app_tile(ui, app, tile, icons.get(&app.package)).clicked() {
                                            launch = Some((*app).clone());
                                        }
                                    }
                                });
                            }
                            if matches.is_empty() {
                                ui.colored_label(pal.text_dim, "No apps match");
                            }
                        });
                    }
                }
            });

        // Esc or a click outside the launcher (but not on its taskbar button) closes it.
        let outside_click = ctx.input(|i| {
            i.pointer.any_pressed()
                && i.pointer.interact_pos().is_some_and(|p| !rect.contains(p) && p.y < desk.max.y)
        });
        if ctx.input(|i| i.key_pressed(Key::Escape)) || outside_click {
            close = true;
        }
        if let Some(app) = launch {
            self.open_app(ctx, &app, desk);
            close = true;
        }
        if close {
            self.launcher.open = false;
            self.launcher.query.clear();
        }
    }
}

fn decode_png(bytes: &[u8]) -> Option<egui::ColorImage> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None; // the companion always sends 8-bit RGBA
    }
    let size = [info.width as usize, info.height as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, &buf[..info.buffer_size()]))
}

fn content_px(rect: Rect, ppp: f32) -> [u32; 2] {
    px_size(rect.size() - vec2(0.0, TITLE_H), ppp)
}

/// A size in points as even device pixels, which encoders want.
fn px_size(size: Vec2, ppp: f32) -> [u32; 2] {
    let s = size * ppp;
    // Encoders want even sizes; the server aligns further if it needs to.
    [(s.x.round() as u32) & !1, (s.y.round() as u32) & !1].map(|v| v.max(64))
}

/// Largest rect with `size`'s aspect ratio centered in `within`.
fn fit(size: Vec2, within: Rect) -> Rect {
    let scale = (within.width() / size.x).min(within.height() / size.y);
    let fitted = Rect::from_center_size(within.center(), size * scale);
    // Snap to the full rect when within a pixel, so corners stay rounded.
    if (fitted.width() - within.width()).abs() < 1.5 && (fitted.height() - within.height()).abs() < 1.5 {
        within
    } else {
        fitted
    }
}

fn toggle_maximize(w: &mut Win, desk: Rect) {
    match w.restore.take() {
        Some(r) => w.rect = r,
        None => {
            w.restore = Some(w.rect);
            w.rect = desk;
        }
    }
}

/// Edge/corner resizing. Tracked by hand from pointer state: egui's drag
/// arbitration hands these thin strips to the overlapping area instead.
fn resize_handles(ui: &mut Ui, rect: &mut Rect, resizing: &mut Option<[bool; 4]>) {
    let r = *rect;
    let (l, t, rt, b) = (r.min.x, r.min.y, r.max.x, r.max.y);
    // (zone, which edges it moves: left, top, right, bottom); corners first.
    let zones = [
        (Rect::from_min_max(pos2(l - EDGE, t - EDGE), pos2(l + EDGE, t + EDGE)), [true, true, false, false], CursorIcon::ResizeNorthWest),
        (Rect::from_min_max(pos2(rt - EDGE, t - EDGE), pos2(rt + EDGE, t + EDGE)), [false, true, true, false], CursorIcon::ResizeNorthEast),
        (Rect::from_min_max(pos2(l - EDGE, b - EDGE), pos2(l + EDGE, b + EDGE)), [true, false, false, true], CursorIcon::ResizeSouthWest),
        (Rect::from_min_max(pos2(rt - EDGE, b - EDGE), pos2(rt + EDGE, b + EDGE)), [false, false, true, true], CursorIcon::ResizeSouthEast),
        (Rect::from_min_max(pos2(l - EDGE, t), pos2(l + EDGE, b)), [true, false, false, false], CursorIcon::ResizeWest),
        (Rect::from_min_max(pos2(rt - EDGE, t), pos2(rt + EDGE, b)), [false, false, true, false], CursorIcon::ResizeEast),
        (Rect::from_min_max(pos2(l, t - EDGE), pos2(rt, t + 3.0)), [false, true, false, false], CursorIcon::ResizeNorth),
        (Rect::from_min_max(pos2(l, b - EDGE), pos2(rt, b + EDGE)), [false, false, false, true], CursorIcon::ResizeSouth),
    ];
    let cursor_for = |edges: [bool; 4]| zones.iter().find(|z| z.1 == edges).map(|z| z.2);
    let (pressed, down, delta) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.delta()));

    if resizing.is_none() {
        let Some(&(_, edges, cursor)) = zones.iter().find(|z| ui.rect_contains_pointer(z.0)) else { return };
        ui.ctx().set_cursor_icon(cursor);
        if pressed {
            *resizing = Some(edges);
        }
        return;
    }
    let edges = resizing.unwrap();
    if !down {
        *resizing = None;
        return;
    }
    if let Some(cursor) = cursor_for(edges) {
        ui.ctx().set_cursor_icon(cursor);
    }
    let [ml, mt, mr, mb] = edges;
    let mut n = *rect;
    if ml { n.min.x = (n.min.x + delta.x).min(n.max.x - MIN_SIZE.x); }
    if mt { n.min.y = (n.min.y + delta.y).min(n.max.y - MIN_SIZE.y); }
    if mr { n.max.x = (n.max.x + delta.x).max(n.min.x + MIN_SIZE.x); }
    if mb { n.max.y = (n.max.y + delta.y).max(n.min.y + MIN_SIZE.y); }
    *rect = n;
}

/// Sends mouse input over the window content to the phone, as a mouse pointer.
fn forward_pointer(ui: &Ui, surface: &dyn Surface, held: &mut i32, hovered: bool, img: Rect, size: [usize; 2]) {
    let (fw, fh) = (size[0] as f32, size[1] as f32);
    let to_device = |p: Pos2| {
        let x = ((p.x - img.min.x) / img.width() * fw).clamp(0.0, fw - 1.0);
        let y = ((p.y - img.min.y) / img.height() * fh).clamp(0.0, fh - 1.0);
        (x as i32, y as i32)
    };
    let touch = |action: u8, p: Pos2, action_button: i32, buttons: i32| {
        let (x, y) = to_device(p);
        Msg::Touch {
            action, pointer: control::POINTER_MOUSE, x, y, w: size[0] as u16, h: size[1] as u16,
            pressure: if buttons != 0 { 1.0 } else { 0.0 }, action_button, buttons,
        }
    };
    for ev in ui.input(|i| i.events.clone()) {
        match ev {
            Event::PointerButton { pos, button, pressed, .. } => {
                let b = match button {
                    egui::PointerButton::Primary => control::BUTTON_PRIMARY,
                    egui::PointerButton::Secondary => control::BUTTON_SECONDARY,
                    egui::PointerButton::Middle => 4,
                    _ => continue,
                };
                if pressed && hovered && img.contains(pos) {
                    *held |= b;
                    surface.send(touch(control::ACTION_DOWN, pos, b, *held));
                } else if !pressed && *held & b != 0 {
                    *held &= !b;
                    surface.send(touch(control::ACTION_UP, pos, b, *held));
                }
            }
            Event::PointerMoved(pos) if *held != 0 => surface.send(touch(control::ACTION_MOVE, pos, 0, *held)),
            Event::PointerMoved(pos) if hovered && img.contains(pos) => {
                surface.send(touch(ACTION_HOVER_MOVE, pos, 0, 0));
            }
            Event::MouseWheel { unit, delta, .. } if hovered => {
                let Some(pos) = ui.input(|i| i.pointer.hover_pos()) else { continue };
                let lines = match unit {
                    egui::MouseWheelUnit::Line => delta,
                    egui::MouseWheelUnit::Page => delta * 10.0,
                    egui::MouseWheelUnit::Point => delta / 40.0,
                };
                let (x, y) = to_device(pos);
                surface.send(Msg::Scroll {
                    x, y, w: size[0] as u16, h: size[1] as u16,
                    hscroll: lines.x, vscroll: lines.y, buttons: *held,
                });
            }
            _ => {}
        }
    }
}

/// Android MotionEvent.ACTION_HOVER_MOVE
const ACTION_HOVER_MOVE: u8 = 7;

fn send_combo(surface: &dyn Surface, key: Key) {
    let Some(keycode) = android_keycode(key) else { return };
    let meta = android_meta(egui::Modifiers::CTRL);
    for action in [control::ACTION_DOWN, control::ACTION_UP] {
        surface.send(Msg::Key { action, keycode, repeat: 0, meta });
    }
}

fn is_printable(key: Key) -> bool {
    key.symbol_or_name().chars().count() == 1 || key == Key::Space
}

/// egui key -> Android KeyEvent keycode.
fn android_keycode(key: Key) -> Option<i32> {
    use Key::*;
    let letters = [A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z];
    if let Some(i) = letters.iter().position(|&k| k == key) {
        return Some(29 + i as i32);
    }
    let digits = [Num0, Num1, Num2, Num3, Num4, Num5, Num6, Num7, Num8, Num9];
    if let Some(i) = digits.iter().position(|&k| k == key) {
        return Some(7 + i as i32);
    }
    let fkeys = [F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12];
    if let Some(i) = fkeys.iter().position(|&k| k == key) {
        return Some(131 + i as i32);
    }
    Some(match key {
        Enter => 66,
        Backspace => 67,
        Delete => 112,
        Tab => 61,
        Escape => 111,
        Space => 62,
        ArrowUp => 19,
        ArrowDown => 20,
        ArrowLeft => 21,
        ArrowRight => 22,
        Home => 122,
        End => 123,
        PageUp => 92,
        PageDown => 93,
        _ => return None,
    })
}

fn android_meta(m: egui::Modifiers) -> i32 {
    let mut meta = 0;
    if m.shift { meta |= 0x1 | 0x40; } // META_SHIFT_ON | META_SHIFT_LEFT_ON
    if m.alt { meta |= 0x2 | 0x10; } // META_ALT_ON | META_ALT_LEFT_ON
    if m.ctrl { meta |= 0x1000 | 0x2000; } // META_CTRL_ON | META_CTRL_LEFT_ON
    meta
}

fn app_tile(ui: &mut Ui, app: &AppInfo, size: Vec2, icon_tex: Option<&TextureHandle>) -> egui::Response {
    let pal = palette(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &app.label));
    let painter = ui.painter();
    if resp.hovered() {
        painter.rect_filled(rect.shrink(4.0), CornerRadius::same(12), pal.hover);
    }
    let icon = Rect::from_center_size(pos2(rect.center().x, rect.min.y + 36.0), vec2(48.0, 48.0));
    draw_app_icon(painter, icon, app, icon_tex);
    let galley = painter.layout(
        app.label.clone(),
        FontId::proportional(12.5),
        pal.text,
        size.x - 12.0,
    );
    // Two lines at most; elide the rest.
    let text_pos = pos2(rect.center().x - galley.size().x / 2.0, icon.max.y + 8.0);
    let clip = Rect::from_min_max(pos2(rect.min.x, text_pos.y), pos2(rect.max.x, rect.max.y - 2.0));
    painter.with_clip_rect(clip).galley(text_pos, galley, pal.text);
    resp
}

/// The app's real icon if we have it; otherwise a rounded square in a color
/// derived from the package, with the label's first letter.
pub fn draw_app_icon(painter: &egui::Painter, rect: Rect, app: &AppInfo, texture: Option<&TextureHandle>) {
    if let Some(tex) = texture {
        painter.image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
        return;
    }
    let hash = app.package.bytes().fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619));
    let hue = (hash % 360) as f32 / 360.0;
    let top = egui::ecolor::Hsva::new(hue, 0.55, 0.92, 1.0);
    let bottom = egui::ecolor::Hsva::new((hue + 0.06) % 1.0, 0.7, 0.72, 1.0);
    let radius = (rect.width() * 0.28) as u8;
    painter.rect_filled(rect, CornerRadius::same(radius), Color32::from(bottom));
    let upper = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.center().y + rect.height() * 0.1));
    painter.rect_filled(upper, CornerRadius { nw: radius, ne: radius, sw: 0, se: 0 }, Color32::from(top).gamma_multiply(0.6));
    let letter = app.label.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().to_string()).unwrap_or_default();
    painter.text(rect.center(), Align2::CENTER_CENTER, letter, FontId::proportional(rect.height() * 0.5), Color32::WHITE);
}

#[derive(Clone, Copy, PartialEq)]
enum Glyph {
    Close,
    Maximize,
    Restore,
    Minimize,
    Back,
    PopOut,
    Grid,
}

fn draw_glyph(p: &egui::Painter, c: Pos2, glyph: Glyph, color: Color32) {
    let s = Stroke::new(1.4, color);
    match glyph {
        Glyph::Close => {
            p.line_segment([c + vec2(-5.0, -5.0), c + vec2(5.0, 5.0)], s);
            p.line_segment([c + vec2(5.0, -5.0), c + vec2(-5.0, 5.0)], s);
        }
        Glyph::Maximize => {
            p.rect_stroke(Rect::from_center_size(c, vec2(10.0, 10.0)), CornerRadius::same(2), s, StrokeKind::Middle);
        }
        Glyph::Restore => {
            p.rect_stroke(Rect::from_center_size(c + vec2(-1.5, 1.5), vec2(8.0, 8.0)), CornerRadius::same(2), s, StrokeKind::Middle);
            p.line_segment([c + vec2(-1.5, -4.5), c + vec2(4.5, -4.5)], s);
            p.line_segment([c + vec2(4.5, -4.5), c + vec2(4.5, 1.5)], s);
        }
        Glyph::Minimize => {
            p.line_segment([c + vec2(-5.0, 0.0), c + vec2(5.0, 0.0)], s);
        }
        Glyph::Back => {
            p.line_segment([c + vec2(-5.0, 0.0), c + vec2(5.0, 0.0)], s);
            p.line_segment([c + vec2(-5.0, 0.0), c + vec2(-1.0, -4.0)], s);
            p.line_segment([c + vec2(-5.0, 0.0), c + vec2(-1.0, 4.0)], s);
        }
        Glyph::PopOut => {
            // A box with an arrow leaving its top-right corner.
            p.line_segment([c + vec2(-1.0, -5.0), c + vec2(-5.0, -5.0)], s);
            p.line_segment([c + vec2(-5.0, -5.0), c + vec2(-5.0, 5.0)], s);
            p.line_segment([c + vec2(-5.0, 5.0), c + vec2(5.0, 5.0)], s);
            p.line_segment([c + vec2(5.0, 5.0), c + vec2(5.0, 1.0)], s);
            p.line_segment([c + vec2(-1.0, 1.0), c + vec2(5.5, -5.5)], s);
            p.line_segment([c + vec2(1.5, -5.5), c + vec2(5.5, -5.5)], s);
            p.line_segment([c + vec2(5.5, -5.5), c + vec2(5.5, -1.5)], s);
        }
        Glyph::Grid => {
            for i in 0..3 {
                for j in 0..3 {
                    let d = c + vec2((i as f32 - 1.0) * 6.5, (j as f32 - 1.0) * 6.5);
                    p.rect_filled(Rect::from_center_size(d, vec2(4.5, 4.5)), CornerRadius::same(1), color);
                }
            }
        }
    }
}

fn draw_battery(p: &egui::Painter, left_center: Pos2, level: u8) {
    let pal = palette(p.ctx());
    let body = Rect::from_min_size(left_center - vec2(0.0, 5.5), vec2(20.0, 11.0));
    p.rect_stroke(body, CornerRadius::same(3), Stroke::new(1.2, pal.text_dim), StrokeKind::Inside);
    p.rect_filled(Rect::from_min_size(pos2(body.max.x + 0.5, left_center.y - 2.5), vec2(2.0, 5.0)), CornerRadius::same(1), pal.text_dim);
    let fill_w = (body.width() - 4.0) * f32::from(level.min(100)) / 100.0;
    let color = if level <= 15 { pal.close } else { pal.text };
    p.rect_filled(Rect::from_min_size(body.min + vec2(2.0, 2.0), vec2(fill_w, body.height() - 4.0)), CornerRadius::same(1), color);
}

/// A smooth four-corner gradient with a couple of soft glows. No image assets.
pub fn paint_wallpaper(p: &egui::Painter, r: Rect) {
    let pal = palette(p.ctx());
    let corners = pal.wallpaper;
    let lerp = |a: Color32, b: Color32, t: f32| {
        Color32::from_rgb(
            (a.r() as f32 + (b.r() as f32 - a.r() as f32) * t) as u8,
            (a.g() as f32 + (b.g() as f32 - a.g() as f32) * t) as u8,
            (a.b() as f32 + (b.b() as f32 - a.b() as f32) * t) as u8,
        )
    };
    let n = 12;
    let mut mesh = Mesh::default();
    for j in 0..=n {
        for i in 0..=n {
            let (u, v) = (i as f32 / n as f32, j as f32 / n as f32);
            let color = lerp(lerp(corners[0], corners[1], u), lerp(corners[2], corners[3], u), v);
            mesh.colored_vertex(pos2(r.min.x + u * r.width(), r.min.y + v * r.height()), color);
        }
    }
    for j in 0..n {
        for i in 0..n {
            let a = (j * (n + 1) + i) as u32;
            let b = a + 1;
            let c = a + (n + 1) as u32;
            mesh.add_triangle(a, b, c);
            mesh.add_triangle(b, c + 1, c);
        }
    }
    p.add(Shape::mesh(mesh));
    glow(p, pos2(r.min.x + r.width() * 0.78, r.min.y + r.height() * 0.22), r.height() * 0.55, pal.glows[0]);
    glow(p, pos2(r.min.x + r.width() * 0.15, r.min.y + r.height() * 0.85), r.height() * 0.5, pal.glows[1]);
}

fn glow(p: &egui::Painter, c: Pos2, radius: f32, color: Color32) {
    let steps = 24;
    for i in 0..steps {
        let t = i as f32 / steps as f32;
        let alpha = (t * t * 10.0) as u8;
        p.circle_filled(c, radius * (1.0 - t), Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keycodes() {
        assert_eq!(android_keycode(Key::A), Some(29));
        assert_eq!(android_keycode(Key::Z), Some(54));
        assert_eq!(android_keycode(Key::Num0), Some(7));
        assert_eq!(android_keycode(Key::Enter), Some(66));
        assert!(is_printable(Key::A) && !is_printable(Key::Enter));
    }

    #[test]
    fn fit_keeps_aspect() {
        let r = fit(vec2(100.0, 50.0), Rect::from_min_size(Pos2::ZERO, vec2(200.0, 200.0)));
        assert_eq!(r.size(), vec2(200.0, 100.0));
        assert_eq!(r.center(), pos2(100.0, 100.0));
    }
}
