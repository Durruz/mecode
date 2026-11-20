use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Агент, который участвует в чате.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentId {
    Claude,
    Codex,
}

impl AgentId {
    pub const ALL: [AgentId; 2] = [AgentId::Claude, AgentId::Codex];

    pub fn name(self) -> &'static str {
        match self {
            AgentId::Claude => "claude",
            AgentId::Codex => "codex",
        }
    }

    pub fn display(self) -> &'static str {
        match self {
            AgentId::Claude => "Claude",
            AgentId::Codex => "Codex",
        }
    }

    pub fn other(self) -> AgentId {
        match self {
            AgentId::Claude => AgentId::Codex,
            AgentId::Codex => AgentId::Claude,
        }
    }
}

/// Автор сообщения.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Author {
    User,
    Claude,
    Codex,
    System,
}

impl Author {
    pub fn name(self) -> &'static str {
        match self {
            Author::User => "user",
            Author::Claude => "claude",
            Author::Codex => "codex",
            Author::System => "system",
        }
    }
}

impl From<AgentId> for Author {
    fn from(a: AgentId) -> Self {
        match a {
            AgentId::Claude => Author::Claude,
            AgentId::Codex => Author::Codex,
        }
    }
}

/// Вид сообщения. Агентам пересылаются только `Chat`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageKind {
    /// Обычная реплика в чате.
    Chat,
    /// Действие агента: команда, правка файла, вызов инструмента.
    Activity,
    /// Ошибка запуска или работы агента.
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    /// Порядковый номер внутри комнаты, начиная с 1.
    pub seq: u64,
    pub author: Author,
    pub kind: MessageKind,
    pub text: String,
    pub ts: DateTime<Utc>,
}

/// Кому по умолчанию уходит сообщение пользователя без @упоминаний.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    #[default]
    All,
    Claude,
    Codex,
}

/// Комната (чат) с рабочей папкой проекта.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Room {
    pub id: String,
    pub title: String,
    pub workspace: PathBuf,
    pub created: DateTime<Utc>,
    #[serde(default)]
    pub default_target: Target,
    /// ID сессий CLI, чтобы продолжать разговор, а не начинать заново.
    #[serde(default)]
    pub sessions: HashMap<AgentId, String>,
    /// Последнее сообщение, которое агент уже видел.
    #[serde(default)]
    pub cursors: HashMap<AgentId, u64>,
}

/// События для интерфейса (уходят по WebSocket).
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Message { room_id: String, message: Message },
    AgentStatus { room_id: String, agent: AgentId, busy: bool },
    RoomUpdated { room: Room },
    RoomDeleted { room_id: String },
    /// Текст, который агент печатает прямо сейчас (накопленный целиком). Не сохраняется.
    Delta { room_id: String, agent: AgentId, text: String },
}
