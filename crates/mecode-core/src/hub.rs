//! Оркестратор общего чата.
//!
//! Как это работает:
//! - У каждого агента своя сессия CLI. В каждый ход он получает только те сообщения чата,
//!   которых ещё не видел (курсор `room.cursors`), с подписью автора: `[user]`, `[codex]`…
//! - Сообщение пользователя будит агентов по @упоминанию, иначе — по `default_target` комнаты.
//! - Ответ агента с `@другой` будит другого агента. Цепочка без участия пользователя
//!   ограничена `max_agent_hops`, чтобы агенты не переписывались бесконечно.
//! - Если агент занят, новые сообщения для него копятся, и он отвечает сразу после текущего хода.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::agents::info::{self, ModelInfo};
use crate::agents::{self, AgentEvent, TurnSpec};
use crate::config::Config;
use crate::model::{
    AgentId, AgentSettings, Attachment, Author, ContextUsage, Event, Limits, Message, MessageKind, Room, SavedPrompt, Target,
};
use crate::router;
use crate::store::Store;

/// Комната для списка в интерфейсе: данные + кто сейчас работает.
#[derive(Clone, Debug, Serialize)]
pub struct RoomView {
    #[serde(flatten)]
    pub room: Room,
    pub busy: Vec<AgentId>,
}

struct RoomState {
    room: Room,
    messages: Vec<Message>,
    running: HashMap<AgentId, CancellationToken>,
    /// Агенты, которым пришли новые сообщения, пока они были заняты.
    pending: HashSet<AgentId>,
    /// Сколько раз подряд агенты разбудили друг друга после последнего сообщения пользователя.
    hops: u32,
    /// Загруженные, но ещё не отправленные вложения.
    uploads: HashMap<String, Attachment>,
}

impl RoomState {
    fn next_seq(&self) -> u64 {
        self.messages.last().map_or(1, |m| m.seq + 1)
    }

    fn view(&self) -> RoomView {
        let mut busy: Vec<AgentId> = self.running.keys().copied().collect();
        busy.sort_by_key(|a| a.name());
        RoomView { room: self.room.clone(), busy }
    }
}

/// Список моделей обоих агентов (для `GET /api/models`).
#[derive(Clone, Debug, Serialize)]
pub struct Models {
    pub claude: Vec<ModelInfo>,
    pub codex: Vec<ModelInfo>,
}

/// Последние известные лимиты подписок.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AllLimits {
    pub claude: Option<Limits>,
    pub codex: Option<Limits>,
}

/// Сколько держать в памяти список моделей Codex.
const MODELS_TTL: Duration = Duration::from_secs(600);

pub struct Hub {
    cfg: Config,
    data_dir: PathBuf,
    store: Store,
    rooms: Mutex<HashMap<String, RoomState>>,
    limits: Mutex<AllLimits>,
    models: tokio::sync::Mutex<Option<(Instant, Models)>>,
    tx: broadcast::Sender<Event>,
    prompts_lock: Mutex<()>,
}

impl Hub {
    pub fn new(cfg: Config, data_dir: &Path) -> anyhow::Result<Arc<Self>> {
        let store = Store::new(data_dir)?;
        let mut rooms = HashMap::new();
        for (mut room, messages) in store.load_rooms()? {
            // Чаты из старых версий: дописываем настройки агентов по умолчанию.
            for a in AgentId::ALL {
                room.agents.entry(a).or_insert_with(|| cfg.agent(a).defaults());
            }
            rooms.insert(
                room.id.clone(),
                RoomState { room, messages, running: HashMap::new(), pending: HashSet::new(), hops: 0, uploads: HashMap::new() },
            );
        }
        let limits = std::fs::read_to_string(data_dir.join("limits.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let (tx, _) = broadcast::channel(1024);
        Ok(Arc::new(Self {
            cfg,
            data_dir: data_dir.to_path_buf(),
            store,
            rooms: Mutex::new(rooms),
            limits: Mutex::new(limits),
            models: tokio::sync::Mutex::new(None),
            prompts_lock: Mutex::new(()),
            tx,
        }))
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Модели Claude (псевдонимы) и Codex (из `codex app-server`, кэш 10 минут).
    pub async fn models(&self) -> Models {
        let mut cache = self.models.lock().await;
        if let Some((at, m)) = cache.as_ref() {
            if at.elapsed() < MODELS_TTL {
                return m.clone();
            }
        }
        let codex = match info::codex_models(&self.cfg.codex.bin).await {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!("список моделей Codex: {e:#}");
                Vec::new()
            }
        };
        let m = Models { claude: info::claude_models(), codex };
        if !m.codex.is_empty() {
            *cache = Some((Instant::now(), m.clone()));
        }
        m
    }

    pub fn limits(&self) -> AllLimits {
        self.limits.lock().unwrap().clone()
    }

    fn set_limits(&self, agent: AgentId, l: Limits) {
        let snapshot = {
            let mut all = self.limits.lock().unwrap();
            let slot = match agent {
                AgentId::Claude => &mut all.claude,
                AgentId::Codex => &mut all.codex,
            };
            // У Claude план приходит не в каждом событии — сохраняем известный.
            let plan = l.plan.clone().or_else(|| slot.as_ref().and_then(|o| o.plan.clone()));
            *slot = Some(Limits { plan, ..l });
            all.clone()
        };
        if let Ok(s) = serde_json::to_string_pretty(&snapshot) {
            let _ = std::fs::write(self.data_dir.join("limits.json"), s);
        }
        let limits = match agent {
            AgentId::Claude => snapshot.claude,
            AgentId::Codex => snapshot.codex,
        };
        if let Some(limits) = limits {
            self.emit(Event::Limits { agent, limits });
        }
    }

    /// Обновляет лимиты. Codex — бесплатно; Claude — только если `claude == true`
    /// (это крошечный запрос к модели).
    pub async fn refresh_limits(&self, claude: bool) -> AllLimits {
        let probe_dir = self.data_dir.join("probe");
        let _ = std::fs::create_dir_all(&probe_dir);
        let (cx, cl) = tokio::join!(info::codex_limits(&self.cfg.codex.bin), async {
            if !claude {
                return None;
            }
            let (limits, plan) = tokio::join!(
                info::claude_limits_probe(&self.cfg.claude.bin, &probe_dir),
                info::claude_plan(&self.cfg.claude.bin)
            );
            Some(limits.map(|l| Limits { plan: plan.or(l.plan.clone()), ..l }))
        });
        match cx {
            Ok(l) => self.set_limits(AgentId::Codex, l),
            Err(e) => tracing::warn!("лимиты Codex: {e:#}"),
        }
        match cl {
            Some(Ok(l)) => self.set_limits(AgentId::Claude, l),
            Some(Err(e)) => tracing::warn!("лимиты Claude: {e:#}"),
            None => {}
        }
        self.limits()
    }

    /// Меняет модель, рассуждение и разрешения агента в чате. Действует со следующего хода.
    pub fn set_agent(&self, room_id: &str, agent: AgentId, settings: AgentSettings) -> anyhow::Result<Room> {
        settings.validate()?;
        let mut rooms = self.rooms.lock().unwrap();
        let st = rooms.get_mut(room_id).context("комната не найдена")?;
        st.room.agents.insert(agent, settings);
        self.store.save_room(&st.room)?;
        self.emit(Event::RoomUpdated { room: st.room.clone() });
        Ok(st.room.clone())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    fn emit(&self, ev: Event) {
        let _ = self.tx.send(ev);
    }

    pub fn list_rooms(&self) -> Vec<RoomView> {
        let rooms = self.rooms.lock().unwrap();
        let mut out: Vec<RoomView> = rooms.values().map(RoomState::view).collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.room.created));
        out
    }

    pub fn room(&self, room_id: &str) -> anyhow::Result<RoomView> {
        let rooms = self.rooms.lock().unwrap();
        Ok(rooms.get(room_id).context("комната не найдена")?.view())
    }

    pub fn messages(&self, room_id: &str) -> anyhow::Result<Vec<Message>> {
        let rooms = self.rooms.lock().unwrap();
        Ok(rooms.get(room_id).context("комната не найдена")?.messages.clone())
    }

    pub fn create_room(&self, title: &str, workspace: &str) -> anyhow::Result<Room> {
        let workspace = PathBuf::from(workspace.trim());
        if !workspace.is_dir() {
            bail!("папка не найдена: {}", workspace.display());
        }
        let title = match title.trim() {
            "" => workspace.file_name().map_or("Новый чат".into(), |n| n.to_string_lossy().into_owned()),
            t => t.to_string(),
        };
        let room = Room {
            id: uuid::Uuid::new_v4().simple().to_string()[..12].to_string(),
            title,
            workspace,
            created: Utc::now(),
            default_target: Target::All,
            sessions: HashMap::new(),
            cursors: HashMap::new(),
            agents: AgentId::ALL.iter().map(|a| (*a, self.cfg.agent(*a).defaults())).collect(),
            context: HashMap::new(),
        };
        self.store.save_room(&room)?;
        let state = RoomState { room: room.clone(), messages: Vec::new(), running: HashMap::new(), pending: HashSet::new(), hops: 0, uploads: HashMap::new() };
        self.rooms.lock().unwrap().insert(room.id.clone(), state);
        self.emit(Event::RoomUpdated { room: room.clone() });
        Ok(room)
    }

    pub fn set_target(&self, room_id: &str, target: Target) -> anyhow::Result<()> {
        let mut rooms = self.rooms.lock().unwrap();
        let st = rooms.get_mut(room_id).context("комната не найдена")?;
        st.room.default_target = target;
        self.store.save_room(&st.room)?;
        self.emit(Event::RoomUpdated { room: st.room.clone() });
        Ok(())
    }

    pub fn rename(&self, room_id: &str, title: &str) -> anyhow::Result<Room> {
        let title = title.trim();
        if title.is_empty() {
            bail!("пустое название");
        }
        let mut rooms = self.rooms.lock().unwrap();
        let st = rooms.get_mut(room_id).context("комната не найдена")?;
        st.room.title = title.to_string();
        self.store.save_room(&st.room)?;
        self.emit(Event::RoomUpdated { room: st.room.clone() });
        Ok(st.room.clone())
    }

    /// Удаляет комнату и её историю. Работающие агенты останавливаются.
    pub fn delete(&self, room_id: &str) -> anyhow::Result<()> {
        let st = self.rooms.lock().unwrap().remove(room_id).context("комната не найдена")?;
        for token in st.running.values() {
            token.cancel();
        }
        self.store.delete_room(&st.room.id)?;
        self.emit(Event::RoomDeleted { room_id: st.room.id });
        Ok(())
    }

    /// Сохраняет приложенный файл. Он уйдёт агентам с ближайшим сообщением, где указан его `id`.
    pub fn add_attachment(&self, room_id: &str, name: &str, bytes: &[u8]) -> anyhow::Result<Attachment> {
        const MAX: usize = 25 * 1024 * 1024;
        anyhow::ensure!(!bytes.is_empty(), "пустой файл");
        anyhow::ensure!(bytes.len() <= MAX, "файл больше 25 МБ");
        let mut rooms = self.rooms.lock().unwrap();
        let st = rooms.get_mut(room_id).context("комната не найдена")?;
        let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
        let att = self.store.save_attachment(room_id, &id, name, bytes)?;
        st.uploads.insert(id, att.clone());
        Ok(att)
    }

    /// Сообщение пользователя в чат.
    pub fn send(self: &Arc<Self>, room_id: &str, text: &str, opts: SendOptions) -> anyhow::Result<Message> {
        let text = text.trim();
        if text.is_empty() && opts.attachments.is_empty() {
            bail!("пустое сообщение");
        }
        let mut rooms = self.rooms.lock().unwrap();
        let st = rooms.get_mut(room_id).context("комната не найдена")?;
        let mut attachments = Vec::new();
        for id in &opts.attachments {
            attachments.push(st.uploads.remove(id).with_context(|| format!("вложение {id} не найдено"))?);
        }
        let msg = Message {
            seq: st.next_seq(),
            author: Author::User,
            kind: MessageKind::Chat,
            text: text.to_string(),
            ts: Utc::now(),
            attachments,
            plan: opts.plan,
        };
        self.push_message(st, msg.clone());
        st.hops = 0;
        for agent in router::user_targets(text, st.room.default_target) {
            self.schedule(st, agent);
        }
        Ok(msg)
    }

    /// Останавливает всех агентов в комнате.
    pub fn stop(&self, room_id: &str) -> anyhow::Result<()> {
        let mut rooms = self.rooms.lock().unwrap();
        let st = rooms.get_mut(room_id).context("комната не найдена")?;
        st.pending.clear();
        if st.running.is_empty() {
            return Ok(());
        }
        for token in st.running.values() {
            token.cancel();
        }
        self.push(st, Author::System, MessageKind::Chat, "Остановлено пользователем.".into());
        Ok(())
    }

    fn push(&self, st: &mut RoomState, author: Author, kind: MessageKind, text: String) -> Message {
        let msg = Message {
            seq: st.next_seq(),
            author,
            kind,
            text,
            ts: Utc::now(),
            attachments: Vec::new(),
            plan: false,
        };
        self.push_message(st, msg.clone());
        msg
    }

    fn push_message(&self, st: &mut RoomState, msg: Message) {
        if let Err(e) = self.store.append_message(&st.room.id, &msg) {
            tracing::error!("не удалось сохранить сообщение: {e}");
        }
        st.messages.push(msg.clone());
        self.emit(Event::Message { room_id: st.room.id.clone(), message: msg });
    }

    /// Новый чат в той же папке, куда переносится история текущего (как «Hand off to new thread»).
    /// Агенты начинают новые сессии и получают историю первым сообщением.
    pub fn handoff(&self, room_id: &str, title: &str) -> anyhow::Result<Room> {
        let (workspace, old_title, agents, transcript) = {
            let rooms = self.rooms.lock().unwrap();
            let st = rooms.get(room_id).context("комната не найдена")?;
            (st.room.workspace.clone(), st.room.title.clone(), st.room.agents.clone(), handoff_transcript(&st.messages))
        };
        let title = if title.trim().is_empty() { format!("{old_title} (продолжение)") } else { title.to_string() };
        let room = self.create_room(&title, &workspace.to_string_lossy())?;
        let mut rooms = self.rooms.lock().unwrap();
        let st = rooms.get_mut(&room.id).context("комната не найдена")?;
        st.room.agents = agents;
        self.store.save_room(&st.room)?;
        if !transcript.is_empty() {
            self.push(
                st,
                Author::System,
                MessageKind::Chat,
                format!("Продолжение чата «{old_title}». Краткая история:\n\n{transcript}"),
            );
        }
        self.emit(Event::RoomUpdated { room: st.room.clone() });
        Ok(st.room.clone())
    }

    pub fn prompts(&self) -> Vec<SavedPrompt> {
        self.load_prompts()
    }

    fn load_prompts(&self) -> Vec<SavedPrompt> {
        std::fs::read_to_string(self.data_dir.join("prompts.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Создаёт (пустой `id`) или обновляет сохранённый промпт.
    pub fn save_prompt(&self, mut p: SavedPrompt) -> anyhow::Result<SavedPrompt> {
        anyhow::ensure!(!p.text.trim().is_empty(), "пустой промпт");
        let _guard = self.prompts_lock.lock().unwrap();
        let mut all = self.load_prompts();
        if p.title.trim().is_empty() {
            p.title = p.text.trim().chars().take(40).collect();
        }
        match all.iter_mut().find(|x| !p.id.is_empty() && x.id == p.id) {
            Some(x) => *x = p.clone(),
            None => {
                p.id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
                all.push(p.clone());
            }
        }
        std::fs::write(self.data_dir.join("prompts.json"), serde_json::to_string_pretty(&all)?)?;
        Ok(p)
    }

    pub fn delete_prompt(&self, id: &str) -> anyhow::Result<()> {
        let _guard = self.prompts_lock.lock().unwrap();
        let mut all = self.load_prompts();
        let before = all.len();
        all.retain(|p| p.id != id);
        anyhow::ensure!(all.len() < before, "промпт не найден");
        std::fs::write(self.data_dir.join("prompts.json"), serde_json::to_string_pretty(&all)?)?;
        Ok(())
    }

    fn schedule(self: &Arc<Self>, st: &mut RoomState, agent: AgentId) {
        if st.running.contains_key(&agent) {
            st.pending.insert(agent);
        } else {
            self.start_turn(st, agent);
        }
    }

    fn start_turn(self: &Arc<Self>, st: &mut RoomState, agent: AgentId) {
        let cursor = st.room.cursors.get(&agent).copied().unwrap_or(0);
        let fresh: Vec<&Message> = st
            .messages
            .iter()
            .filter(|m| m.seq > cursor && m.kind == MessageKind::Chat && m.author != Author::from(agent))
            .collect();
        let Some(last) = st.messages.last() else { return };
        if fresh.is_empty() {
            return;
        }
        let plan = fresh.iter().any(|m| m.plan);
        let mut prompt = format_prompt(agent, &fresh);
        if plan {
            prompt.push_str(
                "\n\nРежим плана: ничего не меняй и не запускай изменяющих команд. Изучи проект и предложи пошаговый план.",
            );
        }
        let attachments: Vec<&Attachment> = fresh.iter().flat_map(|m| &m.attachments).collect();
        let images = attachments.iter().filter(|a| a.is_image).map(|a| a.path.clone()).collect();
        let extra_dirs = attachments
            .iter()
            .filter_map(|a| a.path.parent().map(|p| p.to_path_buf()))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        st.room.cursors.insert(agent, last.seq);

        let mut settings = st.room.agents.get(&agent).cloned().unwrap_or_else(|| self.cfg.agent(agent).defaults());
        if plan {
            settings.permission = crate::model::Permission::Read;
        }
        let spec = TurnSpec {
            agent,
            workspace: st.room.workspace.clone(),
            session: st.room.sessions.get(&agent).cloned(),
            prompt,
            system: system_prompt(agent),
            settings,
            images,
            extra_dirs,
        };
        let token = CancellationToken::new();
        st.running.insert(agent, token.clone());
        self.emit(Event::AgentStatus { room_id: st.room.id.clone(), agent, busy: true });

        let hub = Arc::clone(self);
        let room_id = st.room.id.clone();
        tokio::spawn(async move { hub.run(room_id, spec, token).await });
    }

    async fn run(self: Arc<Self>, room_id: String, spec: TurnSpec, token: CancellationToken) {
        let agent = spec.agent;
        let (etx, mut erx) = mpsc::unbounded_channel();
        let hub = Arc::clone(&self);
        let tok = token.clone();
        let task = tokio::spawn(async move { agents::run_turn(&hub.cfg, spec, etx, tok).await });

        let mut said = String::new();
        let mut typing = String::new();
        let mut context_changed = false;
        while let Some(ev) = erx.recv().await {
            match ev {
                AgentEvent::Delta(chunk) => {
                    typing.push_str(&chunk);
                    self.emit(Event::Delta { room_id: room_id.clone(), agent, text: typing.clone() });
                    continue;
                }
                AgentEvent::Limits(l) => {
                    self.set_limits(agent, l);
                    continue;
                }
                _ => {}
            }
            let mut rooms = self.rooms.lock().unwrap();
            let Some(st) = rooms.get_mut(&room_id) else { break };
            match ev {
                AgentEvent::Session(id) => {
                    st.room.sessions.insert(agent, id);
                    let _ = self.store.save_room(&st.room);
                }
                AgentEvent::Delta(_) | AgentEvent::Limits(_) => {}
                AgentEvent::Context { used, window } => {
                    let c = st.room.context.entry(agent).or_insert(ContextUsage::default());
                    if let Some(u) = used {
                        c.used = u;
                    }
                    if window.is_some() {
                        c.window = window;
                    }
                    context_changed = true;
                }
                AgentEvent::Text(t) => {
                    typing.clear();
                    said.push_str(&t);
                    said.push('\n');
                    self.push(st, agent.into(), MessageKind::Chat, t);
                }
                AgentEvent::Activity(a) => {
                    self.push(st, agent.into(), MessageKind::Activity, a);
                }
                AgentEvent::Error(e) => {
                    self.push(st, agent.into(), MessageKind::Error, e);
                }
            }
        }

        let failure = match task.await {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(format!("{e:#}")),
            Err(e) => Some(format!("сбой задачи: {e}")),
        };
        self.finish(&room_id, agent, &said, failure, token.is_cancelled(), context_changed);
    }

    fn finish(self: &Arc<Self>, room_id: &str, agent: AgentId, said: &str, failure: Option<String>, cancelled: bool, context_changed: bool) {
        let mut rooms = self.rooms.lock().unwrap();
        let Some(st) = rooms.get_mut(room_id) else { return };
        st.running.remove(&agent);
        if let Err(e) = self.store.save_room(&st.room) {
            tracing::error!("не удалось сохранить комнату: {e}");
        }
        if context_changed {
            self.emit(Event::RoomUpdated { room: st.room.clone() });
        }
        if let Some(f) = failure {
            self.push(st, agent.into(), MessageKind::Error, f);
        }
        self.emit(Event::AgentStatus { room_id: room_id.to_string(), agent, busy: false });
        if cancelled {
            return;
        }

        for other in router::agent_targets(agent, said) {
            if st.hops >= self.cfg.max_agent_hops {
                self.push(
                    st,
                    Author::System,
                    MessageKind::Chat,
                    format!(
                        "Агенты обменялись {} сообщениями подряд. Пауза до вашего ответа.",
                        self.cfg.max_agent_hops
                    ),
                );
                break;
            }
            st.hops += 1;
            self.schedule(st, other);
        }
        if st.pending.remove(&agent) {
            self.schedule(st, agent);
        }
    }
}

fn format_prompt(agent: AgentId, fresh: &[&Message]) -> String {
    let mut s = String::from("Новые сообщения в общем чате Mecode:\n\n");
    for m in fresh {
        s.push_str(&format!("[{}]: {}\n", m.author.name(), m.text));
        if !m.attachments.is_empty() {
            s.push_str("Вложения (прочитай с диска):\n");
            for a in &m.attachments {
                s.push_str(&format!("- {}: {}\n", a.name, a.path.display()));
            }
        }
        s.push('\n');
    }
    s.push_str(&format!("Ответь как {}.", agent.display()));
    s
}

/// Параметры сообщения пользователя.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct SendOptions {
    /// `id` вложений, загруженных через `add_attachment`.
    pub attachments: Vec<String>,
    pub plan: bool,
}

/// История для нового чата: последние реплики, обрезанные по длине.
fn handoff_transcript(messages: &[Message]) -> String {
    const MAX_MSG: usize = 1500;
    const MAX_TOTAL: usize = 30_000;
    let mut parts: Vec<String> = Vec::new();
    let mut total = 0;
    for m in messages.iter().rev().filter(|m| m.kind == MessageKind::Chat) {
        let line = format!("[{}]: {}", m.author.name(), agents_clip(&m.text, MAX_MSG));
        total += line.len();
        if total > MAX_TOTAL {
            break;
        }
        parts.push(line);
    }
    parts.reverse();
    parts.join("\n\n")
}

fn agents_clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max { s.to_string() } else { format!("{}…", s.chars().take(max).collect::<String>()) }
}

fn system_prompt(agent: AgentId) -> String {
    let me = agent.display();
    let other = agent.other().display();
    let other_tag = agent.other().name();
    format!(
        "You are {me}, a coding agent inside Mecode — a shared group chat with a human user and another AI coding agent, {other}. \
Both of you work in the same project folder.\n\
- You receive every chat message, prefixed with its author: [user], [{other_tag}], [system].\n\
- Whatever text you write is posted to the chat for everyone to read. Do not prefix your reply with your own name.\n\
- To ask {other} something, hand over work or request a review, mention @{other_tag} in your reply. That wakes {other} up. \
Do not mention @{other_tag} when you need nothing from {other}, otherwise you two loop forever.\n\
- Coordinate before editing: say which files you are going to change, and do not edit files {other} said it is working on.\n\
- The user may address only one of you. If the message was not for you and you have nothing useful to add, reply briefly.\n\
- Reply in the language the user writes in."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(seq: u64, author: Author, text: &str) -> Message {
        Message { seq, author, kind: MessageKind::Chat, text: text.into(), ts: Utc::now(), attachments: Vec::new(), plan: false }
    }

    #[test]
    fn prompt_lists_authors() {
        let a = msg(1, Author::User, "сделай тесты");
        let b = msg(2, Author::Codex, "@claude я беру api.rs");
        let p = format_prompt(AgentId::Claude, &[&a, &b]);
        assert!(p.contains("[user]: сделай тесты"));
        assert!(p.contains("[codex]: @claude я беру api.rs"));
        assert!(p.ends_with("Ответь как Claude."));
    }

    #[test]
    fn creates_room_and_persists() {
        let dir = std::env::temp_dir().join(format!("mecode-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let hub = Hub::new(Config::default(), &dir).unwrap();
        let room = hub.create_room("", dir.to_str().unwrap()).unwrap();
        hub.set_target(&room.id, Target::Codex).unwrap();
        hub.rename(&room.id, "Мой чат").unwrap();
        drop(hub);
        let hub = Hub::new(Config::default(), &dir).unwrap();
        let rooms = hub.list_rooms();
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].room.default_target, Target::Codex);
        assert_eq!(rooms[0].room.title, "Мой чат");

        let att = hub.add_attachment(&room.id, "..\\..\\x/../ev il?.PNG", b"img").unwrap();
        assert_eq!(att.name, "ev il_.PNG");
        assert!(att.is_image);
        assert!(att.path.starts_with(std::path::absolute(dir.join("rooms").join(&room.id)).unwrap()));
        assert!(hub.add_attachment(&room.id, "a.txt", b"").is_err());

        let p = hub.save_prompt(SavedPrompt { id: String::new(), title: String::new(), text: "Проверь тесты".into() }).unwrap();
        assert_eq!(p.title, "Проверь тесты");
        hub.save_prompt(SavedPrompt { title: "T".into(), ..p.clone() }).unwrap();
        assert_eq!(hub.prompts().len(), 1);
        assert_eq!(hub.prompts()[0].title, "T");
        hub.delete_prompt(&p.id).unwrap();
        assert!(hub.prompts().is_empty());

        hub.delete(&room.id).unwrap();
        assert!(hub.list_rooms().is_empty());
        assert!(!dir.join("rooms").join(&room.id).exists());
        assert!(hub.delete("../x").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
