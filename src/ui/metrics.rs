//! Metrics boards: a grid of widgets (scoreboard, table, bar, line, pie), each backed by a read-only query.

use super::grid::{self, GridState, ResultSource};
use super::workspace::TabCtx;
use super::{dim, faint, icon_button, p, primary_button, Icon};
use crate::db::{is_read_only, QueryResult};
use crate::model::{MetricWidget, MetricsBoard, WidgetKind};
use crate::runtime::Task;
use crate::store::Store;
use egui::{vec2, Color32, RichText, Stroke};
use std::collections::HashMap;
use std::time::Instant;
use uuid::Uuid;

enum WidgetData {
    Loading(Task<anyhow::Result<QueryResult>>),
    Ready(QueryResult),
    Failed(String),
}

pub struct MetricsTab {
    pub id: u64,
    pub board: Uuid,
    name: String,
    data: HashMap<Uuid, WidgetData>,
    grids: HashMap<Uuid, GridState>,
    editing: Option<MetricWidget>,
    last_refresh: Option<Instant>,
}

fn numeric(s: Option<&str>) -> Option<f64> {
    s.and_then(|v| v.trim().parse::<f64>().ok())
}

impl MetricsTab {
    pub fn new(id: u64, board: Uuid, name: String) -> Self {
        Self { id, board, name, data: HashMap::new(), grids: HashMap::new(), editing: None, last_refresh: None }
    }

    pub fn title(&self) -> String {
        self.name.clone()
    }

    fn refresh(&mut self, board: &MetricsBoard, cx: &TabCtx) {
        self.last_refresh = Some(Instant::now());
        for w in &board.widgets {
            self.run_widget(w, cx);
        }
    }

    fn run_widget(&mut self, w: &MetricWidget, cx: &TabCtx) {
        if w.sql.trim().is_empty() {
            self.data.insert(w.id, WidgetData::Failed("No query yet — edit the widget to add one.".into()));
            return;
        }
        if !is_read_only(&w.sql) {
            self.data.insert(w.id, WidgetData::Failed("Metrics widgets only run read-only queries.".into()));
            return;
        }
        let sql = w.sql.clone();
        self.data.insert(w.id, WidgetData::Loading(cx.session.spawn(cx.ctx, move |d| async move { d.lock().await.query_one(&sql).await })));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut TabCtx, boards: &mut Vec<MetricsBoard>, store: &Store) {
        let pal = p();
        let Some(bi) = boards.iter().position(|b| b.id == self.board) else {
            super::empty_state(ui, Icon::Chart, "Board not found", "It may have been deleted");
            return;
        };
        for v in self.data.values_mut() {
            if let WidgetData::Loading(t) = v {
                if let Some(r) = t.poll() {
                    *v = match r {
                        Ok(q) => WidgetData::Ready(q),
                        Err(e) => WidgetData::Failed(format!("{e:#}")),
                    };
                }
            }
        }
        let board_snapshot = boards[bi].clone();
        let due = match (self.last_refresh, board_snapshot.refresh_secs) {
            (None, _) => true,
            (Some(t), s) if s > 0 => t.elapsed().as_secs() >= s as u64,
            _ => false,
        };
        if due {
            self.refresh(&board_snapshot, cx);
        }
        if board_snapshot.refresh_secs > 0 {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
        }

        let mut changed = false;
        egui::Frame::new().fill(pal.content).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
            ui.horizontal(|ui| {
                let b = &mut boards[bi];
                if ui.add(crate::ui::line_edit(&mut b.name).font(egui::TextStyle::Heading).frame(egui::Frame::NONE).desired_width(260.0)).changed() {
                    self.name = b.name.clone();
                    changed = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if primary_button(ui, "+ Widget").clicked() {
                        self.editing = Some(MetricWidget { id: Uuid::new_v4(), title: "New widget".into(), kind: WidgetKind::Scoreboard, sql: String::new(), wide: false });
                    }
                    if icon_button(ui, Icon::Refresh, "Refresh all").clicked() {
                        self.refresh(&board_snapshot, cx);
                    }
                    egui::ComboBox::from_id_salt(("refresh", self.id)).width(110.0).selected_text(if b.refresh_secs == 0 { "Manual".to_string() } else { format!("Every {}s", b.refresh_secs) }).show_ui(ui, |ui| {
                        for s in [0u32, 5, 15, 30, 60, 300] {
                            if ui.selectable_value(&mut b.refresh_secs, s, if s == 0 { "Manual".to_string() } else { format!("Every {s}s") }).clicked() {
                                changed = true;
                            }
                        }
                    });
                    if let Some(t) = self.last_refresh {
                        ui.label(faint(format!("updated {}s ago", t.elapsed().as_secs())).size(11.0));
                    }
                });
            });
        });

        if board_snapshot.widgets.is_empty() {
            super::empty_state(ui, Icon::Chart, "No widgets yet", "Add a scoreboard, table or chart backed by a query");
        }

        // Pack widgets into rows of three units; wide widgets take two.
        let mut rows: Vec<Vec<&MetricWidget>> = Vec::new();
        let mut used = 0;
        for w in &board_snapshot.widgets {
            let units = if w.wide { 2 } else { 1 };
            if rows.is_empty() || used + units > 3 {
                rows.push(Vec::new());
                used = 0;
            }
            rows.last_mut().expect("row exists").push(w);
            used += units;
        }
        let mut edit = None;
        let mut remove = None;
        let mut rerun = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            egui::Frame::new().inner_margin(egui::Margin::same(12)).show(ui, |ui| {
                let gap = 12.0;
                let unit = (ui.available_width() - gap * 2.0) / 3.0;
                for row in rows {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        for w in row {
                            let width = if w.wide { unit * 2.0 + gap } else { unit };
                            ui.allocate_ui(vec2(width, 260.0), |ui| {
                                super::card_frame().show(ui, |ui| {
                                    ui.set_width(width - 30.0);
                                    ui.set_height(228.0);
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new(&w.title).strong());
                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            if icon_button(ui, Icon::Trash, "Remove widget").clicked() {
                                                remove = Some(w.id);
                                            }
                                            if icon_button(ui, Icon::Edit, "Edit widget").clicked() {
                                                edit = Some(w.clone());
                                            }
                                            if icon_button(ui, Icon::Refresh, "Refresh").clicked() {
                                                rerun = Some(w.clone());
                                            }
                                        });
                                    });
                                    self.widget_body(ui, w, cx);
                                });
                            });
                        }
                    });
                    ui.add_space(gap);
                }
            });
        });
        if let Some(w) = rerun {
            self.run_widget(&w, cx);
        }
        if let Some(w) = edit {
            self.editing = Some(w);
        }
        if let Some(id) = remove {
            boards[bi].widgets.retain(|w| w.id != id);
            changed = true;
        }
        if let Some(saved) = self.editor_ui(ui.ctx()) {
            let b = &mut boards[bi];
            match b.widgets.iter_mut().find(|w| w.id == saved.id) {
                Some(w) => *w = saved.clone(),
                None => b.widgets.push(saved.clone()),
            }
            self.run_widget(&saved, cx);
            changed = true;
        }
        if changed {
            let _ = store.save_boards(boards);
        }
    }

    fn widget_body(&mut self, ui: &mut egui::Ui, w: &MetricWidget, cx: &TabCtx) {
        let pal = p();
        let r = match self.data.get(&w.id) {
            None | Some(WidgetData::Loading(_)) => {
                ui.centered_and_justified(|ui| ui.add(egui::Spinner::new()));
                return;
            }
            Some(WidgetData::Failed(e)) => {
                ui.add(egui::Label::new(RichText::new(e).color(pal.danger).size(12.0)).wrap());
                return;
            }
            Some(WidgetData::Ready(r)) => r,
        };
        if r.rows.is_empty() {
            ui.label(dim("No rows"));
            return;
        }
        match w.kind {
            WidgetKind::Scoreboard => {
                let v = r.cell(0, 0).unwrap_or("—").to_string();
                ui.add_space(30.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(v).size(44.0).strong().color(pal.accent));
                    ui.label(faint(r.columns.first().cloned().unwrap_or_default()));
                    if let Some(sub) = r.cell(0, 1) {
                        ui.label(dim(format!("{} {}", r.columns.get(1).cloned().unwrap_or_default(), sub)));
                    }
                });
            }
            WidgetKind::Table => {
                let gs = self.grids.entry(w.id).or_default();
                let opts = grid::GridOptions { id: egui::Id::unique(("metric-grid", w.id)), editable: false, row_numbers: false, alternating: true, null_text: "NULL", font_size: cx.settings.grid_font_size - 0.5, row_offset: 0, profiles: false };
                let src = ResultSource { meta: grid::result_meta(r), rows: &r.rows, sort: &[] };
                grid::show(ui, &src, gs, &opts);
            }
            WidgetKind::Bar => {
                let bars: Vec<egui_plot::Bar> = r
                    .rows
                    .iter()
                    .enumerate()
                    .filter_map(|(i, row)| {
                        let y = numeric(row.get(1).and_then(|v| v.as_deref())).or_else(|| numeric(row.first().and_then(|v| v.as_deref())))?;
                        let label = row.first().cloned().flatten().unwrap_or_default();
                        Some(egui_plot::Bar::new(i as f64, y).name(label).fill(pal.chart[i % 5]).width(0.7))
                    })
                    .collect();
                let labels: Vec<String> = r.rows.iter().map(|row| row.first().cloned().flatten().unwrap_or_default()).collect();
                egui_plot::Plot::new(("bar", w.id))
                    .allow_drag(false)
                    .allow_zoom(false)
                    .allow_scroll(false)
                    .show_grid([false, true])
                    .x_axis_formatter(move |m, _| labels.get(m.value.round() as usize).filter(|_| (m.value - m.value.round()).abs() < 1e-6).cloned().unwrap_or_default())
                    .show(ui, |pu| pu.bar_chart(egui_plot::BarChart::new(w.title.clone(), bars).color(pal.accent)));
            }
            WidgetKind::Line => {
                let x_is_numeric = r.rows.iter().all(|row| numeric(row.first().and_then(|v| v.as_deref())).is_some());
                let series_cols: Vec<usize> = (if x_is_numeric { 1 } else { 1 }..r.columns.len()).filter(|&c| r.rows.iter().any(|row| numeric(row.get(c).and_then(|v| v.as_deref())).is_some())).collect();
                let labels: Vec<String> = r.rows.iter().map(|row| row.first().cloned().flatten().unwrap_or_default()).collect();
                let rows = r.rows.clone();
                let columns = r.columns.clone();
                egui_plot::Plot::new(("line", w.id))
                    .allow_drag(false)
                    .allow_scroll(false)
                    .legend(egui_plot::Legend::default())
                    .x_axis_formatter(move |m, _| if x_is_numeric { format!("{}", m.value) } else { labels.get(m.value.round() as usize).filter(|_| (m.value - m.value.round()).abs() < 1e-6).cloned().unwrap_or_default() })
                    .show(ui, |pu| {
                        for (si, &c) in series_cols.iter().enumerate() {
                            let pts: Vec<[f64; 2]> = rows
                                .iter()
                                .enumerate()
                                .filter_map(|(i, row)| {
                                    let x = if x_is_numeric { numeric(row.first().and_then(|v| v.as_deref()))? } else { i as f64 };
                                    Some([x, numeric(row.get(c).and_then(|v| v.as_deref()))?])
                                })
                                .collect();
                            pu.line(egui_plot::Line::new(columns[c].clone(), pts).color(pal.chart[si % 5]).width(2.0));
                        }
                    });
            }
            WidgetKind::Pie => pie(ui, r),
        }
    }

    fn editor_ui(&mut self, ctx: &egui::Context) -> Option<MetricWidget> {
        let w = self.editing.as_mut()?;
        let mut open = true;
        let mut saved = None;
        egui::Window::new("Widget").id(egui::Id::unique(("widget-editor", self.id))).open(&mut open).collapsible(false).default_width(520.0).show(ctx, |ui| {
            ui.add(crate::ui::line_edit(&mut w.title).hint_text("Title").desired_width(f32::INFINITY));
            ui.horizontal(|ui| {
                for k in WidgetKind::ALL {
                    ui.selectable_value(&mut w.kind, k, k.label());
                }
            });
            ui.checkbox(&mut w.wide, "Double width");
            ui.label(faint(match w.kind {
                WidgetKind::Scoreboard => "First column of the first row is the big number.",
                WidgetKind::Table => "Shows the result as a table.",
                WidgetKind::Bar | WidgetKind::Pie => "First column is the label, second column the value.",
                WidgetKind::Line => "First column is the x axis; every numeric column after it becomes a line.",
            }));
            ui.add(egui::TextEdit::multiline(&mut w.sql).code_editor().desired_rows(6).desired_width(f32::INFINITY).hint_text("SELECT count(*) AS users FROM users"));
            ui.horizontal(|ui| {
                if primary_button(ui, "Save").clicked() {
                    saved = Some(w.clone());
                }
            });
        });
        if saved.is_some() || !open {
            self.editing = None;
        }
        saved
    }
}

fn pie(ui: &mut egui::Ui, r: &QueryResult) {
    let pal = p();
    let slices: Vec<(String, f64)> = r.rows.iter().filter_map(|row| Some((row.first().cloned().flatten().unwrap_or_default(), numeric(row.get(1).and_then(|v| v.as_deref()))?.max(0.0)))).take(12).collect();
    let total: f64 = slices.iter().map(|s| s.1).sum();
    if total <= 0.0 {
        ui.label(dim("Nothing to chart"));
        return;
    }
    ui.horizontal(|ui| {
        let size = ui.available_height().min(190.0);
        let (rect, _) = ui.allocate_exact_size(vec2(size, size), egui::Sense::hover());
        let c = rect.center();
        let radius = size / 2.0 - 4.0;
        let mut start = -std::f64::consts::FRAC_PI_2;
        for (i, (_, v)) in slices.iter().enumerate() {
            let sweep = v / total * std::f64::consts::TAU;
            let steps = ((sweep / 0.05).ceil() as usize).max(2);
            let mut pts = vec![c];
            for s in 0..=steps {
                let a = start + sweep * s as f64 / steps as f64;
                pts.push(c + vec2(a.cos() as f32, a.sin() as f32) * radius);
            }
            let color = pal.chart[i % 5].gamma_multiply(if i >= 5 { 0.6 } else { 1.0 });
            // Fan of triangles keeps each slice convex for the tessellator.
            for t in 1..pts.len() - 1 {
                ui.painter().add(egui::Shape::convex_polygon(vec![pts[0], pts[t], pts[t + 1]], color, Stroke::NONE));
            }
            start += sweep;
        }
        ui.painter().circle_filled(c, radius * 0.55, pal.surface);
        ui.vertical(|ui| {
            for (i, (label, v)) in slices.iter().enumerate() {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().rect_filled(r, 2.0, pal.chart[i % 5]);
                    ui.label(RichText::new(format!("{label}  {:.1}%", v / total * 100.0)).size(12.0));
                });
            }
        });
    });
    let _ = Color32::TRANSPARENT;
}
