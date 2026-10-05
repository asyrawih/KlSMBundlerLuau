use crate::analyze::Aliases;
use crate::minify::Minify;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The Script Sync folder (the one that maps to `game`).
    pub root: PathBuf,
    /// Defaults to `<root>/sourcemap.json`; the folder layout is used if it's missing.
    pub sourcemap: Option<PathBuf>,
    /// Rewrite the sourcemap from the folder layout whenever it's out of date (on by default).
    #[serde(default = "yes")]
    pub regenerate_sourcemap: bool,
    #[serde(default)]
    pub minify: Minify,
    /// Instance paths whose modules are left in the game instead of bundled
    /// (`["ReplicatedStorage"]`, `["ReplicatedStorage.Packages"]`).
    #[serde(default)]
    pub external: Vec<String>,
    /// Instance paths bundled even though they sit under `external`
    /// (`["ReplicatedStorage.AddonLoader"]`).
    #[serde(default)]
    pub internal: Vec<String>,
    /// Folders holding one folder per feature, from `root` (`Addon/*/Features`); the
    /// wildcard parts name the sides. Used by client profiles (`clients/<name>.toml`).
    pub features: Option<String>,
    /// Folder of `.rbxm` / `.rbxmx` files exported from Studio, laid out by service
    /// (`assets/ReplicatedStorage/EffectDonation.rbxm`); client packages ship them.
    pub assets: Option<PathBuf>,
    /// Studio-only content read from the published place (`[place]`); client packages ship it.
    pub place: Option<crate::place::Place>,
    #[serde(rename = "bundle", default)]
    pub bundles: Vec<Target>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    /// Entry script, relative to `root`.
    pub entry: PathBuf,
    /// Output `.luau`; a `.map.json` is written next to it.
    pub output: PathBuf,
    /// Optional `.rbxmx` model holding the bundle as a ready-to-insert script.
    pub rbxmx: Option<PathBuf>,
    pub minify: Option<Minify>,
    /// Name of the script at runtime; defaults to the output file name up to the first dot.
    pub name: Option<String>,
    /// Added to the top-level `external` list for this bundle only.
    #[serde(default)]
    pub external: Vec<String>,
    /// Added to the top-level `internal` list for this bundle only.
    #[serde(default)]
    pub internal: Vec<String>,
    /// Globs of module files (relative to `root`) bundled even though nothing requires
    /// them statically, e.g. `Addon/Server/Features/*/*Service.luau` for modules a loader
    /// finds with `GetChildren()`.
    #[serde(default)]
    pub include: Vec<String>,
    /// Globs taken out of what `include` matched (`Addon/Server/Features/Affiliate/*`).
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Luau run before the bundle runtime (a client package's installer); set in code only.
    #[serde(skip)]
    pub prologue: Option<String>,
}

impl Target {
    /// The top-level and per-bundle external paths together.
    pub fn external<'a>(&'a self, config_external: &'a [String]) -> Vec<String> {
        config_external
            .iter()
            .chain(&self.external)
            .cloned()
            .collect()
    }

    /// The top-level and per-bundle internal paths together.
    pub fn internal<'a>(&'a self, config_internal: &'a [String]) -> Vec<String> {
        config_internal
            .iter()
            .chain(&self.internal)
            .cloned()
            .collect()
    }

    pub fn script_name(&self) -> String {
        self.name.clone().unwrap_or_else(|| {
            let file = self
                .output
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            file.split('.').next().unwrap_or("Bundle").to_string()
        })
    }
}

impl Config {
    /// Where the sourcemap lives (and is regenerated to).
    pub fn sourcemap_path(&self) -> PathBuf {
        self.sourcemap
            .clone()
            .unwrap_or_else(|| self.root.join("sourcemap.json"))
    }

    pub fn load(path: &Path) -> Result<Config> {
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut config: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let base = path.parent().unwrap_or(Path::new("."));
        config.root = base.join(&config.root);
        config.sourcemap = config.sourcemap.map(|s| config.root.join(s));
        config.assets = config.assets.map(|a| base.join(a));
        for target in &mut config.bundles {
            target.output = base.join(&target.output);
            target.rbxmx = target.rbxmx.take().map(|p| base.join(p));
        }
        if config.bundles.is_empty() {
            bail!("{} has no [[bundle]] entries", path.display());
        }
        Ok(config)
    }
}

#[derive(Deserialize)]
struct LuauRc {
    #[serde(default)]
    aliases: HashMap<String, String>,
}

/// Reads `<root>/.luaurc` aliases (used by string requires like `require("@shared/X")`).
pub fn load_aliases(root: &Path) -> Result<Aliases> {
    let path = root.join(".luaurc");
    if !path.is_file() {
        return Ok(Aliases::new());
    }
    let text = fs::read_to_string(&path)?;
    let rc: LuauRc =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(rc
        .aliases
        .into_iter()
        .map(|(k, v)| {
            (
                k.to_ascii_lowercase(),
                crate::tree::normalize(&v).trim_end_matches('/').to_string(),
            )
        })
        .collect())
}
