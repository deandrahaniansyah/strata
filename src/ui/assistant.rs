//! AI assistant in the right sidebar: writes SQL from a question using the current schema.
//! Talks to the Anthropic Messages API, or to any OpenAI-compatible endpoint.

use super::editor::Catalog;
use super::workspace::{TabCtx, WsAction};
use super::{dim, faint, p, primary_button};
use crate::model::{AiProvider, Settings};
use crate::runtime::{take, Task};
use crate::store::Store;
use egui::RichText;
use serde_json::{json, Value};

pub const API_KEY_SECRET: &str = "ai:api-key";
const ANTHROPIC_VERSION: &str = "2026-10-21";

#[derive(Clone)]
struct Msg {
    user: bool,
    text: String,
}

#[derive(Default)]
pub struct Assistant {
    input: String,
    history: Vec<Msg>,
    task: Option<Task<anyhow::Result<String>>>,
    consent: bool,
}

fn system_prompt(dialect: &str, db: &str, catalog: &Catalog) -> String {
    let mut schema = String::new();
    for t in catalog.tables.iter().take(200) {
        let cols = catalog.columns.get(t).map(|c| c.join(", ")).unwrap_or_default();
        schema.push_str(&format!("- {t}({cols})\n"));
    }
    format!(
        "You help a developer query a {dialect} database named \"{db}\" inside a database client.\n\
         Write correct {dialect} for the schema below. Put each query in a fenced ```sql block, \
         keep explanations to a sentence or two, and say so when the schema can't answer the question \
         instead of inventing tables or columns. Prefer read-only queries unless the user asks to change data.\n\nSchema:\n{schema}"
    )
}

async fn ask_anthropic(base: &str, key: &str, model: &str, system: String, messages: Vec<Value>) -> anyhow::Result<String> {
    let body = json!({
        "model": model,
        "max_tokens": 16000,
        "system": system,
        "messages": messages,
        "output_config": { "effort": "medium" },
        "fallbacks": "default",
    });
    let resp = reqwest::Client::new()
        .post(format!("{}/v1/messages", base.trim_end_matches('/')))
        .header("x-api-key", key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("anthropic-beta", "server-side-fallback-2026-07-01")
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let v: Value = resp.json().await?;
    if !status.is_success() {
        anyhow::bail!("{} ({status})", v["error"]["message"].as_str().unwrap_or("request failed"));
    }
    if v["stop_reason"] == "refusal" {
        let why = v["stop_details"]["explanation"].as_str().unwrap_or("the request was declined");
        anyhow::bail!("The model declined: {why}");
    }
    let text: Vec<&str> = v["content"].as_array().map(|blocks| blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect()).unwrap_or_default();
    Ok(text.join("\n"))
}

async fn ask_openai_compatible(base: &str, key: &str, model: &str, system: String, messages: Vec<Value>) -> anyhow::Result<String> {
    let mut all = vec![json!({"role": "system", "content": system})];
    all.extend(messages);
    let mut req = reqwest::Client::new().post(format!("{}/chat/completions", base.trim_end_matches('/'))).json(&json!({ "model": model, "messages": all }));
    if !key.is_empty() {
        req = req.bearer_auth(key);
    }
    let resp = req.send().await?;
    let status = resp.status();
    let v: Value = resp.json().await?;
    if !status.is_success() {
        anyhow::bail!("{} ({status})", v["error"]["message"].as_str().unwrap_or("request failed"));
    }
    Ok(v["choices"][0]["message"]["content"].as_str().unwrap_or("").to_string())
}

/// The API key: ANTHROPIC_API_KEY when set (Anthropic only), else the one saved in Settings.
pub fn api_key(s: &Settings, store: &Store) -> String {
    std::env::var("ANTHROPIC_API_KEY").ok().filter(|k| !k.is_empty() && s.ai_provider == AiProvider::Anthropic).unwrap_or_else(|| store.load_secret_value(API_KEY_SECRET))
}

/// One question in, the first SQL block out (for the query tab's inline "ask").
pub fn ask_for_sql(s: &Settings, store: &Store, dialect: &str, db: &str, catalog: &Catalog, question: &str) -> impl std::future::Future<Output = anyhow::Result<String>> + Send + 'static {
    let key = api_key(s, store);
    let (provider, base, model) = (s.ai_provider, s.ai_base_url.clone(), s.ai_model.clone());
    let system = system_prompt(dialect, db, catalog) + "\nAnswer with exactly one ```sql block and nothing else.";
    let messages = vec![json!({"role": "user", "content": question})];
    async move {
        if key.is_empty() && provider == AiProvider::Anthropic {
            anyhow::bail!("Add an API key in Settings → AI, or set ANTHROPIC_API_KEY.");
        }
        let reply = match provider {
            AiProvider::Anthropic => ask_anthropic(&base, &key, &model, system, messages).await?,
            AiProvider::OpenAiCompatible => ask_openai_compatible(&base, &key, &model, system, messages).await?,
        };
        segments(&reply).into_iter().find(|(code, _)| *code).map(|(_, sql)| sql).ok_or_else(|| anyhow::anyhow!("{}", reply.trim()))
    }
}

/// Splits a reply into prose and ```sql blocks.
fn segments(text: &str) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("```") {
        out.push((false, rest[..start].trim().to_string()));
        let after = &rest[start + 3..];
        let body_start = after.find('\n').map(|i| i + 1).unwrap_or(0);
        let body = &after[body_start..];
        match body.find("```") {
            Some(end) => {
                out.push((true, body[..end].trim().to_string()));
                rest = &body[end + 3..];
            }
            None => {
                out.push((true, body.trim().to_string()));
                rest = "";
            }
        }
    }
    out.push((false, rest.trim().to_string()));
    out.into_iter().filter(|(_, s)| !s.is_empty()).collect()
}

impl Assistant {
    fn send(&mut self, cx: &TabCtx, catalog: &Catalog, store: &Store) {
        let question = self.input.trim().to_string();
        if question.is_empty() {
            return;
        }
        self.input.clear();
        self.history.push(Msg { user: true, text: question });
        let s: &Settings = cx.settings;
        let key = api_key(s, store);
        let (provider, base, model) = (s.ai_provider, s.ai_base_url.clone(), s.ai_model.clone());
        let system = system_prompt(cx.session.cfg.driver.label(), &cx.session.cfg.database, catalog);
        let messages: Vec<Value> = self.history.iter().map(|m| json!({"role": if m.user { "user" } else { "assistant" }, "content": m.text})).collect();
        self.task = Some(Task::spawn(cx.ctx, async move {
            if key.is_empty() && provider == AiProvider::Anthropic {
                anyhow::bail!("Add an API key in Settings → AI, or set ANTHROPIC_API_KEY.");
            }
            match provider {
                AiProvider::Anthropic => ask_anthropic(&base, &key, &model, system, messages).await,
                AiProvider::OpenAiCompatible => ask_openai_compatible(&base, &key, &model, system, messages).await,
            }
        }));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut TabCtx, catalog: &Catalog, store: &Store) {
        let pal = p();
        if let Some(r) = take(&mut self.task) {
            match r {
                Ok(t) => self.history.push(Msg { user: false, text: t }),
                Err(e) => self.history.push(Msg { user: false, text: format!("⚠ {e:#}") }),
            }
        }
        if !cx.settings.ai_enabled && !self.consent {
            ui.add(egui::Label::new(dim("When you ask a question, Strata sends it with your table and column names (never row data) to the LLM provider set in Settings → AI. Use with your consent.")).wrap());
            ui.add_space(8.0);
            if primary_button(ui, "Enable assistant").clicked() {
                self.consent = true;
            }
            return;
        }
        let input_h = 86.0;
        egui::ScrollArea::vertical().stick_to_bottom(true).max_height(ui.available_height() - input_h).auto_shrink([false, false]).show(ui, |ui| {
            if self.history.is_empty() {
                ui.label(faint("Ask for a query, e.g. “top 10 customers by revenue this month”."));
            }
            for m in &self.history {
                let frame = egui::Frame::new()
                    .fill(if m.user { super::mix(pal.sidebar, pal.accent, 0.16) } else { pal.surface })
                    .corner_radius(egui::CornerRadius::same(10))
                    .inner_margin(egui::Margin::same(10));
                frame.show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if m.user {
                        ui.add(egui::Label::new(RichText::new(&m.text).size(12.5)).wrap());
                        return;
                    }
                    for (is_code, seg) in segments(&m.text) {
                        if is_code {
                            egui::Frame::new().fill(pal.editor_bg).corner_radius(egui::CornerRadius::same(6)).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                                ui.add(egui::Label::new(RichText::new(&seg).monospace().size(12.0)).wrap());
                            });
                            ui.horizontal(|ui| {
                                if ui.small_button("Open in editor").clicked() {
                                    cx.actions.push(WsAction::OpenQuery { sql: seg.clone(), title: Some("Assistant".into()), run: false });
                                }
                                if ui.small_button("Run").clicked() {
                                    cx.actions.push(WsAction::OpenQuery { sql: seg.clone(), title: Some("Assistant".into()), run: true });
                                }
                                if ui.small_button("Copy").clicked() {
                                    ui.ctx().copy_text(seg.clone());
                                }
                            });
                        } else {
                            ui.add(egui::Label::new(RichText::new(&seg).size(12.5)).wrap());
                        }
                    }
                });
                ui.add_space(6.0);
            }
            if self.task.is_some() {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(13.0));
                    ui.label(faint("Thinking…"));
                });
            }
        });
        ui.add_space(6.0);
        let r = ui.add(egui::TextEdit::multiline(&mut self.input).hint_text("Ask about your data…  (⌘↩ to send)").desired_rows(2).desired_width(f32::INFINITY));
        let send = r.has_focus() && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
        ui.horizontal(|ui| {
            ui.label(faint(format!("{} · {}", if cx.settings.ai_provider == AiProvider::Anthropic { "Anthropic" } else { "Custom LLM" }, cx.settings.ai_model)).size(10.5));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if (primary_button(ui, "Send").clicked() || send) && self.task.is_none() {
                    self.send(cx, catalog, store);
                }
                if !self.history.is_empty() && ui.small_button("Clear").clicked() {
                    self.history.clear();
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_prose_and_sql_blocks() {
        let s = segments("Here you go:\n```sql\nSELECT 1;\n```\nThat's it.");
        assert_eq!(s, vec![(false, "Here you go:".into()), (true, "SELECT 1;".into()), (false, "That's it.".into())]);
    }

    #[test]
    fn unterminated_block_still_counts_as_code() {
        let s = segments("```sql\nSELECT 2");
        assert_eq!(s, vec![(true, "SELECT 2".into())]);
    }
}
