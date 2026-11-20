//! Claude Code: `claude -p --output-format stream-json --verbose`.

use serde_json::Value;
use tokio::process::Command;

use super::{AgentEvent, TurnSpec, clip};
use crate::config::ClaudeConfig;

pub fn command(cfg: &ClaudeConfig, spec: &TurnSpec) -> (Command, String) {
    let mut cmd = Command::new(&cfg.bin);
    cmd.current_dir(&spec.workspace)
        .args(["-p", "--output-format", "stream-json", "--verbose", "--include-partial-messages"])
        .args(["--append-system-prompt", &spec.system]);
    if !cfg.permission_mode.is_empty() {
        cmd.args(["--permission-mode", &cfg.permission_mode]);
    }
    if !cfg.model.is_empty() {
        cmd.args(["--model", &cfg.model]);
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
        Some("assistant") => {
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
            if v["is_error"].as_bool() == Some(true) {
                let msg = v["result"].as_str().unwrap_or("ошибка Claude");
                out.push(AgentEvent::Error(msg.to_string()));
            }
        }
        _ => {}
    }
    out
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
    fn reports_error_result() {
        let r = json!({"type":"result","is_error":true,"result":"limit"});
        assert_eq!(parse_line(&r), vec![AgentEvent::Error("limit".into())]);
    }
}
