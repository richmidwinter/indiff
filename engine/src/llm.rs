//! Calls the reviewer model. Two backends: Anthropic Messages and any
//! OpenAI-compatible chat completions endpoint (OpenAI, OpenRouter, Ollama, vLLM).

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

pub struct Config {
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub max_tokens: u32,
    /// "fast" or "thorough". See `speed_fields`.
    pub speed: String,
}

/// What a model call returned.
pub struct Reply {
    pub text: String,
    /// Tokens the model generated, including any thinking, when the provider reports it.
    pub output_tokens: Option<u64>,
}

/// What the user chose in Settings. Empty fields fall back to environment variables, then defaults.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub api_key: String,
    /// "fast" (default) or "thorough".
    pub speed: String,
}

fn pick(v: &str, env: &[&str]) -> Option<String> {
    if !v.trim().is_empty() {
        return Some(v.trim().to_string());
    }
    env.iter().find_map(|k| std::env::var(k).ok().filter(|s| !s.is_empty()))
}

impl Config {
    pub fn resolve(s: &Settings) -> Config {
        let provider = pick(&s.provider, &["INDIFF_PROVIDER"]).unwrap_or_else(|| "anthropic".into());
        let openai = provider == "openai";
        let model = pick(&s.model, &["INDIFF_MODEL"]).unwrap_or_else(|| {
            if openai { "gpt-5".into() } else { "claude-sonnet-5-5".into() }
        });
        let base_url = pick(&s.base_url, &["INDIFF_BASE_URL"]).unwrap_or_else(|| {
            if openai { "https://api.openai.com/v1".into() } else { "https://api.anthropic.com".into() }
        });
        let api_key = if openai {
            pick(&s.api_key, &["INDIFF_API_KEY", "OPENAI_API_KEY"])
        } else {
            pick(&s.api_key, &["INDIFF_API_KEY", "ANTHROPIC_API_KEY"])
        };
        let speed = if pick(&s.speed, &["INDIFF_SPEED"]).as_deref() == Some("thorough") { "thorough" } else { "fast" };
        Config { provider, model, base_url, api_key, max_tokens: 16_000, speed: speed.into() }
    }
}

/// Anthropic request fields for the chosen speed.
///
/// Recent Claude models think by default when `thinking` is omitted, which makes a
/// review slow. "fast" turns thinking off where the model allows it and sets effort
/// to low; "thorough" keeps thinking on at high effort. Models not listed here get
/// no extra fields, so older models (which reject them) keep working.
pub fn speed_fields(model: &str, speed: &str) -> Vec<(&'static str, Value)> {
    let m = model.trim().to_lowercase();
    let effort = [
        "claude-fable-5", "claude-mythos-5", "claude-opus-5", "claude-opus-4-8", "claude-opus-4-7",
        "claude-opus-4-6", "claude-sonnet-5", "claude-sonnet-4-6", "claude-haiku-5-5",
    ];
    if !effort.iter().any(|p| m.starts_with(p)) {
        return Vec::new();
    }
    if speed == "thorough" {
        return vec![("output_config", json!({"effort": "high"}))];
    }
    let mut out = vec![("output_config", json!({"effort": "low"}))];
    // How each model turns thinking off. Opus 5.5 and Fable can't: low effort is all there is.
    if m.starts_with("claude-sonnet-5-5") {
        out.push(("thinking", json!({"type": "between_tools"})));
    } else if m.starts_with("claude-haiku-5-5") || m == "claude-sonnet-5" || m == "claude-opus-5" {
        out.push(("thinking", json!({"type": "disabled"})));
    }
    out
}

fn host_of(url: &str) -> String {
    url.split("://").nth(1).unwrap_or(url).split(['/', ':']).next().unwrap_or("").to_lowercase()
}

/// Honours HTTPS_PROXY for https URLs, except hosts listed in NO_PROXY and local addresses.
fn agent(url: &str) -> ureq::Agent {
    let mut b = ureq::AgentBuilder::new().timeout(Duration::from_secs(600));
    let host = host_of(url);
    let local = host == "localhost" || host == "127.0.0.1" || host == "::1";
    let no_proxy = std::env::var("NO_PROXY").or_else(|_| std::env::var("no_proxy")).unwrap_or_default();
    let excluded = no_proxy.split(',').map(|s| s.trim().trim_start_matches('.').to_lowercase()).any(|s| {
        !s.is_empty() && (s == "*" || host == s || host.ends_with(&format!(".{s}")))
    });
    if url.starts_with("https://") && !local && !excluded {
        if let Some(p) = std::env::var("HTTPS_PROXY").ok().or_else(|| std::env::var("https_proxy").ok()) {
            if let Ok(proxy) = ureq::Proxy::new(p) {
                b = b.proxy(proxy);
            }
        }
    }
    b.build()
}

fn post(url: &str, headers: &[(&str, String)], body: Value) -> Result<Value> {
    let mut req = agent(url).post(url).set("content-type", "application/json");
    for (k, v) in headers {
        req = req.set(k, v);
    }
    match req.send_json(body) {
        Ok(resp) => Ok(resp.into_json()?),
        Err(ureq::Error::Status(code, resp)) => {
            let text = resp.into_string().unwrap_or_default();
            bail!("model API returned {code}: {}", crate::context::truncate(&text, 600))
        }
        Err(e) => Err(anyhow!(e)).context("calling model API"),
    }
}

pub fn complete(cfg: &Config, system: &str, user: &str) -> Result<Reply> {
    let key = cfg.api_key.clone().ok_or_else(|| {
        anyhow!("no API key: set ANTHROPIC_API_KEY (or OPENAI_API_KEY with --provider openai, or INDIFF_API_KEY)")
    })?;
    let base = cfg.base_url.trim_end_matches('/');
    if cfg.provider == "openai" {
        let body = json!({
            "model": cfg.model,
            "max_completion_tokens": cfg.max_tokens,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user}
            ]
        });
        let v = post(&format!("{base}/chat/completions"), &[("authorization", format!("Bearer {key}"))], body)?;
        let text = v
            .pointer("/choices/0/message/content")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("unexpected response shape: {}", crate::context::truncate(&v.to_string(), 400)))?;
        Ok(Reply { text, output_tokens: v.pointer("/usage/completion_tokens").and_then(|t| t.as_u64()) })
    } else {
        let mut body = json!({
            "model": cfg.model,
            "max_tokens": cfg.max_tokens,
            "system": system,
            "messages": [{"role": "user", "content": user}]
        });
        for (k, val) in speed_fields(&cfg.model, &cfg.speed) {
            body[k] = val;
        }
        // Constrain the reply to the review schema, so it is always valid JSON.
        if crate::schema::supports_structured(&cfg.model) {
            body["output_config"]["format"] = json!({"type": "json_schema", "schema": crate::schema::review()});
        }
        let v = post(
            &format!("{base}/v1/messages"),
            &[("x-api-key", key), ("anthropic-version", "2023-06-01".into())],
            body,
        )?;
        let parts = v
            .get("content")
            .and_then(|c| c.as_array())
            .ok_or_else(|| anyhow!("unexpected response shape: {}", crate::context::truncate(&v.to_string(), 400)))?;
        let text = parts
            .iter()
            .filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("");
        if v.get("stop_reason").and_then(|s| s.as_str()) == Some("max_tokens") {
            bail!("the model ran out of output space before finishing the review; try Fast speed or a smaller diff");
        }
        Ok(Reply { text, output_tokens: v.pointer("/usage/output_tokens").and_then(|t| t.as_u64()) })
    }
}

#[cfg(test)]
mod speed_tests {
    use super::speed_fields;

    fn get(model: &str, speed: &str) -> String {
        let mut f = speed_fields(model, speed);
        f.sort_by_key(|(k, _)| *k);
        f.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn fast_turns_thinking_off_where_allowed() {
        assert_eq!(get("claude-haiku-5-5", "fast"), r#"output_config={"effort":"low"} thinking={"type":"disabled"}"#);
        assert_eq!(get("claude-sonnet-5-5", "fast"), r#"output_config={"effort":"low"} thinking={"type":"between_tools"}"#);
        // Opus 5.5 rejects disabled thinking at every effort level.
        assert_eq!(get("claude-opus-5-5", "fast"), r#"output_config={"effort":"low"}"#);
        assert_eq!(get("claude-opus-5", "fast"), r#"output_config={"effort":"low"} thinking={"type":"disabled"}"#);
    }

    #[test]
    fn thorough_and_unknown_models() {
        assert_eq!(get("claude-haiku-5-5", "thorough"), r#"output_config={"effort":"high"}"#);
        assert_eq!(get("claude-haiku-4-5", "fast"), "");
        assert_eq!(get("gpt-5", "fast"), "");
    }
}

/// Finds the JSON object in a model reply, tolerating code fences or stray prose.
pub fn extract_json(text: &str) -> Result<Value> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return Ok(v);
    }
    let start = t.find('{').ok_or_else(|| anyhow!("model reply contained no JSON object"))?;
    let end = t.rfind('}').ok_or_else(|| anyhow!("model reply contained no JSON object"))?;
    serde_json::from_str(&t[start..=end]).context("model reply was not valid JSON")
}
