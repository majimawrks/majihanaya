#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
#[cfg(debug_assertions)]
mod demo;
mod logfile;
mod model;
mod platform;
mod proc;
mod providers;
mod tray;
mod trayview;
mod ui;

use config::Config;
use eframe::egui;
use platform::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::sync::{Arc, mpsc};
use std::sync::atomic::{AtomicBool, Ordering};
use tray_icon::TrayIconEvent;
use tray_icon::menu::MenuEvent;

fn main() -> eframe::Result {
    if std::env::args().any(|a| a == "--dump") {
        dump(&Config::load());
        return Ok(());
    }

    logfile::install_panic_hook();
    // Started by Restart: let the old instance finish exiting first.
    platform::wait_for_predecessor();
    // Debug screenshot runs (one frame, then exit) may run beside the
    // user's own copy.
    let shot = cfg!(debug_assertions) && std::env::var_os("MAJIHANAYA_SHOT").is_some();
    let _instance = platform::single_instance();
    if _instance.is_none() && !shot {
        platform::activate_existing();
        return Ok(());
    }

    #[cfg(debug_assertions)]
    let demo = demo::enabled();
    #[cfg(not(debug_assertions))]
    let demo = false;
    // The demo never reads or writes the user's config.
    let config = if demo { Config::default() } else { Config::load() };
    logfile::ERRORS.line(concat!("majihanaya ", env!("CARGO_PKG_VERSION"), " started"));
    #[cfg(debug_assertions)]
    let state: model::Shared = if demo { std::sync::Arc::new(std::sync::Mutex::new(demo::state())) } else { Default::default() };
    #[cfg(not(debug_assertions))]
    let state: model::Shared = Default::default();
    // The compact window at the app's zoom; resized to its content after the
    // first frame.
    let z = ui::zoom(config.scale);
    let size = [ui::COMPACT_W * z, 440.0 * z];
    // Borderless + transparent: the app draws the design's rounded rack and
    // title bar itself, and opacity is a paint multiplier.
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(platform::WINDOW_TITLE)
        .with_inner_size(size)
        .with_resizable(false)
        .with_decorations(false)
        .with_transparent(true)
        // Appear without taking focus: never steal the keyboard from
        // whatever the user is typing in (a tray click still focuses it).
        .with_active(false)
        .with_taskbar(false);
    if config.topmost {
        viewport = viewport.with_always_on_top();
    }
    // Saved drag position wins while it's still on a monitor; otherwise
    // (or after a monitor is gone) sit beside the tray.
    let saved = config.left.zip(config.top).map(|(x, y)| [x, y]).filter(|&[x, y]| platform::logical_on_screen(x, y));
    if let Some(pos) = saved.or_else(|| platform::default_position(size)) {
        viewport = viewport.with_position(pos);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };

    eframe::run_native(
        platform::WINDOW_TITLE,
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            if demo {
                config::publish(&config);
            } else {
                spawn_workers(&state, &config, Arc::new(move || ctx.request_repaint()));
            }
            Ok(Box::new(App::new(cc, config, state)?))
        }),
    )
}

fn spawn_workers(state: &model::Shared, config: &Config, notify: model::Notify) {
    use providers::*;
    config::publish(config);
    claude::spawn(state.clone(), notify.clone());
    codex::spawn(state.clone(), notify.clone());
    sessions::spawn(state.clone(), notify.clone());
    status::spawn(state.clone(), notify.clone());
    version::spawn(state.clone(), notify);
}

/// `--dump`: one refresh of every source, the state as JSON on stdout, exit.
/// Works alongside a running instance (no single-instance check).
fn dump(config: &Config) {
    use providers::*;
    platform::attach_parent_console();
    let state: model::Shared = Default::default();
    let refreshers: [fn(&model::Shared, &Config); 5] =
        [claude::refresh, codex::refresh, sessions::refresh, status::refresh, version::refresh];
    let state = &state;
    std::thread::scope(|s| {
        for refresh in refreshers {
            s.spawn(move || refresh(&state, config));
        }
    });
    let json = serde_json::to_string_pretty(&*model::lock(state)).unwrap_or_default();
    println!("{json}");
}

struct App {
    config: Config,
    state: model::Shared,
    window: Window,
    /// Set by the tray's Exit; any other close request only hides the window.
    quitting: Arc<AtomicBool>,
    tray: tray::Tray,
    /// Menu actions that need `App` (config, tray); drained in `logic`.
    actions: mpsc::Receiver<tray::Action>,
    ui: ui::WindowUi,
    errors: logfile::ErrorWatch,
    config_mtime: Option<std::time::SystemTime>,
    /// The text of our last save, to tell our writes from outside edits.
    last_written: Option<String>,
    config_checked: std::time::Instant,
    /// Content size last sent to the viewport.
    size: egui::Vec2,
    /// Outer position we last placed or saw, and a drag-move awaiting save.
    pos: Option<egui::Pos2>,
    /// Set when the user starts a title-bar drag; cleared once it is saved.
    dragging: Option<std::time::Instant>,
    /// Debug builds: when to take the `MAJIHANAYA_SHOT` frame.
    #[cfg(debug_assertions)]
    shot_at: Option<std::time::Instant>,
}

impl App {
    fn new(
        cc: &eframe::CreationContext<'_>,
        config: Config,
        state: model::Shared,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let window = match cc.window_handle()?.as_raw() {
            RawWindowHandle::Win32(h) => Window::from_raw(h.hwnd.get()),
            _ => return Err("expected a Win32 window".into()),
        };
        cc.egui_ctx.set_zoom_factor(ui::zoom(config.scale));
        let saved_ok = config.left.zip(config.top).is_some_and(|(x, y)| platform::logical_on_screen(x, y));
        if !saved_ok {
            window.snap_to_tray_corner();
        }

        // Tray must be created on this (the event loop's) thread.
        let tray = tray::Tray::new(platform::autostart_enabled()).map_err(|e| e.to_string())?;
        let quitting = Arc::new(AtomicBool::new(false));

        // Handlers act on the window directly where they can: eframe runs no
        // ui pass while the window is hidden, and these must work then too.
        let ctx = cc.egui_ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let Some(tray::Action::Toggle) = tray::tray_action(&event) {
                window.toggle();
                ctx.request_repaint();
            }
        }));

        let (tx, actions) = mpsc::channel();
        let (ids, quit, ctx) = (tray.ids.clone(), quitting.clone(), cc.egui_ctx.clone());
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| match tray::menu_action(&event, &ids) {
            Some(tray::Action::Refresh) => model::REFRESH.trigger(),
            Some(tray::Action::OpenConfig) => open_config_file(),
            Some(tray::Action::Exit) => {
                quit.store(true, Ordering::SeqCst);
                window.post_close();
            }
            Some(tray::Action::Restart) => match platform::spawn_successor() {
                Ok(()) => {
                    quit.store(true, Ordering::SeqCst);
                    window.post_close();
                }
                Err(e) => logfile::ERRORS.line(&format!("restart failed: {e}")),
            },
            Some(other) => {
                // `logic` runs on request_repaint even while the window is hidden.
                let _ = tx.send(other);
                ctx.request_repaint();
            }
            None => {}
        }));

        let ui = ui::WindowUi::new(&cc.egui_ctx, &config);
        Ok(Self {
            config,
            state,
            window,
            quitting,
            tray,
            actions,
            ui,
            errors: Default::default(),
            config_mtime: config_mtime(),
            last_written: None,
            config_checked: std::time::Instant::now(),
            size: egui::vec2(ui::COMPACT_W, 440.0),
            pos: None,
            dragging: None,
            #[cfg(debug_assertions)]
            shot_at: demo::shot_path().map(|_| std::time::Instant::now() + std::time::Duration::from_secs(2)),
        })
    }

    /// Forgets the dragged position and puts the window back beside the tray
    /// (title-bar triple-click, or the tray menu when it's off-screen).
    fn reset_position(&mut self) {
        self.dragging = None;
        self.config.left = None;
        self.config.top = None;
        self.save_config();
        self.window.snap_to_tray_corner();
    }

    /// Resizes the window to the content in one move, keeping the corner
    /// nearest the tray where it is (the window grows away from the tray).
    fn fit(&mut self, ctx: &egui::Context, size: egui::Vec2) {
        if (size - self.size).length() < 0.5 {
            return;
        }
        self.size = size;
        let ppp = ctx.pixels_per_point();
        self.window.resize_anchored((size.x * ppp).round() as i32, (size.y * ppp).round() as i32);
    }

    /// Saves the position after a title-bar drag, once the window stops
    /// moving. Only drags count: our own fit/snap moves are never saved.
    fn track_position(&mut self, ctx: &egui::Context) {
        const SETTLE: std::time::Duration = std::time::Duration::from_millis(600);
        let Some(outer) = ctx.input(|i| i.viewport().outer_rect) else { return };
        let Some(since) = self.dragging else { return };
        if self.pos.is_some_and(|p| (p - outer.min).length() > 0.5) {
            // Still moving: wait for it to settle.
            self.pos = Some(outer.min);
            self.dragging = Some(std::time::Instant::now());
            ctx.request_repaint_after(SETTLE);
        } else if since.elapsed() >= SETTLE {
            self.dragging = None;
            self.pos = Some(outer.min);
            self.config.left = Some(outer.min.x);
            self.config.top = Some(outer.min.y);
            self.save_config();
        } else {
            ctx.request_repaint_after(SETTLE);
        }
    }

    /// Saves and publishes to the workers. Remembers the exact text written,
    /// so our own write is never mistaken for an outside edit (and an outside
    /// edit landing right after ours is never mistaken for ours).
    fn save_config(&mut self) {
        #[cfg(debug_assertions)]
        if demo::enabled() {
            config::publish(&self.config);
            return;
        }
        if let Ok(text) = self.config.save_text() {
            self.last_written = Some(text);
        }
        config::publish(&self.config);
    }

    /// Picks up edits made to config.json while running (checked at most
    /// once a second, on the frames the app runs anyway).
    fn reload_if_edited(&mut self, ctx: &egui::Context) {
        if self.config_checked.elapsed() < std::time::Duration::from_secs(1) {
            return;
        }
        self.config_checked = std::time::Instant::now();
        let mtime = config_mtime();
        if mtime.is_none() || mtime == self.config_mtime {
            return;
        }
        let text = match config::path().map(|p| proc::read_text_capped(&p, config::MAX_BYTES)) {
            Some(Ok(text)) => text,
            // Too large or not UTF-8: invalid, like a typo (below).
            Some(Err(e)) if e.kind() == std::io::ErrorKind::InvalidData => {
                self.config_mtime = mtime;
                logfile::ERRORS.line("config.json changed on disk but is not valid: kept current settings");
                if let Some(p) = config::path() {
                    config::back_up_bad(&p);
                }
                return;
            }
            // Unreadable right now (an editor holding it): try again next time.
            _ => return,
        };
        self.config_mtime = mtime;
        if self.last_written.as_deref() == Some(text.as_str()) {
            return;
        }
        match Config::parse(&text) {
            Some(loaded) => {
                config::mark_valid();
                logfile::ERRORS.line("config.json changed on disk: reloaded");
                let autostart = platform::autostart_enabled();
                self.apply_settings(ctx, loaded, autostart, false);
            }
            // Invalid (mid-save, a typo): keep the current settings, and
            // back the file up so a later save (a drag, Settings) can't
            // destroy the edit. The next write is looked at again.
            None => {
                logfile::ERRORS.line("config.json changed on disk but is not valid: kept current settings");
                if let Some(p) = config::path() {
                    config::back_up_bad(&p);
                }
            }
        }
    }

    /// `save`: false when the config came from the file itself (an outside
    /// edit), so it isn't rewritten under the editor that made it.
    fn apply_settings(&mut self, ctx: &egui::Context, cfg: Config, autostart: bool, save: bool) {
        if cfg.topmost != self.config.topmost {
            let level = if cfg.topmost { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal };
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
        }
        if cfg.scale != self.config.scale {
            ctx.set_zoom_factor(ui::zoom(cfg.scale));
        }
        // Keep the saved position: the panel doesn't edit it.
        let (left, top) = (self.config.left, self.config.top);
        self.config = Config { left, top, ..cfg.sanitized() };
        if save {
            self.save_config();
        } else {
            config::publish(&self.config);
        }
        if autostart != platform::autostart_enabled() && platform::set_autostart(autostart).is_ok() {
            self.tray.set_autostart_checked(autostart);
        }
    }

    fn handle(&mut self, ctx: &egui::Context, action: tray::Action) {
        match action {
            tray::Action::Larger | tray::Action::Smaller => {
                let step = if matches!(action, tray::Action::Larger) { 0.1 } else { -0.1 };
                // Rounded so repeated steps don't drift (1.0999…).
                self.config.scale = ((self.config.scale + step) * 10.0).round().clamp(7.5, 20.0) / 10.0;
                ctx.set_zoom_factor(ui::zoom(self.config.scale));
                self.save_config();
            }
            tray::Action::Settings => {
                self.ui.open_settings(&self.config);
                self.window.show();
            }
            tray::Action::ResetPosition => {
                self.reset_position();
                self.window.show();
            }
            tray::Action::Autostart => {
                let want = self.tray.autostart_checked();
                if platform::set_autostart(want).is_err() {
                    self.tray.set_autostart_checked(!want);
                }
            }
            _ => {}
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting.load(Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.window.hide();
        }
        while let Ok(action) = self.actions.try_recv() {
            self.handle(ctx, action);
        }
        self.reload_if_edited(ctx);

        let now = chrono::Utc::now();
        let (view, tip, errors) = {
            let s = model::lock(&self.state);
            let errors = [
                ("claude", s.claude.error.clone()),
                ("codex", s.codex.error.clone()),
                ("claude status", s.claude_status.error.clone()),
                ("codex status", s.codex_status.error.clone()),
            ];
            let view = trayview::view(&s, &self.config, now);
            let tip = trayview::tooltip(&view, self.config.tray_reset_clock, now);
            (view, tip, errors)
        };
        // Log file I/O happens after the state lock is released.
        for (source, e) in &errors {
            self.errors.check(source, e.as_deref());
        }
        self.tray.update(&view, tip);
        // Countdowns and staleness move with the clock: look again at the next
        // minute boundary (one wakeup a minute, hidden or not).
        let secs = 60 - (now.timestamp().rem_euclid(60)) as u64;
        ctx.request_repaint_after(std::time::Duration::from_secs(secs));
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Transparent outside the rounded rack.
        [0.0; 4]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ui.set_opacity(self.config.opacity.clamp(0.5, 1.0));
        let view_before = self.ui.view;
        // Draw from a copy: layout must never hold up the workers' writes.
        let snapshot = model::lock(&self.state).clone();
        let (size, actions) = self.ui.draw(ui, &snapshot, &self.config);
        for a in actions {
            match a {
                ui::Action::Hide => self.window.hide(),
                ui::Action::StartDrag => {
                    self.pos = ctx.input(|i| i.viewport().outer_rect).map(|r| r.min);
                    self.dragging = Some(std::time::Instant::now());
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                ui::Action::ResetPosition => self.reset_position(),
                ui::Action::OpenConfig => open_config_file(),
                ui::Action::Apply(cfg, autostart) => self.apply_settings(&ctx, *cfg, autostart, true),
            }
        }
        // A click switched views during this pass, so its size is the old
        // view's: skip it (no in-between resize); the next frame fits.
        if self.ui.view == view_before {
            self.fit(&ctx, size.ceil());
        }
        self.track_position(&ctx);
        #[cfg(debug_assertions)]
        self.take_shot(&ctx);
    }

}

#[cfg(debug_assertions)]
impl App {
    /// `MAJIHANAYA_SHOT`: once the window has settled, save a frame and exit.
    fn take_shot(&mut self, ctx: &egui::Context) {
        let Some(at) = self.shot_at else { return };
        if std::time::Instant::now() < at {
            ctx.request_repaint_after(at - std::time::Instant::now());
            return;
        }
        self.shot_at = None;
        let (Some(path), window, quit) = (demo::shot_path(), self.window, self.quitting.clone()) else { return };
        ctx.request_screenshot(move |image| {
            demo::save(&path, &image);
            quit.store(true, Ordering::SeqCst);
            window.post_close();
        });
        ctx.request_repaint();
    }
}

fn config_mtime() -> Option<std::time::SystemTime> {
    config::path().and_then(|p| std::fs::metadata(p).ok()).and_then(|m| m.modified().ok())
}

fn open_config_file() {
    let Some(path) = config::path() else { return };
    if !path.exists() {
        let _ = Config::default().save();
    }
    platform::open_file(&path);
}
