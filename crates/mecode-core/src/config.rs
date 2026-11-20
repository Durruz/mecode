use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Порт локального сервера.
    pub port: u16,
    /// Сколько раз подряд агенты могут будить друг друга без участия пользователя.
    pub max_agent_hops: u32,
    pub claude: ClaudeConfig,
    pub codex: CodexConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 7878,
            max_agent_hops: 8,
            claude: ClaudeConfig::default(),
            codex: CodexConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ClaudeConfig {
    pub bin: String,
    /// Пусто = модель по умолчанию из настроек Claude Code.
    pub model: String,
    /// default | acceptEdits | plan | bypassPermissions
    pub permission_mode: String,
    pub extra_args: Vec<String>,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            bin: "claude".into(),
            model: String::new(),
            permission_mode: "acceptEdits".into(),
            extra_args: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CodexConfig {
    pub bin: String,
    pub model: String,
    /// read-only | workspace-write | danger-full-access
    pub sandbox: String,
    pub extra_args: Vec<String>,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            bin: "codex".into(),
            model: String::new(),
            sandbox: "workspace-write".into(),
            extra_args: Vec::new(),
        }
    }
}

impl Config {
    /// Читает `config.json` из папки данных; если файла нет, создаёт его с настройками по умолчанию.
    pub fn load_or_create(data_dir: &Path) -> anyhow::Result<Self> {
        let path = Self::path(data_dir);
        if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            Ok(serde_json::from_str(&raw)?)
        } else {
            let cfg = Config::default();
            std::fs::create_dir_all(data_dir)?;
            std::fs::write(&path, serde_json::to_string_pretty(&cfg)?)?;
            Ok(cfg)
        }
    }

    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("config.json")
    }
}
