//! Список моделей и лимиты подписок.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, bail};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use super::{claude, codex};
use crate::model::Limits;

#[derive(Clone, Debug, Serialize)]
pub struct ModelInfo {
    /// Передаётся в CLI; "" = по умолчанию.
    pub id: String,
    pub name: String,
    pub description: String,
    pub efforts: Vec<String>,
    pub default_effort: Option<String>,
    pub is_default: bool,
}

const CLAUDE_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// У Claude Code нет команды со списком моделей; псевдонимы всегда указывают на последние версии.
pub fn claude_models() -> Vec<ModelInfo> {
    [
        ("", "По умолчанию", "Модель из настроек Claude Code"),
        ("fable", "Fable", "Самая мощная модель Claude"),
        ("opus", "Opus", "Сложные задачи и большой код"),
        ("sonnet", "Sonnet", "Баланс скорости и качества"),
        ("haiku", "Haiku", "Быстрая и экономная"),
    ]
    .iter()
    .map(|(id, name, desc)| ModelInfo {
        id: (*id).into(),
        name: (*name).into(),
        description: (*desc).into(),
        efforts: CLAUDE_EFFORTS.iter().map(|s| s.to_string()).collect(),
        default_effort: None,
        is_default: id.is_empty(),
    })
    .collect()
}

fn hidden(cmd: &mut Command) {
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let _ = cmd;
}

/// Один запрос к `codex app-server` (JSON-RPC по stdio).
async fn codex_rpc(bin: &str, method: &str, params: Value) -> anyhow::Result<Value> {
    let mut cmd = Command::new(bin);
    cmd.arg("app-server").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    hidden(&mut cmd);
    let mut child = cmd.spawn().with_context(|| format!("не удалось запустить `{bin} app-server`"))?;
    let mut stdin = child.stdin.take().context("нет stdin")?;
    let mut lines = BufReader::new(child.stdout.take().context("нет stdout")?).lines();

    let msgs = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"mecode","version":env!("CARGO_PKG_VERSION")}}}),
        json!({"jsonrpc":"2.0","method":"initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":method,"params":params}),
    ];
    for m in msgs {
        stdin.write_all(format!("{m}\n").as_bytes()).await?;
    }
    stdin.flush().await?;

    let read = async {
        while let Some(line) = lines.next_line().await? {
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            if v["id"] == 2 {
                if let Some(e) = v.get("error") {
                    bail!("codex app-server: {e}");
                }
                return Ok(v["result"].clone());
            }
        }
        bail!("codex app-server закрылся без ответа")
    };
    let res = tokio::time::timeout(Duration::from_secs(30), read).await.context("codex app-server не ответил за 30 с")?;
    let _ = child.kill().await;
    res
}

pub async fn codex_models(bin: &str) -> anyhow::Result<Vec<ModelInfo>> {
    let r = codex_rpc(bin, "model/list", json!({})).await?;
    let mut out = vec![ModelInfo {
        id: String::new(),
        name: "По умолчанию".into(),
        description: "Модель из настроек Codex".into(),
        efforts: Vec::new(),
        default_effort: None,
        is_default: true,
    }];
    for m in r["data"].as_array().into_iter().flatten().filter(|m| m["hidden"] != true) {
        let efforts: Vec<String> = m["supportedReasoningEfforts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e["reasoningEffort"].as_str().map(String::from))
            .collect();
        out.push(ModelInfo {
            id: m["id"].as_str().unwrap_or_default().into(),
            name: m["displayName"].as_str().or(m["id"].as_str()).unwrap_or_default().into(),
            description: m["description"].as_str().unwrap_or_default().into(),
            efforts,
            default_effort: m["defaultReasoningEffort"].as_str().map(String::from),
            is_default: false,
        });
    }
    // Уровни «по умолчанию» = уровни модели, которая стоит по умолчанию у Codex.
    if let Some(def) = r["data"].as_array().into_iter().flatten().find(|m| m["isDefault"] == true) {
        let id = def["id"].as_str().unwrap_or_default();
        if let Some(src) = out.iter().find(|m| m.id == id).map(|m| m.efforts.clone()) {
            out[0].efforts = src;
        }
    }
    Ok(out)
}

/// Лимиты Codex: бесплатный запрос к app-server.
pub async fn codex_limits(bin: &str) -> anyhow::Result<Limits> {
    let r = codex_rpc(bin, "account/rateLimits/read", Value::Null).await?;
    codex::parse_rate_limits(&r["rateLimits"], "usedPercent", "resetsAt", "planType").context("Codex не вернул лимиты")
}

/// Тариф Claude (`pro`, `max`…) из `claude auth status --json`. Бесплатно, без запроса к модели.
pub async fn claude_plan(bin: &str) -> Option<String> {
    let mut cmd = Command::new(bin);
    cmd.args(["auth", "status", "--json"]).stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    hidden(&mut cmd);
    let out = tokio::time::timeout(Duration::from_secs(20), cmd.output()).await.ok()?.ok()?;
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    v["subscriptionType"].as_str().or(v["planTier"].as_str()).map(String::from)
}

/// Лимиты Claude приходят только вместе с ответом модели, поэтому делаем минимальный запрос
/// на Haiku без инструментов и без сохранения сессии. Тратит крошечную часть лимита.
pub async fn claude_limits_probe(bin: &str, cwd: &Path) -> anyhow::Result<Limits> {
    let mut cmd = Command::new(bin);
    cmd.current_dir(cwd)
        .args(["-p", "--output-format", "stream-json", "--verbose", "--model", "haiku"])
        // Промпт идёт через stdin: `--tools` принимает несколько значений и съел бы позиционный аргумент.
        .args(["--no-session-persistence", "--tools", ""])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    hidden(&mut cmd);
    let mut child = cmd.spawn().with_context(|| format!("не удалось запустить `{bin}`"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(b"Reply with: ok").await?;
        stdin.shutdown().await?;
    }
    let out = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output())
        .await
        .context("Claude не ответил за 60 с")??;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["type"] == "rate_limit_event")
        .and_then(|v| claude::parse_limits(&v["rate_limit_info"]))
        .context("Claude не вернул лимиты")
}
