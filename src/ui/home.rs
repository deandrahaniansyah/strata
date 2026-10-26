//! Connections home: searchable cards grouped into folders, plus the slide-in connection editor.

use super::*;
use crate::model::*;
use crate::runtime::{take, test_connection, Task};
use crate::store::ConnectionFile;
use egui::{vec2, RichText, Stroke};
use uuid::Uuid;

pub enum HomeAction {
    Open(Uuid),
    /// Open a connection and put this SQL in a new query tab.
    OpenWithQuery(Uuid, String),
    /// Connect straight from a pasted URL (saved as a new connection).
    ConnectUrl(ConnectionConfig),
    ImportTablePlus,
    Save { cfg: ConnectionConfig, open: bool },
    Delete(Uuid),
    Duplicate(Uuid),
    CreateGroup(String),
    RenameGroup(Uuid, String),
    DeleteGroup(Uuid),
    MoveToGroup(Uuid, Option<Uuid>),
    ExportConnections,
    ImportConnections,
    OpenSqliteFile,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditorTab {
    General,
    Ssh,
    Advanced,
}

pub struct ConnEditor {
    pub cfg: ConnectionConfig,
    pub is_new: bool,
    tab: EditorTab,
    test: Option<Task<anyhow::Result<String>>>,
    test_result: Option<(bool, String)>,
}

#[derive(Default)]
pub struct Home {
    search: String,
    group: Option<Option<Uuid>>,
    env: Option<Tag>,
    /// Queries per day over the last 24 days, per connection (from history files).
    activity: std::collections::HashMap<Uuid, Vec<u32>>,
    recent: Vec<(Uuid, HistoryEntry)>,
    loaded_at: Option<std::time::Instant>,
    pub editor: Option<ConnEditor>,
    picking_driver: bool,
    driver_filter: Option<u8>,
    import_url: Option<String>,
    new_group: Option<String>,
    renaming_group: Option<(Uuid, String)>,
}

const SWATCHES: [[u8; 3]; 8] = [[91, 140, 255], [54, 201, 138], [242, 177, 64], [242, 92, 100], [185, 140, 255], [43, 179, 163], [255, 138, 61], [120, 128, 140]];

impl Home {
    pub fn begin_connection(&mut self) { self.picking_driver = true; }
    pub fn edit(&mut self, cfg: ConnectionConfig) {
        self.editor = Some(ConnEditor { cfg, is_new: false, tab: EditorTab::General, test: None, test_result: None });
    }

    fn new_connection(&mut self, driver: DriverKind, settings: &Settings) {
        let mut cfg = ConnectionConfig::new(driver);
        cfg.safe_mode = settings.default_safe_mode;
        cfg.group = self.group.flatten();
        self.editor = Some(ConnEditor { cfg, is_new: true, tab: EditorTab::General, test: None, test_result: None });
        self.picking_driver = false;
    }

    fn load_activity(&mut self, conns: &ConnectionFile, store: &crate::store::Store, settings: &Settings) {
        if self.loaded_at.map(|t| t.elapsed().as_secs() < 60).unwrap_or(false) {
            return;
        }
        self.loaded_at = Some(std::time::Instant::now());
        self.activity.clear();
        let mut recent = Vec::new();
        let today = chrono::Utc::now().date_naive();
        for c in &conns.connections {
            let hist = store.load_history(c.id, settings.history_days.min(60));
            let mut days = vec![0u32; 24];
            for e in &hist {
                let age = (today - e.at.date_naive()).num_days();
                if (0..24).contains(&age) {
                    days[23 - age as usize] += 1;
                }
            }
            self.activity.insert(c.id, days);
            if let Some(e) = hist.iter().rev().find(|e| e.ok) {
                recent.push((c.id, e.clone()));
            }
        }
        recent.sort_by(|a, b| b.1.at.cmp(&a.1.at));
        recent.truncate(4);
        self.recent = recent;
    }

    /// Forces history-based activity to reload on the next frame.
    pub fn refresh_activity(&mut self) {
        self.loaded_at = None;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, conns: &ConnectionFile, open: &[Uuid], settings: &Settings, store: &crate::store::Store) -> Vec<HomeAction> {
        let pal = p();
        let mut actions = Vec::new();
        self.load_activity(conns, store, settings);

        if self.editor.is_some() {
            egui::Panel::right("conn-editor")
                .resizable(false)
                .exact_size((ui.available_width() * 0.52).clamp(340.0, 452.0))
                .frame(egui::Frame::new().inner_margin(egui::Margin { left: 0, right: 12, top: 12, bottom: 12 }))
                .show(ui, |ui| {
                    glass_frame().inner_margin(egui::Margin::same(20)).show(ui, |ui| {
                        ui.set_min_size(ui.available_size());
                        self.editor_ui(ui, conns, &mut actions);
                    });
                });
        }

        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
            // Drag the window by the empty top band (the title bar is hidden on macOS).
            let band = egui::Rect::from_min_size(ui.max_rect().min, vec2(ui.max_rect().width(), 40.0));
            let drag = ui.interact(band, egui::Id::unique("home-drag"), egui::Sense::click_and_drag());
            if drag.drag_started() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let side = ((ui.available_width() - 1180.0) / 2.0).clamp(20.0, 120.0);
                let max_w = (ui.available_width() - 2.0 * side).max(0.0);
                egui::Frame::new().inner_margin(egui::Margin { left: side as i8, right: side as i8, top: 44, bottom: 40 }).show(ui, |ui| {
                    ui.set_width(max_w);
                    self.top_ui(ui, conns, &mut actions);
                    if conns.connections.is_empty() {
                        self.welcome_ui(ui);
                        return;
                    }
                    ui.add_space(30.0);
                    self.filters_ui(ui, conns, &mut actions);
                    ui.add_space(12.0);
                    let q = self.search.to_lowercase();
                    let looks_like_url = q.contains("://");
                    let visible: Vec<&ConnectionConfig> = conns
                        .connections
                        .iter()
                        .filter(|c| self.group.map(|g| c.group == g).unwrap_or(true))
                        .filter(|c| self.env.map(|e| c.tag == e).unwrap_or(true))
                        .filter(|c| looks_like_url || q.is_empty() || c.display_name().to_lowercase().contains(&q) || c.host.to_lowercase().contains(&q) || c.database.to_lowercase().contains(&q) || c.driver.label().to_lowercase().contains(&q))
                        .collect();
                    if visible.is_empty() {
                        ui.add_space(30.0);
                        ui.vertical_centered(|ui| {
                            ui.label(title("No connections match", 15.0));
                            ui.label(dim("Try another name, host or environment"));
                        });
                    } else {
                        let mut sorted = visible.clone();
                        sorted.sort_by(|a, b| b.last_used.cmp(&a.last_used).then_with(|| a.display_name().to_lowercase().cmp(&b.display_name().to_lowercase())));
                        self.cards(ui, &sorted, conns, open, &mut actions, "all");
                    }
                    if !self.recent.is_empty() && q.is_empty() {
                        ui.add_space(28.0);
                        ui.label(title("Pick up where you left off", 15.0));
                        ui.add_space(10.0);
                        glass_frame().inner_margin(egui::Margin::same(6)).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            for (id, e) in self.recent.clone() {
                                let Some(c) = conns.connections.iter().find(|c| c.id == id) else { continue };
                                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), egui::Sense::click());
                                if resp.hovered() {
                                    ui.painter().rect_filled(r, egui::CornerRadius::same(14), pal.surface_hover);
                                }
                                let tile = egui::Rect::from_min_size(r.min + vec2(12.0, 11.0), vec2(30.0, 30.0));
                                ui.painter().rect_filled(tile, egui::CornerRadius::same(9), pal.surface);
                                paint_icon(ui.painter(), egui::Rect::from_center_size(tile.center(), vec2(14.0, 14.0)), Icon::Code, pal.text_dim);
                                let painter = ui.painter_at(r.with_max_x(r.right() - 120.0));
                                painter.text(egui::pos2(tile.right() + 12.0, r.top() + 17.0), egui::Align2::LEFT_CENTER, crate::db::sqlgen::ellipsize(&e.sql, 90), egui::FontId::monospace(12.5), pal.text);
                                let env = if c.tag == Tag::None { String::new() } else { format!(" ({})", c.tag.label()) };
                                painter.text(egui::pos2(tile.right() + 12.0, r.top() + 36.0), egui::Align2::LEFT_CENTER, format!("{}{env} · {} · {} ms", c.display_name(), e.database, e.elapsed_ms), egui::FontId::proportional(11.5), pal.text_dim);
                                ui.painter().text(egui::pos2(r.right() - 14.0, r.center().y), egui::Align2::RIGHT_CENTER, relative_time(e.at), egui::FontId::proportional(11.5), pal.text_faint);
                                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                    actions.push(HomeAction::OpenWithQuery(id, e.sql.clone()));
                                }
                            }
                        });
                    }
                });
            });
        });

        self.driver_picker(ui.ctx(), settings);
        self.small_dialogs(ui.ctx(), &mut actions);
        actions
    }

    fn top_ui(&mut self, ui: &mut egui::Ui, conns: &ConnectionFile, actions: &mut Vec<HomeAction>) {
        let pal = p();
        let compact = ui.available_width() < 640.0;
        if compact {
            ui.vertical(|ui| {
                ui.label(title("Where to dig today?", if ui.available_width() < 600.0 { 24.0 } else { 32.0 }));
                ui.add_space(2.0);
                let n = conns.connections.len();
                ui.label(dim(format!("{n} saved connection{}", if n == 1 { "" } else { "s" })));
            });
            ui.add_space(12.0);
            ui.horizontal(|ui| self.home_actions_ui(ui, actions));
        } else {
            ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(title("Where to dig today?", if ui.available_width() < 600.0 { 24.0 } else { 32.0 }));
                ui.add_space(2.0);
                let n = conns.connections.len();
                ui.label(dim(format!("{n} saved connection{}", if n == 1 { "" } else { "s" })));
            });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| self.home_actions_ui(ui, actions));
            });
        }
        ui.add_space(18.0);
        // One field for both: paste a URL to connect, or type to find a saved connection.
        let parsed = self.search.contains("://").then(|| ConnectionConfig::from_url(&self.search));
        let mut connect = false;
        egui::Frame::new()
            .fill(pal.sidebar)
            .stroke(Stroke::new(1.0, pal.border))
            .corner_radius(egui::CornerRadius::same(12))
            .inner_margin(egui::Margin { left: 18, right: 8, top: 8, bottom: 8 })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(18.0, 40.0), egui::Sense::hover());
                    paint_icon(ui.painter(), egui::Rect::from_center_size(r.center(), vec2(17.0, 17.0)), if parsed.is_some() { Icon::Plug } else { Icon::Search }, pal.text_dim);
                    let resp = ui.add(
                        crate::ui::line_edit(&mut self.search)
                            .hint_text(faint("Paste a URL like postgres://… or type a connection name"))
                            .font(egui::FontId::proportional(15.0))
                            .frame(egui::Frame::NONE)
                            .desired_width(ui.available_width() - 120.0),
                    );
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        connect = true;
                    }
                    let b = egui::Button::new(RichText::new("Connect").font(semibold(13.0)).color(pal.on_accent)).fill(pal.accent).corner_radius(egui::CornerRadius::same(14)).min_size(vec2(104.0, 40.0));
                    if ui.add(b).clicked() {
                        connect = true;
                    }
                });
            });
        ui.add_space(6.0);
        match &parsed {
            Some(Err(e)) if !self.search.trim().is_empty() => {
                ui.label(RichText::new(e.to_string()).size(12.0).color(pal.danger));
            }
            Some(Ok(c)) => {
                ui.label(dim(format!("{} · {}. Press Enter to connect; it's saved as a new connection.", c.driver.label(), c.subtitle())));
            }
            _ => {
                ui.label(faint("Strata detects the engine from the URL. Drop a .sqlite or .db file on the window to open it."));
            }
        }
        if connect {
            match parsed {
                Some(Ok(c)) => {
                    actions.push(HomeAction::ConnectUrl(c));
                    self.search.clear();
                }
                Some(Err(_)) => {}
                None => {
                    let q = self.search.to_lowercase();
                    let best = conns.connections.iter().filter(|c| self.env.map(|e| c.tag == e).unwrap_or(true)).find(|c| q.is_empty() || c.display_name().to_lowercase().contains(&q) || c.host.to_lowercase().contains(&q));
                    if let Some(c) = best {
                        actions.push(HomeAction::Open(c.id));
                    }
                }
            }
        }
    }

    fn home_actions_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<HomeAction>) {
        let pal = p();
                if primary_button(ui, "  + New connection  ").clicked() {
                    self.picking_driver = true;
                }
                let more = ui.add(egui::Button::new(RichText::new("More").color(pal.text_dim)).fill(egui::Color32::TRANSPARENT).stroke(Stroke::new(1.0, pal.border)).corner_radius(egui::CornerRadius::same(10)).min_size(vec2(0.0, 32.0)));
                egui::Popup::menu(&more).show(|ui| {
                    ui.set_min_width(240.0);
                    if ui.button("Import from TablePlus…").clicked() {
                        actions.push(HomeAction::ImportTablePlus);
                        ui.close();
                    }
                    if ui.button("Open SQLite file…").clicked() {
                        actions.push(HomeAction::OpenSqliteFile);
                        ui.close();
                    }
                    if ui.button("New group…").clicked() {
                        self.new_group = Some(String::new());
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Import connections…").clicked() {
                        actions.push(HomeAction::ImportConnections);
                        ui.close();
                    }
                    if ui.button("Export connections…").clicked() {
                        actions.push(HomeAction::ExportConnections);
                        ui.close();
                    }
                });

    }

    fn filters_ui(&mut self, ui: &mut egui::Ui, conns: &ConnectionFile, actions: &mut Vec<HomeAction>) {
        ui.horizontal_wrapped(|ui| {
            ui.label(title("Connections", 15.0));
            ui.add_space(10.0);
            let chip = |ui: &mut egui::Ui, selected: bool, label: &str, dot: Option<egui::Color32>, n: usize| -> egui::Response {
                let pal = p();
                let font = egui::FontId::proportional(12.5);
                let g = ui.painter().layout_no_wrap(label.to_string(), font, if selected { pal.text } else { pal.text_dim });
                let ng = ui.painter().layout_no_wrap(n.to_string(), egui::FontId::monospace(11.0), pal.text_faint);
                let dot_w = if dot.is_some() { 14.0 } else { 0.0 };
                let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + ng.size().x + dot_w + 30.0, 30.0), egui::Sense::click());
                ui.painter().rect_filled(r, egui::CornerRadius::same(255), if selected { pal.surface_hover } else if resp.hovered() { pal.glass } else { egui::Color32::TRANSPARENT });
                ui.painter().rect_stroke(r, egui::CornerRadius::same(255), Stroke::new(1.0, if selected { mix(pal.border, pal.text, 0.2) } else { pal.border }), egui::StrokeKind::Inside);
                let mut x = r.left() + 12.0;
                if let Some(c) = dot {
                    ui.painter().circle_filled(egui::pos2(x + 3.5, r.center().y), 3.5, c);
                    x += dot_w;
                }
                let gw = g.size().x;
                ui.painter().galley(egui::pos2(x, r.center().y - g.size().y / 2.0), g, pal.text);
                ui.painter().galley(egui::pos2(x + gw + 6.0, r.center().y - ng.size().y / 2.0), ng, pal.text_faint);
                resp.on_hover_cursor(egui::CursorIcon::PointingHand)
            };
            if chip(ui, self.env.is_none() && self.group.is_none(), "All", None, conns.connections.len()).clicked() {
                self.env = None;
                self.group = None;
            }
            for t in [Tag::Production, Tag::Staging, Tag::Testing, Tag::Development, Tag::Local] {
                let n = conns.connections.iter().filter(|c| c.tag == t).count();
                if n == 0 {
                    continue;
                }
                let label = t.label();
                let label = format!("{}{}", label[..1].to_uppercase(), &label[1..]);
                if chip(ui, self.env == Some(t), &label, Some(crate::theme::rgb(t.color())), n).clicked() {
                    self.env = if self.env == Some(t) { None } else { Some(t) };
                }
            }
            for g in &conns.groups {
                let n = conns.connections.iter().filter(|c| c.group == Some(g.id)).count();
                let r = chip(ui, self.group == Some(Some(g.id)), &g.name, Some(crate::theme::rgb(g.color)), n);
                if r.clicked() {
                    self.group = if self.group == Some(Some(g.id)) { None } else { Some(Some(g.id)) };
                }
                r.context_menu(|ui| {
                    if ui.button("Rename…").clicked() {
                        self.renaming_group = Some((g.id, g.name.clone()));
                        ui.close();
                    }
                    if ui.button("Delete group").clicked() {
                        actions.push(HomeAction::DeleteGroup(g.id));
                        if self.group == Some(Some(g.id)) {
                            self.group = None;
                        }
                        ui.close();
                    }
                });
            }
        });
    }

    fn welcome_ui(&mut self, ui: &mut egui::Ui) {
        let pal = p();
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.18);
            let (r, _) = ui.allocate_exact_size(vec2(64.0, 64.0), egui::Sense::hover());
            paint_logo(ui.painter(), r);
            ui.add_space(14.0);
            ui.label(title("Welcome to Strata", 24.0));
            ui.label(dim("A fast, native workbench for PostgreSQL, MySQL, SQLite, SQL Server, Redis, MongoDB and more."));
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 330.0).max(0.0) / 2.0);
                if primary_button(ui, "  Create a connection  ").clicked() {
                    self.picking_driver = true;
                }
                if secondary_button(ui, "Import from URL").clicked() {
                    self.import_url = Some(String::new());
                }
            });
            ui.add_space(30.0);
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 7.0 * 46.0).max(0.0) / 2.0);
                for d in DriverKind::ALL.iter().take(7) {
                    driver_tile(ui, *d, 38.0).on_hover_text(d.label());
                }
            });
            ui.label(faint(format!("…and {} more drivers", DriverKind::ALL.len() - 7)).size(11.5)).on_hover_text(DriverKind::ALL[7..].iter().map(|d| d.label()).collect::<Vec<_>>().join(", "));
            let _ = pal;
        });
    }

    fn cards(&mut self, ui: &mut egui::Ui, items: &[&ConnectionConfig], conns: &ConnectionFile, open: &[Uuid], actions: &mut Vec<HomeAction>, _salt: &str) {
        let pal = p();
        let gap = 12.0;
        let min_w = 268.0;
        let cols = ((ui.available_width() + gap) / (min_w + gap)).floor().clamp(1.0, 4.0) as usize;
        let card_w = (ui.available_width() - gap * (cols as f32 - 1.0)) / cols as f32;
        for row in items.chunks(cols) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for c in row {
                    let (rect, resp) = ui.allocate_exact_size(vec2(card_w, 132.0), egui::Sense::click());
                    let hovered = resp.hovered();
                    let editing = self.editor.as_ref().map(|e| e.cfg.id == c.id).unwrap_or(false);
                    let env = crate::theme::rgb(c.accent());
                    let painter = ui.painter();
                    painter.rect_filled(rect, egui::CornerRadius::same(12), if hovered || editing { pal.surface_hover } else { pal.sidebar });
                    painter.rect_stroke(rect, egui::CornerRadius::same(12), Stroke::new(1.0, if editing { pal.accent } else if hovered { mix(pal.border, pal.text, 0.15) } else { pal.border }), egui::StrokeKind::Inside);
                    // Environment strip along the top edge.
                    if c.tag != Tag::None {
                        painter.line_segment([rect.left_top() + vec2(18.0, 1.0), rect.right_top() + vec2(-18.0, 1.0)], Stroke::new(2.0, crate::theme::rgb(c.tag.color())));
                    }
                    let tile = egui::Rect::from_min_size(rect.min + vec2(16.0, 18.0), vec2(40.0, 40.0));
                    let brand = crate::theme::rgb(c.driver.brand_color());
                    painter.rect_filled(tile, egui::CornerRadius::same(12), pal.surface);
                    painter.rect_stroke(tile, egui::CornerRadius::same(12), Stroke::new(1.0, pal.glass_stroke), egui::StrokeKind::Inside);
                    if !paint_driver_mark(painter, tile, c.driver, 0.5, mix(brand, pal.text, 0.25)) {
                        painter.text(tile.center(), egui::Align2::CENTER_CENTER, c.driver.badge().to_uppercase(), egui::FontId::monospace(12.0), mix(brand, pal.text, 0.45));
                    }
                    let tx = tile.right() + 12.0;
                    let right_w = 16.0;
                    let clip = ui.painter_at(egui::Rect::from_min_max(egui::pos2(tx, rect.top()), egui::pos2(rect.right() - right_w, rect.bottom())));
                    clip.text(egui::pos2(tx, rect.top() + 28.0), egui::Align2::LEFT_CENTER, c.display_name(), semibold(14.0), pal.text);
                    let sub_clip = ui.painter_at(egui::Rect::from_min_max(egui::pos2(tx, rect.top()), egui::pos2(rect.right() - 16.0, rect.bottom())));
                    sub_clip.text(egui::pos2(tx, rect.top() + 48.0), egui::Align2::LEFT_CENTER, c.subtitle(), egui::FontId::monospace(11.0), pal.text_dim);
                    if c.tag != Tag::None {
                        let tc = crate::theme::rgb(c.tag.color());
                        let g = painter.layout_no_wrap(c.tag.label().to_string(), semibold(11.0), readable_on_tint(tc));
                        let pr = egui::Rect::from_min_size(egui::pos2(rect.right() - 16.0 - g.size().x - 16.0, rect.bottom() - 29.0), vec2(g.size().x + 16.0, 22.0));
                        painter.rect_filled(pr, egui::CornerRadius::same(255), egui::Color32::from_rgba_unmultiplied(tc.r(), tc.g(), tc.b(), 34));
                        painter.galley(pr.center() - g.size() / 2.0, g, pal.text);
                    }
                    // Status line
                    let is_open = open.contains(&c.id);
                    let (dot, status) = if is_open { (pal.success, "connected".to_string()) } else { (pal.text_faint, c.last_used.map(|t| format!("used {}", relative_time(t))).unwrap_or_else(|| "never opened".into())) };
                    let sy = rect.top() + 82.0;
                    painter.circle_filled(egui::pos2(rect.left() + 20.0, sy), 3.5, dot);
                    painter.text(egui::pos2(rect.left() + 30.0, sy), egui::Align2::LEFT_CENTER, status, egui::FontId::proportional(12.0), pal.text_dim);
                    let mut rx = rect.right() - 16.0;
                    if c.ssh.enabled {
                        let g = painter.layout_no_wrap("SSH".into(), egui::FontId::monospace(10.5), pal.text_dim);
                        let pr = egui::Rect::from_min_size(egui::pos2(rx - g.size().x - 12.0, sy - 10.0), vec2(g.size().x + 12.0, 20.0));
                        painter.rect_filled(pr, egui::CornerRadius::same(6), pal.surface_hover);
                        painter.galley(pr.center() - g.size() / 2.0, g, pal.text);
                        rx = pr.left() - 6.0;
                    }
                    if c.safe_mode != SafeMode::Silent || c.read_only {
                        paint_icon(painter, egui::Rect::from_center_size(egui::pos2(rx - 8.0, sy), vec2(13.0, 13.0)), Icon::Shield, pal.success);
                    }
                    // Activity: queries per day over the last 24 days.
                    let days = self.activity.get(&c.id).cloned().unwrap_or_else(|| vec![0; 24]);
                    let max = days.iter().copied().max().unwrap_or(0).max(1) as f32;
                    let area = egui::Rect::from_min_max(egui::pos2(rect.left() + 16.0, rect.bottom() - 30.0), egui::pos2(rect.right() - if c.tag != Tag::None { 112.0 } else { 16.0 }, rect.bottom() - 12.0));
                    let bw = area.width() / days.len() as f32 - 2.0;
                    for (i, n) in days.iter().enumerate() {
                        let h = if *n == 0 { 2.0 } else { (*n as f32 / max * area.height()).max(3.0) };
                        let x = area.left() + i as f32 * (bw + 2.0);
                        let col = if *n == 0 { pal.surface_hover } else if i == days.len() - 1 { env } else { pal.text_faint.gamma_multiply(0.7) };
                        painter.rect_filled(egui::Rect::from_min_max(egui::pos2(x, area.bottom() - h), egui::pos2(x + bw, area.bottom())), egui::CornerRadius::same(1), col);
                    }
                    let resp = resp.on_hover_text(format!("{} · {} queries in the last 24 days", c.driver.label(), days.iter().sum::<u32>()));
                    let menu = |ui: &mut egui::Ui, actions: &mut Vec<HomeAction>, home: &mut Home| {
                        ui.set_min_width(200.0);
                        if ui.button("Connect").clicked() {
                            actions.push(HomeAction::Open(c.id));
                            ui.close();
                        }
                        if ui.button("Edit…").clicked() {
                            home.edit((*c).clone());
                            ui.close();
                        }
                        if ui.button("Duplicate").clicked() {
                            actions.push(HomeAction::Duplicate(c.id));
                            ui.close();
                        }
                        if ui.button("Copy connection URL").clicked() {
                            ui.ctx().copy_text(c.to_url(false));
                            ui.close();
                        }
                        if !conns.groups.is_empty() {
                            ui.menu_button("Move to group", |ui| {
                                if ui.button("Ungrouped").clicked() {
                                    actions.push(HomeAction::MoveToGroup(c.id, None));
                                    ui.close();
                                }
                                for g in &conns.groups {
                                    if ui.button(&g.name).clicked() {
                                        actions.push(HomeAction::MoveToGroup(c.id, Some(g.id)));
                                        ui.close();
                                    }
                                }
                            });
                        }
                        ui.separator();
                        if ui.button(RichText::new("Delete").color(p().danger)).clicked() {
                            actions.push(HomeAction::Delete(c.id));
                            ui.close();
                        }
                    };
                    if resp.clicked() {
                        actions.push(HomeAction::Open(c.id));
                    }
                    resp.context_menu(|ui| menu(ui, actions, self));
                    if hovered {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                }
            });
            ui.add_space(gap);
        }
    }

    fn driver_picker(&mut self, ctx: &egui::Context, settings: &Settings) {
        if !self.picking_driver {
            return;
        }
        let pal = p();
        let mut chosen = None;
        let resp = egui::Modal::new(egui::Id::unique("driver-picker")).frame(egui::Frame::window(&ctx.global_style()).inner_margin(egui::Margin::same(20))).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 64.0).clamp(320.0, 640.0));
            ui.label(title("Choose a database", 17.0));
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                for (value, label) in [(None, "All engines"), (Some(0), "OLTP / SQL"), (Some(1), "OLAP / warehouses"), (Some(2), "Operational / NoSQL")] {
                    if ui.selectable_label(self.driver_filter == value, label).clicked() { self.driver_filter = value; }
                }
            });
            ui.add_space(8.0);
            let engines: Vec<_> = DriverKind::ALL.into_iter().filter(|driver| match self.driver_filter {
                None => true, Some(0) => driver.workload().starts_with("OLTP"), Some(1) => driver.workload().starts_with("OLAP"), Some(2) => driver.workload().starts_with("Operational"), _ => false,
            }).collect();
            egui::ScrollArea::vertical().max_height((ctx.content_rect().height() - 190.0).max(180.0)).show(ui, |ui| {
                let columns = if ui.available_width() < 500.0 { 2 } else { 4 };
                let width = (ui.available_width() - (columns - 1) as f32 * 8.0) / columns as f32;
                for row in engines.chunks(columns) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        for driver in row {
                            let (rect, response) = ui.allocate_exact_size(vec2(width, 108.0), egui::Sense::click());
                            ui.painter().rect_filled(rect, egui::CornerRadius::same(12), if response.hovered() { pal.surface_hover } else { pal.surface });
                            ui.painter().rect_stroke(rect, egui::CornerRadius::same(12), Stroke::new(1.0, if response.hovered() { pal.accent } else { pal.border }), egui::StrokeKind::Inside);
                            paint_driver_badge(ui.painter(), egui::Rect::from_center_size(rect.center() - vec2(0.0, 19.0), vec2(36.0,36.0)), *driver);
                            ui.painter().text(rect.center_bottom() - vec2(0.0, 32.0), egui::Align2::CENTER_CENTER, driver.label(), egui::FontId::proportional(12.0), pal.text);
                            ui.painter().text(rect.center_bottom() - vec2(0.0, 14.0), egui::Align2::CENTER_CENTER, if driver.is_odbc() { "Vendor ODBC" } else { "Built-in driver" }, egui::FontId::proportional(11.0), pal.text_dim);
                            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, driver.label()));
                            if response.on_hover_text(driver.workload()).clicked() { chosen = Some(*driver); }
                        }
                    });
                    ui.add_space(8.0);
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if secondary_button(ui, "Cancel").clicked() {
                        self.picking_driver = false;
                    }
                });
            });
        });
        if resp.should_close() {
            self.picking_driver = false;
        }
        if let Some(d) = chosen {
            self.new_connection(d, settings);
        }
    }

    fn small_dialogs(&mut self, ctx: &egui::Context, actions: &mut Vec<HomeAction>) {
        let pal = p();
        if let Some(url) = &mut self.import_url {
            let mut close = false;
            let mut parsed = None;
            let r = egui::Modal::new(egui::Id::unique("import-url")).show(ctx, |ui| {
                ui.set_width(520.0);
                ui.label(RichText::new("Import from URL").size(16.0).strong());
                ui.label(dim("postgresql://user:password@host:5432/db?sslmode=require&name=Production"));
                ui.add(crate::ui::line_edit(url).desired_width(f32::INFINITY).hint_text("Paste a connection URL")).request_focus();
                let res = ConnectionConfig::from_url(url);
                if let (Err(e), false) = (&res, url.trim().is_empty()) {
                    ui.label(RichText::new(e.to_string()).color(pal.danger).size(12.0));
                }
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if primary_button(ui, "Import").clicked() {
                            parsed = res.ok();
                        }
                        if secondary_button(ui, "Cancel").clicked() {
                            close = true;
                        }
                    });
                });
            });
            if let Some(cfg) = parsed {
                self.editor = Some(ConnEditor { cfg, is_new: true, tab: EditorTab::General, test: None, test_result: None });
                close = true;
            }
            if close || r.should_close() {
                self.import_url = None;
            }
        }
        if let Some(name) = &mut self.new_group {
            let mut close = false;
            let r = egui::Modal::new(egui::Id::unique("new-group")).show(ctx, |ui| {
                ui.set_width(360.0);
                ui.label(RichText::new("New group").size(16.0).strong());
                ui.add(crate::ui::line_edit(name).hint_text("e.g. Client A, Staging").desired_width(f32::INFINITY)).request_focus();
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if primary_button(ui, "Create").clicked() && !name.trim().is_empty() {
                            actions.push(HomeAction::CreateGroup(name.trim().to_string()));
                            close = true;
                        }
                        if secondary_button(ui, "Cancel").clicked() {
                            close = true;
                        }
                    });
                });
            });
            if close || r.should_close() {
                self.new_group = None;
            }
        }
        if let Some((id, name)) = &mut self.renaming_group {
            let mut close = false;
            let r = egui::Modal::new(egui::Id::unique("rename-group")).show(ctx, |ui| {
                ui.set_width(360.0);
                ui.label(RichText::new("Rename group").size(16.0).strong());
                ui.add(crate::ui::line_edit(name).desired_width(f32::INFINITY));
                ui.horizontal(|ui| {
                    if primary_button(ui, "Save").clicked() && !name.trim().is_empty() {
                        actions.push(HomeAction::RenameGroup(*id, name.trim().to_string()));
                        close = true;
                    }
                    if secondary_button(ui, "Cancel").clicked() {
                        close = true;
                    }
                });
            });
            if close || r.should_close() {
                self.renaming_group = None;
            }
        }
    }

    fn editor_ui(&mut self, ui: &mut egui::Ui, conns: &ConnectionFile, actions: &mut Vec<HomeAction>) {
        let pal = p();
        let Some(ed) = &mut self.editor else { return };
        if let Some(r) = take(&mut ed.test) {
            ed.test_result = Some(match r {
                Ok(m) => (true, m),
                Err(e) => (false, format!("{e:#}")),
            });
        }
        let mut close = false;
        ui.horizontal(|ui| {
            driver_tile(ui, ed.cfg.driver, 34.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(if ed.is_new { "New connection" } else { "Edit connection" }).size(15.0).strong());
                ui.label(faint(ed.cfg.driver.label()).size(11.5));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, Icon::Close, "Close").clicked() {
                    close = true;
                }
            });
        });
        ui.add_space(12.0);
        let mut tab = ed.tab;
        let mut tabs = vec![(EditorTab::General, "General")];
        if ed.cfg.driver.supports_ssh() {
            tabs.push((EditorTab::Ssh, "SSH"));
        }
        tabs.push((EditorTab::Advanced, "Advanced"));
        segmented(ui, &mut tab, &tabs);
        ed.tab = tab;
        ui.add_space(12.0);

        let field = |ui: &mut egui::Ui, label: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
            ui.label(RichText::new(label).size(11.5).color(pal.text_dim));
            add(ui);
            ui.add_space(8.0);
        };
        let footer_h = 96.0;
        egui::ScrollArea::vertical().max_height(ui.available_height() - footer_h).auto_shrink([false, true]).show(ui, |ui| {
            let c = &mut ed.cfg;
            let full = f32::INFINITY;
            match ed.tab {
                EditorTab::General => {
                    field(ui, "Name", &mut |ui| {
                        let hint = c.display_name();
                        ui.add(crate::ui::line_edit(&mut c.name).hint_text(hint).desired_width(full));
                    });
                    field(ui, "Tag", &mut |ui| {
                        ui.horizontal_wrapped(|ui| {
                            for t in Tag::ALL {
                                let col = crate::theme::rgb(t.color());
                                let selected = c.tag == t;
                                let b = egui::Button::new(RichText::new(t.label()).size(11.5).color(if selected { col } else { pal.text_dim }))
                                    .fill(if selected { col.gamma_multiply(0.18) } else { egui::Color32::TRANSPARENT })
                                    .stroke(Stroke::new(1.0, if selected { col } else { pal.border }))
                                    .corner_radius(egui::CornerRadius::same(10));
                                if ui.add(b).clicked() {
                                    c.tag = t;
                                }
                            }
                        });
                    });
                    field(ui, "Color", &mut |ui| {
                        ui.horizontal(|ui| {
                            let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), egui::Sense::click());
                            ui.painter().circle_stroke(r.center(), 9.0, Stroke::new(if c.color.is_none() { 2.0 } else { 1.0 }, if c.color.is_none() { pal.text } else { pal.border }));
                            ui.painter().line_segment([r.center() - vec2(5.0, -5.0), r.center() + vec2(5.0, -5.0)], Stroke::new(1.0, pal.text_faint));
                            if resp.on_hover_text("Use tag / driver color").clicked() {
                                c.color = None;
                            }
                            for s in SWATCHES {
                                let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), egui::Sense::click());
                                ui.painter().circle_filled(r.center(), 9.0, crate::theme::rgb(s));
                                if c.color == Some(s) {
                                    ui.painter().circle_stroke(r.center(), 11.0, Stroke::new(2.0, pal.text));
                                }
                                if resp.clicked() {
                                    c.color = Some(s);
                                }
                            }
                        });
                    });
                    if !conns.groups.is_empty() {
                        field(ui, "Group", &mut |ui| {
                            let current = c.group.and_then(|g| conns.groups.iter().find(|x| x.id == g)).map(|g| g.name.clone()).unwrap_or_else(|| "Ungrouped".into());
                            egui::ComboBox::from_id_salt("conn-group").width(200.0).selected_text(current).show_ui(ui, |ui| {
                                ui.selectable_value(&mut c.group, None, "Ungrouped");
                                for g in &conns.groups {
                                    ui.selectable_value(&mut c.group, Some(g.id), &g.name);
                                }
                            });
                        });
                    }
                    match c.driver {
                        driver if driver.is_odbc() => {
                            ui.label(dim(format!("{} through an installed vendor ODBC driver", driver.workload())));
                            field(ui, "ODBC data source name (DSN)", &mut |ui| { ui.add(crate::ui::line_edit(&mut c.odbc_dsn).desired_width(full)); });
                            field(ui, "User", &mut |ui| { ui.add(crate::ui::line_edit(&mut c.user).desired_width(full)); });
                            field(ui, "Password / token", &mut |ui| { ui.add(crate::ui::line_edit(&mut c.password).password(true).desired_width(full)); });
                            field(ui, "Catalog filter (optional)", &mut |ui| { ui.add(crate::ui::line_edit(&mut c.database).desired_width(full)); });
                            ui.label(dim("Configure the DSN in your ODBC administrator with the server, TLS and authentication settings. Passwords entered here are stored in the OS keychain."));
                        }
                        DriverKind::Sqlite => field(ui, "Database file", &mut |ui| {
                            ui.horizontal(|ui| {
                                ui.add(crate::ui::line_edit(&mut c.path).hint_text("/path/to/database.sqlite").desired_width(ui.available_width() - 80.0));
                                if ui.button("Browse…").clicked() {
                                    if let Some(p) = rfd::FileDialog::new().add_filter("SQLite", &["sqlite", "sqlite3", "db", "db3"]).pick_file() {
                                        c.path = p.display().to_string();
                                    }
                                }
                            });
                        }),
                        DriverKind::LibSql => {
                            field(ui, "Database URL", &mut |ui| {
                                ui.add(crate::ui::line_edit(&mut c.host).hint_text("libsql://my-db-org.turso.io").desired_width(full));
                            });
                            field(ui, "Auth token", &mut |ui| {
                                ui.add(crate::ui::line_edit(&mut c.password).password(true).desired_width(full));
                            });
                        }
                        DriverKind::CloudflareD1 => {
                            field(ui, "Account ID", &mut |ui| {
                                ui.add(crate::ui::line_edit(&mut c.host).desired_width(full));
                            });
                            field(ui, "Database ID", &mut |ui| {
                                ui.add(crate::ui::line_edit(&mut c.database).desired_width(full));
                            });
                            field(ui, "API token", &mut |ui| {
                                ui.add(crate::ui::line_edit(&mut c.password).password(true).desired_width(full));
                            });
                        }
                        _ => {
                            let uri = c.mongo_uri();
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(if c.driver == DriverKind::MongoDb { "Host or mongodb:// URI" } else { "Host" }).size(11.5).color(pal.text_dim));
                                    ui.add(crate::ui::line_edit(&mut c.host).desired_width(if uri { full } else { 250.0 }));
                                });
                                if !uri {
                                    ui.vertical(|ui| {
                                        ui.label(RichText::new("Port").size(11.5).color(pal.text_dim));
                                        ui.add(egui::DragValue::new(&mut c.port).range(1..=65535));
                                    });
                                }
                            });
                            ui.add_space(8.0);
                            if uri {
                                ui.label(faint("Port, user, password and TLS are read from the URI.").size(11.5));
                                ui.add_space(8.0);
                            } else {
                                let user_label = match c.driver {
                                    DriverKind::Redis => "User (ACL, optional)",
                                    DriverKind::MongoDb | DriverKind::Elasticsearch => "User (optional)",
                                    _ => "User",
                                };
                                field(ui, user_label, &mut |ui| {
                                    ui.add(crate::ui::line_edit(&mut c.user).desired_width(full));
                                });
                                field(ui, "Password", &mut |ui| {
                                    ui.add(crate::ui::line_edit(&mut c.password).password(true).desired_width(full).hint_text("Stored in your system keychain"));
                                });
                            }
                            let (db_label, db_hint) = match c.driver {
                                DriverKind::Redis => ("Database index", "0"),
                                DriverKind::Elasticsearch => ("", ""),
                                DriverKind::ClickHouse => ("Database", "default"),
                                DriverKind::MongoDb => ("Database", if uri { "from the URI, or test" } else { "test" }),
                                _ => ("Database", "optional"),
                            };
                            if !db_label.is_empty() {
                                field(ui, db_label, &mut |ui| {
                                    ui.add(crate::ui::line_edit(&mut c.database).desired_width(full).hint_text(db_hint));
                                });
                            }
                            if matches!(c.driver, DriverKind::ClickHouse | DriverKind::Elasticsearch) {
                                ui.label(faint("Connects over HTTP. With SSL on, Strata uses HTTPS on the same port.").size(11.5));
                                ui.add_space(6.0);
                            }
                            if !uri {
                                field(ui, "SSL", &mut |ui| {
                                    ui.horizontal(|ui| {
                                        for m in SslMode::ALL {
                                            ui.selectable_value(&mut c.ssl, m, m.label());
                                        }
                                    });
                                });
                            }
                        }
                    }
                }
                EditorTab::Ssh => {
                    ui.checkbox(&mut c.ssh.enabled, "Connect over an SSH tunnel");
                    ui.add_space(8.0);
                    ui.add_enabled_ui(c.ssh.enabled, |ui| {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(RichText::new("SSH host").size(11.5).color(pal.text_dim));
                                ui.add(crate::ui::line_edit(&mut c.ssh.host).desired_width(250.0));
                            });
                            ui.vertical(|ui| {
                                ui.label(RichText::new("Port").size(11.5).color(pal.text_dim));
                                ui.add(egui::DragValue::new(&mut c.ssh.port).range(1..=65535));
                            });
                        });
                        ui.add_space(8.0);
                        field(ui, "SSH user", &mut |ui| {
                            ui.add(crate::ui::line_edit(&mut c.ssh.user).desired_width(full));
                        });
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut c.ssh.use_key, false, "Password");
                            ui.selectable_value(&mut c.ssh.use_key, true, "Private key");
                        });
                        ui.add_space(6.0);
                        if c.ssh.use_key {
                            field(ui, "Private key", &mut |ui| {
                                ui.horizontal(|ui| {
                                    ui.add(crate::ui::line_edit(&mut c.ssh.key_path).hint_text("~/.ssh/id_ed25519").desired_width(ui.available_width() - 80.0));
                                    if ui.button("Browse…").clicked() {
                                        let start = dirs::home_dir().map(|h| h.join(".ssh")).unwrap_or_default();
                                        if let Some(p) = rfd::FileDialog::new().set_directory(start).pick_file() {
                                            c.ssh.key_path = p.display().to_string();
                                        }
                                    }
                                });
                            });
                            field(ui, "Key passphrase", &mut |ui| {
                                ui.add(crate::ui::line_edit(&mut c.ssh.key_passphrase).password(true).desired_width(full).hint_text("if the key is encrypted"));
                            });
                        } else {
                            field(ui, "SSH password", &mut |ui| {
                                ui.add(crate::ui::line_edit(&mut c.ssh.password).password(true).desired_width(full));
                            });
                        }
                        ui.label(faint("The database host and port are resolved from the SSH server.").size(11.0));
                    });
                }
                EditorTab::Advanced => {
                    field(ui, "Safe mode", &mut |ui| {
                        for m in SafeMode::ALL {
                            ui.radio_value(&mut c.safe_mode, m, m.label());
                        }
                    });
                    ui.checkbox(&mut c.read_only, "Read-only (block any statement that changes data)");
                    ui.checkbox(&mut c.keep_alive, "Keep connection alive (ping every 30 s)");
                    ui.add_space(8.0);
                    field(ui, "Startup query (runs after connecting)", &mut |ui| {
                        ui.add(egui::TextEdit::multiline(&mut c.startup_query).code_editor().desired_rows(3).desired_width(full).hint_text("SET search_path TO app, public;"));
                    });
                }
            }
        });

        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.horizontal(|ui| {
                let problem = ed.cfg.form_problem();
                let can_save = problem.is_none();
                if ui.add_enabled(can_save, egui::Button::new(RichText::new("Connect").strong().color(pal.on_accent)).fill(pal.accent).corner_radius(egui::CornerRadius::same(8)).min_size(vec2(90.0, 30.0))).clicked() {
                    actions.push(HomeAction::Save { cfg: ed.cfg.clone(), open: true });
                    close = true;
                }
                if ui.add_enabled(can_save, egui::Button::new("Save").corner_radius(egui::CornerRadius::same(8)).min_size(vec2(70.0, 30.0))).clicked() {
                    actions.push(HomeAction::Save { cfg: ed.cfg.clone(), open: false });
                    close = true;
                }
                if ed.test.is_some() {
                    ui.add(egui::Spinner::new());
                } else if ui.add_enabled(can_save, egui::Button::new("Test").corner_radius(egui::CornerRadius::same(8)).min_size(vec2(60.0, 30.0))).clicked() {
                    ed.test_result = None;
                    let cfg = ed.cfg.clone();
                    ed.test = Some(Task::spawn(ui.ctx(), test_connection(cfg)));
                }
            });
            if let (Some(problem), None) = (ed.cfg.form_problem(), &ed.test_result) {
                ui.label(faint(problem).size(12.0));
            }
            if let Some((ok, msg)) = &ed.test_result {
                ui.add(egui::Label::new(RichText::new(msg).size(12.0).color(if *ok { pal.success } else { pal.danger })).wrap());
            }
        });
        if close {
            self.editor = None;
        }
    }
}

/// "3 min ago", "yesterday", "12 Oct".
pub fn relative_time(t: chrono::DateTime<chrono::Utc>) -> String {
    let secs = (chrono::Utc::now() - t).num_seconds().max(0);
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        86_400..=172_799 => "yesterday".into(),
        172_800..=604_799 => format!("{} days ago", secs / 86_400),
        _ => t.with_timezone(&chrono::Local).format("%-d %b").to_string(),
    }
}

/// Strata's mark: three offset strata layers.
pub fn paint_logo(painter: &egui::Painter, r: egui::Rect) {
    let pal = p();
    let s = r.width();
    let c = r.center();
    let u = s / 24.0;
    let pt = |x: f32, y: f32| egui::pos2(c.x + (x - 12.0) * u, c.y + (y - 12.0) * u);
    painter.add(egui::Shape::convex_polygon(vec![pt(3.0, 7.0), pt(12.0, 3.0), pt(21.0, 7.0), pt(12.0, 11.0)], pal.accent, Stroke::NONE));
    painter.add(egui::Shape::line(vec![pt(3.0, 12.0), pt(12.0, 16.0), pt(21.0, 12.0)], Stroke::new(2.2 * u, pal.accent)));
    painter.add(egui::Shape::line(vec![pt(3.0, 17.0), pt(12.0, 21.0), pt(21.0, 17.0)], Stroke::new(2.2 * u, pal.accent2)));
}
