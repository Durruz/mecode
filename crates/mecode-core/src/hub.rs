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

use anyhow::{Context, bail};
use chrono::Utc;
use serde::Serialize;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::agents::{self, AgentEvent, TurnSpec};
use crate::config::Config;
use crate::model::{AgentId, Author, Event, Message, MessageKind, Room, Target};
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

pub struct Hub {
    cfg: Config,
    store: Store,
    rooms: Mutex<HashMap<String, RoomState>>,
    tx: broadcast::Sender<Event>,
}

impl Hub {
    pub fn new(cfg: Config, data_dir: &Path) -> anyhow::Result<Arc<Self>> {
        let store = Store::new(data_dir)?;
        let mut rooms = HashMap::new();
        for (room, messages) in store.load_rooms()? {
            rooms.insert(
                room.id.clone(),
                RoomState { room, messages, running: HashMap::new(), pending: HashSet::new(), hops: 0 },
            );
        }
        let (tx, _) = broadcast::channel(1024);
        Ok(Arc::new(Self { cfg, store, rooms: Mutex::new(rooms), tx }))
    }

    pub fn config(&self) -> &Config {
        &self.cfg
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
        };
        self.store.save_room(&room)?;
        let state = RoomState { room: room.clone(), messages: Vec::new(), running: HashMap::new(), pending: HashSet::new(), hops: 0 };
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

    /// Сообщение пользователя в чат.
    pub fn send(self: &Arc<Self>, room_id: &str, text: &str) -> anyhow::Result<Message> {
        let text = text.trim();
        if text.is_empty() {
            bail!("пустое сообщение");
        }
        let mut rooms = self.rooms.lock().unwrap();
        let st = rooms.get_mut(room_id).context("комната не найдена")?;
        let msg = self.push(st, Author::User, MessageKind::Chat, text.to_string());
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
        let msg = Message { seq: st.next_seq(), author, kind, text, ts: Utc::now() };
        if let Err(e) = self.store.append_message(&st.room.id, &msg) {
            tracing::error!("не удалось сохранить сообщение: {e}");
        }
        st.messages.push(msg.clone());
        self.emit(Event::Message { room_id: st.room.id.clone(), message: msg.clone() });
        msg
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
        let prompt = format_prompt(agent, &fresh);
        st.room.cursors.insert(agent, last.seq);

        let spec = TurnSpec {
            agent,
            workspace: st.room.workspace.clone(),
            session: st.room.sessions.get(&agent).cloned(),
            prompt,
            system: system_prompt(agent),
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
        while let Some(ev) = erx.recv().await {
            if let AgentEvent::Delta(chunk) = &ev {
                typing.push_str(chunk);
                self.emit(Event::Delta { room_id: room_id.clone(), agent, text: typing.clone() });
                continue;
            }
            let mut rooms = self.rooms.lock().unwrap();
            let Some(st) = rooms.get_mut(&room_id) else { break };
            match ev {
                AgentEvent::Session(id) => {
                    st.room.sessions.insert(agent, id);
                    let _ = self.store.save_room(&st.room);
                }
                AgentEvent::Delta(_) => {}
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
        self.finish(&room_id, agent, &said, failure, token.is_cancelled());
    }

    fn finish(self: &Arc<Self>, room_id: &str, agent: AgentId, said: &str, failure: Option<String>, cancelled: bool) {
        let mut rooms = self.rooms.lock().unwrap();
        let Some(st) = rooms.get_mut(room_id) else { return };
        st.running.remove(&agent);
        if let Err(e) = self.store.save_room(&st.room) {
            tracing::error!("не удалось сохранить комнату: {e}");
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
        s.push_str(&format!("[{}]: {}\n\n", m.author.name(), m.text));
    }
    s.push_str(&format!("Ответь как {}.", agent.display()));
    s
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
        Message { seq, author, kind: MessageKind::Chat, text: text.into(), ts: Utc::now() }
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

        hub.delete(&room.id).unwrap();
        assert!(hub.list_rooms().is_empty());
        assert!(!dir.join("rooms").join(&room.id).exists());
        assert!(hub.delete("../x").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
