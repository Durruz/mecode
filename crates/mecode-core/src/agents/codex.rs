//! Codex CLI: `codex exec --json` и `codex exec resume --json <id>`.

use serde_json::Value;
use tokio::process::Command;

use super::{AgentEvent, TurnSpec, clip};
use crate::config::CodexConfig;

pub fn command(cfg: &CodexConfig, spec: &TurnSpec) -> (Command, String) {
    let mut cmd = Command::new(&cfg.bin);
    cmd.current_dir(&spec.workspace).arg("exec");
    if spec.session.is_some() {
        cmd.arg("resume");
    }
    cmd.args(["--json", "--skip-git-repo-check"]);
    // У `exec resume` нет флага --sandbox, поэтому задаём режим через конфиг для обоих случаев.
    if !cfg.sandbox.is_empty() {
        cmd.args(["-c", &format!("sandbox_mode=\"{}\"", cfg.sandbox)]);
    }
    if !cfg.model.is_empty() {
        cmd.args(["-m", &cfg.model]);
    }
    cmd.args(&cfg.extra_args);
    if let Some(s) = &spec.session {
        cmd.arg(s);
    }
    // `-` = промпт читается из stdin.
    cmd.arg("-");

    // У `codex exec` нет флага для системного промпта: в новой сессии ставим правила перед сообщениями,
    // дальше они остаются в истории сессии.
    let stdin = match spec.session {
        None => format!("{}\n\n---\n\n{}", spec.system, spec.prompt),
        Some(_) => spec.prompt.clone(),
    };
    (cmd, stdin)
}

pub fn parse_line(v: &Value) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    match v["type"].as_str() {
        Some("thread.started") => {
            if let Some(id) = v["thread_id"].as_str() {
                out.push(AgentEvent::Session(id.to_string()));
            }
        }
        Some("item.completed") => {
            let item = &v["item"];
            match item["type"].as_str() {
                Some("agent_message") => {
                    let t = item["text"].as_str().unwrap_or("").trim();
                    if !t.is_empty() {
                        out.push(AgentEvent::Text(t.to_string()));
                    }
                }
                Some("command_execution") => {
                    let cmd = item["command"].as_str().unwrap_or("");
                    let code = item["exit_code"].as_i64().map(|c| format!(" → {c}")).unwrap_or_default();
                    out.push(AgentEvent::Activity(format!("$ {}{code}", clip(cmd, 160))));
                }
                Some("file_change") => {
                    let files: Vec<String> = item["changes"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|c| format!("{} {}", c["kind"].as_str().unwrap_or("edit"), c["path"].as_str().unwrap_or("?")))
                        .collect();
                    out.push(AgentEvent::Activity(format!("файлы: {}", files.join(", "))));
                }
                Some("mcp_tool_call") => {
                    let server = item["server"].as_str().unwrap_or("mcp");
                    let tool = item["tool"].as_str().unwrap_or("tool");
                    out.push(AgentEvent::Activity(format!("{server}.{tool}")));
                }
                Some("web_search") => {
                    out.push(AgentEvent::Activity(format!("поиск: {}", item["query"].as_str().unwrap_or(""))));
                }
                Some("error") => {
                    out.push(AgentEvent::Error(item["message"].as_str().unwrap_or("ошибка Codex").to_string()));
                }
                _ => {}
            }
        }
        Some("turn.failed") => {
            let msg = v["error"]["message"].as_str().unwrap_or("ход Codex не удался");
            out.push(AgentEvent::Error(msg.to_string()));
        }
        Some("error") => {
            out.push(AgentEvent::Error(v["message"].as_str().unwrap_or("ошибка Codex").to_string()));
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_real_sample() {
        let lines = [
            json!({"type":"thread.started","thread_id":"t1"}),
            json!({"type":"turn.started"}),
            json!({"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"pong"}}),
            json!({"type":"turn.completed","usage":{}}),
        ];
        let evs: Vec<_> = lines.iter().flat_map(parse_line).collect();
        assert_eq!(evs, vec![AgentEvent::Session("t1".into()), AgentEvent::Text("pong".into())]);
    }

    #[test]
    fn parses_activity() {
        let c = json!({"type":"item.completed","item":{"type":"command_execution","command":"ls","exit_code":0}});
        assert_eq!(parse_line(&c), vec![AgentEvent::Activity("$ ls → 0".into())]);
        let f = json!({"type":"item.completed","item":{"type":"file_change","changes":[{"path":"a.rs","kind":"update"}]}});
        assert_eq!(parse_line(&f), vec![AgentEvent::Activity("файлы: update a.rs".into())]);
    }
}
