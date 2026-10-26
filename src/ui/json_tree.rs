//! Collapsible JSON tree with in-place editing of keys and values.

use super::{p, paint_icon, Icon};
use egui::{vec2, RichText, Ui};
use serde_json::Value;

/// Parses a cell that holds a JSON object or array.
pub fn parse_json(s: &str) -> Option<Value> {
    let t = s.trim_start();
    if !(t.starts_with('{') || t.starts_with('[')) {
        return None;
    }
    serde_json::from_str(s).ok()
}

/// Turns typed text back into a JSON scalar: numbers, true/false/null stay bare, the rest is a string.
pub fn scalar_from_text(text: &str) -> Value {
    match text.trim() {
        "null" => Value::Null,
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        t => t.parse::<serde_json::Number>().map(Value::Number).unwrap_or_else(|_| Value::String(text.to_string())),
    }
}

fn scalar_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Shows `v` as a tree. Returns true when an edit changed it.
pub fn show(ui: &mut Ui, v: &mut Value, editable: bool, id: egui::Id) -> bool {
    let mut changed = false;
    ui.spacing_mut().item_spacing.y = 2.0;
    node(ui, None, v, editable, id, 0, &mut changed);
    changed
}

/// Renders one node. Returns `true` when the parent should remove it.
fn node(ui: &mut Ui, key: Option<&mut String>, v: &mut Value, editable: bool, id: egui::Id, depth: usize, changed: &mut bool) -> bool {
    let pal = p();
    let mono = egui::FontId::monospace(12.5);
    let mut remove = false;
    let container = matches!(v, Value::Object(_) | Value::Array(_));
    let open_id = id.with("open");
    let mut open = ui.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(depth < 2);
    ui.horizontal(|ui| {
        ui.add_space(depth as f32 * 14.0);
        if container {
            let (r, resp) = ui.allocate_exact_size(vec2(14.0, 18.0), egui::Sense::click());
            paint_icon(ui.painter(), egui::Rect::from_center_size(r.center(), vec2(10.0, 10.0)), if open { Icon::ChevronDown } else { Icon::ChevronRight }, pal.text_faint);
            if resp.clicked() {
                open = !open;
                ui.data_mut(|d| d.insert_temp(open_id, open));
            }
        } else {
            ui.add_space(18.0);
        }
        if let Some(k) = key {
            if editable {
                let w = (k.chars().count() as f32 * 7.6 + 12.0).clamp(30.0, 160.0);
                if ui.add(crate::ui::line_edit(k).font(mono.clone()).text_color(pal.accent2).desired_width(w).frame(egui::Frame::NONE)).changed() {
                    *changed = true;
                }
            } else {
                ui.label(RichText::new(k.as_str()).font(mono.clone()).color(pal.accent2));
            }
            ui.label(RichText::new(":").font(mono.clone()).color(pal.text_faint));
        }
        match &mut *v {
            Value::Object(m) => {
                ui.label(RichText::new(format!("{{ {} key{} }}", m.len(), if m.len() == 1 { "" } else { "s" })).font(mono.clone()).color(pal.text_faint));
            }
            Value::Array(a) => {
                ui.label(RichText::new(format!("[ {} item{} ]", a.len(), if a.len() == 1 { "" } else { "s" })).font(mono.clone()).color(pal.text_faint));
            }
            scalar => {
                let color = match scalar {
                    Value::String(_) => pal.editor_string,
                    Value::Number(_) => pal.editor_number,
                    _ => pal.text_faint,
                };
                if editable {
                    let mut text = scalar_text(scalar);
                    let w = (ui.available_width() - 30.0).max(60.0);
                    if ui.add(crate::ui::line_edit(&mut text).font(mono.clone()).text_color(color).desired_width(w).frame(egui::Frame::NONE)).changed() {
                        // Keep strings as strings while typing, so "12" in a string stays a string.
                        *scalar = if matches!(scalar, Value::String(_)) { Value::String(text) } else { scalar_from_text(&text) };
                        *changed = true;
                    }
                } else {
                    let shown = match scalar {
                        Value::String(s) => format!("\"{s}\""),
                        other => other.to_string(),
                    };
                    ui.label(RichText::new(shown).font(mono.clone()).color(color));
                }
            }
        }
        if editable && depth > 0 && super::icon_button(ui, Icon::Close, "Remove").clicked() {
            remove = true;
        }
    });
    if container && open {
        match v {
            Value::Object(m) => {
                let entries: Vec<(String, Value)> = std::mem::take(m).into_iter().collect();
                let mut kept = serde_json::Map::new();
                for (i, (mut k, mut child)) in entries.into_iter().enumerate() {
                    let before = k.clone();
                    let gone = node(ui, Some(&mut k), &mut child, editable, id.with(("k", i)), depth + 1, changed);
                    if gone {
                        *changed = true;
                        continue;
                    }
                    if k != before && kept.contains_key(&k) {
                        k = before;
                    }
                    kept.insert(k, child);
                }
                *m = kept;
                if editable {
                    ui.horizontal(|ui| {
                        ui.add_space((depth + 1) as f32 * 14.0 + 18.0);
                        if ui.small_button("+ key").clicked() {
                            let mut n = 1;
                            while m.contains_key(&format!("key{n}")) {
                                n += 1;
                            }
                            m.insert(format!("key{n}"), Value::String(String::new()));
                            *changed = true;
                        }
                    });
                }
            }
            Value::Array(a) => {
                let mut i = 0;
                while i < a.len() {
                    let mut label = i.to_string();
                    let gone = node(ui, Some(&mut label), &mut a[i], editable, id.with(("i", i)), depth + 1, changed);
                    if gone {
                        a.remove(i);
                        *changed = true;
                    } else {
                        i += 1;
                    }
                }
                if editable {
                    ui.horizontal(|ui| {
                        ui.add_space((depth + 1) as f32 * 14.0 + 18.0);
                        if ui.small_button("+ item").clicked() {
                            a.push(Value::Null);
                            *changed = true;
                        }
                    });
                }
            }
            _ => {}
        }
    }
    remove
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_round_trip_from_text() {
        assert_eq!(scalar_from_text("42"), Value::from(42));
        assert_eq!(scalar_from_text("4.5"), serde_json::json!(4.5));
        assert_eq!(scalar_from_text("true"), Value::Bool(true));
        assert_eq!(scalar_from_text("null"), Value::Null);
        assert_eq!(scalar_from_text("Lisbon"), Value::String("Lisbon".into()));
    }

    #[test]
    fn only_objects_and_arrays_count_as_json_cells() {
        assert!(parse_json(r#"{"a": 1}"#).is_some());
        assert!(parse_json("[1,2]").is_some());
        assert!(parse_json("42").is_none());
        assert!(parse_json("{not json").is_none());
    }
}
