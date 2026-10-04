// Desktop front-end for klsm: pick the features each client gets, then build their bundles.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use klsm_bundler::config::Config;
use klsm_bundler::profile::{self, Feature, Profile};
use serde::Serialize;
use std::path::{Path, PathBuf};

type Res<T> = Result<T, String>;

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

#[derive(Serialize)]
struct Project {
    root: String,
    features: Vec<Feature>,
    clients: Vec<String>,
}

#[derive(Serialize)]
struct BuildResult {
    ok: bool,
    log: Vec<String>,
    /// The one-file package to drop into ServerScriptService (empty when the build failed).
    package: String,
}

/// `bundle.toml` in the working directory or its parent (where `cargo tauri dev` runs).
#[tauri::command]
fn default_config() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    [cwd.join("bundle.toml"), cwd.join("../bundle.toml")]
        .into_iter()
        .find(|p| p.is_file())
        .and_then(|p| p.canonicalize().ok())
        .map(|p| p.display().to_string())
}

#[tauri::command]
fn open_project(config: String) -> Res<Project> {
    let path = PathBuf::from(&config);
    let cfg = Config::load(&path).map_err(err)?;
    Ok(Project {
        root: cfg.root.display().to_string(),
        features: profile::features(&cfg).map_err(err)?,
        clients: profile::list(&path).map_err(err)?,
    })
}

#[tauri::command]
fn get_client(config: String, name: String) -> Res<Profile> {
    profile::load(Path::new(&config), &name).map_err(err)
}

#[tauri::command]
fn save_client(config: String, name: String, disabled: Vec<String>) -> Res<()> {
    profile::save(Path::new(&config), &name, &Profile { disabled }).map_err(err)
}

#[tauri::command]
async fn build(config: String, client: String) -> Res<BuildResult> {
    // full-moon's parser is recursive; bundles need a big stack (same as the CLI).
    let job = std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || run_build(&config, &client))
        .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || job.join())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|_| "the build crashed".to_string())?
}

fn run_build(config: &str, client: &str) -> Res<BuildResult> {
    let path = Path::new(config);
    let mut cfg = Config::load(path).map_err(err)?;
    let prof = profile::load(path, client).map_err(err)?;
    profile::apply(&mut cfg, path, client, &prof).map_err(err)?;
    let mut log = Vec::new();
    let ok = klsm_bundler::build_all(&cfg, false, &mut |l| log.push(l.to_string())).map_err(err)?;
    let package = if ok {
        let p = profile::write_package(&cfg, path, client).map_err(err)?;
        log.push(format!("✓ package → {}", p.display()));
        p.display().to_string()
    } else {
        String::new()
    };
    Ok(BuildResult { ok, log, package })
}

// ponytail: macOS `open -R` only; use the opener plugin if this ever ships on Windows.
#[tauri::command]
fn reveal(path: String) -> Res<()> {
    std::process::Command::new("open")
        .args(["-R", &path])
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            default_config,
            open_project,
            get_client,
            save_client,
            build,
            reveal
        ])
        .run(tauri::generate_context!())
        .expect("error while running the app");
}
