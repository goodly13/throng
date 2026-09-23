//! The preferences window: settings generated from their metadata (so a new setting needs no UI
//! code), the theme editor, and the key bindings.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Context, RichText, Ui};
use throng_core::keymap::{COMMANDS, Chord, Keymap, Scope, TerminalTier, command, terminal_tier};
use throng_core::project::Colour;
use throng_core::settings::{SETTINGS, SettingKind, SettingValue, Settings};
use throng_core::terminal::ShellInfo;
use throng_core::theme::{TOKENS, Theme, contrast};

use crate::themes::ThemeStore;

/// How long a colour must stay put before the theme file is written.
const SAVE_AFTER: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Section {
    #[default]
    Settings,
    Theme,
    Keys,
}

/// A chord waiting on the user: one that already runs something else.
#[derive(Clone, Debug)]
struct Pending {
    id: &'static str,
    chord: Chord,
    rivals: Vec<&'static str>,
}

/// The window's own state.
#[derive(Default)]
pub struct Prefs {
    pub open: bool,
    pub section: Section,
    /// A user theme being edited, ahead of its file, and when it last changed.
    draft: Option<(Theme, Instant)>,
    problem: Option<String>,
    /// The command whose next chord is being captured.
    capturing: Option<&'static str>,
    pending: Option<Pending>,
    filter: String,
    key_problem: Option<String>,
}

/// What the window needs from the rest of the app.
pub struct PrefsCtx<'a> {
    pub settings: &'a mut Settings,
    pub themes: &'a mut ThemeStore,
    pub shells: &'a [ShellInfo],
    pub icon_packs: &'a [String],
    pub path: &'a str,
    pub system_dark: bool,
    pub keymap: &'a mut Arc<Keymap>,
    pub keybindings_path: &'a Path,
}

impl Prefs {
    /// Show the window. Returns true when a setting changed (the caller writes the file).
    pub fn show(&mut self, ctx: &Context, p: &mut PrefsCtx<'_>) -> bool {
        let mut changed = false;
        let mut open = self.open;
        egui::Window::new("Preferences")
            .open(&mut open)
            .default_width(600.0)
            .default_height(560.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.section, Section::Settings, "Settings");
                    ui.selectable_value(&mut self.section, Section::Theme, "Themes");
                    ui.selectable_value(&mut self.section, Section::Keys, "Key Bindings");
                });
                ui.separator();
                match self.section {
                    Section::Settings => changed |= settings_ui(ui, p),
                    Section::Theme => changed |= self.theme_ui(ui, p),
                    Section::Keys => self.keys_ui(ui, p),
                }
            });
        self.open = open;
        if !open {
            self.capturing = None;
            self.pending = None;
        }
        self.tick(p.themes, !open);
        changed
    }

    /// Whether the next key press is a chord being captured (the caller lets nothing else have it).
    #[must_use]
    pub fn capturing(&self) -> bool {
        self.open && self.capturing.is_some()
    }

    /// Take this frame's key presses for the chord being captured. Escape alone cancels.
    pub fn capture(&mut self, ctx: &Context, p: &mut PrefsCtx<'_>) {
        let Some(id) = self.capturing else { return };
        let pressed = ctx.input_mut(|i| {
            let found = i.events.iter().find_map(|e| match e {
                egui::Event::Key { key, pressed: true, modifiers, .. } => Some((*key, *modifiers)),
                egui::Event::Copy => Some((egui::Key::C, egui::Modifiers::COMMAND)),
                egui::Event::Cut => Some((egui::Key::X, egui::Modifiers::COMMAND)),
                egui::Event::Paste(_) => Some((egui::Key::V, egui::Modifiers::COMMAND)),
                _ => None,
            });
            if found.is_some() {
                i.events.clear();
            }
            found
        });
        let Some((key, modifiers)) = pressed else { return };
        self.capturing = None;
        if key == egui::Key::Escape && modifiers.is_none() {
            return;
        }
        let Some(chord) = crate::keymap::chord(key, modifiers) else { return };
        if let Some(reason) = p.keymap.refusal(id, &chord) {
            self.key_problem = Some(reason);
            return;
        }
        let rivals = p.keymap.conflicts(id, &chord);
        if rivals.is_empty() {
            self.bind(p, id, chord);
        } else {
            self.pending = Some(Pending { id, chord, rivals });
        }
    }

    fn bind(&mut self, p: &mut PrefsCtx<'_>, id: &'static str, chord: Chord) {
        Arc::make_mut(p.keymap).bind(id, chord);
        self.write_keys(p);
    }

    /// Write the key bindings over the file, keeping whatever else it holds.
    fn write_keys(&mut self, p: &mut PrefsCtx<'_>) {
        let existing = std::fs::read_to_string(p.keybindings_path).ok();
        let text = p.keymap.write_into(existing.as_deref());
        self.key_problem = throng_platform::fs::atomic_write(p.keybindings_path, text.as_bytes())
            .err()
            .map(|e| format!("Could not save keybindings.json: {e}"));
    }

    fn keys_ui(&mut self, ui: &mut Ui, p: &mut PrefsCtx<'_>) {
        ui.weak(format!(
            "Stored in {}. Only changes from the defaults are written; edits made there by hand are picked up automatically.",
            p.keybindings_path.display()
        ));
        ui.horizontal(|ui| {
            ui.label("Filter");
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("command or chord")
                    .desired_width(220.0),
            );
        });
        if let Some(problem) = &self.key_problem {
            ui.colored_label(ui.visuals().error_fg_color, problem);
        }
        if let Some(id) = self.capturing {
            let label = command(id).map_or(id, |c| c.label);
            ui.colored_label(
                ui.visuals().hyperlink_color,
                format!("Press the keys for \"{label}\"… (Escape cancels)"),
            );
        }
        if let Some(pending) = self.pending.clone() {
            let names: Vec<&str> =
                pending.rivals.iter().filter_map(|r| command(r)).map(|c| c.label).collect();
            let chord = crate::keymap::display(&pending.chord);
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("{chord} already runs {}. Reassign it?", names.join(", ")),
            );
            ui.horizontal(|ui| {
                if ui.button("Reassign").clicked() {
                    self.pending = None;
                    self.bind(p, pending.id, pending.chord.clone());
                }
                if ui.button("Cancel").clicked() {
                    self.pending = None;
                }
            });
        }
        ui.add_space(4.0);
        let filter = self.filter.trim().to_lowercase();
        let mac = crate::keymap::mac();
        egui::ScrollArea::vertical().id_salt("keys").show(ui, |ui| {
            let mut group = "";
            egui::Grid::new("keys").num_columns(3).spacing([12.0, 6.0]).striped(true).show(ui, |ui| {
                for c in COMMANDS {
                    let chords = p.keymap.chords(c.id).to_vec();
                    let shown: Vec<String> = chords.iter().map(crate::keymap::display).collect();
                    let matches = filter.is_empty()
                        || c.label.to_lowercase().contains(&filter)
                        || c.id.to_lowercase().contains(&filter)
                        || shown.iter().any(|s| s.to_lowercase().contains(&filter));
                    if !matches {
                        continue;
                    }
                    if c.group != group {
                        group = c.group;
                        ui.strong(group);
                        ui.end_row();
                    }
                    ui.label(c.label).on_hover_text(format!("{}\nLive in: {}", c.id, scope_names(c.scopes)));
                    ui.horizontal(|ui| {
                        ui.set_min_width(200.0);
                        for (chord, text) in chords.iter().zip(&shown) {
                            let mut hover = format!("Remove {text}");
                            if c.live_in(Scope::Terminal)
                                && let TerminalTier::Shadowable(what) = terminal_tier(chord, mac)
                            {
                                hover = format!("Terminals lose {text} ({what}) to this. {hover}");
                            }
                            let button =
                                egui::Button::new(RichText::new(format!("{text}  ×")).monospace().small())
                                    .wrap_mode(egui::TextWrapMode::Extend);
                            if ui.add(button).on_hover_text(hover).clicked() {
                                Arc::make_mut(p.keymap).unbind(c.id, chord);
                                self.write_keys(p);
                            }
                        }
                        if chords.is_empty() {
                            ui.weak("unbound");
                        }
                    });
                    ui.horizontal(|ui| {
                        let name = format!("Add a chord for {}", c.label);
                        let add = ui.add(egui::Button::new("Add…").small());
                        add.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &name));
                        if add.on_hover_text(&name).clicked() {
                            self.capturing = Some(c.id);
                            self.pending = None;
                            self.key_problem = None;
                        }
                        if !p.keymap.is_default(c.id)
                            && ui
                                .small_button(crate::icons::atom(ui.ctx(), "retry"))
                                .on_hover_text("Back to the default")
                                .clicked()
                        {
                            Arc::make_mut(p.keymap).reset(c.id);
                            self.write_keys(p);
                        }
                    });
                    ui.end_row();
                }
            });
        });
    }

    /// Write a draft that has settled (or at once, when the window closes).
    pub fn tick(&mut self, themes: &mut ThemeStore, now: bool) {
        if let Some((theme, at)) = &self.draft
            && (now || at.elapsed() >= SAVE_AFTER)
        {
            if let Err(e) = themes.save(theme) {
                self.problem = Some(format!("Could not save \"{}\": {e}", theme.name));
            }
            self.draft = None;
        }
    }

    /// Whether a draft is waiting to be written (the caller repaints to get it written).
    #[must_use]
    pub fn pending(&self) -> bool {
        self.draft.is_some()
    }

    /// After the themes folder was re-read: an unsaved draft still wins.
    pub fn reapply(&self, themes: &mut ThemeStore) {
        if let Some((theme, _)) = &self.draft {
            themes.update(theme.clone());
        }
    }

    fn theme_ui(&mut self, ui: &mut Ui, p: &mut PrefsCtx<'_>) -> bool {
        let mut changed = false;
        let active = p.themes.resolve(p.settings.theme(), p.system_dark).clone();
        ui.horizontal(|ui| {
            ui.label("Theme");
            if let Some(name) = theme_combo(ui, "theme-picker", p.settings.theme(), p.themes) {
                changed |= p.settings.set("appearance.theme", SettingValue::Text(name));
            }
            if ui.button("Duplicate").on_hover_text("Make an editable copy of this theme").clicked() {
                let copy = active.cloned_as(&p.themes.unused_name(&format!("{} copy", active.name)));
                match p.themes.save(&copy) {
                    Ok(()) => {
                        changed |= p.settings.set("appearance.theme", SettingValue::Text(copy.name));
                        self.problem = None;
                    }
                    Err(e) => self.problem = Some(format!("Could not save the copy: {e}")),
                }
            }
            let delete = ui.add_enabled(!active.builtin, egui::Button::new("Delete")).on_hover_text(
                if active.builtin {
                    "Built-in themes cannot be deleted"
                } else {
                    "Move this theme's file to the trash"
                },
            );
            if delete.clicked() {
                self.draft = None;
                match p.themes.delete(&active.name) {
                    Ok(()) => {
                        p.settings.reset("appearance.theme");
                        changed = true;
                        self.problem = None;
                    }
                    Err(e) => self.problem = Some(e),
                }
            }
        });
        ui.weak(format!("Your own themes are JSON files in {}.", p.themes.dir().display()));
        if let Some(problem) = &self.problem {
            ui.colored_label(ui.visuals().error_fg_color, problem);
        }
        if active.builtin {
            ui.weak("Built-in themes cannot be changed. Duplicate this one to make your own.");
        }
        ui.add_space(4.0);
        if self.draft.as_ref().is_some_and(|(t, _)| t.name != active.name) {
            // The user moved to another theme mid-edit: the edit is kept.
            self.tick(p.themes, true);
        }
        let mut theme = self.draft.as_ref().map_or(active, |(t, _)| t.clone());
        let mut edited = false;
        egui::ScrollArea::vertical().id_salt("tokens").show(ui, |ui| {
            let mut group = "";
            for def in TOKENS {
                if def.group != group {
                    group = def.group;
                    ui.add_space(6.0);
                    ui.strong(group);
                }
                ui.horizontal(|ui| {
                    let colour = theme.colour(def.key);
                    let mut rgb = [colour.r, colour.g, colour.b];
                    let response =
                        ui.add_enabled_ui(!theme.builtin, |ui| ui.color_edit_button_srgb(&mut rgb));
                    let response = response.inner.on_hover_text(def.key);
                    if response.changed() {
                        theme.set(def.key, Colour { r: rgb[0], g: rgb[1], b: rgb[2] });
                        edited = true;
                    }
                    ui.label(def.label);
                    ui.weak(RichText::new(colour.to_hex()).monospace().small());
                    if let Some(ground) = ground_of(def.key) {
                        let ratio = contrast(theme.colour(def.key), theme.colour(ground));
                        if ratio < 4.5 {
                            ui.colored_label(ui.visuals().warn_fg_color, format!("{ratio:.1}:1"))
                                .on_hover_text(format!(
                                    "Hard to read on {ground}: text wants 4.5:1 or more."
                                ));
                        }
                    }
                });
            }
        });
        if edited {
            p.themes.update(theme.clone());
            self.draft = Some((theme, Instant::now()));
        }
        changed
    }
}

fn scope_names(scopes: &[Scope]) -> String {
    if scopes.len() == throng_core::keymap::EVERYWHERE.len() {
        return "everywhere".to_owned();
    }
    let name = |s: &Scope| match s {
        Scope::Editor => "editors",
        Scope::Terminal => "terminals",
        Scope::Explorer => "the file tree",
        Scope::Preview => "previews",
        Scope::FindInFiles => "Find in Files",
    };
    scopes.iter().map(name).collect::<Vec<_>>().join(", ")
}

/// The token a text token is read on, when it has one.
fn ground_of(key: &str) -> Option<&'static str> {
    match key {
        "text" | "textMuted" => Some("appBg"),
        "editorFg" => Some("editorBg"),
        "editorGutterFg" => Some("editorGutterBg"),
        "editorStatusStripFg" => Some("editorStatusStripBg"),
        "terminalFg" => Some("terminalBg"),
        k if k.starts_with("syntax") => Some("editorBg"),
        _ => None,
    }
}

/// A combo box of every theme, built-ins first. Returns the name picked, if it changed.
fn theme_combo(ui: &mut Ui, salt: &str, current: &str, themes: &ThemeStore) -> Option<String> {
    let mut chosen = current.to_owned();
    let shown = if current == "system" { "Follow the system" } else { current };
    let combo = egui::ComboBox::from_id_salt(salt).selected_text(shown).width(200.0).show_ui(ui, |ui| {
        ui.selectable_value(&mut chosen, "system".to_owned(), "Follow the system")
            .on_hover_text("throng when the system is dark, Light when it is light");
        let mut own = false;
        for theme in themes.all() {
            if !theme.builtin && !own {
                own = true;
                ui.separator();
            }
            let on = chosen.eq_ignore_ascii_case(&theme.name);
            if ui.selectable_label(on, &theme.name).clicked() {
                chosen.clone_from(&theme.name);
            }
        }
    });
    combo.response.widget_info(|| {
        let mut info = egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, "Active theme");
        info.current_text_value = Some(shown.to_owned());
        info
    });
    (chosen != current).then_some(chosen)
}

fn settings_ui(ui: &mut Ui, p: &mut PrefsCtx<'_>) -> bool {
    let mut changed = false;
    ui.weak(format!("Stored in {}. Edits made in that file by hand are picked up automatically.", p.path));
    ui.add_space(6.0);
    egui::ScrollArea::vertical().show(ui, |ui| {
        let mut section = "";
        egui::Grid::new("prefs").num_columns(3).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
            for def in SETTINGS {
                let group = def.key.split('.').next().unwrap_or("");
                if group != section {
                    section = group;
                    ui.strong(capitalise(group));
                    ui.end_row();
                }
                ui.label(def.label).on_hover_text(def.help);
                if let Some(value) = control(ui, def.key, def.kind, p.settings.get(def.key).cloned(), p) {
                    changed |= p.settings.set(def.key, value);
                }
                if ui
                    .small_button(crate::icons::atom(ui.ctx(), "retry"))
                    .on_hover_text("Reset to default")
                    .clicked()
                {
                    p.settings.reset(def.key);
                    changed = true;
                }
                ui.end_row();
            }
        });
    });
    changed
}

fn capitalise(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map_or_else(String::new, |c| c.to_uppercase().collect::<String>() + chars.as_str())
}

fn control(
    ui: &mut Ui,
    key: &str,
    kind: SettingKind,
    value: Option<SettingValue>,
    p: &PrefsCtx<'_>,
) -> Option<SettingValue> {
    match (kind, value) {
        (SettingKind::Bool { .. }, Some(SettingValue::Bool(mut b))) => {
            ui.checkbox(&mut b, "").changed().then_some(SettingValue::Bool(b))
        }
        (SettingKind::Int { min, max, .. }, Some(SettingValue::Int(mut i))) => {
            let speed = ((max - min) as f64 / 500.0).max(1.0);
            ui.add(egui::DragValue::new(&mut i).range(min..=max).speed(speed))
                .changed()
                .then_some(SettingValue::Int(i))
        }
        (SettingKind::Float { min, max, step, .. }, Some(SettingValue::Float(mut f))) => ui
            .add(egui::DragValue::new(&mut f).range(min..=max).speed(step).fixed_decimals(2))
            .changed()
            .then_some(SettingValue::Float(f)),
        (SettingKind::Choice { options, .. }, Some(SettingValue::Text(current))) => {
            let mut chosen = current.clone();
            egui::ComboBox::from_id_salt(key).selected_text(&current).show_ui(ui, |ui| {
                for option in options {
                    ui.selectable_value(&mut chosen, (*option).to_owned(), *option);
                }
            });
            (chosen != current).then_some(SettingValue::Text(chosen))
        }
        (SettingKind::Text { .. }, Some(SettingValue::Text(current))) if key == "appearance.theme" => {
            theme_combo(ui, key, &current, p.themes).map(SettingValue::Text)
        }
        (SettingKind::Text { .. }, Some(SettingValue::Text(current))) if key == "appearance.iconPack" => {
            let mut chosen = current.clone();
            let shown = if current.is_empty() { "throng" } else { current.as_str() };
            egui::ComboBox::from_id_salt(key).selected_text(shown).show_ui(ui, |ui| {
                ui.selectable_value(&mut chosen, String::new(), "throng");
                for pack in p.icon_packs {
                    ui.selectable_value(&mut chosen, pack.clone(), pack);
                }
            });
            (chosen != current).then_some(SettingValue::Text(chosen))
        }
        (SettingKind::Text { .. }, Some(SettingValue::Text(mut current))) => ui
            .add(egui::TextEdit::singleline(&mut current).desired_width(200.0))
            .changed()
            .then_some(SettingValue::Text(current)),
        (SettingKind::OptText, Some(SettingValue::OptText(current))) => {
            let mut chosen = current.clone();
            let label = current.clone().unwrap_or_else(|| "Login shell".into());
            egui::ComboBox::from_id_salt(key).selected_text(label).show_ui(ui, |ui| {
                ui.selectable_value(&mut chosen, None, "Login shell");
                for shell in p.shells {
                    ui.selectable_value(&mut chosen, Some(shell.id.clone()), &shell.label);
                }
            });
            (chosen != current).then_some(SettingValue::OptText(chosen))
        }
        (SettingKind::List { .. }, Some(SettingValue::List(items))) => {
            let mut text = items.join("\n");
            let response = ui.add(egui::TextEdit::multiline(&mut text).desired_rows(3).desired_width(220.0));
            response.changed().then(|| {
                SettingValue::List(
                    text.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect(),
                )
            })
        }
        _ => {
            ui.label("—");
            None
        }
    }
}
