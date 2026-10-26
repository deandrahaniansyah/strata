//! Structured object creation, with validated SQL preview before sending anything.
use super::*;
use crate::db::{objects::{self, ObjectDraft}, DbObject, ObjectKind};
use crate::model::Dialect;

pub fn fields(ui: &mut egui::Ui, draft: &mut ObjectDraft, d: Dialect, objects: &[DbObject]) {
    input(ui, "Name", &mut draft.name);
    if draft.kind != ObjectKind::Extension || d == Dialect::Postgres { input(ui, "Schema", &mut draft.schema); }
    if matches!(draft.kind, ObjectKind::Index | ObjectKind::Trigger | ObjectKind::Constraint) {
        ui.label("Table");
        egui::ComboBox::from_id_salt("object-table").width(ui.available_width()).selected_text(if draft.table.is_empty() { "Choose a table" } else { &draft.table }).show_ui(ui, |ui| {
            for object in objects.iter().filter(|o| o.kind == ObjectKind::Table && (draft.schema.is_empty() || o.schema == draft.schema)) { ui.selectable_value(&mut draft.table, object.name.clone(), &object.name); }
        });
    }
    match draft.kind {
        ObjectKind::Index => { input(ui, "Columns (comma-separated)", &mut draft.columns); ui.checkbox(&mut draft.unique, "Unique index"); }
        ObjectKind::Enum => { body(ui, "Values (one per line)", &mut draft.body); }
        ObjectKind::Type => {
            choice(ui, "Type", &mut draft.type_kind, &["DOMAIN", "COMPOSITE"]);
            if draft.type_kind == "DOMAIN" { input(ui, "Base SQL type", &mut draft.base_type); body(ui, "Optional CHECK expression (use VALUE for the domain value)", &mut draft.body); }
            else { body(ui, "Attributes (attribute_name: SQL type, one per line)", &mut draft.body); }
        }
        ObjectKind::View | ObjectKind::MaterializedView => {
            if draft.kind == ObjectKind::MaterializedView && d == Dialect::ClickHouse { input(ui, "Destination database", &mut draft.ref_schema); input(ui, "Destination table", &mut draft.ref_table); }
            body(ui, "SELECT statement", &mut draft.body);
        }
        ObjectKind::Sequence => { ui.horizontal(|ui| { ui.label("Start"); ui.add(egui::DragValue::new(&mut draft.start)); ui.label("Increment"); ui.add(egui::DragValue::new(&mut draft.increment)); }); }
        ObjectKind::Function | ObjectKind::Procedure => {
            input(ui, "Parameters (SQL declarations)", &mut draft.parameters);
            if draft.kind == ObjectKind::Function { input(ui, "Return SQL type", &mut draft.returns); }
            if d == Dialect::Postgres { choice(ui, "Language", &mut draft.language, &["sql", "plpgsql"]); }
            if d == Dialect::MySql { choice(ui, "Data access", &mut draft.routine_access, &["CONTAINS SQL", "NO SQL", "READS SQL DATA", "MODIFIES SQL DATA"]); }
            body(ui, "Routine body", &mut draft.body);
            ui.label(dim("Enter the complete body, including BEGIN / END when the engine requires it."));
        }
        ObjectKind::Trigger => {
            choice(ui, "Timing", &mut draft.timing, if d == Dialect::MySql { &["BEFORE", "AFTER"] } else if d == Dialect::SqlServer { &["AFTER", "INSTEAD OF"] } else { &["BEFORE", "AFTER", "INSTEAD OF"] });
            choice(ui, "Event", &mut draft.event, &["INSERT", "UPDATE", "DELETE"]);
            if d == Dialect::Postgres { input(ui, "Trigger function (same schema, no arguments)", &mut draft.function); }
            else { body(ui, "Trigger body", &mut draft.body); }
        }
        ObjectKind::Event => {
            ui.horizontal(|ui| { ui.label("Run every"); ui.add(egui::DragValue::new(&mut draft.interval).range(1..=u32::MAX)); });
            choice(ui, "Interval unit", &mut draft.unit, &["SECOND", "MINUTE", "HOUR", "DAY", "WEEK", "MONTH", "YEAR"]);
            body(ui, "Event SQL body", &mut draft.body);
            ui.label(dim("Created disabled. Enable it from the event's maintenance menu when ready. The server's event scheduler must also be enabled."));
        }
        ObjectKind::Constraint => {
            choice(ui, "Constraint", &mut draft.constraint, &["FOREIGN KEY", "CHECK", "UNIQUE", "PRIMARY KEY"]);
            if draft.constraint == "CHECK" { body(ui, "CHECK expression", &mut draft.body); }
            else { input(ui, "Columns (comma-separated)", &mut draft.columns); }
            if draft.constraint == "FOREIGN KEY" {
                input(ui, "Referenced schema", &mut draft.ref_schema);
                input(ui, "Referenced table", &mut draft.ref_table);
                input(ui, "Referenced columns (comma-separated)", &mut draft.ref_columns);
                choice(ui, "On delete", &mut draft.on_delete, &["NO ACTION", "RESTRICT", "CASCADE", "SET NULL", "SET DEFAULT"]);
            }
        }
        ObjectKind::Extension => { ui.label(dim("The extension must be installed on the server. Creating it may require administrator privileges.")); }
        _ => {}
    }
    ui.add_space(12.0);
    match draft.sql(d) {
        Ok(sql) => { ui.collapsing("SQL preview", |ui| { ui.label(egui::RichText::new(sql).monospace()); }); }
        Err(error) => { ui.label(egui::RichText::new(error.to_string()).color(p().warning)); }
    }
}
pub fn title(draft: &ObjectDraft) -> String { format!("Create {}", objects::label(draft.kind).to_lowercase()) }
fn input(ui: &mut egui::Ui, label: &str, value: &mut String) { ui.label(label); ui.add(crate::ui::line_edit(value).desired_width(f32::INFINITY)); }
fn body(ui: &mut egui::Ui, label: &str, value: &mut String) { ui.label(label); ui.add(egui::TextEdit::multiline(value).code_editor().desired_width(f32::INFINITY).desired_rows(6)); }
fn choice(ui: &mut egui::Ui, label: &str, value: &mut String, options: &[&str]) {
    ui.label(label);
    egui::ComboBox::from_id_salt(label).selected_text(value.as_str()).show_ui(ui, |ui| { for option in options { ui.selectable_value(value, (*option).to_string(), *option); } });
}
