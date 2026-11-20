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
    /// Файлы, приложенные пользователем.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    /// Режим плана: агенты только читают и предлагают план.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub plan: bool,
}

/// Приложенный файл. Хранится в папке данных чата.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
    pub is_image: bool,
}

/// Сохранённый промпт.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedPrompt {
    pub id: String,
    pub title: String,
    pub text: String,
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

/// Что агенту разрешено делать.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    /// Только чтение.
    Read,
    /// Правит внутри папки проекта, остальное запрещено.
    #[default]
    Edits,
    /// Та же песочница, запросы за её пределы проверяются автоматически.
    Auto,
    /// Без песочницы и подтверждений.
    Full,
}

/// Настройки агента в конкретном чате.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AgentSettings {
    /// "" = модель по умолчанию.
    pub model: String,
    /// "" = уровень рассуждения по умолчанию.
    pub effort: String,
    pub permission: Permission,
    /// Быстрый режим (сейчас только Codex: service_tier="priority", расходует лимит быстрее).
    pub fast: bool,
}

impl AgentSettings {
    /// Значения попадают в аргументы CLI, поэтому пропускаем только безопасные символы.
    pub fn validate(&self) -> anyhow::Result<()> {
        let ok = |s: &str, extra: &str| s.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(c));
        anyhow::ensure!(self.model.len() <= 100 && ok(&self.model, "._:-[]/"), "недопустимое имя модели");
        anyhow::ensure!(self.effort.len() <= 20 && ok(&self.effort, "_-"), "недопустимый уровень рассуждения");
        Ok(())
    }
}

/// Заполненность контекста агента в чате.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ContextUsage {
    pub used: u64,
    pub window: Option<u64>,
}

/// Окно лимита подписки (5 часов, 7 дней).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LimitWindow {
    /// "5h" | "7d"
    pub id: String,
    pub used_percent: f64,
    /// Unix-время сброса, секунды.
    pub resets_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    pub plan: Option<String>,
    pub windows: Vec<LimitWindow>,
    pub updated_at: DateTime<Utc>,
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
    /// Модель, рассуждение и разрешения каждого агента.
    #[serde(default)]
    pub agents: HashMap<AgentId, AgentSettings>,
    /// Сколько контекста занято у каждого агента (по последнему ходу).
    #[serde(default)]
    pub context: HashMap<AgentId, ContextUsage>,
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
    Limits { agent: AgentId, limits: Limits },
}
