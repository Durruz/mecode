use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::{AgentId, AgentSettings, Permission};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Порт локального сервера.
    pub port: u16,
    /// Сколько раз подряд агенты могут будить друг друга без участия пользователя.
    pub max_agent_hops: u32,
    pub claude: AgentConfig,
    pub codex: AgentConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 7878,
            max_agent_hops: 8,
            claude: AgentConfig::new("claude"),
            codex: AgentConfig::new("codex"),
        }
    }
}

/// Настройки агента. `model`, `effort`, `permission` — значения по умолчанию для новых чатов.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub bin: String,
    pub model: String,
    pub effort: String,
    pub permission: Permission,
    pub extra_args: Vec<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self::new("")
    }
}

impl AgentConfig {
    fn new(bin: &str) -> Self {
        Self {
            bin: bin.into(),
            model: String::new(),
            effort: String::new(),
            permission: Permission::Edits,
            extra_args: Vec::new(),
        }
    }

    pub fn defaults(&self) -> AgentSettings {
        AgentSettings { model: self.model.clone(), effort: self.effort.clone(), permission: self.permission, fast: false }
    }
}

impl Config {
    pub fn agent(&self, agent: AgentId) -> &AgentConfig {
        match agent {
            AgentId::Claude => &self.claude,
            AgentId::Codex => &self.codex,
        }
    }

    /// Читает `config.json` из папки данных; если файла нет, создаёт его с настройками по умолчанию.
    pub fn load_or_create(data_dir: &Path) -> anyhow::Result<Self> {
        let path = Self::path(data_dir);
        if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            let mut cfg: Config = serde_json::from_str(&raw)?;
            if cfg.claude.bin.is_empty() {
                cfg.claude.bin = "claude".into();
            }
            if cfg.codex.bin.is_empty() {
                cfg.codex.bin = "codex".into();
            }
            Ok(cfg)
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
