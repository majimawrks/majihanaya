#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod logfile;
mod model;
mod platform;
mod proc;
mod present;
mod render;
mod providers;
mod tray;
mod trayview;
mod ui;

use config::Config;
use platform::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::platform::windows::WindowAttributesExtWindows;
use winit::window::{WindowAttributes, WindowId, WindowLevel};
use std::sync::{Arc, mpsc};
use std::sync::atomic::{AtomicBool, Ordering};
use tray_icon::TrayIconEvent;
use tray_icon::menu::MenuEvent;

fn main() {
    if std::env::args().any(|a| a == "--dump") {
        dump(&Config::load());
        return;
    }

    logfile::install_panic_hook();
    let Some(_instance) = platform::single_instance() else {
        platform::activate_existing();
        return;
    };

    let config = Config::load();
    logfile::ERRORS.line(concat!("majihanaya ", env!("CARGO_PKG_VERSION"), " started"));
    let Ok(event_loop) = EventLoop::<Wake>::with_user_event().build() else {
        logfile::ERRORS.line("cannot create the event loop");
        return;
    };
    let ctx = egui::Context::default();
    // Every repaint request (ours, egui's, a worker's) becomes a wake-up at
    // the time asked for; nothing polls.
    let proxy = event_loop.create_proxy();
    let p = proxy.clone();
    ctx.set_request_repaint_callback(move |info| {
        let _ = p.send_event(Wake::At(Instant::now() + info.delay.min(Duration::from_secs(3600))));
    });
    let state: model::Shared = Default::default();
    // Workers wake the shell directly, not through egui: while hidden no
    // egui pass runs, and egui then stops forwarding repaint requests.
    let p = proxy.clone();
    spawn_workers(&state, &config, Arc::new(move || {
        let _ = p.send_event(Wake::At(Instant::now()));
    }));
    let mut shell = Shell { ctx, proxy, config: Some(config), state, app: None, next: None };
    let _ = event_loop.run_app(&mut shell);
}

enum Wake {
    /// Run a frame at (or after) this instant.
    At(Instant),
    /// The tray's Exit.
    Quit,
}

/// The winit side: owns the event loop plumbing and schedules frames.
struct Shell {
    ctx: egui::Context,
    proxy: EventLoopProxy<Wake>,
    /// Handed to `App` once the window exists.
    config: Option<Config>,
    state: model::Shared,
    app: Option<App>,
    /// The earliest pending frame.
    next: Option<Instant>,
}

impl Shell {
    fn wake_at(&mut self, at: Instant) {
        self.next = Some(self.next.map_or(at, |n| n.min(at)));
    }

    fn schedule(&self, el: &ActiveEventLoop) {
        el.set_control_flow(match self.next {
            Some(t) => ControlFlow::WaitUntil(t),
            None => ControlFlow::Wait,
        });
    }
}

impl ApplicationHandler<Wake> for Shell {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.app.is_some() {
            return;
        }
        let Some(config) = self.config.take() else { return };
        match App::new(el, &self.ctx, self.proxy.clone(), config, self.state.clone()) {
            Ok(app) => {
                self.app = Some(app);
                self.wake_at(Instant::now());
            }
            Err(e) => {
                logfile::ERRORS.line(&format!("cannot create the window: {e}"));
                el.exit();
            }
        }
        self.schedule(el);
    }

    fn user_event(&mut self, el: &ActiveEventLoop, e: Wake) {
        match e {
            Wake::At(at) => self.wake_at(at),
            Wake::Quit => el.exit(),
        }
        self.schedule(el);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(app) = self.app.as_mut() else { return };
        let now = app.on_window_event(&event).then(Instant::now);
        let own = app.wake.take();
        for t in [now, own].into_iter().flatten() {
            self.wake_at(t);
        }
        self.schedule(el);
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if self.next.is_some_and(|t| t <= Instant::now()) {
            self.next = None;
            if let Some(app) = self.app.as_mut() {
                app.frame();
            }
        }
        if let Some(t) = self.app.as_mut().and_then(|a| a.wake.take()) {
            self.wake_at(t);
        }
        self.schedule(el);
    }
}

/// The app: tray, window, settings and the frame.
struct App {
    config: Config,
    state: model::Shared,
    ctx: egui::Context,
    window: winit::window::Window,
    hwnd: Window,
    egui_winit: egui_winit::State,
    presenter: present::Presenter,
    renderer: render::Renderer,
    tray: tray::Tray,
    /// Set by the tray's Exit; any other close request only hides the window.
    quitting: Arc<AtomicBool>,
    /// Menu actions that need `App` (config, tray); drained each frame.
    actions: mpsc::Receiver<tray::Action>,
    ui: ui::WindowUi,
    errors: logfile::ErrorWatch,
    config_mtime: Option<std::time::SystemTime>,
    /// The text of our last save, to tell our writes from outside edits.
    last_written: Option<String>,
    config_checked: Instant,
    /// Shown at least once (the first frame shows it without focus).
    shown: bool,
    /// The app's own next wake-up (minute timer, drag settle), handed to the
    /// shell after each event; independent of egui's repaint requests.
    wake: Option<Instant>,
    /// The next frame moves the window back beside the tray (after a reset).
    re_anchor: bool,
    /// Set when the user starts a title-bar drag: the time of the last move.
    /// The position is saved once moves stop for `DRAG_SETTLE`.
    dragging: Option<Instant>,
}

impl App {
    fn new(
        el: &ActiveEventLoop,
        ctx: &egui::Context,
        proxy: EventLoopProxy<Wake>,
        config: Config,
        state: model::Shared,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Borderless, no taskbar button, hidden until the first frame is
        // drawn, and never activated on its own (the first show doesn't take
        // focus from whatever the user is typing in).
        let mut attrs = WindowAttributes::default()
            .with_title(platform::WINDOW_TITLE)
            .with_decorations(false)
            .with_resizable(false)
            .with_visible(false)
            .with_active(false)
            .with_skip_taskbar(true)
            .with_inner_size(winit::dpi::PhysicalSize::new(1, 1));
        if config.topmost {
            attrs = attrs.with_window_level(WindowLevel::AlwaysOnTop);
        }
        let window = el.create_window(attrs)?;
        let hwnd = match window.window_handle()?.as_raw() {
            RawWindowHandle::Win32(h) => Window::from_raw(h.hwnd.get()),
            _ => return Err("expected a Win32 window".into()),
        };
        let presenter = present::Presenter::new(hwnd.raw());
        let egui_winit =
            egui_winit::State::new(ctx.clone(), egui::ViewportId::ROOT, el, Some(window.scale_factor() as f32), None, None);
        ctx.set_zoom_factor(config.scale);

        // Tray must be created on this (the event loop's) thread.
        let tray = tray::Tray::new(platform::autostart_enabled()).map_err(|e| e.to_string())?;
        let quitting = Arc::new(AtomicBool::new(false));

        // Tray handlers run on this thread inside the message loop, hidden
        // or not; they act on the window directly and wake a frame.
        let p = proxy.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let Some(tray::Action::Toggle) = tray::tray_action(&event) {
                hwnd.toggle();
                let _ = p.send_event(Wake::At(Instant::now()));
            }
        }));
        let (tx, actions) = mpsc::channel();
        let (ids, quit) = (tray.ids.clone(), quitting.clone());
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| match tray::menu_action(&event, &ids) {
            Some(tray::Action::Refresh) => model::REFRESH.trigger(),
            Some(tray::Action::OpenConfig) => open_config_file(),
            Some(tray::Action::Exit) => {
                quit.store(true, Ordering::SeqCst);
                let _ = proxy.send_event(Wake::Quit);
            }
            Some(other) => {
                let _ = tx.send(other);
                let _ = proxy.send_event(Wake::At(Instant::now()));
            }
            None => {}
        }));

        let ui = ui::WindowUi::new(ctx, &config);
        Ok(Self {
            config,
            state,
            ctx: ctx.clone(),
            window,
            hwnd,
            egui_winit,
            presenter,
            renderer: render::Renderer::default(),
            tray,
            quitting,
            actions,
            ui,
            errors: Default::default(),
            config_mtime: config_mtime(),
            last_written: None,
            config_checked: Instant::now(),
            shown: false,
            wake: None,
            re_anchor: false,
            dragging: None,
        })
    }

    fn wake_in(&mut self, d: Duration) {
        let at = Instant::now() + d;
        self.wake = Some(self.wake.map_or(at, |w| w.min(at)));
    }

    /// Input and window events; true when a frame should follow.
    fn on_window_event(&mut self, event: &WindowEvent) -> bool {
        match event {
            WindowEvent::CloseRequested => {
                // Alt+F4 and the like hide; only the tray's Exit quits.
                if !self.quitting.load(Ordering::SeqCst) {
                    self.hwnd.hide();
                }
                false
            }
            WindowEvent::Moved(_) if self.dragging.is_some() => {
                self.dragging = Some(Instant::now());
                self.wake_in(DRAG_SETTLE);
                false
            }
            _ => self.egui_winit.on_window_event(&self.window, event).repaint,
        }
    }

    /// One tick: tray, config, logs; then, if the window is showing, the UI.
    fn frame(&mut self) {
        self.tick();
        if self.hwnd.is_visible() || !self.shown {
            self.paint();
        }
    }

    fn tick(&mut self) {
        while let Ok(action) = self.actions.try_recv() {
            self.handle(action);
        }
        self.reload_if_edited();
        self.save_dragged_position();

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
        // Countdowns and staleness move with the clock: look again at the
        // next minute boundary (one wake-up a minute, hidden or not).
        let secs = 60 - (now.timestamp().rem_euclid(60)) as u64;
        self.wake_in(Duration::from_secs(secs));
    }

    fn paint(&mut self) {
        let mut raw = self.egui_winit.take_egui_input(&self.window);
        // The content sizes the window, not the other way round: lay out in
        // an unbounded area, or a growing view (details, settings) would be
        // measured and clipped at the current, smaller window size.
        raw.screen_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(8192.0, 8192.0)));
        let mut actions = Vec::new();
        let mut size = egui::Vec2::ZERO;
        let view_before = self.ui.view;
        let out = {
            let (wui, state, config) = (&mut self.ui, &self.state, &self.config);
            self.ctx.run_ui(raw, |ui| {
                let s = model::lock(state);
                let (sz, a) = wui.draw(ui, &s, config);
                size = sz;
                actions = a;
            })
        };
        self.egui_winit.handle_platform_output(&self.window, out.platform_output);
        let freed = self.renderer.update_textures(out.textures_delta);
        let ppp = out.pixels_per_point;
        let prims = self.ctx.tessellate(out.shapes, ppp);

        let (w, h) = ((size.x * ppp).ceil().max(1.0) as usize, (size.y * ppp).ceil().max(1.0) as usize);
        let old = self.presenter.size();
        // A click switched views during this pass, so it was laid out for the
        // old view: don't show it (that's a visible in-between size); the
        // immediate next frame draws the new view.
        let switched = self.ui.view != view_before && self.shown;
        let ready = if switched { false } else { match self.presenter.ensure_size(w, h) {
            Ok(recreated) => {
                if recreated {
                    self.renderer.invalidate();
                }
                true
            }
            // Out of GDI memory: keep showing the last frame, try again soon.
            Err(()) => {
                self.wake_in(Duration::from_secs(1));
                false
            }
        } };
        if ready {
            self.renderer.render(self.presenter.buffer(), w, h, &prims, ppp);
            let pos = self.place(old, (w, h));
            self.presenter.present(pos, self.config.opacity);
            self.re_anchor = false;
        }
        self.renderer.free_textures(freed);
        if !self.shown {
            self.shown = true;
            self.hwnd.show_no_activate();
        }

        for a in actions {
            match a {
                ui::Action::Hide => self.hwnd.hide(),
                ui::Action::StartDrag => {
                    self.dragging = Some(Instant::now());
                    let _ = self.window.drag_window();
                }
                ui::Action::ResetPosition => {
                    self.dragging = None;
                    self.config.left = None;
                    self.config.top = None;
                    self.save_config();
                    self.re_anchor = true;
                    self.wake_in(Duration::ZERO);
                }
                ui::Action::OpenConfig => open_config_file(),
                ui::Action::Apply(cfg, autostart) => self.apply_settings(*cfg, autostart, true),
            }
        }
    }

    /// Where the window goes for this frame (physical px), or `None` to stay.
    /// First frame (or after a position reset): the saved spot, else the
    /// tray corner. Later, on a size change: shift so the corner nearest the
    /// tray stays put.
    fn place(&self, old: (usize, usize), new: (usize, usize)) -> Option<(i32, i32)> {
        let corner = platform::tray_corner();
        if !self.shown || self.re_anchor {
            // Saved in physical px (no per-monitor scale to get wrong), and
            // used only while that spot is still on a monitor.
            if let (Some(x), Some(y)) = (self.config.left, self.config.top) {
                let (x, y) = (x.round() as i32, y.round() as i32);
                if platform::on_screen(x + 8, y + 8) {
                    return Some((x, y));
                }
            }
            let ((l, t, r, b), c, s) = corner?;
            let gap = (12.0 * s).round() as i32;
            let (w, h) = (new.0 as i32, new.1 as i32);
            let x = if c == platform::Corner::BottomLeft { l + gap } else { r - gap - w };
            let y = if c == platform::Corner::TopRight { t + gap } else { b - gap - h };
            return Some((x, y));
        }
        if old == new {
            return None;
        }
        let pos = self.window.outer_position().ok()?;
        let c = corner.map_or(platform::Corner::BottomRight, |(_, c, _)| c);
        let dx = if c == platform::Corner::BottomLeft { 0 } else { new.0 as i32 - old.0 as i32 };
        let dy = if c == platform::Corner::TopRight { 0 } else { new.1 as i32 - old.1 as i32 };
        Some((pos.x - dx, pos.y - dy))
    }

    /// Saves the position once a title-bar drag has settled. Only drags
    /// count: our own anchoring moves are never saved.
    fn save_dragged_position(&mut self) {
        let Some(last_move) = self.dragging else { return };
        if last_move.elapsed() < DRAG_SETTLE {
            self.wake_in(DRAG_SETTLE);
            return;
        }
        self.dragging = None;
        if let Ok(p) = self.window.outer_position() {
            self.config.left = Some(p.x as f32);
            self.config.top = Some(p.y as f32);
            self.save_config();
        }
    }

    /// Saves and publishes to the workers. Remembers the exact text written,
    /// so our own write is never mistaken for an outside edit (and an outside
    /// edit landing right after ours is never mistaken for ours).
    fn save_config(&mut self) {
        if let Ok(text) = self.config.save_text() {
            self.last_written = Some(text);
        }
        config::publish(&self.config);
    }

    /// Picks up edits made to config.json while running (checked at most
    /// once a second, on the frames the app runs anyway).
    fn reload_if_edited(&mut self) {
        if self.config_checked.elapsed() < std::time::Duration::from_secs(1) {
            return;
        }
        self.config_checked = std::time::Instant::now();
        let mtime = config_mtime();
        if mtime.is_none() || mtime == self.config_mtime {
            return;
        }
        // Unreadable right now (an editor holding it): try again next time.
        let Some(text) = config::path().and_then(|p| std::fs::read_to_string(p).ok()) else { return };
        self.config_mtime = mtime;
        if self.last_written.as_deref() == Some(text.as_str()) {
            return;
        }
        match Config::parse(&text) {
            Some(loaded) => {
                logfile::ERRORS.line("config.json changed on disk: reloaded");
                let autostart = platform::autostart_enabled();
                self.apply_settings(loaded, autostart, false);
            }
            // Invalid (mid-save, a typo): keep the current settings; the
            // next write changes the mtime and is looked at again.
            None => logfile::ERRORS.line("config.json changed on disk but is not valid: kept current settings"),
        }
    }

    /// `save`: false when the config came from the file itself (an outside
    /// edit), so it isn't rewritten under the editor that made it.
    fn apply_settings(&mut self, cfg: Config, autostart: bool, save: bool) {
        if cfg.topmost != self.config.topmost {
            self.window.set_window_level(if cfg.topmost { WindowLevel::AlwaysOnTop } else { WindowLevel::Normal });
        }
        if cfg.scale != self.config.scale {
            self.ctx.set_zoom_factor(cfg.scale);
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

    fn handle(&mut self, action: tray::Action) {
        match action {
            tray::Action::Larger | tray::Action::Smaller => {
                let step = if matches!(action, tray::Action::Larger) { 0.1 } else { -0.1 };
                // Rounded so repeated steps don't drift (1.0999…).
                self.config.scale = ((self.config.scale + step) * 10.0).round().clamp(7.5, 20.0) / 10.0;
                self.ctx.set_zoom_factor(self.config.scale);
                self.save_config();
            }
            tray::Action::Settings => {
                self.ui.open_settings(&self.config);
                self.hwnd.show();
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

/// How long the window must stay still after a drag before it's saved.
const DRAG_SETTLE: Duration = Duration::from_millis(600);

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
