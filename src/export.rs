//! Export (CSV, JSON, SQL, Markdown, Excel) and import (CSV, JSON) of tabular data.

use crate::db::{sqlgen, TableRef};
use crate::model::Dialect;
use crate::ui::grid::CopyFormat;
use anyhow::{Context, Result};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
    Sql,
    Markdown,
    Excel,
}

impl ExportFormat {
    pub const ALL: [ExportFormat; 5] = [ExportFormat::Csv, ExportFormat::Json, ExportFormat::Sql, ExportFormat::Markdown, ExportFormat::Excel];

    pub fn label(self) -> &'static str {
        match self {
            ExportFormat::Csv => "CSV",
            ExportFormat::Json => "JSON",
            ExportFormat::Sql => "SQL",
            ExportFormat::Markdown => "Markdown",
            ExportFormat::Excel => "Excel",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Csv => "csv",
            ExportFormat::Json => "json",
            ExportFormat::Sql => "sql",
            ExportFormat::Markdown => "md",
            ExportFormat::Excel => "xlsx",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub format: ExportFormat,
    pub delimiter: char,
    pub header: bool,
    pub null_as_empty: bool,
    pub newline_to_space: bool,
    pub sql_batch: usize,
    pub include_create: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self { format: ExportFormat::Csv, delimiter: ',', header: true, null_as_empty: true, newline_to_space: false, sql_batch: 500, include_create: false }
    }
}

type Rows = [Vec<Option<String>>];

fn csv_text(names: &[String], rows: &Rows, delim: u8, header: bool, null: &str, flatten: bool) -> Result<String> {
    let mut w = csv::WriterBuilder::new().delimiter(delim).from_writer(Vec::new());
    if header {
        w.write_record(names)?;
    }
    for r in rows {
        w.write_record(r.iter().map(|v| {
            let s = v.as_deref().unwrap_or(null);
            if flatten { s.replace(['\r', '\n'], " ") } else { s.to_string() }
        }))?;
    }
    Ok(String::from_utf8(w.into_inner()?)?)
}

fn json_text(names: &[String], rows: &Rows) -> String {
    let arr: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let mut m = serde_json::Map::new();
            for (i, n) in names.iter().enumerate() {
                let v = r.get(i).cloned().flatten();
                m.insert(n.clone(), v.map(serde_json::Value::String).unwrap_or(serde_json::Value::Null));
            }
            serde_json::Value::Object(m)
        })
        .collect();
    serde_json::to_string_pretty(&arr).unwrap_or_default()
}

fn markdown_text(names: &[String], rows: &Rows, null: &str) -> String {
    let esc = |s: &str| s.replace('|', "\\|").replace('\n', "<br>");
    let mut out = format!("| {} |\n|{}|\n", names.iter().map(|n| esc(n)).collect::<Vec<_>>().join(" | "), names.iter().map(|_| " --- ").collect::<Vec<_>>().join("|"));
    for r in rows {
        out.push_str(&format!("| {} |\n", r.iter().map(|v| esc(v.as_deref().unwrap_or(null))).collect::<Vec<_>>().join(" | ")));
    }
    out
}

fn sql_text(d: Dialect, t: &TableRef, names: &[String], rows: &Rows, batch: usize) -> String {
    rows.chunks(batch.max(1)).map(|chunk| sqlgen::insert_statement(d, t, names, chunk)).collect::<Vec<_>>().join("\n")
}

/// Clipboard rendering for "Copy rows as".
pub fn render_text(names: &[String], rows: &Rows, fmt: CopyFormat, null: &str, table: Option<(&Dialect, &TableRef)>) -> String {
    match fmt {
        CopyFormat::Tsv => rows.iter().map(|r| r.iter().map(|v| v.as_deref().unwrap_or(null).replace(['\t', '\n'], " ")).collect::<Vec<_>>().join("\t")).collect::<Vec<_>>().join("\n"),
        CopyFormat::Csv => csv_text(names, rows, b',', true, "", false).unwrap_or_default(),
        CopyFormat::Json => json_text(names, rows),
        CopyFormat::Markdown => markdown_text(names, rows, null),
        CopyFormat::SqlInsert => {
            let (d, t) = table.map(|(d, t)| (*d, t.clone())).unwrap_or((Dialect::Postgres, TableRef { schema: String::new(), name: "table_name".into() }));
            sql_text(d, &t, names, rows, 1000)
        }
    }
}

pub fn write_file(path: &Path, names: &[String], rows: &Rows, opt: &ExportOptions, sql_target: Option<(Dialect, &TableRef, Option<&str>)>) -> Result<()> {
    let null = if opt.null_as_empty { "" } else { "NULL" };
    let body = match opt.format {
        ExportFormat::Csv => csv_text(names, rows, opt.delimiter as u8, opt.header, null, opt.newline_to_space)?,
        ExportFormat::Json => json_text(names, rows),
        ExportFormat::Markdown => markdown_text(names, rows, "NULL"),
        ExportFormat::Sql => {
            let (d, t, ddl) = sql_target.map(|(d, t, ddl)| (d, t.clone(), ddl)).unwrap_or((Dialect::Postgres, TableRef { schema: String::new(), name: "exported".into() }, None));
            let mut s = String::new();
            if opt.include_create {
                if let Some(ddl) = ddl {
                    s.push_str(ddl);
                    s.push_str("\n\n");
                }
            }
            s.push_str(&sql_text(d, &t, names, rows, opt.sql_batch));
            s
        }
        ExportFormat::Excel => return write_xlsx(path, names, rows),
    };
    std::fs::write(path, body).with_context(|| format!("Can't write {}", path.display()))
}

fn write_xlsx(path: &Path, names: &[String], rows: &Rows) -> Result<()> {
    let mut wb = rust_xlsxwriter::Workbook::new();
    let ws = wb.add_worksheet();
    let bold = rust_xlsxwriter::Format::new().set_bold();
    for (c, n) in names.iter().enumerate() {
        ws.write_string_with_format(0, c as u16, n, &bold)?;
    }
    for (r, row) in rows.iter().enumerate() {
        for (c, v) in row.iter().enumerate() {
            let Some(v) = v else { continue };
            let (rr, cc) = ((r + 1) as u32, c as u16);
            // Keep identifiers like zip codes as text; only plain decimals become numbers.
            let numeric = v.parse::<f64>().ok().filter(|n| n.is_finite() && (v == "0" || v.starts_with("0.") || !v.starts_with('0')));
            match numeric {
                Some(n) => ws.write_number(rr, cc, n)?,
                None => ws.write_string(rr, cc, v)?,
            };
        }
    }
    wb.save(path)?;
    Ok(())
}

/// Reads a CSV file: (header, rows). Without a header row, columns are named c1..cn.
pub fn read_csv(path: &Path, delimiter: u8, has_header: bool) -> Result<(Vec<String>, Vec<Vec<String>>)> {
    let mut rdr = csv::ReaderBuilder::new().delimiter(delimiter).has_headers(has_header).flexible(true).from_path(path)?;
    let mut rows = Vec::new();
    for rec in rdr.records() {
        rows.push(rec?.iter().map(str::to_string).collect::<Vec<_>>());
    }
    let header = if has_header {
        rdr.headers()?.iter().map(str::to_string).collect()
    } else {
        let n = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        (1..=n).map(|i| format!("c{i}")).collect()
    };
    Ok((header, rows))
}

/// Reads a JSON array of objects: (keys in first-seen order, rows).
pub fn read_json(path: &Path) -> Result<(Vec<String>, Vec<Vec<Option<String>>>)> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let arr = v.as_array().context("Expected a JSON array of objects")?;
    let mut keys: Vec<String> = Vec::new();
    for item in arr {
        if let Some(o) = item.as_object() {
            for k in o.keys() {
                if !keys.contains(k) {
                    keys.push(k.clone());
                }
            }
        }
    }
    let rows = arr
        .iter()
        .map(|item| {
            keys.iter()
                .map(|k| match &item[k] {
                    serde_json::Value::Null => None,
                    serde_json::Value::String(s) => Some(s.clone()),
                    other => Some(other.to_string()),
                })
                .collect()
        })
        .collect();
    Ok((keys, rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (Vec<String>, Vec<Vec<Option<String>>>) {
        (vec!["id".into(), "note".into()], vec![vec![Some("1".into()), Some("a,\"b\"\nc".into())], vec![Some("2".into()), None]])
    }

    #[test]
    fn csv_quotes_and_nulls() {
        let (n, r) = sample();
        let s = csv_text(&n, &r, b',', true, "", false).unwrap();
        assert_eq!(s, "id,note\n1,\"a,\"\"b\"\"\nc\"\n2,\n");
    }

    #[test]
    fn json_keeps_nulls_and_order() {
        let (n, r) = sample();
        let v: serde_json::Value = serde_json::from_str(&json_text(&n, &r)).unwrap();
        assert_eq!(v[1]["note"], serde_json::Value::Null);
        assert_eq!(v[0]["id"], "1");
    }

    #[test]
    fn sql_batches_inserts() {
        let (n, r) = sample();
        let t = TableRef { schema: String::new(), name: "t".into() };
        let s = sql_text(Dialect::Postgres, &t, &n, &r, 1);
        assert_eq!(s.matches("INSERT INTO").count(), 2);
        assert!(s.contains("'a,\"b\"\nc'"));
        assert!(s.contains("NULL"));
    }

    #[test]
    fn csv_and_json_round_trip_through_files() {
        let dir = std::env::temp_dir().join(format!("strata-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (n, r) = sample();
        let csv_path = dir.join("x.csv");
        write_file(&csv_path, &n, &r, &ExportOptions::default(), None).unwrap();
        let (h, rows) = read_csv(&csv_path, b',', true).unwrap();
        assert_eq!(h, n);
        assert_eq!(rows[0][1], "a,\"b\"\nc");
        let json_path = dir.join("x.json");
        write_file(&json_path, &n, &r, &ExportOptions { format: ExportFormat::Json, ..Default::default() }, None).unwrap();
        let (k, jr) = read_json(&json_path).unwrap();
        assert_eq!(k, n);
        assert_eq!(jr[1][1], None);
        let xlsx = dir.join("x.xlsx");
        write_file(&xlsx, &n, &r, &ExportOptions { format: ExportFormat::Excel, ..Default::default() }, None).unwrap();
        assert!(std::fs::metadata(&xlsx).unwrap().len() > 1000);
        let _ = std::fs::remove_dir_all(dir);
    }
}
