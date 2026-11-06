//! Parses unified diff output into files, hunks and numbered lines.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Line {
    /// '+', '-' or ' '
    pub kind: char,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Hunk {
    pub id: String,
    pub file: String,
    pub old_file: String,
    pub header: String,
    pub lines: Vec<Line>,
}

impl Hunk {
    pub fn changed_lines(&self) -> usize {
        self.lines.iter().filter(|l| l.kind != ' ').count()
    }

    pub fn contains(&self, file: &str, side: &str, line: u32) -> bool {
        let old = side == "old";
        let f = if old { &self.old_file } else { &self.file };
        if f != file {
            return false;
        }
        self.lines
            .iter()
            .any(|l| if old { l.old == Some(line) } else { l.new == Some(line) })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FileDiff {
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub binary: bool,
}

impl FileDiff {
    pub fn display(&self) -> String {
        match (&self.old_path, &self.new_path) {
            (Some(o), Some(n)) if o != n => format!("{o} → {n}"),
            (_, Some(n)) => n.clone(),
            (Some(o), None) => format!("{o} (deleted)"),
            (None, None) => "?".into(),
        }
    }
}

pub struct Parsed {
    pub files: Vec<FileDiff>,
    pub hunks: Vec<Hunk>,
}

fn strip_prefix(p: &str) -> Option<String> {
    let p = p.trim_end_matches('\t').trim();
    if p == "/dev/null" {
        return None;
    }
    let p = p.trim_matches('"');
    Some(
        p.strip_prefix("a/")
            .or_else(|| p.strip_prefix("b/"))
            .unwrap_or(p)
            .to_string(),
    )
}

fn parse_range(tok: &str) -> u32 {
    tok[1..].split(',').next().and_then(|s| s.parse().ok()).unwrap_or(0)
}

pub fn parse(text: &str) -> Parsed {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut cur: Option<Hunk> = None;
    let (mut o, mut n) = (0u32, 0u32);
    let mut in_header = false;

    let flush = |cur: &mut Option<Hunk>, hunks: &mut Vec<Hunk>| {
        if let Some(h) = cur.take() {
            hunks.push(h);
        }
    };

    for raw in text.lines() {
        if let Some(rest) = raw.strip_prefix("diff --git ") {
            flush(&mut cur, &mut hunks);
            in_header = true;
            let (a, b) = match rest.rfind(" b/") {
                Some(i) => (&rest[..i], &rest[i + 1..]),
                None => (rest, rest),
            };
            files.push(FileDiff { old_path: strip_prefix(a), new_path: strip_prefix(b), binary: false });
            continue;
        }
        if in_header {
            if let Some(p) = raw.strip_prefix("--- ") {
                if let Some(f) = files.last_mut() {
                    f.old_path = strip_prefix(p);
                }
                continue;
            }
            if let Some(p) = raw.strip_prefix("+++ ") {
                if let Some(f) = files.last_mut() {
                    f.new_path = strip_prefix(p);
                }
                continue;
            }
            if raw.starts_with("Binary files ") {
                if let Some(f) = files.last_mut() {
                    f.binary = true;
                }
                continue;
            }
            if let Some(p) = raw.strip_prefix("rename to ") {
                if let Some(f) = files.last_mut() {
                    f.new_path = Some(p.to_string());
                }
                continue;
            }
            if let Some(p) = raw.strip_prefix("rename from ") {
                if let Some(f) = files.last_mut() {
                    f.old_path = Some(p.to_string());
                }
                continue;
            }
        }
        if raw.starts_with("@@ ") {
            flush(&mut cur, &mut hunks);
            in_header = false;
            let mut toks = raw.split_whitespace().skip(1);
            o = toks.next().map(parse_range).unwrap_or(0);
            n = toks.next().map(parse_range).unwrap_or(0);
            let file = files.last();
            let new_file = file.and_then(|f| f.new_path.clone().or(f.old_path.clone())).unwrap_or_default();
            let old_file = file.and_then(|f| f.old_path.clone().or(f.new_path.clone())).unwrap_or_default();
            cur = Some(Hunk {
                id: format!("H{}", hunks.len() + 1),
                file: new_file,
                old_file,
                header: raw.to_string(),
                lines: Vec::new(),
            });
            continue;
        }
        let Some(h) = cur.as_mut() else { continue };
        if raw.starts_with('\\') {
            continue;
        }
        let (kind, body) = match raw.chars().next() {
            Some('+') => ('+', &raw[1..]),
            Some('-') => ('-', &raw[1..]),
            Some(' ') => (' ', &raw[1..]),
            None => (' ', ""),
            _ => continue,
        };
        let line = match kind {
            '+' => {
                n += 1;
                Line { kind, old: None, new: Some(n - 1), text: body.to_string() }
            }
            '-' => {
                o += 1;
                Line { kind, old: Some(o - 1), new: None, text: body.to_string() }
            }
            _ => {
                o += 1;
                n += 1;
                Line { kind, old: Some(o - 1), new: Some(n - 1), text: body.to_string() }
            }
        };
        h.lines.push(line);
    }
    flush(&mut cur, &mut hunks);

    // Unified diff ranges are 1-based start lines; a start of 0 means an empty side.
    for h in &mut hunks {
        for l in &mut h.lines {
            if l.old == Some(0) {
                l.old = None;
            }
        }
    }
    Parsed { files, hunks }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_lines() {
        let d = "diff --git a/x.ts b/x.ts\n--- a/x.ts\n+++ b/x.ts\n@@ -1,3 +1,4 @@\n a\n-b\n+c\n+d\n e\n";
        let p = parse(d);
        assert_eq!(p.hunks.len(), 1);
        let l = &p.hunks[0].lines;
        assert_eq!((l[0].old, l[0].new), (Some(1), Some(1)));
        assert_eq!((l[1].old, l[1].new), (Some(2), None));
        assert_eq!((l[2].old, l[2].new), (None, Some(2)));
        assert_eq!((l[3].old, l[3].new), (None, Some(3)));
        assert_eq!((l[4].old, l[4].new), (Some(3), Some(4)));
    }

    #[test]
    fn new_file() {
        let d = "diff --git a/n.ts b/n.ts\nnew file mode 100644\n--- /dev/null\n+++ b/n.ts\n@@ -0,0 +1,2 @@\n+x\n+y\n";
        let p = parse(d);
        assert_eq!(p.files[0].old_path, None);
        assert_eq!(p.hunks[0].file, "n.ts");
        assert_eq!(p.hunks[0].lines[1].new, Some(2));
    }
}
