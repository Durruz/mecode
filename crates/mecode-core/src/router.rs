//! Кто должен ответить: разбор @упоминаний.

use crate::model::{AgentId, Target};

/// Агенты, упомянутые в тексте. `@all` / `@все` упоминают обоих.
pub fn mentions(text: &str) -> Vec<AgentId> {
    let lower = text.to_lowercase();
    let mut out = Vec::new();
    let has = |names: &[&str]| names.iter().any(|n| has_mention(&lower, n));
    let all = has(&["all", "все", "both", "оба"]);
    if all || has(&["claude", "клод"]) {
        out.push(AgentId::Claude);
    }
    if all || has(&["codex", "кодекс"]) {
        out.push(AgentId::Codex);
    }
    out
}

/// `@name`, за которым не идёт буква или цифра (чтобы `@claudeX` не считался).
fn has_mention(lower: &str, name: &str) -> bool {
    let pat = format!("@{name}");
    let mut start = 0;
    while let Some(pos) = lower[start..].find(&pat) {
        let at = start + pos;
        let end = at + pat.len();
        let before_ok = lower[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_');
        let after_ok = lower[end..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_');
        if before_ok && after_ok {
            return true;
        }
        start = end;
    }
    false
}

/// Кому уходит сообщение пользователя.
pub fn user_targets(text: &str, default: Target) -> Vec<AgentId> {
    let m = mentions(text);
    if !m.is_empty() {
        return m;
    }
    match default {
        Target::All => AgentId::ALL.to_vec(),
        Target::Claude => vec![AgentId::Claude],
        Target::Codex => vec![AgentId::Codex],
    }
}

/// Кого будит ответ агента: только другого агента, если тот упомянут.
pub fn agent_targets(from: AgentId, text: &str) -> Vec<AgentId> {
    mentions(text).into_iter().filter(|a| *a != from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mentions() {
        assert_eq!(mentions("@claude глянь"), vec![AgentId::Claude]);
        assert_eq!(mentions("эй @Codex, ты тут?"), vec![AgentId::Codex]);
        assert_eq!(mentions("@all привет"), AgentId::ALL.to_vec());
        assert_eq!(mentions("@все привет"), AgentId::ALL.to_vec());
        assert!(mentions("claude без собаки").is_empty());
        assert!(mentions("mail@codex.com").is_empty());
        assert!(mentions("@claudeX").is_empty());
    }

    #[test]
    fn user_routing_uses_default_without_mentions() {
        assert_eq!(user_targets("привет", Target::All), AgentId::ALL.to_vec());
        assert_eq!(user_targets("привет", Target::Codex), vec![AgentId::Codex]);
        assert_eq!(user_targets("@claude привет", Target::Codex), vec![AgentId::Claude]);
    }

    #[test]
    fn agent_does_not_wake_itself() {
        assert!(agent_targets(AgentId::Claude, "я @claude").is_empty());
        assert_eq!(agent_targets(AgentId::Claude, "@codex проверь"), vec![AgentId::Codex]);
        assert_eq!(agent_targets(AgentId::Codex, "@all готово"), vec![AgentId::Claude]);
    }
}
