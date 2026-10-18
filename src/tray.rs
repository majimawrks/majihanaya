//! Tray icon and its menu. M0: placeholder icon; real rendering arrives in M2.

use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const SIZE: u32 = 32;
const NO_DATA: [u8; 3] = [0x6B, 0x72, 0x80];

pub struct Tray {
    // Dropping the icon removes it from the tray, so it lives as long as the app.
    _icon: TrayIcon,
    pub open_config: MenuId,
    pub exit: MenuId,
}

pub enum Action {
    Toggle,
    OpenConfig,
    Exit,
}

impl Tray {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let open_config = MenuItem::new("Open config", true, None);
        let exit = MenuItem::new("Exit", true, None);
        let menu = Menu::new();
        menu.append_items(&[&open_config, &PredefinedMenuItem::separator(), &exit])?;

        let icon = TrayIconBuilder::new()
            .with_icon(placeholder_icon())
            .with_tooltip("majihanaya")
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()?;

        Ok(Self { _icon: icon, open_config: open_config.id().clone(), exit: exit.id().clone() })
    }
}

/// Maps a raw tray/menu event to what the app should do, if anything.
pub fn tray_action(event: &TrayIconEvent) -> Option<Action> {
    match event {
        TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } => {
            Some(Action::Toggle)
        }
        _ => None,
    }
}

pub fn menu_action(event: &MenuEvent, open_config: &MenuId, exit: &MenuId) -> Option<Action> {
    if &event.id == open_config {
        Some(Action::OpenConfig)
    } else if &event.id == exit {
        Some(Action::Exit)
    } else {
        None
    }
}

/// A grey "no data" disc.
fn placeholder_icon() -> Icon {
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    let c = (SIZE as f32 - 1.0) / 2.0;
    let r = SIZE as f32 / 2.0 - 1.0;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            // One pixel of anti-aliasing at the edge.
            let a = (r - d + 0.5).clamp(0.0, 1.0);
            let i = ((y * SIZE + x) * 4) as usize;
            rgba[i..i + 3].copy_from_slice(&NO_DATA);
            rgba[i + 3] = (a * 255.0) as u8;
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).expect("valid icon size")
}
