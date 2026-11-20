//! Запуск агентов через их CLI (`claude -p`, `codex exec`) с потоковым JSONL-выводом.
//! Работают по подпискам пользователя, API-ключи не нужны.

pub mod claude;
pub mod codex;
pub mod info;

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, bail};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::model::{AgentId, AgentSettings, Limits};

/// Что агент сообщает во время хода.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentEvent {
    /// ID сессии CLI, чтобы следующий ход продолжил тот же разговор.
    Session(String),
    /// Кусок текста, который агент печатает сейчас (до готовой реплики `Text`).
    Delta(String),
    /// Реплика в чат.
    Text(String),
    /// Действие: команда, правка файла, инструмент.
    Activity(String),
    Error(String),
    /// Заполненность контекста; `None` = это поле не изменилось.
    Context { used: Option<u64>, window: Option<u64> },
    Limits(Limits),
}

/// Один ход агента.
#[derive(Clone, Debug)]
pub struct TurnSpec {
    pub agent: AgentId,
    pub workspace: PathBuf,
    /// Сессия для продолжения; `None` = новая.
    pub session: Option<String>,
    /// Новые сообщения чата, оформленные для агента.
    pub prompt: String,
    /// Правила группового чата.
    pub system: String,
    /// Модель, рассуждение, разрешения.
    pub settings: AgentSettings,
    /// Картинки из новых сообщений (Codex получает их через `-i`).
    pub images: Vec<PathBuf>,
    /// Папки вложений (Claude получает к ним доступ через `--add-dir`).
    pub extra_dirs: Vec<PathBuf>,
}

pub async fn run_turn(
    cfg: &Config,
    spec: TurnSpec,
    tx: UnboundedSender<AgentEvent>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    match spec.agent {
        AgentId::Claude => {
            let (cmd, stdin) = claude::command(&cfg.claude, &spec);
            run_jsonl(cmd, stdin, |v| claude::parse_line(&v), &tx, cancel).await
        }
        AgentId::Codex => {
            let (cmd, stdin) = codex::command(&cfg.codex, &spec);
            let session = std::sync::Mutex::new(spec.session.clone());
            let res = run_jsonl(
                cmd,
                stdin,
                |v| {
                    let evs = codex::parse_line(&v);
                    for ev in &evs {
                        if let AgentEvent::Session(id) = ev {
                            *session.lock().unwrap() = Some(id.clone());
                        }
                    }
                    evs
                },
                &tx,
                cancel,
            )
            .await;
            // `codex exec --json` не сообщает контекст и лимиты, они есть в файле сессии.
            if let Some(id) = session.into_inner().unwrap() {
                if let Ok(evs) = tokio::task::spawn_blocking(move || codex::read_rollout_stats(&id)).await {
                    for ev in evs {
                        let _ = tx.send(ev);
                    }
                }
            }
            res
        }
    }
}

/// Запускает процесс, пишет промпт в stdin (без лимита длины командной строки Windows),
/// читает JSONL из stdout и переводит строки в события.
async fn run_jsonl(
    mut cmd: Command,
    stdin_text: String,
    parse: impl Fn(Value) -> Vec<AgentEvent>,
    tx: &UnboundedSender<AgentEvent>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW: из GUI-приложения не всплывают консольные окна.
        cmd.creation_flags(0x0800_0000);
    }

    let program = cmd.as_std().get_program().to_string_lossy().into_owned();
    let mut child = cmd
        .spawn()
        .with_context(|| format!("не удалось запустить `{program}`: установлен ли он и есть ли в PATH?"))?;

    let mut stdin = child.stdin.take().context("нет stdin")?;
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(stdin_text.as_bytes()).await;
        let _ = stdin.shutdown().await;
    });

    let mut stderr = child.stderr.take().context("нет stderr")?;
    let stderr_task = tokio::spawn(async move {
        let mut buf = String::new();
        let _ = stderr.read_to_string(&mut buf).await;
        buf
    });

    let mut lines = BufReader::new(child.stdout.take().context("нет stdout")?).lines();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                return Ok(());
            }
            line = lines.next_line() => {
                let Some(line) = line? else { break };
                let line = line.trim();
                if line.is_empty() { continue; }
                match serde_json::from_str::<Value>(line) {
                    Ok(v) => for ev in parse(v) { let _ = tx.send(ev); },
                    Err(_) => tracing::debug!("не JSON: {line}"),
                }
            }
        }
    }

    let _ = writer.await;
    let status = child.wait().await?;
    let stderr = stderr_task.await.unwrap_or_default();
    if !status.success() {
        let tail: String = stderr.lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
        bail!("`{program}` завершился с кодом {:?}\n{tail}", status.code());
    }
    Ok(())
}

/// Обрезает строку по символам, а не по байтам.
pub(crate) fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}
