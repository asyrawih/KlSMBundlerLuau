// Desktop front-end for klsm: pick the features each client gets, then build their bundles.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use klsm_bundler::config::Config;
use klsm_bundler::place;
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
    /// `[place] id`: client builds download this place's assets with the Open Cloud key.
    place_id: Option<u64>,
    has_api_key: bool,
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
        place_id: cfg.place.map(|p| p.id),
        has_api_key: place::api_key(config_dir(&path)).is_ok(),
    })
}

fn config_dir(config: &Path) -> &Path {
    config.parent().unwrap_or(Path::new("."))
}

/// Sets `[place] id` in bundle.toml, keeping its comments and layout; a missing `[place]`
/// is added with no assets yet.
#[tauri::command]
fn save_place(config: String, id: u64) -> Res<()> {
    let text = std::fs::read_to_string(&config).map_err(|e| e.to_string())?;
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{e}"))?;
    if !doc.contains_table("place") {
        let mut table = toml_edit::Table::new();
        table["assets"] = toml_edit::value(toml_edit::Array::new());
        doc["place"] = toml_edit::Item::Table(table);
    }
    doc["place"]["id"] = toml_edit::value(id as i64);
    std::fs::write(&config, doc.to_string()).map_err(|e| e.to_string())
}

/// Stores the Open Cloud key next to `bundle.toml`, readable by the owner only.
#[tauri::command]
fn save_api_key(config: String, key: String) -> Res<()> {
    let key = key.trim();
    if key.is_empty() {
        return Err("the API key is empty".into());
    }
    let file = config_dir(Path::new(&config)).join(place::API_KEY_FILE);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut out = options.open(&file).map_err(|e| e.to_string())?;
    std::io::Write::write_all(&mut out, key.as_bytes()).map_err(|e| e.to_string())
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
        log.extend(cfg.place.as_ref().and_then(|p| p.summary()));
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
            save_place,
            save_api_key,
            reveal
        ])
        .run(tauri::generate_context!())
        .expect("error while running the app");
}

#[cfg(test)]
mod tests {
    use super::save_place;

    #[test]
    fn save_place_keeps_comments_and_adds_missing_table() {
        let dir = std::env::temp_dir().join(format!("klsm-app-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("bundle.toml");
        let path = file.to_string_lossy().to_string();

        std::fs::write(&file, "root = \"x\" # sync\n\n[place]\nid = 1\nassets = [\"ReplicatedStorage.A\"]\n\n[[bundle]]\nentry = \"e\"\noutput = \"o\"\n").unwrap();
        save_place(path.clone(), 82391043752226).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            text.contains("# sync") && text.contains("id = 82391043752226"),
            "{text}"
        );
        assert!(text.contains("\"ReplicatedStorage.A\""), "{text}");

        std::fs::write(
            &file,
            "root = \"x\"\n\n[[bundle]]\nentry = \"e\"\noutput = \"o\"\n",
        )
        .unwrap();
        save_place(path, 7).unwrap();
        let parsed: toml_edit::DocumentMut =
            std::fs::read_to_string(&file).unwrap().parse().unwrap();
        assert_eq!(parsed["place"]["id"].as_integer(), Some(7));
        assert_eq!(parsed["bundle"][0]["entry"].as_str(), Some("e"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
