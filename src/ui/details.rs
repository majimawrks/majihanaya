//! Details view (632 wide, two columns): see the design's Expanded board.

use super::feeds::{Feed, Wall};
use super::monitor::Fx;
use super::textures::Textures;
use crate::config::Config;
use crate::model::AppState;
use chrono::{DateTime, FixedOffset, Utc};
use eframe::egui;

/// Draws the details body (everything between title bar and footer).
#[allow(clippy::too_many_arguments)]
pub fn draw(
    ui: &mut egui::Ui,
    tex: &Textures,
    wall: &Wall,
    state: &AppState,
    config: &Config,
    now: DateTime<Utc>,
    tz: FixedOffset,
    blink_on: bool,
    fx: &dyn Fn(&Feed) -> Fx,
) {
    let _ = (tex, wall, state, config, now, tz, blink_on, fx);
    ui.label("details: TODO(M3)");
}
