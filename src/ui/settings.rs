//! Settings panel (632 wide): see the design's Settings board.

use crate::config::Config;
use eframe::egui;

/// The edited copy of the settings, compared with what was loaded.
pub struct Draft {
    pub saved: Config,
    pub cur: Config,
    pub autostart_saved: bool,
    pub autostart: bool,
}

pub enum Outcome {
    Apply(Box<Config>, bool),
    Cancel,
    OpenConfig,
}

impl Draft {
    pub fn new(config: &Config, autostart: bool) -> Self {
        Self { saved: config.clone(), cur: config.clone(), autostart_saved: autostart, autostart }
    }
}

pub fn draw(ui: &mut egui::Ui, draft: &mut Draft) -> Option<Outcome> {
    let _ = draft;
    ui.label("settings: TODO(M3)");
    None
}
