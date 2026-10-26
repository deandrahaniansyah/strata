//! Settings window.

use super::*;
use crate::model::*;
use crate::store::{hash_passcode, Store};
use crate::theme::Palette;
use egui::RichText;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Section {
    #[default]
    General,
    Data,
    Editor,
    Security,
    Ai,
    Shortcuts,
    About,
}

#[derive(Default)]
pub struct SettingsUi {
    section: Section,
    new_passcode: String,
    confirm_passcode: String,
    api_key: Option<String>,
    message: Option<(bool, String)>,
}

pub enum SettingsEvent {
    ImportTheme(Palette),
}

impl SettingsUi {
    pub fn show(&mut self, ctx: &egui::Context, open: &mut bool, s: &mut Settings, store: &Store, themes: &[Palette]) -> Vec<SettingsEvent> {
        let pal = p();
        let mut events = Vec::new();
        let before = s.clone();
        egui::Window::new("Settings").open(open).collapsible(false).resizable(true).default_size([720.0, 520.0]).show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(150.0);
                    for (sec, label) in [
                        (Section::General, "General"),
                        (Section::Data, "Data table"),
                        (Section::Editor, "SQL editor"),
                        (Section::Security, "Security"),
                        (Section::Ai, "AI"),
                        (Section::Shortcuts, "Shortcuts"),
                        (Section::About, "About"),
                    ] {
                        if ui.add_sized([140.0, 28.0], egui::Button::selectable(self.section == sec, label)).clicked() {
                            self.section = sec;
                            self.message = None;
                        }
                    }
                });
                ui.separator();
                ui.vertical(|ui| {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        ui.set_min_width(480.0);
                        match self.section {
                            Section::General => {
                                section_label(ui, "Appearance");
                                ui.horizontal(|ui| {
                                    ui.selectable_value(&mut s.theme, ThemeChoice::Auto, "Auto");
                                    ui.selectable_value(&mut s.theme, ThemeChoice::Dark, "Dark");
                                    ui.selectable_value(&mut s.theme, ThemeChoice::Light, "Light");
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Dark theme");
                                    egui::ComboBox::from_id_salt("dark-theme").selected_text(&s.dark_theme).show_ui(ui, |ui| {
                                        for t in themes.iter().filter(|t| t.dark) {
                                            ui.selectable_value(&mut s.dark_theme, t.name.clone(), &t.name);
                                        }
                                    });
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Light theme");
                                    egui::ComboBox::from_id_salt("light-theme").selected_text(&s.light_theme).show_ui(ui, |ui| {
                                        for t in themes.iter().filter(|t| !t.dark) {
                                            ui.selectable_value(&mut s.light_theme, t.name.clone(), &t.name);
                                        }
                                    });
                                });
                                // Theme previews
                                ui.horizontal_wrapped(|ui| {
                                    for t in themes {
                                        let (r, resp) = ui.allocate_exact_size(egui::vec2(92.0, 58.0), egui::Sense::click());
                                        ui.painter().rect_filled(r, egui::CornerRadius::same(8), t.window);
                                        ui.painter().rect_filled(egui::Rect::from_min_size(r.min, egui::vec2(24.0, r.height())), egui::CornerRadius { nw: 8, sw: 8, ne: 0, se: 0 }, t.sidebar);
                                        ui.painter().rect_filled(egui::Rect::from_min_size(r.min + egui::vec2(30.0, 10.0), egui::vec2(48.0, 6.0)), egui::CornerRadius::same(3), t.accent);
                                        ui.painter().rect_filled(egui::Rect::from_min_size(r.min + egui::vec2(30.0, 22.0), egui::vec2(40.0, 4.0)), egui::CornerRadius::same(2), t.text_dim);
                                        ui.painter().text(r.center_bottom() - egui::vec2(0.0, 9.0), egui::Align2::CENTER_CENTER, &t.name, egui::FontId::proportional(10.0), t.text);
                                        let selected = (t.dark && s.dark_theme == t.name) || (!t.dark && s.light_theme == t.name);
                                        ui.painter().rect_stroke(r, egui::CornerRadius::same(8), egui::Stroke::new(if selected { 2.0 } else { 1.0 }, if selected { pal.accent } else { pal.border }), egui::StrokeKind::Inside);
                                        if resp.clicked() {
                                            if t.dark {
                                                s.dark_theme = t.name.clone();
                                                s.theme = ThemeChoice::Dark;
                                            } else {
                                                s.light_theme = t.name.clone();
                                                s.theme = ThemeChoice::Light;
                                            }
                                        }
                                    }
                                });
                                if ui.button("Import theme JSON…").clicked() {
                                    if let Some(path) = rfd::FileDialog::new().add_filter("Theme", &["json"]).pick_file() {
                                        match Palette::load_file(&path) {
                                            Ok(t) => {
                                                let _ = std::fs::copy(&path, store.themes_dir().join(path.file_name().unwrap_or_default()));
                                                self.message = Some((true, format!("Imported “{}”", t.name)));
                                                events.push(SettingsEvent::ImportTheme(t));
                                            }
                                            Err(e) => self.message = Some((false, format!("{e:#}"))),
                                        }
                                    }
                                }
                                ui.add_space(10.0);
                                ui.horizontal(|ui| {
                                    ui.label("Interface size");
                                    ui.add(egui::Slider::new(&mut s.ui_scale, 0.8..=1.5).step_by(0.05));
                                });
                                ui.add_space(10.0);
                                section_label(ui, "Workspaces");
                                ui.checkbox(&mut s.reopen_workspaces, "Reopen workspaces at startup");
                            }
                            Section::Data => {
                                section_label(ui, "Data table");
                                ui.horizontal(|ui| {
                                    ui.label("Rows per page");
                                    ui.add(egui::DragValue::new(&mut s.page_size).range(10..=100_000));
                                });
                                ui.checkbox(&mut s.show_row_numbers, "Show line numbers in table view");
                                ui.checkbox(&mut s.alternating_rows, "Alternating row background colors");
                                ui.horizontal(|ui| {
                                    ui.label("NULL is shown as");
                                    ui.add(crate::ui::line_edit(&mut s.null_display).desired_width(100.0));
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Grid font size");
                                    ui.add(egui::Slider::new(&mut s.grid_font_size, 10.0..=18.0).step_by(0.5));
                                });
                                ui.horizontal(|ui| {
                                    ui.label("CSV default delimiter");
                                    for (c, l) in [(',', ","), (';', ";"), ('\t', "Tab")] {
                                        ui.selectable_value(&mut s.csv_delimiter, c, l);
                                    }
                                });
                            }
                            Section::Editor => {
                                section_label(ui, "SQL editor");
                                ui.horizontal(|ui| {
                                    ui.label("Font size");
                                    ui.add(egui::Slider::new(&mut s.editor_font_size, 10.0..=22.0).step_by(0.5));
                                });
                                ui.checkbox(&mut s.uppercase_keywords, "Automatically UPPERCASE keywords");
                                ui.checkbox(&mut s.auto_close_brackets, "Automatically insert closing braces and quotes");
                                ui.checkbox(&mut s.enable_query_params, "Enable query params (:name)");
                                ui.horizontal(|ui| {
                                    ui.label("Indent with");
                                    for w in [2usize, 4, 8] {
                                        ui.selectable_value(&mut s.tab_width, w, format!("{w} spaces"));
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Query time out (seconds, 0 = none)");
                                    ui.add(egui::DragValue::new(&mut s.query_timeout_secs).range(0..=86_400));
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Keep history");
                                    for (d, l) in [(30u32, "30 days"), (365, "1 year"), (730, "2 years"), (36_500, "Forever")] {
                                        ui.selectable_value(&mut s.history_days, d, l);
                                    }
                                });
                            }
                            Section::Security => {
                                section_label(ui, "Passcode");
                                ui.label(dim("With a passcode, Strata asks for it at launch and after being idle."));
                                if s.passcode_hash.is_some() {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new("Passcode is on").color(pal.success));
                                        if ui.button("Turn off").clicked() {
                                            s.passcode_hash = None;
                                        }
                                    });
                                } else {
                                    ui.add(crate::ui::line_edit(&mut self.new_passcode).password(true).hint_text("New passcode"));
                                    ui.add(crate::ui::line_edit(&mut self.confirm_passcode).password(true).hint_text("Re-enter new passcode"));
                                    if ui.button("Enable passcode").clicked() {
                                        if self.new_passcode.is_empty() {
                                            self.message = Some((false, "Passcode cannot be blank!".into()));
                                        } else if self.new_passcode != self.confirm_passcode {
                                            self.message = Some((false, "Passcode does not match!".into()));
                                        } else {
                                            s.passcode_hash = Some(hash_passcode(&self.new_passcode));
                                            self.new_passcode.clear();
                                            self.confirm_passcode.clear();
                                            self.message = Some((true, "Passcode enabled".into()));
                                        }
                                    }
                                }
                                ui.horizontal(|ui| {
                                    ui.label("Auto lock if away for");
                                    for (m, l) in [(0u32, "Never"), (1, "1 minute"), (5, "5 minutes"), (60, "1 hour")] {
                                        ui.selectable_value(&mut s.auto_lock_minutes, m, l);
                                    }
                                });
                                ui.add_space(10.0);
                                section_label(ui, "Safe mode");
                                ui.label(dim("Default safe mode for new connections"));
                                for m in SafeMode::ALL {
                                    ui.radio_value(&mut s.default_safe_mode, m, m.label());
                                }
                            }
                            Section::Ai => {
                                section_label(ui, "Assistant");
                                ui.checkbox(&mut s.ai_enabled, "Enable the assistant in the right sidebar");
                                ui.label(dim("Your question, plus table and column names, is sent to the provider below. Row data is never sent."));
                                ui.add_space(6.0);
                                ui.horizontal(|ui| {
                                    if ui.selectable_value(&mut s.ai_provider, AiProvider::Anthropic, "Anthropic").clicked() {
                                        s.ai_base_url = "https://api.anthropic.com".into();
                                        s.ai_model = "claude-opus-5-5".into();
                                    }
                                    if ui.selectable_value(&mut s.ai_provider, AiProvider::OpenAiCompatible, "Custom LLM (OpenAI-compatible)").clicked() {
                                        s.ai_base_url = "http://localhost:11434/v1".into();
                                        s.ai_model = String::new();
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Base URL");
                                    ui.add(crate::ui::line_edit(&mut s.ai_base_url).desired_width(320.0));
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Model");
                                    ui.add(crate::ui::line_edit(&mut s.ai_model).desired_width(220.0));
                                });
                                let key = self.api_key.get_or_insert_with(|| store.load_secret_value(super::assistant::API_KEY_SECRET));
                                ui.horizontal(|ui| {
                                    ui.label("API key");
                                    ui.add(crate::ui::line_edit(key).password(true).desired_width(260.0).hint_text("stored in the system keychain"));
                                    if ui.button("Save key").clicked() {
                                        store.save_secret_value(super::assistant::API_KEY_SECRET, key.trim());
                                        self.message = Some((true, "API key saved".into()));
                                    }
                                });
                                if s.ai_provider == AiProvider::Anthropic {
                                    ui.label(faint("ANTHROPIC_API_KEY in the environment is used when no key is saved."));
                                }
                            }
                            Section::Shortcuts => {
                                section_label(ui, "Keyboard shortcuts");
                                egui::Grid::new("keys").num_columns(2).striped(true).spacing([24.0, 6.0]).show(ui, |ui| {
                                    for (k, v) in [
                                        ("⌘ T / ⌘ E", "New SQL query tab"),
                                        ("⌘ ↩", "Run current statement / selection"),
                                        ("⌘ ⇧ ↩", "Run all statements"),
                                        ("⌘ .", "Stop the running query"),
                                        ("⌘ S", "Commit changes / save query"),
                                        ("⌘ R", "Reload table"),
                                        ("⌘ F", "Toggle filters"),
                                        ("⌘ P", "Open anything"),
                                        ("⌘ K", "Switch database"),
                                        ("⌘ I", "Insert row"),
                                        ("⌘ D", "Duplicate row"),
                                        ("⌫", "Delete selected rows"),
                                        ("Space", "Quick look cell"),
                                        ("⌘ 1 / ⌘ 2", "Data / Structure"),
                                        ("⌘ /", "Toggle line comment"),
                                        ("⌘ ⇧ F", "Format SQL"),
                                        ("⌘ ⇧ [ / ]", "Previous / next tab"),
                                        ("⌘ W", "Close tab"),
                                        ("⌘ ⇧ C", "Toggle console log"),
                                        ("⌘ ⇧ R", "Reconnect"),
                                        ("⌘ ,", "Settings"),
                                        ("⌘ L", "Lock Strata (with passcode)"),
                                    ] {
                                        ui.label(RichText::new(k).monospace().color(pal.accent));
                                        ui.label(v);
                                        ui.end_row();
                                    }
                                });
                                ui.label(faint("⌘ is Ctrl on Windows and Linux."));
                            }
                            Section::About => {
                                ui.horizontal(|ui| {
                                    let (r, _) = ui.allocate_exact_size(egui::vec2(44.0, 44.0), egui::Sense::hover());
                                    super::home::paint_logo(ui.painter(), r);
                                    ui.vertical(|ui| {
                                        ui.label(RichText::new("Strata").size(18.0).strong());
                                        ui.label(dim(format!("Version {}", env!("CARGO_PKG_VERSION"))));
                                    });
                                });
                                ui.add_space(8.0);
                                ui.label("A native, cross-platform database workbench written in Rust.");
                                ui.add_space(8.0);
                                ui.horizontal(|ui| {
                                    ui.label(dim("Data folder"));
                                    ui.label(RichText::new(store.root().display().to_string()).monospace().size(11.5));
                                });
                                ui.label(faint("Connections, queries, history and boards live there; passwords live in the system keychain."));
                            }
                        }
                        if let Some((ok, m)) = &self.message {
                            ui.add_space(8.0);
                            ui.label(RichText::new(m).color(if *ok { pal.success } else { pal.danger }));
                        }
                    });
                });
            });
        });
        if *s != before {
            let _ = store.save_settings(s);
        }
        events
    }
}
