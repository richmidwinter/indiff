//! Gathers what the reviewer model sees: intent, the numbered diff,
//! full old and new files, and where changed symbols are used elsewhere.

use crate::diff::{Hunk, Parsed};
use crate::git::{self, Spec};
use anyhow::Result;
use regex::Regex;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fmt::Write;

pub struct Limits {
    pub file_chars: usize,
    pub total_chars: usize,
    pub symbols: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { file_chars: 40_000, total_chars: 300_000, symbols: 15 }
    }
}

pub struct Intent {
    pub text: String,
    pub sources: Vec<String>,
}

pub fn gather_intent(
    spec: &Spec,
    intent: Option<&str>,
    intent_file: Option<&str>,
    transcript: Option<&str>,
) -> Result<Intent> {
    let mut text = String::new();
    let mut sources = Vec::new();
    if let Some(i) = intent {
        writeln!(text, "## Stated by the user\n{i}\n")?;
        sources.push("--intent".into());
    }
    if let Some(p) = intent_file {
        let body = std::fs::read_to_string(p)?;
        writeln!(text, "## From {p}\n{}\n", truncate(&body, 20_000))?;
        sources.push(p.to_string());
    }
    if let Some(p) = transcript {
        let t = read_transcript(p)?;
        if !t.is_empty() {
            writeln!(text, "## Agent session transcript ({p})\n{t}\n")?;
            sources.push(format!("transcript {p}"));
        }
    }
    let msgs = git::commit_messages(spec);
    if !msgs.trim().is_empty() {
        writeln!(text, "## Commit messages\n{}\n", truncate(&msgs, 10_000))?;
        sources.push("commit messages".into());
    }
    Ok(Intent { text, sources })
}

/// Pulls user requests and agent replies out of a Claude Code JSONL transcript.
/// Unknown formats fall back to the raw text.
fn read_transcript(path: &str) -> Result<String> {
    let raw = std::fs::read_to_string(path)?;
    let mut out = String::new();
    let mut parsed_any = false;
    for line in raw.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let role = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if role != "user" && role != "assistant" {
            continue;
        }
        parsed_any = true;
        let content = v.pointer("/message/content");
        let mut texts: Vec<String> = Vec::new();
        match content {
            Some(Value::String(s)) => texts.push(s.clone()),
            Some(Value::Array(items)) => {
                for it in items {
                    if it.get("type").and_then(|t| t.as_str()) == Some("text") {
                        if let Some(s) = it.get("text").and_then(|t| t.as_str()) {
                            texts.push(s.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
        for t in texts {
            let t = t.trim();
            // Skip injected system text and command wrappers.
            if t.is_empty() || t.starts_with('<') {
                continue;
            }
            let (label, cap) = if role == "user" { ("User", 3_000) } else { ("Agent", 800) };
            writeln!(out, "{label}: {}", truncate(t, cap))?;
        }
        if out.len() > 30_000 {
            out.push_str("[transcript truncated]\n");
            break;
        }
    }
    if !parsed_any {
        return Ok(truncate(&raw, 30_000));
    }
    Ok(out)
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[truncated]", &s[..end])
}

/// Appends `s` if it fits under `max`, otherwise as much as fits.
/// Returns false when `s` had to be cut, meaning the budget is spent.
fn push_within(p: &mut String, s: &str, max: usize) -> bool {
    let room = max.saturating_sub(p.len());
    if s.len() <= room {
        p.push_str(s);
        return true;
    }
    p.push_str(&truncate(s, room));
    p.push('\n');
    false
}

fn numbered(body: &str) -> String {
    let mut out = String::new();
    for (i, l) in body.lines().enumerate() {
        let _ = writeln!(out, "{:>5}| {}", i + 1, l);
    }
    out
}

pub fn render_hunk(h: &Hunk) -> String {
    let mut out = format!("### {} {}\n{}\n", h.id, h.file, h.header);
    out.push_str("  old   new\n");
    for l in &h.lines {
        let o = l.old.map(|v| v.to_string()).unwrap_or_default();
        let n = l.new.map(|v| v.to_string()).unwrap_or_default();
        let _ = writeln!(out, "{o:>5} {n:>5} {} {}", l.kind, l.text);
    }
    out
}

/// Names of functions, types and top-level values the change defines or modifies,
/// including the function that encloses each changed line.
pub fn symbols(spec: &Spec, parsed: &Parsed, max: usize) -> Vec<String> {
    // Definitions anywhere on a line, and top-level (unindented) bindings.
    let defs = Regex::new(
        r"(?:\bfn|\bdef|\bfunction|\bclass|\bfunc|\binterface|\bstruct|\btrait|\benum|\btype)\s+([A-Za-z_][A-Za-z0-9_]*)|^(?:export\s+|pub\s+)?(?:const|let|var|static)\s+([A-Za-z_][A-Za-z0-9_]*)\s*[=:]|^\s*(?:export\s+)?(?:async\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*\([^)]*\)\s*\{",
    )
    .unwrap();
    let skip = ["if", "for", "while", "switch", "return", "catch", "main", "new", "test", "self"];
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut push = |s: &str, out: &mut Vec<String>| {
        if s.len() >= 3 && !skip.contains(&s) && seen.insert(s.to_string()) {
            out.push(s.to_string());
        }
    };
    let first = |text: &str| -> Option<String> {
        defs.captures(text).and_then(|c| c.get(1).or(c.get(2)).or(c.get(3)).map(|m| m.as_str().to_string()))
    };
    let mut files: std::collections::HashMap<String, Vec<String>> = Default::default();
    for h in &parsed.hunks {
        if let Some(ctx) = h.header.splitn(3, "@@").nth(2) {
            if let Some(s) = first(ctx) {
                push(&s, &mut out);
            }
        }
        for l in h.lines.iter().filter(|l| l.kind != ' ') {
            for c in defs.captures_iter(&l.text) {
                if let Some(m) = c.get(1).or(c.get(2)).or(c.get(3)) {
                    push(m.as_str(), &mut out);
                }
            }
        }
        // Walk up from each changed new line to its enclosing definition.
        let lines = files
            .entry(h.file.clone())
            .or_insert_with(|| git::read_side(spec, "new", &h.file).map(|b| b.lines().map(String::from).collect()).unwrap_or_default());
        let mut done_above = 0usize;
        for start in h.lines.iter().filter(|l| l.kind == '+').filter_map(|l| l.new) {
            let start = (start as usize).min(lines.len());
            if start <= done_above {
                continue;
            }
            for i in (0..start).rev() {
                if let Some(s) = first(&lines[i]) {
                    push(&s, &mut out);
                    break;
                }
            }
            done_above = start;
        }
    }
    out.truncate(max);
    out
}

pub struct Context {
    pub prompt: String,
    pub symbols: Vec<String>,
    pub truncated: bool,
}

pub fn build(spec: &Spec, parsed: &Parsed, intent: &Intent, limits: &Limits) -> Result<Context> {
    let mut p = String::new();
    // truncated: something was cut, so the model is told. full: the budget is
    // spent, so later sections are skipped. One long file is truncated, not full.
    let mut truncated = false;
    let mut full = false;

    writeln!(p, "# Comparison\n{}\n", spec.describe())?;
    writeln!(p, "# Intent")?;
    if intent.text.trim().is_empty() {
        writeln!(p, "No intent was supplied. Infer the aims from the code and mark them source \"inferred\".\n")?;
    } else {
        writeln!(p, "{}", intent.text)?;
    }

    // Every section is added through push_within, so the prompt never exceeds
    // total_chars whatever the repository contains.
    let max = limits.total_chars;

    writeln!(p, "# Diff (hunk ids, old and new line numbers)\n")?;
    for h in &parsed.hunks {
        if !push_within(&mut p, &(render_hunk(h) + "\n"), max) {
            truncated = true;
            full = true;
            break;
        }
    }

    if !full {
        writeln!(p, "# Full files (line numbers match citations)\n")?;
    }
    'files: for f in parsed.files.iter().filter(|f| !f.binary) {
        if full {
            break;
        }
        let mut sides = Vec::new();
        if let Some(path) = &f.new_path {
            sides.push(("NEW", "new", path));
        }
        if let Some(path) = &f.old_path {
            let has_removals = parsed
                .hunks
                .iter()
                .any(|h| &h.old_file == path && h.lines.iter().any(|l| l.kind == '-'));
            if has_removals {
                sides.push(("OLD", "old", path));
            }
        }
        for (label, side, path) in sides {
            let Some(body) = git::read_side(spec, side, path) else { continue };
            if body.len() > limits.file_chars {
                truncated = true;
            }
            let section = format!("## {label} {path}\n{}\n", numbered(&truncate(&body, limits.file_chars)));
            if !push_within(&mut p, &section, max) {
                truncated = true;
                break 'files;
            }
        }
    }

    let syms = symbols(spec, parsed, limits.symbols);
    if !syms.is_empty() && p.len() < max {
        writeln!(p, "# References to changed symbols elsewhere (new side, path:line)\n")?;
        let changed: BTreeSet<String> = parsed.files.iter().filter_map(|f| f.new_path.clone()).collect();
        for s in &syms {
            let hits = git::grep_word(spec, s);
            let mut kept = String::new();
            for block in hits.split("\n--\n") {
                let file = block.split(|c| c == ':' || c == '-').next().unwrap_or("");
                if changed.contains(file) {
                    continue;
                }
                if kept.len() + block.len() > 6_000 {
                    kept.push_str("[more references omitted]\n");
                    break;
                }
                kept.push_str(block.trim_end());
                kept.push_str("\n--\n");
            }
            if !kept.trim().is_empty() && !push_within(&mut p, &format!("## {s}\n{kept}\n"), max) {
                truncated = true;
                break;
            }
        }
    }

    if truncated {
        writeln!(p, "\nNote: some context was truncated to fit. Say so in the verdict if it limits your review.")?;
    }
    Ok(Context { prompt: p, symbols: syms, truncated })
}

#[cfg(test)]
mod sym_tests {
    #[test]
    fn finds_symbols_in_example() {
        let repo = std::env::var("INDIFF_TEST_REPO").unwrap_or_default();
        if repo.is_empty() { return; }
        crate::git::set_repo(std::path::Path::new(&repo));
        let (spec, args) = crate::git::parse_spec(&["main..feature/webhook-retry".into()], false).unwrap();
        let p = crate::diff::parse(&crate::git::diff(&args).unwrap());
        let s = super::symbols(&spec, &p, 15);
        eprintln!("SYMS {:?}", s);
        eprintln!("REFS {:?}", crate::git::references(&spec, "deliver", 10));
    }
}

#[cfg(test)]
mod budget_tests {
    use super::push_within;

    #[test]
    fn push_within_never_exceeds_budget() {
        let mut p = String::from("head\n");
        assert!(push_within(&mut p, "small\n", 100));
        let huge = "x".repeat(3_500_000);
        assert!(!push_within(&mut p, &huge, 100));
        assert!(p.len() <= 100 + "\n[truncated]\n".len());
        // Once spent, nothing more gets in.
        let before = p.len();
        assert!(!push_within(&mut p, "more", 100));
        assert!(p.len() <= before + "\n[truncated]\n".len());
    }
}
