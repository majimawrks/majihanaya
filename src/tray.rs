//! Tray icon and its menu. What it shows comes from `trayview`; this module
//! only owns the Win32-side objects, which must stay on the UI thread.

use crate::trayview::{self, View};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[derive(Clone)]
pub struct Ids {
    refresh: MenuId,
    settings: MenuId,
    larger: MenuId,
    smaller: MenuId,
    open_config: MenuId,
    autostart: MenuId,
    exit: MenuId,
}

pub enum Action {
    Toggle,
    Refresh,
    Settings,
    Larger,
    Smaller,
    OpenConfig,
    /// The menu has already flipped the check mark; read it with `autostart_checked`.
    Autostart,
    Exit,
}

pub struct Tray {
    // Dropping the icon removes it from the tray, so it lives as long as the app.
    icon: TrayIcon,
    autostart: CheckMenuItem,
    pub ids: Ids,
    size: u32,
    /// Last drawn (text, level, size) and tooltip, so unchanged state costs nothing.
    shown: Option<(String, trayview::Level, u32)>,
    tip: String,
}

impl Tray {
    pub fn new(autostart_on: bool) -> Result<Self, Box<dyn std::error::Error>> {
        let refresh = MenuItem::new("Refresh now", true, None);
        let settings = MenuItem::new("Settings…", true, None);
        let larger = MenuItem::new("Larger", true, None);
        let smaller = MenuItem::new("Smaller", true, None);
        let open_config = MenuItem::new("Open config", true, None);
        let autostart = CheckMenuItem::new("Start with Windows", true, autostart_on, None);
        let exit = MenuItem::new("Exit", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &refresh,
            &settings,
            &PredefinedMenuItem::separator(),
            &larger,
            &smaller,
            &PredefinedMenuItem::separator(),
            &open_config,
            &autostart,
            &PredefinedMenuItem::separator(),
            &exit,
        ])?;

        let size = crate::platform::tray_icon_size();
        let icon = TrayIconBuilder::new()
            .with_icon(render("--", trayview::Level::NoData, size))
            .with_tooltip("majihanaya")
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()?;

        let ids = Ids {
            refresh: refresh.id().clone(),
            settings: settings.id().clone(),
            larger: larger.id().clone(),
            smaller: smaller.id().clone(),
            open_config: open_config.id().clone(),
            autostart: autostart.id().clone(),
            exit: exit.id().clone(),
        };
        Ok(Self { icon, autostart, ids, size, shown: None, tip: String::new() })
    }

    /// Redraws the icon and tooltip only when they changed.
    pub fn update(&mut self, view: &View, tooltip: String) {
        let key = (view.text.clone(), view.level, self.size);
        if self.shown.as_ref() != Some(&key) {
            let _ = self.icon.set_icon(Some(render(&view.text, view.level, self.size)));
            self.shown = Some(key);
        }
        if tooltip != self.tip {
            let _ = self.icon.set_tooltip(Some(&tooltip));
            self.tip = tooltip;
        }
    }

    pub fn autostart_checked(&self) -> bool {
        self.autostart.is_checked()
    }

    /// Puts the check mark back when the change it asked for failed.
    pub fn set_autostart_checked(&self, on: bool) {
        self.autostart.set_checked(on);
    }
}

fn render(text: &str, level: trayview::Level, size: u32) -> Icon {
    let (rgba, s) = trayview::icon_rgba(text, level, size);
    Icon::from_rgba(rgba, s, s).expect("valid icon size")
}

/// Maps a raw tray event to what the app should do, if anything.
pub fn tray_action(event: &TrayIconEvent) -> Option<Action> {
    match event {
        TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } => {
            Some(Action::Toggle)
        }
        _ => None,
    }
}

pub fn menu_action(event: &MenuEvent, ids: &Ids) -> Option<Action> {
    let id = &event.id;
    Some(if id == &ids.refresh {
        Action::Refresh
    } else if id == &ids.settings {
        Action::Settings
    } else if id == &ids.larger {
        Action::Larger
    } else if id == &ids.smaller {
        Action::Smaller
    } else if id == &ids.open_config {
        Action::OpenConfig
    } else if id == &ids.autostart {
        Action::Autostart
    } else if id == &ids.exit {
        Action::Exit
    } else {
        return None;
    })
}
