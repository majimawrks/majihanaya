#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod platform;
mod tray;

use config::Config;
use eframe::egui;
use platform::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tray_icon::TrayIconEvent;
use tray_icon::menu::MenuEvent;

fn main() -> eframe::Result {
    let Some(_instance) = platform::single_instance() else {
        platform::activate_existing();
        return Ok(());
    };

    let config = Config::load();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(platform::WINDOW_TITLE)
            .with_inner_size([340.0, 420.0])
            .with_resizable(false)
            .with_taskbar(false),
        ..Default::default()
    };

    eframe::run_native(
        platform::WINDOW_TITLE,
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, config)?))),
    )
}

struct App {
    config: Config,
    window: Window,
    /// Set by the tray's Exit; any other close request only hides the window.
    quitting: Arc<AtomicBool>,
    _tray: tray::Tray,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, config: Config) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let window = match cc.window_handle()?.as_raw() {
            RawWindowHandle::Win32(h) => Window::from_raw(h.hwnd.get()),
            _ => return Err("expected a Win32 window".into()),
        };

        // Tray must be created on this (the event loop's) thread.
        let tray = tray::Tray::new().map_err(|e| e.to_string())?;
        let quitting = Arc::new(AtomicBool::new(false));

        // Handlers act on the window directly: eframe runs no ui pass while the
        // window is hidden, and these must work then too.
        let ctx = cc.egui_ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let Some(tray::Action::Toggle) = tray::tray_action(&event) {
                window.toggle();
                ctx.request_repaint();
            }
        }));

        let (open_config, exit) = (tray.open_config.clone(), tray.exit.clone());
        let quit = quitting.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            match tray::menu_action(&event, &open_config, &exit) {
                Some(tray::Action::OpenConfig) => open_config_file(),
                Some(tray::Action::Exit) => {
                    quit.store(true, Ordering::SeqCst);
                    window.post_close();
                }
                _ => {}
            }
        }));

        Ok(Self { config, window, quitting, _tray: tray })
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting.load(Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.window.hide();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("majihanaya");
            ui.label("Skeleton (M0). No data yet.");
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.config.save();
    }
}

fn open_config_file() {
    let Some(path) = config::path() else { return };
    if !path.exists() {
        let _ = Config::default().save();
    }
    platform::open_file(&path);
}
