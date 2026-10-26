//! Cell value viewer used by quick look: plain text, a JSON tree, or a geometry map.

use super::{dim, geo, json_tree, segmented};
use egui::Ui;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Text,
    Tree,
    Map,
}

pub struct ValueViewer {
    pub text: String,
    pub mode: ViewMode,
    tree: Option<Value>,
    /// The text was pretty-printed here, so compact it again before writing back.
    pub pretty: bool,
}

impl ValueViewer {
    pub fn new(text: String) -> Self {
        let tree = json_tree::parse_json(&text);
        let mode = if tree.is_some() {
            ViewMode::Tree
        } else if geo::parse(&text).is_some() {
            ViewMode::Map
        } else {
            ViewMode::Text
        };
        Self { text, mode, tree, pretty: false }
    }

    /// The value to store: compact JSON when it was reformatted here.
    pub fn value_for_cell(&self) -> String {
        if let (Some(t), ViewMode::Tree) = (&self.tree, self.mode) {
            return t.to_string();
        }
        if self.pretty {
            if let Ok(v) = serde_json::from_str::<Value>(&self.text) {
                return v.to_string();
            }
        }
        self.text.clone()
    }

    pub fn ui(&mut self, ui: &mut Ui, editable: bool, id: egui::Id) {
        let is_geo = geo::parse(&self.text).is_some();
        let mut modes = vec![(ViewMode::Text, "Text")];
        if self.tree.is_some() {
            modes.push((ViewMode::Tree, "Tree"));
        }
        if is_geo {
            modes.push((ViewMode::Map, "Map"));
        }
        ui.horizontal(|ui| {
            if modes.len() > 1 {
                let mut m = self.mode;
                if segmented(ui, &mut m, &modes) {
                    // Leaving the tree: carry its edits into the text.
                    if self.mode == ViewMode::Tree {
                        if let Some(t) = &self.tree {
                            self.text = serde_json::to_string_pretty(t).unwrap_or_default();
                            self.pretty = true;
                        }
                    }
                    if m == ViewMode::Tree {
                        self.tree = json_tree::parse_json(&self.text);
                    }
                    self.mode = m;
                }
            }
            if self.mode == ViewMode::Text && ui.button("Format JSON").clicked() {
                if let Ok(v) = serde_json::from_str::<Value>(&self.text) {
                    self.text = serde_json::to_string_pretty(&v).unwrap_or_default();
                    self.pretty = true;
                }
            }
            if ui.button("Copy").clicked() {
                ui.ctx().copy_text(self.value_for_cell());
            }
        });
        ui.add_space(6.0);
        match self.mode {
            ViewMode::Tree => {
                egui::ScrollArea::vertical().max_height(440.0).auto_shrink([false, true]).show(ui, |ui| {
                    if let Some(t) = &mut self.tree {
                        if json_tree::show(ui, t, editable, id.with("tree")) {
                            self.text = t.to_string();
                        }
                    }
                });
            }
            ViewMode::Map => match geo::parse(&self.text) {
                Some((srid, g)) => {
                    geo::show(ui, &g, srid, 300.0);
                    ui.label(dim(geo::summary(&g)));
                }
                None => {
                    ui.label(dim("Not a geometry"));
                }
            },
            ViewMode::Text => {
                egui::ScrollArea::vertical().max_height(440.0).show(ui, |ui| {
                    ui.add(egui::TextEdit::multiline(&mut self.text).code_editor().desired_width(f32::INFINITY).desired_rows(16).interactive(editable));
                });
            }
        }
    }
}
