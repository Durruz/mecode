//! Claude Code: `claude -p --output-format stream-json --verbose`.

use serde_json::Value;
use tokio::process::Command;

use chrono::Utc;

use super::{AgentEvent, TurnSpec, clip};
use crate::config::AgentConfig;
use crate::model::{LimitWindow, Limits, Permission};

pub fn permission_mode(p: Permission) -> &'static str {
    match p {
        // Не `plan`: в нём Claude Code подменяет выбранную модель на Sonnet.
        // `dontAsk` разрешает чтение и молча отклоняет всё, что требует разрешения.
        Permission::Read => "dontAsk",
        Permission::Edits => "acceptEdits",
        Permission::Auto => "auto",
        Permission::Full => "bypassPermissions",
    }
}

pub fn command(cfg: &AgentConfig, spec: &TurnSpec) -> (Command, String) {
    let s = &spec.settings;
    let mut cmd = Command::new(&cfg.bin);
    cmd.current_dir(&spec.workspace)
        .args(["-p", "--output-format", "stream-json", "--verbose", "--include-partial-messages"])
        .args(["--append-system-prompt", &spec.system])
        .args(["--permission-mode", permission_mode(s.permission)]);
    if !s.model.is_empty() {
        cmd.args(["--model", &s.model]);
    }
    if !s.effort.is_empty() {
        cmd.args(["--effort", &s.effort]);
    }
    for d in &spec.extra_dirs {
        cmd.arg("--add-dir").arg(d);
    }
    if let Some(s) = &spec.session {
        cmd.args(["--resume", s]);
    }
    cmd.args(&cfg.extra_args);
    (cmd, spec.prompt.clone())
}

pub fn parse_line(v: &Value) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    match v["type"].as_str() {
        Some("system") if v["subtype"] == "init" => {
            if let Some(id) = v["session_id"].as_str() {
                out.push(AgentEvent::Session(id.to_string()));
            }
        }
        Some("stream_event") => {
            let ev = &v["event"];
            if ev["type"] == "content_block_delta" && ev["delta"]["type"] == "text_delta" {
                if let Some(t) = ev["delta"]["text"].as_str().filter(|t| !t.is_empty()) {
                    out.push(AgentEvent::Delta(t.to_string()));
                }
            }
        }
        Some("rate_limit_event") => {
            if let Some(l) = parse_limits(&v["rate_limit_info"]) {
                out.push(AgentEvent::Limits(l));
            }
        }
        Some("assistant") => {
            // Размер контекста = всё, что ушло на вход последнего запроса к модели.
            let u = &v["message"]["usage"];
            if u.is_object() {
                let used = ["input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"]
                    .iter()
                    .filter_map(|k| u[*k].as_u64())
                    .sum::<u64>();
                if used > 0 {
                    out.push(AgentEvent::Context { used: Some(used), window: None });
                }
            }
            for block in v["message"]["content"].as_array().into_iter().flatten() {
                match block["type"].as_str() {
                    Some("text") => {
                        let t = block["text"].as_str().unwrap_or("").trim();
                        if !t.is_empty() {
                            out.push(AgentEvent::Text(t.to_string()));
                        }
                    }
                    Some("tool_use") => out.push(AgentEvent::Activity(tool_summary(block))),
                    _ => {}
                }
            }
        }
        Some("result") => {
            let window = v["modelUsage"]
                .as_object()
                .and_then(|m| m.values().filter_map(|x| x["contextWindow"].as_u64()).max());
            if window.is_some() {
                out.push(AgentEvent::Context { used: None, window });
            }
            if v["is_error"].as_bool() == Some(true) {
                let msg = v["result"].as_str().unwrap_or("ошибка Claude");
                out.push(AgentEvent::Error(msg.to_string()));
            }
        }
        _ => {}
    }
    out
}

/// `rate_limit_info.unifiedWindows` → окна 5h/7d (utilization — доля от 0 до 1).
pub fn parse_limits(info: &Value) -> Option<Limits> {
    let w = info["unifiedWindows"].as_object()?;
    let windows: Vec<LimitWindow> = [("five_hour", "5h"), ("seven_day", "7d")]
        .iter()
        .filter_map(|(key, id)| {
            let x = w.get(*key)?;
            Some(LimitWindow {
                id: (*id).into(),
                used_percent: (x["utilization"].as_f64()? * 1000.0).round() / 10.0,
                resets_at: x["resetsAt"].as_i64(),
            })
        })
        .collect();
    (!windows.is_empty()).then(|| Limits { plan: None, windows, updated_at: Utc::now() })
}

fn tool_summary(block: &Value) -> String {
    let name = block["name"].as_str().unwrap_or("tool");
    let input = &block["input"];
    let key = ["command", "file_path", "path", "pattern", "url", "query", "description"]
        .iter()
        .find_map(|k| input[*k].as_str());
    match key {
        Some(arg) => format!("{name}: {}", clip(arg, 160)),
        None => format!("{name} {}", clip(&input.to_string(), 160)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_init_text_and_tools() {
        let init = json!({"type":"system","subtype":"init","session_id":"abc"});
        assert_eq!(parse_line(&init), vec![AgentEvent::Session("abc".into())]);

        let msg = json!({"type":"assistant","message":{"content":[
            {"type":"text","text":"pong"},
            {"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}
        ]}});
        assert_eq!(
            parse_line(&msg),
            vec![AgentEvent::Text("pong".into()), AgentEvent::Activity("Bash: cargo test".into())]
        );

        let hook = json!({"type":"system","subtype":"hook_started","session_id":"abc"});
        assert!(parse_line(&hook).is_empty());
    }

    #[test]
    fn parses_text_delta_only() {
        let d = json!({"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"од"}}});
        assert_eq!(parse_line(&d), vec![AgentEvent::Delta("од".into())]);
        let th = json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hm"}}});
        assert!(parse_line(&th).is_empty());
    }

    #[test]
    fn parses_context_and_limits() {
        let a = json!({"type":"assistant","message":{"usage":{"input_tokens":2,"cache_creation_input_tokens":100,"cache_read_input_tokens":900},"content":[]}});
        assert_eq!(parse_line(&a), vec![AgentEvent::Context { used: Some(1002), window: None }]);
        let r = json!({"type":"result","modelUsage":{"claude-haiku-5-5":{"contextWindow":1000000}}});
        assert_eq!(parse_line(&r), vec![AgentEvent::Context { used: None, window: Some(1_000_000) }]);
        let l = json!({"type":"rate_limit_event","rate_limit_info":{"unifiedWindows":{
            "five_hour":{"utilization":0.33,"resetsAt":1795193400},"seven_day":{"utilization":0.375,"resetsAt":1795582800}}}});
        let evs = parse_line(&l);
        let [AgentEvent::Limits(lim)] = evs.as_slice() else { panic!("{evs:?}") };
        assert_eq!(lim.windows[0].id, "5h");
        assert_eq!(lim.windows[0].used_percent, 33.0);
        assert_eq!(lim.windows[1].used_percent, 37.5);
        assert_eq!(lim.windows[1].resets_at, Some(1795582800));
    }

    #[test]
    fn reports_error_result() {
        let r = json!({"type":"result","is_error":true,"result":"limit"});
        assert_eq!(parse_line(&r), vec![AgentEvent::Error("limit".into())]);
    }
}
