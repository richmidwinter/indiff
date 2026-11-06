//! Checks every citation against the real files. Claims left without a valid
//! citation are removed, so what indiff shows is always tied to code that exists.

use crate::diff::Hunk;
use crate::git::{self, Spec};
use crate::model::{CheckReport, Cite, Review};
use std::collections::{BTreeSet, HashMap};

struct Files<'a> {
    spec: &'a Spec,
    cache: HashMap<(String, String), Option<Vec<String>>>,
}

impl<'a> Files<'a> {
    fn lines(&mut self, side: &str, path: &str) -> Option<&Vec<String>> {
        let key = (side.to_string(), path.to_string());
        if !self.cache.contains_key(&key) {
            let body = git::read_side(self.spec, side, path).map(|b| b.lines().map(|l| l.to_string()).collect());
            self.cache.insert(key.clone(), body);
        }
        self.cache.get(&key).and_then(|v| v.as_ref())
    }
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

use crate::fix::clean_path;

enum Verdict {
    Ok(Cite),
    Corrected(Cite),
    Bad,
}

fn check(files: &mut Files, c: &Cite) -> Verdict {
    let side = if c.side == "old" { "old" } else { "new" };
    let path = clean_path(&c.file);
    let quote = norm(&c.quote);
    if quote.len() < 3 || c.line == 0 {
        return Verdict::Bad;
    }
    let Some(lines) = files.lines(side, &path) else { return Verdict::Bad };
    let fixed = |line: u32| Cite { file: path.clone(), side: side.to_string(), line, quote: c.quote.clone() };
    let at = |n: u32| lines.get(n as usize - 1).map(|l| norm(l).contains(&quote)).unwrap_or(false);
    if at(c.line) {
        return Verdict::Ok(fixed(c.line));
    }
    // Models are often a line or two out; accept a nearby exact match.
    for d in 1..=3u32 {
        if at(c.line + d) {
            return Verdict::Corrected(fixed(c.line + d));
        }
        if c.line > d && at(c.line - d) {
            return Verdict::Corrected(fixed(c.line - d));
        }
    }
    Verdict::Bad
}

fn check_all(files: &mut Files, cites: &[Cite], report: &mut CheckReport) -> Vec<Cite> {
    let mut out = Vec::new();
    for c in cites {
        match check(files, c) {
            Verdict::Ok(c) => out.push(c),
            Verdict::Corrected(c) => {
                report.corrected_citations += 1;
                out.push(c)
            }
            Verdict::Bad => report.dropped_citations += 1,
        }
    }
    out
}

pub fn validate(spec: &Spec, hunks: &[Hunk], intent_text: &str, mut review: Review) -> (Review, CheckReport) {
    let mut files = Files { spec, cache: HashMap::new() };
    let mut report = CheckReport::default();
    let intent_norm = norm(intent_text);
    for f in &mut review.findings {
        let q = norm(f.why.quote.trim_matches(|c| c == '"' || c == '“' || c == '”' || c == '…'));
        if !f.why.quote.is_empty() && (q.len() < 4 || !intent_norm.contains(&q)) {
            f.why = Default::default();
            report.dropped_why += 1;
        }
    }
    let hunk_ids: BTreeSet<String> = hunks.iter().map(|h| h.id.clone()).collect();

    let mut kept = Vec::new();
    for mut f in review.findings.drain(..) {
        f.evidence = check_all(&mut files, &f.evidence, &mut report);
        if f.evidence.is_empty() {
            report.dropped_findings.push(format!("{}: {}", f.id, f.title));
        } else {
            kept.push(f);
        }
    }
    review.findings = kept;
    for f in &mut review.findings {
        let Some(fix) = f.fix.as_mut() else { continue };
        let ok = if crate::fix::kind_allows_fix(&f.kind) {
            crate::fix::check(spec, fix).map_err(|e| e.to_string())
        } else {
            Err(format!("fixes are only offered for bugs and security findings, not {}", f.kind))
        };
        if let Err(why) = ok {
            report.dropped_fixes.push(format!("{}: {} ({why})", f.id, f.title));
            f.fix = None;
        }
    }
    let finding_ids: BTreeSet<String> = review.findings.iter().map(|f| f.id.clone()).collect();

    let mut rows = Vec::new();
    for mut r in review.behaviour.drain(..) {
        r.evidence = check_all(&mut files, &r.evidence, &mut report);
        if r.evidence.is_empty() {
            report.dropped_behaviour_rows.push(format!("{} · {}", r.subject, r.aspect));
            continue;
        }
        if let Some(id) = &r.finding {
            if !finding_ids.contains(id) {
                r.finding = None;
            }
        }
        rows.push(r);
    }
    review.behaviour = rows;

    for a in &mut review.aims {
        a.evidence = check_all(&mut files, &a.evidence, &mut report);
        a.hunks.retain(|h| {
            let ok = hunk_ids.contains(h);
            if !ok {
                report.unknown_hunks.push(h.clone());
            }
            ok
        });
    }
    for f in &mut review.findings {
        if let Some(a) = &f.aim {
            if !review.aims.iter().any(|x| &x.id == a) {
                f.aim = None;
            }
        }
    }

    let att = &mut review.attention;
    att.must_read.retain(|h| {
        let ok = hunk_ids.contains(h);
        if !ok {
            report.unknown_hunks.push(h.clone());
        }
        ok
    });
    // A hunk where a serious finding is anchored (its first citation) is must-read, whatever the model said.
    for h in hunks {
        let serious = review.findings.iter().any(|f| {
            matches!(f.kind.as_str(), "bug" | "security" | "scope_creep" | "needs_decision")
                && f.evidence.first().map(|c| h.contains(&c.file, &c.side, c.line)).unwrap_or(false)
        });
        if serious && !att.must_read.contains(&h.id) {
            att.must_read.push(h.id.clone());
        }
    }
    att.skip_reasons.retain(|k, _| hunk_ids.contains(k) && !att.must_read.contains(k));
    report.unknown_hunks.sort();
    report.unknown_hunks.dedup();
    (review, report)
}
