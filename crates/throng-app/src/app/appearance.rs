//! The menus that change the look in one click: View → Theme and View → Style, each listing its
//! choices with the one in force marked, and (on macOS) the app's own menu with Preferences.

use egui::{Ui, ViewportCommand};

use super::ThrongApp;
use crate::dialogs::Dialog;
use crate::style::Style;

impl ThrongApp {
    /// The macOS app menu: About, Preferences and Quit.
    pub(super) fn app_menu(&mut self, ui: &mut Ui) {
        if ui.button("About throng").clicked() {
            self.dialog = Some(Dialog::About);
            ui.close();
        }
        ui.separator();
        self.preferences_item(ui);
        ui.separator();
        if ui.button("Quit throng").clicked() {
            ui.ctx().send_viewport_cmd(ViewportCommand::Close);
            ui.close();
        }
    }

    /// *Preferences…*, with the chord bound to it.
    pub(super) fn preferences_item(&mut self, ui: &mut Ui) {
        let item = egui::Button::new("Preferences…")
            .shortcut_text(crate::keymap::label(ui.ctx(), "app.preferences"));
        if ui.add(item).clicked() {
            self.prefs.open = true;
            ui.close();
        }
    }

    /// View → Theme and View → Style. A choice applies at once and is written to the settings.
    pub(super) fn appearance_menus(&mut self, ui: &mut Ui) {
        ui.menu_button("Theme", |ui| {
            let current = self.settings.theme().to_owned();
            let mut chosen = None;
            if ui.radio(current == "system", "Follow the System").clicked() {
                chosen = Some("system".to_owned());
            }
            ui.separator();
            let mut own = false;
            egui::ScrollArea::vertical().max_height(ui.ctx().content_rect().height() * 0.8).show(ui, |ui| {
                for theme in self.themes.all() {
                    if !theme.builtin && !own {
                        own = true;
                        ui.separator();
                    }
                    let on = current != "system" && self.look.name == theme.name;
                    if ui.radio(on, &theme.name).clicked() {
                        chosen = Some(theme.name.clone());
                    }
                }
            });
            ui.separator();
            if ui.button("Edit Themes…").clicked() {
                self.prefs.open = true;
                self.prefs.section = crate::prefs::Section::Theme;
                ui.close();
            }
            if let Some(name) = chosen {
                self.set_text("appearance.theme", &name);
                ui.close();
            }
        });
        ui.menu_button("Style", |ui| {
            let current = Style::parse(self.settings.style());
            for style in Style::ALL {
                if ui.radio(style == current, style.label()).clicked() {
                    self.set_text("appearance.style", style.key());
                    ui.close();
                }
            }
        });
    }
}
