//! Codex CLI: `codex exec --json` и `codex exec resume --json <id>`.

use serde_json::Value;
use tokio::process::Command;

use std::path::{Path, PathBuf};

use chrono::Utc;

use super::{AgentEvent, TurnSpec, clip};
use crate::config::AgentConfig;
use crate::model::{LimitWindow, Limits, Permission};

/// Аргументы разрешений. У `exec resume` нет флагов --sandbox и --approve-for-me,
/// поэтому режим задаётся ключами конфига: они работают и в новой, и в продолженной сессии.
pub fn permission_args(p: Permission) -> Vec<String> {
    let c = |kv: &str| ["-c".to_string(), kv.to_string()];
    match p {
        Permission::Read => c("sandbox_mode=\"read-only\"").to_vec(),
        Permission::Edits => c("sandbox_mode=\"workspace-write\"").to_vec(),
        Permission::Auto => [
            c("sandbox_mode=\"workspace-write\""),
            c("approval_policy=\"on-request\""),
            c("approvals_reviewer=\"auto_review\""),
        ]
        .concat(),
        Permission::Full => vec!["--dangerously-bypass-approvals-and-sandbox".into()],
    }
}

pub fn command(cfg: &AgentConfig, spec: &TurnSpec) -> (Command, String) {
    let s = &spec.settings;
    let mut cmd = Command::new(&cfg.bin);
    cmd.current_dir(&spec.workspace).arg("exec");
    if spec.session.is_some() {
        cmd.arg("resume");
    }
    cmd.args(["--json", "--skip-git-repo-check"]);
    cmd.args(permission_args(s.permission));
    if !s.model.is_empty() {
        cmd.args(["-m", &s.model]);
    }
    if !s.effort.is_empty() {
        cmd.args(["-c", &format!("model_reasoning_effort=\"{}\"", s.effort)]);
    }
    if s.fast {
        cmd.args(["-c", "service_tier=\"priority\""]);
    }
    cmd.args(&cfg.extra_args);
    // `-i` принимает несколько файлов подряд, поэтому `--` отделяет их от id сессии и `-`.
    for img in &spec.images {
        cmd.arg("-i").arg(img);
    }
    if !spec.images.is_empty() {
        cmd.arg("--");
    }
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

/// Папка Codex: `$CODEX_HOME` или `~/.codex`.
pub fn codex_home() -> Option<PathBuf> {
    if let Some(h) = std::env::var_os("CODEX_HOME") {
        return Some(h.into());
    }
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(|h| PathBuf::from(h).join(".codex"))
}

/// Ищет файл сессии `sessions/ГГГГ/ММ/ДД/rollout-…-<id>.jsonl`, начиная с новых дат.
fn find_rollout(sessions: &Path, id: &str) -> Option<PathBuf> {
    let suffix = format!("{id}.jsonl");
    let sorted_desc = |p: &Path| -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).collect();
        v.sort();
        v.reverse();
        v
    };
    for year in sorted_desc(sessions) {
        for month in sorted_desc(&year) {
            for day in sorted_desc(&month) {
                for f in sorted_desc(&day) {
                    if f.file_name().is_some_and(|n| n.to_string_lossy().ends_with(&suffix)) {
                        return Some(f);
                    }
                }
            }
        }
    }
    None
}

/// Контекст и лимиты из последнего `token_count` в файле сессии Codex.
pub fn read_rollout_stats(id: &str) -> Vec<AgentEvent> {
    let Some(path) = codex_home().and_then(|h| find_rollout(&h.join("sessions"), id)) else { return Vec::new() };
    let Ok(raw) = std::fs::read_to_string(path) else { return Vec::new() };
    let Some(v) = raw
        .lines()
        .rev()
        .filter(|l| l.contains("\"token_count\""))
        .find_map(|l| serde_json::from_str::<Value>(l).ok())
    else {
        return Vec::new();
    };
    parse_token_count(&v["payload"])
}

pub fn parse_token_count(p: &Value) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    let info = &p["info"];
    if let Some(used) = info["last_token_usage"]["total_tokens"].as_u64() {
        out.push(AgentEvent::Context { used: Some(used), window: info["model_context_window"].as_u64() });
    }
    if let Some(l) = parse_rate_limits(&p["rate_limits"], "used_percent", "resets_at", "plan_type") {
        out.push(AgentEvent::Limits(l));
    }
    out
}

/// Лимиты Codex. Ключи отличаются в файле сессии (snake_case) и в app-server (camelCase).
pub fn parse_rate_limits(r: &Value, used: &str, resets: &str, plan: &str) -> Option<Limits> {
    let windows: Vec<LimitWindow> = [("primary", "5h"), ("secondary", "7d")]
        .iter()
        .filter_map(|(key, id)| {
            let x = &r[*key];
            Some(LimitWindow { id: (*id).into(), used_percent: x[used].as_f64()?, resets_at: x[resets].as_i64() })
        })
        .collect();
    (!windows.is_empty()).then(|| Limits {
        plan: r[plan].as_str().map(|s| s.to_string()),
        windows,
        updated_at: Utc::now(),
    })
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
    fn parses_rollout_token_count() {
        let p = json!({"type":"token_count","info":{
            "last_token_usage":{"input_tokens":21893,"total_tokens":21956},
            "model_context_window":475000},
            "rate_limits":{"primary":{"used_percent":6.0,"window_minutes":300,"resets_at":1795193970},
                           "secondary":{"used_percent":22.0,"resets_at":1795712282},"plan_type":"plus"}});
        let evs = parse_token_count(&p);
        assert_eq!(evs[0], AgentEvent::Context { used: Some(21956), window: Some(475000) });
        let AgentEvent::Limits(l) = &evs[1] else { panic!() };
        assert_eq!(l.plan.as_deref(), Some("plus"));
        assert_eq!(l.windows[1].used_percent, 22.0);
    }

    #[test]
    fn permission_flags() {
        assert_eq!(permission_args(Permission::Full), vec!["--dangerously-bypass-approvals-and-sandbox"]);
        assert!(permission_args(Permission::Auto).contains(&"approvals_reviewer=\"auto_review\"".to_string()));
    }

    #[test]
    fn parses_activity() {
        let c = json!({"type":"item.completed","item":{"type":"command_execution","command":"ls","exit_code":0}});
        assert_eq!(parse_line(&c), vec![AgentEvent::Activity("$ ls → 0".into())]);
        let f = json!({"type":"item.completed","item":{"type":"file_change","changes":[{"path":"a.rs","kind":"update"}]}});
        assert_eq!(parse_line(&f), vec![AgentEvent::Activity("файлы: update a.rs".into())]);
    }
}
