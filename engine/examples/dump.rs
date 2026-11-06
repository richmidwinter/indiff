//! Developer helper: prints the app's data for a repo as JSON.
//! cargo run -p indiff-engine --example dump -- <repo> <revs> [canned-reply.json] [intent]

use indiff_engine::{llm::Settings, load, review, ReviewInput};
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let repo = PathBuf::from(&a[0]);
    let revs = vec![a[1].clone()];
    let loaded = load(&repo, &revs, false, false)?;
    let input = ReviewInput {
        intent: a.get(3).cloned(),
        canned_reply: a.get(2).map(std::fs::read_to_string).transpose()?,
        ..Default::default()
    };
    let out = review(&repo, &revs, false, &input, &Settings::default())?;
    println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "diff": loaded, "review": out }))?);
    Ok(())
}
