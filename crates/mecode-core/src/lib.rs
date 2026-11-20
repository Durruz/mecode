//! Ядро Mecode: общий чат, в котором пользователь, Claude и Codex видят сообщения друг друга.

pub mod agents;
pub mod config;
pub mod hub;
pub mod model;
pub mod router;
pub mod store;

pub use config::Config;
pub use hub::Hub;
pub use model::{
    AgentId, AgentSettings, Author, ContextUsage, Event, LimitWindow, Limits, Message, MessageKind, Permission, Room,
    Target,
};
