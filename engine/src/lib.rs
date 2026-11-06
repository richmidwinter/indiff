//! indiff engine: everything the desktop app needs, with no UI.
//!
//! `load` is instant (git only). `review` calls the model, then checks every
//! citation against the real files before returning anything.

pub mod context;
pub mod diff;
pub mod fix;
pub mod git;
pub mod llm;
pub mod model;
pub mod prompt;
pub mod schema;
pub mod transcripts;
pub mod validate;

use anyhow::{Context as _, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct Loaded {
    pub repo: String,
    pub comparison: String,
    pub files: Vec<diff::FileDiff>,
    pub hunks: Vec<diff::Hunk>,
    pub adds: usize,
    pub dels: usize,
    /// Where changed symbols are used outside the changed files.
    pub references: Vec<git::Reference>,
    /// A key that changes whenever the diff changes (for remembering reviewed hunks).
    pub diff_key: String,
}

fn open(repo: &Path, revs: &[String], staged: bool, ignore_ws: bool) -> Result<(git::Spec, diff::Parsed, String)> {
    git::set_repo(repo);
    git::toplevel().context("not a git repository")?;
    let (spec, mut args) = git::parse_spec(revs, staged)?;
    if ignore_ws {
        args.insert(0, "-w".into());
    }
    let raw = git::diff(&args)?;
    Ok((spec, diff::parse(&raw), raw))
}

fn key_of(raw: &str) -> String {
    format!("{:x}", Sha256::digest(raw.as_bytes()))[..16].to_string()
}

/// The `diff_key` that `load` would return, without the parsing and searching.
/// Cheap enough to poll, so the app can notice when files change.
pub fn diff_key(repo: &Path, revs: &[String], staged: bool, ignore_ws: bool) -> Result<String> {
    let (_, _, raw) = open(repo, revs, staged, ignore_ws)?;
    Ok(key_of(&raw))
}

/// Reads the diff. `ignore_ws` compares ignoring whitespace (`git diff -w`).
pub fn load(repo: &Path, revs: &[String], staged: bool, ignore_ws: bool) -> Result<Loaded> {
    let (spec, parsed, raw) = open(repo, revs, staged, ignore_ws)?;
    if parsed.hunks.is_empty() && !git::has_commits() {
        match git::untracked_count() {
            0 => anyhow::bail!("This repository is empty: no commits and no files yet, so there's nothing to review."),
            n => anyhow::bail!(
                "This repository has no commits yet, and {} been added to git, so there's nothing to compare. \
                 Run `git add` on the files you want reviewed, then open it again.",
                if n == 1 { "its 1 file hasn't".to_string() } else { format!("none of its {n} files have") }
            ),
        }
    }
    let changed: BTreeSet<String> = parsed.files.iter().filter_map(|f| f.new_path.clone()).collect();
    let mut references = Vec::new();
    for s in context::symbols(&spec, &parsed, 15) {
        let is_import = |t: &str| {
            let t = t.trim_start();
            t.starts_with("import ") || t.starts_with("use ") || t.starts_with("from ") || t.starts_with("#include") || t.contains("require(")
        };
        references.extend(
            git::references(&spec, &s, 80)
                .into_iter()
                // Keep tests even when they changed: they show what reaches the new code.
                .filter(|r| (r.is_test || !changed.contains(&r.file)) && !is_import(&r.text))
                .take(12),
        );
    }
    let adds = parsed.hunks.iter().flat_map(|h| &h.lines).filter(|l| l.kind == '+').count();
    let dels = parsed.hunks.iter().flat_map(|h| &h.lines).filter(|l| l.kind == '-').count();
    let diff_key = key_of(&raw);
    Ok(Loaded {
        repo: git::toplevel()?.to_string_lossy().into_owned(),
        comparison: spec.describe(),
        files: parsed.files,
        hunks: parsed.hunks,
        adds,
        dels,
        references,
        diff_key,
    })
}

#[derive(Debug, Clone, Default)]
pub struct ReviewInput {
    pub intent: Option<String>,
    pub transcript: Option<String>,
    /// Ignore any cached review.
    pub force: bool,
    /// Use this model reply instead of calling the API (tests and demos).
    pub canned_reply: Option<String>,
    /// Review the diff with whitespace changes ignored (`git diff -w`).
    pub ignore_ws: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewOutput {
    pub review: model::Review,
    pub check: model::CheckReport,
    pub intent_sources: Vec<String>,
    pub model: String,
    pub context_truncated: bool,
    pub cached: bool,
    /// "fast" or "thorough".
    pub speed: String,
    /// How long the model call took, and how many tokens it generated (None when cached).
    pub elapsed_ms: Option<u64>,
    pub output_tokens: Option<u64>,
    /// The diff this review was built on. The files can change while the model runs,
    /// so the app matches these hunks to the diff on screen by content.
    pub diff_key: String,
    pub hunks: Vec<diff::Hunk>,
}

pub fn review(repo: &Path, revs: &[String], staged: bool, input: &ReviewInput, settings: &llm::Settings) -> Result<ReviewOutput> {
    let (spec, parsed, raw) = open(repo, revs, staged, input.ignore_ws)?;
    if parsed.hunks.is_empty() {
        anyhow::bail!("no changes to review");
    }
    let intent = context::gather_intent(&spec, input.intent.as_deref(), None, input.transcript.as_deref())?;
    let ctx = context::build(&spec, &parsed, &intent, &context::Limits::default())?;
    let cfg = llm::Config::resolve(settings);

    let parse = |text: &str| -> Result<model::Review> {
        let value = llm::extract_json(text)?;
        serde_json::from_value(value).context("model reply did not match the review format")
    };

    let (mut elapsed_ms, mut output_tokens) = (None, None);
    let (rv, cached) = if let Some(r) = &input.canned_reply {
        (parse(r)?, false)
    } else {
        let mut h = Sha256::new();
        for part in [prompt::VERSION, &cfg.provider, &cfg.model, &cfg.speed, prompt::SYSTEM, &ctx.prompt] {
            h.update(part.as_bytes());
            h.update([0u8]);
        }
        let dir = git::git_dir()?.join("indiff-cache");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{:x}.json", h.finalize()));
        // A cached reply that doesn't parse (saved by an older version) is ignored, not fatal.
        let hit = if input.force { None } else { std::fs::read_to_string(&path).ok().and_then(|t| parse(&t).ok()) };
        if let Some(rv) = hit {
            (rv, true)
        } else {
            // Structured outputs guarantee valid JSON. Without them, a model can slip
            // (an unescaped quote in quoted code), so one retry is worth it.
            let structured = cfg.provider != "openai" && schema::supports_structured(&cfg.model);
            let attempts = if structured { 1 } else { 2 };
            let started = std::time::Instant::now();
            let mut result = None;
            let mut last_err = None;
            for _ in 0..attempts {
                let r = llm::complete(&cfg, prompt::SYSTEM, &ctx.prompt)?;
                if let Some(t) = r.output_tokens {
                    output_tokens = Some(output_tokens.unwrap_or(0) + t);
                }
                match parse(&r.text) {
                    Ok(rv) => {
                        // Only replies that parse are cached, so a bad one is never replayed.
                        std::fs::write(&path, &r.text)?;
                        result = Some(rv);
                        break;
                    }
                    Err(e) => last_err = Some(e),
                }
            }
            elapsed_ms = Some(started.elapsed().as_millis() as u64);
            match result {
                Some(rv) => (rv, false),
                None => {
                    let tries = if attempts > 1 { ", twice" } else { "" };
                    return Err(last_err.unwrap().context(format!(
                        "the model's reply wasn't a valid review{tries}. Re-run the review to try again"
                    )));
                }
            }
        }
    };

    let (rv, check) = validate::validate(&spec, &parsed.hunks, &intent.text, rv);
    Ok(ReviewOutput {
        review: rv,
        check,
        intent_sources: intent.sources,
        model: cfg.model,
        context_truncated: ctx.truncated,
        cached,
        speed: cfg.speed,
        elapsed_ms,
        output_tokens,
        diff_key: key_of(&raw),
        hunks: parsed.hunks,
    })
}

/// The prompt that would be sent, for the "show what the model sees" view.
pub fn prompt_preview(repo: &Path, revs: &[String], staged: bool, input: &ReviewInput) -> Result<String> {
    let (spec, parsed, _) = open(repo, revs, staged, input.ignore_ws)?;
    let intent = context::gather_intent(&spec, input.intent.as_deref(), None, input.transcript.as_deref())?;
    let ctx = context::build(&spec, &parsed, &intent, &context::Limits::default())?;
    Ok(format!("=== SYSTEM ===\n{}\n\n=== USER ===\n{}", prompt::SYSTEM, ctx.prompt))
}

/// Writes a proposed fix to the working tree. Returns the files it changed.
pub fn apply_fix(repo: &Path, revs: &[String], staged: bool, fix: &model::Fix) -> Result<Vec<String>> {
    git::set_repo(repo);
    let (spec, _) = git::parse_spec(revs, staged)?;
    fix::apply(&spec, fix)
}

pub fn read_file(repo: &Path, revs: &[String], staged: bool, side: &str, path: &str) -> Result<Option<String>> {
    git::set_repo(repo);
    let (spec, _) = git::parse_spec(revs, staged)?;
    Ok(git::read_side(&spec, side, path))
}

pub fn branches(repo: &Path) -> Vec<String> {
    git::set_repo(repo);
    git::branches()
}

#[cfg(test)]
mod repo_tests {
    use std::path::Path;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap().status.success();
        assert!(ok, "git {args:?} failed");
    }

    #[test]
    fn ignore_ws_and_diff_key() {
        let dir = std::env::temp_dir().join(format!("indiff-ws-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        git(&dir, &["config", "user.email", "t@example.com"]);
        git(&dir, &["config", "user.name", "T"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.join("b.txt"), "x\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "init"]);
        // a.txt: whitespace only. b.txt: a real change.
        std::fs::write(dir.join("a.txt"), "one  \n  two\n").unwrap();
        std::fs::write(dir.join("b.txt"), "y\n").unwrap();

        let all = super::load(&dir, &[], false, false).unwrap();
        let ws = super::load(&dir, &[], false, true).unwrap();
        assert_eq!(all.hunks.len(), 2);
        assert_eq!(ws.hunks.len(), 1);
        assert_eq!(ws.hunks[0].file, "b.txt");
        assert_eq!(super::diff_key(&dir, &[], false, false).unwrap(), all.diff_key);
        assert_eq!(super::diff_key(&dir, &[], false, true).unwrap(), ws.diff_key);

        std::fs::write(dir.join("b.txt"), "z\n").unwrap();
        assert_ne!(super::diff_key(&dir, &[], false, false).unwrap(), all.diff_key);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
