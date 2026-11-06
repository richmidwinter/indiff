//! Finds agent session transcripts for a repository, so intent is picked up
//! without the user doing anything. Claude Code keeps one JSONL file per session
//! under ~/.claude/projects/<repo path with non-alphanumerics replaced by '-'>/.

use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, Serialize)]
pub struct Transcript {
    pub path: String,
    pub agent: String,
    /// Seconds since the Unix epoch.
    pub modified: u64,
    pub first_prompt: String,
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn encode(p: &Path) -> String {
    p.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

fn first_prompt(path: &Path) -> String {
    let Ok(raw) = std::fs::read_to_string(path) else { return String::new() };
    for line in raw.lines().take(400) {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v.get("type").and_then(|t| t.as_str()) != Some("user") {
            continue;
        }
        let text = match v.pointer("/message/content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(a)) => a
                .iter()
                .filter(|i| i.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|i| i.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(" "),
            _ => continue,
        };
        let t = text.trim();
        if !t.is_empty() && !t.starts_with('<') {
            return crate::context::truncate(t, 160).replace("\n[truncated]", "…");
        }
    }
    String::new()
}

/// Claude Code sessions for this repository, newest first.
pub fn for_repo(repo: &Path) -> Vec<Transcript> {
    let Some(home) = home() else { return vec![] };
    let repo = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let dir = home.join(".claude").join("projects").join(encode(&repo));
    let Ok(entries) = std::fs::read_dir(&dir) else { return vec![] };
    let mut out: Vec<Transcript> = entries
        .flatten()
        .filter(|e| e.path().extension().map(|x| x == "jsonl").unwrap_or(false))
        .map(|e| {
            let p = e.path();
            let modified = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            Transcript {
                first_prompt: first_prompt(&p),
                path: p.to_string_lossy().into_owned(),
                agent: "Claude Code".into(),
                modified,
            }
        })
        .collect();
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    out.truncate(20);
    out
}
