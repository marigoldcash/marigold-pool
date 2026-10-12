use serde::Serialize;
use std::fmt;

/// What a worker is called when its miner gave it no name.
pub const DEFAULT_WORKER: &str = "default";

/// The longest worker name kept, in characters. A longer one is cut.
pub const MAX_WORKER_NAME_CHARS: usize = 64;

/// A worker's name as the pool stores and shows it. Miners type these, so the
/// raw text is cleaned once, here: control characters become spaces, the ends
/// are trimmed, the length is capped, and an empty name becomes "default".
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct WorkerName(String);

impl WorkerName {
    pub fn new(raw: &str) -> Self {
        let cleaned: String = raw.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        let trimmed = cleaned.trim();
        if trimmed.is_empty() {
            return WorkerName(DEFAULT_WORKER.to_owned());
        }
        let capped: String = trimmed.chars().take(MAX_WORKER_NAME_CHARS).collect();
        WorkerName(capped.trim_end().to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkerName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_name_is_kept() {
        assert_eq!(WorkerName::new("asic1").as_str(), "asic1");
        assert_eq!(WorkerName::new("rig 2 (garage)").as_str(), "rig 2 (garage)");
    }

    #[test]
    fn an_empty_or_blank_name_becomes_default() {
        assert_eq!(WorkerName::new("").as_str(), "default");
        assert_eq!(WorkerName::new("   \t\n").as_str(), "default");
    }

    #[test]
    fn control_characters_never_survive() {
        assert_eq!(WorkerName::new("a\u{0}b\nc").as_str(), "a b c");
        assert_eq!(WorkerName::new("\u{7}\u{8}").as_str(), "default");
    }

    #[test]
    fn a_long_name_is_cut_by_characters_not_bytes() {
        let long: String = "é".repeat(200);
        let name = WorkerName::new(&long);
        assert_eq!(name.as_str().chars().count(), MAX_WORKER_NAME_CHARS);
    }

    #[test]
    fn a_cut_never_leaves_trailing_space() {
        let raw = format!("{} tail", "x".repeat(MAX_WORKER_NAME_CHARS - 1));
        let name = WorkerName::new(&raw);
        assert_eq!(name.as_str(), "x".repeat(MAX_WORKER_NAME_CHARS - 1));
    }
}
