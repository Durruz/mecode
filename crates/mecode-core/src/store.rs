//! Хранение на диске: `rooms/<id>/room.json` и `rooms/<id>/messages.jsonl`.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::model::{Message, Room};

#[derive(Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(data_dir: &Path) -> anyhow::Result<Self> {
        let root = data_dir.join("rooms");
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn dir(&self, room_id: &str) -> PathBuf {
        self.root.join(room_id)
    }

    pub fn save_room(&self, room: &Room) -> anyhow::Result<()> {
        let dir = self.dir(&room.id);
        std::fs::create_dir_all(&dir)?;
        // Пишем во временный файл и переименовываем, чтобы не оставить битый JSON при сбое.
        let tmp = dir.join("room.json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(room)?)?;
        std::fs::rename(tmp, dir.join("room.json"))?;
        Ok(())
    }

    pub fn append_message(&self, room_id: &str, msg: &Message) -> anyhow::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir(room_id).join("messages.jsonl"))?;
        writeln!(f, "{}", serde_json::to_string(msg)?)?;
        Ok(())
    }

    /// Удаляет папку комнаты. Вызывается только для ID, найденного среди загруженных комнат.
    pub fn delete_room(&self, room_id: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            !room_id.is_empty() && room_id.chars().all(|c| c.is_ascii_alphanumeric()),
            "некорректный id комнаты"
        );
        let dir = self.dir(room_id);
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
        Ok(())
    }

    pub fn load_rooms(&self) -> anyhow::Result<Vec<(Room, Vec<Message>)>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let dir = entry?.path();
            let room_file = dir.join("room.json");
            if !room_file.exists() {
                continue;
            }
            let room: Room = match serde_json::from_str(&std::fs::read_to_string(&room_file)?) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("пропускаю {}: {e}", room_file.display());
                    continue;
                }
            };
            let mut messages = Vec::new();
            if let Ok(raw) = std::fs::read_to_string(dir.join("messages.jsonl")) {
                for line in raw.lines().filter(|l| !l.trim().is_empty()) {
                    match serde_json::from_str::<Message>(line) {
                        Ok(m) => messages.push(m),
                        Err(e) => tracing::warn!("битая строка в {}: {e}", dir.display()),
                    }
                }
            }
            out.push((room, messages));
        }
        out.sort_by_key(|(r, _)| r.created);
        Ok(out)
    }
}
