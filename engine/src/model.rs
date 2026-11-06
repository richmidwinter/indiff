//! The review the model returns, and the checked result indiff renders.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Cite {
    pub file: String,
    /// "new" or "old". Empty is treated as "new".
    pub side: String,
    pub line: u32,
    pub quote: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Aim {
    pub id: String,
    pub text: String,
    /// "intent" or "inferred"
    pub source: String,
    /// achieved | partial | not_achieved | unclear
    pub status: String,
    pub hunks: Vec<String>,
    pub evidence: Vec<Cite>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct BehaviourRow {
    pub subject: String,
    pub aspect: String,
    pub before: String,
    pub after: String,
    pub finding: Option<String>,
    pub evidence: Vec<Cite>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Finding {
    pub id: String,
    /// bug | security | needs_decision | test_gap | scope_creep | achieved
    pub kind: String,
    pub title: String,
    pub detail: String,
    pub suggestion: String,
    pub aim: Option<String>,
    /// "cited" or "inferred"
    pub basis: String,
    /// high | medium | low
    pub confidence: String,
    pub evidence: Vec<Cite>,
    /// Where in the intent or transcript this choice came from, if anywhere.
    pub why: Why,
    /// A proposed code change (bugs and security findings only). Checked against
    /// the new side of the comparison; fixes that don't apply cleanly are removed.
    pub fix: Option<Fix>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Fix {
    /// One line on what the fix does.
    pub note: String,
    /// Applied in order. Each `find` must occur exactly once in its file after the edits before it.
    pub edits: Vec<Edit>,
    /// Set by indiff: true when the new side is the working tree, so the fix can be written to disk.
    pub applicable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Edit {
    pub file: String,
    /// Exact text in the new version of the file, including indentation.
    pub find: String,
    pub replace: String,
    /// Set by indiff: the 1-based line in the new file where `find` starts.
    pub line: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Why {
    /// "user", "agent", "commit" or "ticket"
    pub who: String,
    /// Exact words from the intent section.
    pub quote: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Attention {
    pub must_read: Vec<String>,
    pub minutes: f64,
    /// Hunk id → why it's safe to skip. Accepts a map, or a list of {hunk, reason}
    /// (the structured-output shape, since schemas can't describe maps).
    #[serde(deserialize_with = "skip_reasons")]
    pub skip_reasons: BTreeMap<String, String>,
}

fn skip_reasons<'de, D: serde::Deserializer<'de>>(d: D) -> Result<BTreeMap<String, String>, D::Error> {
    #[derive(Deserialize)]
    struct Item {
        hunk: String,
        reason: String,
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Map(BTreeMap<String, String>),
        List(Vec<Item>),
    }
    Ok(match Option::<Either>::deserialize(d)? {
        Some(Either::Map(m)) => m,
        Some(Either::List(items)) => items.into_iter().map(|i| (i.hunk, i.reason)).collect(),
        None => BTreeMap::new(),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Review {
    pub summary: String,
    pub verdict: String,
    pub aims: Vec<Aim>,
    pub behaviour: Vec<BehaviourRow>,
    pub findings: Vec<Finding>,
    pub attention: Attention,
}

/// What the citation checker removed or fixed.
#[derive(Debug, Clone, Serialize, Default)]
pub struct CheckReport {
    pub dropped_findings: Vec<String>,
    pub dropped_behaviour_rows: Vec<String>,
    pub dropped_citations: usize,
    pub corrected_citations: usize,
    pub unknown_hunks: Vec<String>,
    /// "Why" quotes that weren't found in the intent and were cleared.
    pub dropped_why: usize,
    /// Proposed fixes removed because their text didn't match the code exactly once.
    pub dropped_fixes: Vec<String>,
}

pub fn kind_rank(kind: &str) -> u8 {
    match kind {
        "bug" => 0,
        "security" => 1,
        "scope_creep" => 2,
        "needs_decision" => 3,
        "test_gap" => 4,
        "achieved" => 6,
        _ => 5,
    }
}

pub fn kind_label(kind: &str) -> (&'static str, &'static str) {
    match kind {
        "bug" => ("●", "Bug"),
        "security" => ("◆", "Security"),
        "scope_creep" => ("✱", "Not asked for"),
        "needs_decision" => ("▲", "Needs you"),
        "test_gap" => ("○", "Test gap"),
        "achieved" => ("✓", "Achieved"),
        _ => ("·", "Note"),
    }
}
