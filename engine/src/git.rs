//! Thin wrappers over the git command line. Each thread has its own
//! repository, set with `set_repo`, so concurrent app commands don't clash.

use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::cell::RefCell;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

thread_local! {
    static REPO: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

pub fn set_repo(p: &Path) {
    REPO.with(|r| *r.borrow_mut() = Some(p.to_path_buf()));
}

fn git() -> Command {
    let mut c = Command::new("git");
    REPO.with(|r| {
        if let Some(p) = r.borrow().as_ref() {
            c.arg("-C").arg(p);
        }
    });
    c
}

/// Which two versions are being compared.
#[derive(Debug, Clone)]
pub enum Spec {
    /// Working tree against a revision (plain `git diff` / `git diff REV`).
    Worktree { base: String },
    /// Index against HEAD (`git diff --staged`).
    Staged,
    /// Two revisions.
    Revs { old: String, new: String },
}

impl Spec {
    pub fn describe(&self) -> String {
        match self {
            Spec::Worktree { base } if is_empty_tree(base) => "no commits yet → working tree".to_string(),
            Spec::Worktree { base } => format!("{base} → working tree"),
            Spec::Staged if !has_commits() => "no commits yet → staged".to_string(),
            Spec::Staged => "HEAD → staged".to_string(),
            Spec::Revs { old, new } => format!("{} → {}", short(old), short(new)),
        }
    }
}

fn short(rev: &str) -> String {
    if rev.len() == 40 && rev.chars().all(|c| c.is_ascii_hexdigit()) {
        rev[..10].to_string()
    } else {
        rev.to_string()
    }
}

pub fn run(args: &[&str]) -> Result<String> {
    let out = git()
        .args(args)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn try_run(args: &[&str]) -> Option<String> {
    run(args).ok()
}

pub fn toplevel() -> Result<PathBuf> {
    Ok(PathBuf::from(run(&["rev-parse", "--show-toplevel"])?.trim()))
}

pub fn git_dir() -> Result<PathBuf> {
    Ok(PathBuf::from(run(&["rev-parse", "--absolute-git-dir"])?.trim()))
}

/// Local branches, most recently committed first.
pub fn branches() -> Vec<String> {
    try_run(&["for-each-ref", "--sort=-committerdate", "--format=%(refname:short)", "refs/heads"])
        .map(|s| s.lines().map(|l| l.to_string()).collect())
        .unwrap_or_default()
}

/// Whether HEAD points at a commit. False in a repository made with `git init` and never committed to.
pub fn has_commits() -> bool {
    try_run(&["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]).is_some()
}

/// The empty tree for this repository's hash format, used as the base before the first commit.
fn empty_tree() -> Result<String> {
    Ok(run(&["hash-object", "-t", "tree", "/dev/null"])?.trim().to_string())
}

fn is_empty_tree(rev: &str) -> bool {
    // SHA-1 and SHA-256 repositories respectively.
    rev == "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
        || rev == "6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321"
}

/// Files git isn't tracking yet (respecting .gitignore).
pub fn untracked_count() -> usize {
    try_run(&["ls-files", "--others", "--exclude-standard"]).map(|s| s.lines().count()).unwrap_or(0)
}

/// Turn revision arguments into a comparison and the matching `git diff` arguments.
pub fn parse_spec(args: &[String], staged: bool) -> Result<(Spec, Vec<String>)> {
    let args: Vec<String> = args.iter().map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect();
    if staged {
        if !args.is_empty() {
            bail!("staged changes can't be combined with revisions");
        }
        return Ok((Spec::Staged, vec!["--cached".into()]));
    }
    if !has_commits() {
        if !args.is_empty() {
            bail!(
                "This repository has no commits yet, so there's nothing to compare {} against. \
                 Clear the Compare box to review the files you've added, or make a first commit.",
                args.join(" ")
            );
        }
        // Before the first commit, everything added to git is new: compare against the empty tree.
        let base = empty_tree()?;
        return Ok((Spec::Worktree { base: base.clone() }, vec![base]));
    }
    match args.as_slice() {
        [] => Ok((Spec::Worktree { base: "HEAD".into() }, vec!["HEAD".into()])),
        [one] if one.contains("...") => {
            let (a, b) = one.split_once("...").unwrap();
            let a = if a.is_empty() { "HEAD" } else { a };
            let b = if b.is_empty() { "HEAD" } else { b };
            let base = run(&["merge-base", a, b])?.trim().to_string();
            Ok((Spec::Revs { old: base, new: b.into() }, vec![one.clone()]))
        }
        [one] if one.contains("..") => {
            let (a, b) = one.split_once("..").unwrap();
            let a = if a.is_empty() { "HEAD" } else { a };
            let b = if b.is_empty() { "HEAD" } else { b };
            Ok((Spec::Revs { old: a.into(), new: b.into() }, vec![one.clone()]))
        }
        [one] => Ok((Spec::Worktree { base: one.clone() }, vec![one.clone()])),
        [a, b] => Ok((Spec::Revs { old: a.clone(), new: b.clone() }, vec![a.clone(), b.clone()])),
        _ => bail!("expected at most two revisions, for example main..HEAD"),
    }
}

pub fn diff(diff_args: &[String]) -> Result<String> {
    // Fixed prefixes, so diff.noprefix or diff.mnemonicPrefix can't change the paths diff::strip_prefix sees.
    let mut args: Vec<&str> = vec!["diff", "--no-color", "--no-ext-diff", "--src-prefix=a/", "--dst-prefix=b/", "-U3", "-M"];
    args.extend(diff_args.iter().map(|s| s.as_str()));
    run(&args)
}

/// A repo-relative path that stays inside the repository.
pub fn safe_join(root: &Path, rel: &str) -> Result<PathBuf> {
    let p = Path::new(rel);
    if p.is_absolute() || p.components().any(|c| !matches!(c, Component::Normal(_))) {
        bail!("refusing a path outside the repository: {rel}");
    }
    Ok(root.join(p))
}

/// Contents of a file on one side of the comparison, or None if it does not exist there.
/// Paths come from the webview and the model, so anything outside the repository is refused.
pub fn read_side(spec: &Spec, side: &str, path: &str) -> Option<String> {
    let old = side == "old";
    let rel = safe_join(Path::new(""), path).ok()?;
    match (spec, old) {
        (Spec::Worktree { base }, true) => try_run(&["show", &format!("{base}:{path}")]),
        (Spec::Worktree { .. }, false) => std::fs::read_to_string(toplevel().ok()?.join(rel)).ok(),
        (Spec::Staged, true) => try_run(&["show", &format!("HEAD:{path}")]),
        (Spec::Staged, false) => try_run(&["show", &format!(":{path}")]),
        (Spec::Revs { old: rev, .. }, true) | (Spec::Revs { new: rev, .. }, false) => {
            try_run(&["show", &format!("{rev}:{path}")])
        }
    }
}

fn grep(spec: &Spec, word: &str, context: bool) -> String {
    let mut args: Vec<String> = vec!["grep".into(), "-n".into(), "-w".into(), "-I".into(), "--no-color".into()];
    if context {
        args.push("-C2".into());
    }
    if let Spec::Staged = spec {
        args.insert(1, "--cached".into());
    }
    args.push("-e".into());
    args.push(word.into());
    if let Spec::Revs { new, .. } = spec {
        args.push(new.clone());
    }
    // git grep exits 1 when nothing matches, so read stdout whatever the status.
    let text = git()
        .args(&args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let prefix = match spec {
        Spec::Revs { new, .. } => format!("{new}:"),
        _ => String::new(),
    };
    text.lines()
        .map(|l| clip_line(l.strip_prefix(prefix.as_str()).unwrap_or(l)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Longest grep line kept. Minified or generated files (one-line JSON, bundles)
/// can match with a single multi-megabyte line.
const MAX_GREP_LINE: usize = 300;

fn clip_line(l: &str) -> String {
    if l.len() <= MAX_GREP_LINE {
        return l.to_string();
    }
    let mut end = MAX_GREP_LINE;
    while !l.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} […line clipped]", &l[..end])
}

/// Places a symbol is mentioned on the new side, with two lines of context (for the model).
pub fn grep_word(spec: &Spec, word: &str) -> String {
    grep(spec, word, true)
}

#[derive(Debug, Clone, Serialize)]
pub struct Reference {
    pub symbol: String,
    pub file: String,
    pub line: u32,
    pub text: String,
    pub is_test: bool,
}

pub fn is_test_path(p: &str) -> bool {
    let l = p.to_lowercase();
    l.contains("test") || l.contains("spec") || l.contains("__tests__")
}

/// Exact places a symbol is used on the new side (for the impact panel).
pub fn references(spec: &Spec, word: &str, limit: usize) -> Vec<Reference> {
    let mut out = Vec::new();
    for l in grep(spec, word, false).lines() {
        let mut parts = l.splitn(3, ':');
        let (Some(f), Some(n), Some(t)) = (parts.next(), parts.next(), parts.next()) else { continue };
        let Ok(line) = n.parse() else { continue };
        out.push(Reference {
            symbol: word.to_string(),
            file: f.to_string(),
            line,
            text: t.trim().to_string(),
            is_test: is_test_path(f),
        });
        if out.len() >= limit {
            break;
        }
    }
    out
}

pub fn commit_messages(spec: &Spec) -> String {
    match spec {
        Spec::Revs { old, new } => {
            try_run(&["log", "--reverse", "--format=- %s%n%b", &format!("{old}..{new}")])
                .unwrap_or_default()
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod clip_tests {
    #[test]
    fn clips_long_lines_on_char_boundary() {
        let long = "é".repeat(1_000_000);
        let c = super::clip_line(&long);
        assert!(c.len() < 400);
        assert_eq!(super::clip_line("short"), "short");
    }
}
