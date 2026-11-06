//! Proposed fixes: checking them against the code, and writing them to the working tree.
//!
//! A fix is a list of exact find-and-replace edits on the new side of the comparison.
//! Every `find` must occur exactly once, so an edit can never land in the wrong place.

use crate::git::{self, safe_join, Spec};
use crate::model::{Edit, Fix};
use anyhow::{bail, Result};
use std::collections::BTreeMap;

/// Kinds of finding that may carry a fix.
pub fn kind_allows_fix(kind: &str) -> bool {
    matches!(kind, "bug" | "security")
}

pub(crate) fn clean_path(p: &str) -> String {
    let p = p.trim();
    p.strip_prefix("./")
        .or_else(|| p.strip_prefix("a/"))
        .or_else(|| p.strip_prefix("b/"))
        .unwrap_or(p)
        .to_string()
}

/// Applies the edits in memory. `read` gives a file's current contents.
/// Returns the new contents of every touched file, and fills in each edit's line.
fn apply_in_memory(
    edits: &mut [Edit],
    mut read: impl FnMut(&str) -> Option<String>,
) -> Result<BTreeMap<String, String>> {
    if edits.is_empty() {
        bail!("the fix has no edits");
    }
    let mut files: BTreeMap<String, String> = BTreeMap::new();
    for e in edits.iter_mut() {
        e.file = clean_path(&e.file);
        if e.find.is_empty() {
            bail!("an edit to {} has no text to find", e.file);
        }
        if e.find == e.replace {
            bail!("an edit to {} changes nothing", e.file);
        }
        if !files.contains_key(&e.file) {
            let Some(body) = read(&e.file) else { bail!("{} doesn't exist", e.file) };
            files.insert(e.file.clone(), body);
        }
        let body = files.get_mut(&e.file).unwrap();
        let mut hits = body.match_indices(&e.find);
        let Some((at, _)) = hits.next() else { bail!("the text to replace isn't in {}", e.file) };
        if hits.next().is_some() {
            bail!("the text to replace appears more than once in {}", e.file);
        }
        e.line = body[..at].matches('\n').count() as u32 + 1;
        body.replace_range(at..at + e.find.len(), &e.replace);
    }
    Ok(files)
}

/// Checks a model-proposed fix against the new side. On success the edits are
/// normalised (paths cleaned, lines filled in) and `applicable` is set.
pub fn check(spec: &Spec, fix: &mut Fix) -> Result<()> {
    apply_in_memory(&mut fix.edits, |p| git::read_side(spec, "new", p))?;
    fix.applicable = matches!(spec, Spec::Worktree { .. });
    Ok(())
}

/// Writes a fix to the working tree. Every edit is checked against the files as
/// they are now before anything is written, so a fix either applies fully or not at all.
pub fn apply(spec: &Spec, fix: &Fix) -> Result<Vec<String>> {
    if !matches!(spec, Spec::Worktree { .. }) {
        bail!("fixes can only be applied when comparing against the working tree");
    }
    let root = git::toplevel()?;
    let mut edits = fix.edits.clone();
    for e in &edits {
        safe_join(&root, &clean_path(&e.file))?;
    }
    let files = apply_in_memory(&mut edits, |p| std::fs::read_to_string(root.join(p)).ok())
        .map_err(|e| anyhow::anyhow!("{e}. The file may have changed since the review; re-run it"))?;
    for (path, body) in &files {
        std::fs::write(safe_join(&root, path)?, body)?;
    }
    Ok(files.into_keys().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(file: &str, find: &str, replace: &str) -> Edit {
        Edit { file: file.into(), find: find.into(), replace: replace.into(), line: 0 }
    }

    #[test]
    fn applies_unique_edits_in_order() {
        let mut edits = vec![edit("./a.ts", "b()", "c()"), edit("a.ts", "c();\nd", "c();\ne")];
        let out = apply_in_memory(&mut edits, |_| Some("a();\nb();\nd();\n".into())).unwrap();
        assert_eq!(out["a.ts"], "a();\nc();\ne();\n");
        assert_eq!(edits[0].line, 2);
        assert_eq!(edits[0].file, "a.ts");
    }

    #[test]
    fn rejects_missing_or_ambiguous_text() {
        let src = || Some("x();\nx();\n".to_string());
        assert!(apply_in_memory(&mut [edit("a", "x();", "y();")], |_| src()).is_err());
        assert!(apply_in_memory(&mut [edit("a", "z();", "y();")], |_| src()).is_err());
        assert!(apply_in_memory(&mut [edit("a", "", "y")], |_| src()).is_err());
        assert!(apply_in_memory(&mut [edit("missing", "x", "y")], |_| None).is_err());
    }

    #[test]
    fn refuses_paths_outside_repo() {
        let root = std::path::Path::new("/repo");
        assert!(safe_join(root, "../etc/passwd").is_err());
        assert!(safe_join(root, "/etc/passwd").is_err());
        assert!(safe_join(root, "src/a.ts").is_ok());
    }
}
