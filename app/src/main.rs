//! indiff desktop app: a thin Tauri shell over indiff-engine.
//!
//! Launch from a repository with `git indiff [revs]` (see README), or open the
//! app and choose a repository. Arguments work like `git diff`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use indiff_engine::{self as engine, llm::Settings, ReviewInput};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

#[derive(Serialize, Clone)]
struct Launch {
    repo: Option<String>,
    revs: Vec<String>,
    staged: bool,
}

struct LaunchState(Launch);

fn parse_launch() -> Launch {
    let mut revs = Vec::new();
    let mut staged = false;
    let mut repo: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--staged" | "--cached" => staged = true,
            "--repo" | "-C" => repo = args.next(),
            // macOS adds this when launched from Finder.
            s if s.starts_with("-psn_") => {}
            s if s.starts_with('-') => {}
            _ => revs.push(a),
        }
    }
    // Launched by a git alias or from a terminal: use the current directory if it's a repo.
    if repo.is_none() {
        if let Ok(cwd) = std::env::current_dir() {
            let inside = std::process::Command::new("git")
                .arg("-C")
                .arg(&cwd)
                .args(["rev-parse", "--is-inside-work-tree"])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if inside {
                repo = Some(cwd.to_string_lossy().into_owned());
            }
        }
    }
    Launch { repo, revs, staged }
}

fn err<E: std::fmt::Display>(e: E) -> String {
    format!("{e:#}")
}

fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(err)?;
    std::fs::create_dir_all(&dir).map_err(err)?;
    Ok(dir.join("settings.json"))
}

fn load_settings(app: &AppHandle) -> Settings {
    settings_path(app)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

#[tauri::command]
fn launch_args(state: tauri::State<LaunchState>) -> Launch {
    state.0.clone()
}

#[derive(Serialize)]
struct SettingsView {
    provider: String,
    model: String,
    base_url: String,
    speed: String,
    has_key: bool,
    key_from_env: bool,
}

#[tauri::command]
fn get_settings(app: AppHandle) -> SettingsView {
    let s = load_settings(&app);
    let cfg = engine::llm::Config::resolve(&s);
    SettingsView {
        provider: s.provider,
        model: s.model,
        base_url: s.base_url,
        speed: cfg.speed,
        has_key: cfg.api_key.is_some(),
        key_from_env: s.api_key.is_empty() && cfg.api_key.is_some(),
    }
}

#[derive(Deserialize)]
struct SettingsUpdate {
    provider: String,
    model: String,
    base_url: String,
    #[serde(default)]
    speed: String,
    /// None keeps the saved key; Some("") clears it.
    api_key: Option<String>,
}

#[tauri::command]
fn save_settings(app: AppHandle, update: SettingsUpdate) -> Result<SettingsView, String> {
    let mut s = load_settings(&app);
    s.provider = update.provider;
    s.model = update.model;
    s.base_url = update.base_url;
    s.speed = update.speed;
    if let Some(k) = update.api_key {
        s.api_key = k;
    }
    let path = settings_path(&app)?;
    std::fs::write(&path, serde_json::to_string_pretty(&s).map_err(err)?).map_err(err)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(get_settings(app))
}

#[tauri::command]
async fn load_diff(repo: String, revs: Vec<String>, staged: bool, ignore_ws: bool) -> Result<engine::Loaded, String> {
    tauri::async_runtime::spawn_blocking(move || engine::load(Path::new(&repo), &revs, staged, ignore_ws).map_err(err))
        .await
        .map_err(err)?
}

/// Polled by the UI to notice when the files change.
#[tauri::command]
async fn diff_key(repo: String, revs: Vec<String>, staged: bool, ignore_ws: bool) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || engine::diff_key(Path::new(&repo), &revs, staged, ignore_ws).map_err(err))
        .await
        .map_err(err)?
}

#[tauri::command]
async fn run_review(
    app: AppHandle,
    repo: String,
    revs: Vec<String>,
    staged: bool,
    intent: Option<String>,
    transcript: Option<String>,
    force: bool,
    ignore_ws: bool,
) -> Result<engine::ReviewOutput, String> {
    let settings = load_settings(&app);
    let input = ReviewInput { intent: intent.filter(|s| !s.trim().is_empty()), transcript, force, canned_reply: None, ignore_ws };
    tauri::async_runtime::spawn_blocking(move || {
        engine::review(Path::new(&repo), &revs, staged, &input, &settings).map_err(err)
    })
    .await
    .map_err(err)?
}

#[tauri::command]
async fn prompt_preview(
    repo: String,
    revs: Vec<String>,
    staged: bool,
    intent: Option<String>,
    transcript: Option<String>,
    ignore_ws: bool,
) -> Result<String, String> {
    let input = ReviewInput { intent, transcript, ignore_ws, ..Default::default() };
    tauri::async_runtime::spawn_blocking(move || {
        engine::prompt_preview(Path::new(&repo), &revs, staged, &input).map_err(err)
    })
    .await
    .map_err(err)?
}

#[tauri::command]
async fn read_file(repo: String, revs: Vec<String>, staged: bool, side: String, path: String) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        engine::read_file(Path::new(&repo), &revs, staged, &side, &path).map_err(err)
    })
    .await
    .map_err(err)?
}

#[tauri::command]
async fn apply_fix(repo: String, revs: Vec<String>, staged: bool, fix: engine::model::Fix) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        engine::apply_fix(Path::new(&repo), &revs, staged, &fix).map_err(err)
    })
    .await
    .map_err(err)?
}

/// Opens iTerm2 (or Terminal) in the repository with Claude Code started on `prompt`.
/// The prompt is saved under .git/indiff-handoff, so it never touches the work tree
/// and long prompts don't have to survive shell quoting. Returns the prompt file.
#[tauri::command]
fn open_in_claude(repo: String, prompt: String) -> Result<String, String> {
    engine::git::set_repo(Path::new(&repo));
    let root = engine::git::toplevel().map_err(err)?;
    let dir = engine::git::git_dir().map_err(err)?.join("indiff-handoff");
    std::fs::create_dir_all(&dir).map_err(err)?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let file = dir.join(format!("{stamp}.md"));
    std::fs::write(&file, &prompt).map_err(err)?;
    let quote = |p: &Path| format!("'{}'", p.to_string_lossy().replace('\'', r"'\''"));
    let command = format!("cd {} && claude \"$(cat {})\"", quote(&root), quote(&file));
    launch_terminal(&command)?;
    Ok(file.to_string_lossy().into_owned())
}

#[cfg(target_os = "macos")]
const ITERM_ID: &str = "com.googlecode.iterm2";

/// Whether iTerm2 is installed: the usual locations first, then Spotlight.
#[cfg(target_os = "macos")]
fn has_iterm() -> bool {
    let home = std::env::var("HOME").unwrap_or_default();
    if ["/Applications/iTerm.app".to_string(), format!("{home}/Applications/iTerm.app")].iter().any(|p| Path::new(p).exists()) {
        return true;
    }
    std::process::Command::new("mdfind")
        .arg(format!("kMDItemCFBundleIdentifier == '{ITERM_ID}'"))
        .output()
        .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
        .unwrap_or(false)
}

/// Runs an AppleScript with `command` as its only argument, so the command needs no AppleScript escaping.
#[cfg(target_os = "macos")]
fn osascript(lines: &[&str], command: &str) -> Result<(), String> {
    let mut c = std::process::Command::new("osascript");
    for line in lines {
        c.arg("-e").arg(line);
    }
    let out = c.arg(command).output().map_err(err)?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Opens a new terminal window running `command`: iTerm2 when installed, otherwise Terminal.
#[cfg(target_os = "macos")]
fn launch_terminal(command: &str) -> Result<(), String> {
    let iterm_tell = format!("tell application id \"{ITERM_ID}\"");
    let iterm = [
        "on run argv",
        iterm_tell.as_str(),
        "activate",
        "set w to (create window with default profile)",
        "tell current session of w to write text (item 1 of argv)",
        "end tell",
        "end run",
    ];
    let terminal = ["on run argv", "tell application \"Terminal\"", "activate", "do script (item 1 of argv)", "end tell", "end run"];

    let mut tried = Vec::new();
    if has_iterm() {
        match osascript(&iterm, command) {
            Ok(()) => return Ok(()),
            Err(e) => tried.push(format!("iTerm2: {e}")),
        }
    }
    match osascript(&terminal, command) {
        Ok(()) => Ok(()),
        Err(e) => {
            tried.push(format!("Terminal: {e}"));
            Err(format!(
                "couldn't open a terminal ({}). If macOS blocked it, allow indiff under System Settings → Privacy & Security → Automation.",
                tried.join("; ")
            ))
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn launch_terminal(_command: &str) -> Result<(), String> {
    Err("opening Claude Code is only supported on macOS so far; use Copy prompt instead".into())
}

#[tauri::command]
fn list_transcripts(repo: String) -> Vec<engine::transcripts::Transcript> {
    engine::transcripts::for_repo(Path::new(&repo))
}

#[tauri::command]
fn list_branches(repo: String) -> Vec<String> {
    engine::branches(Path::new(&repo))
}

fn main() {
    let launch = parse_launch();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(LaunchState(launch))
        .invoke_handler(tauri::generate_handler![
            launch_args,
            get_settings,
            save_settings,
            load_diff,
            diff_key,
            run_review,
            prompt_preview,
            read_file,
            apply_fix,
            open_in_claude,
            list_transcripts,
            list_branches
        ])
        .run(tauri::generate_context!())
        .expect("error while running indiff");
}
