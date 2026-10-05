use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use klsm_bundler::config::{Config, Target};
use klsm_bundler::minify::Minify;
use klsm_bundler::tree::{NodeId, ROOT, Tree};
use klsm_bundler::{build_all, output, profile, sourcemap_summary, trace};
use notify::{RecursiveMode, Watcher};
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(
    name = "klsm",
    version,
    about = "Bundle Roblox Studio Script Sync projects into single Luau scripts"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Bundle every [[bundle]] in the config, or a single --entry.
    Build(BuildArgs),
    /// Like build, then rebuild whenever a source file changes.
    Watch(BuildArgs),
    /// Map bundle line numbers back to source files. Reads an error log from stdin
    /// when no lines are given.
    Trace {
        /// The `<output>.map.json` written next to a bundle.
        map: PathBuf,
        lines: Vec<usize>,
    },
    /// Syntax-check Luau files (e.g. a finished bundle).
    Check { files: Vec<PathBuf> },
    /// Print the instance tree resolved from the sync folder.
    Tree {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        sourcemap: Option<PathBuf>,
    },
    /// Rewrite sourcemap.json from the folder layout (scripts Studio hasn't listed yet
    /// are added, entries whose file is gone are dropped).
    Sourcemap {
        /// Config file (default: ./bundle.toml); its `root` and `sourcemap` are used.
        #[arg(short, long, conflicts_with = "root")]
        config: Option<PathBuf>,
        /// Sync folder root, instead of a config.
        #[arg(long)]
        root: Option<PathBuf>,
        /// Write here instead of over the existing sourcemap.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

#[derive(Args, Clone)]
struct BuildArgs {
    /// Config file (default: ./bundle.toml).
    #[arg(short, long)]
    config: Option<PathBuf>,
    /// Sync folder root, for single-entry builds without a config.
    #[arg(long, requires = "entry")]
    root: Option<PathBuf>,
    /// Entry script relative to --root.
    #[arg(long, requires = "root", requires = "output")]
    entry: Option<PathBuf>,
    #[arg(short, long)]
    output: Option<PathBuf>,
    #[arg(long)]
    rbxmx: Option<PathBuf>,
    #[arg(long, value_enum)]
    minify: Option<Minify>,
    /// Treat warnings as errors.
    #[arg(long)]
    strict: bool,
    /// Rewrite sourcemap.json from the folder layout when it's out of date
    /// (same as `regenerate_sourcemap = true` in the config).
    #[arg(long)]
    regenerate_sourcemap: bool,
    /// Instance path whose modules stay in the game (repeatable), e.g. ReplicatedStorage.
    #[arg(long, value_name = "PATH")]
    external: Vec<String>,
    /// Instance path under an external one that is bundled anyway (repeatable).
    #[arg(long, value_name = "PATH")]
    internal: Vec<String>,
    /// Glob of module files to bundle even if nothing requires them statically (repeatable,
    /// single-entry builds only), e.g. 'Addon/Server/Features/*/*Service.luau'.
    #[arg(long, value_name = "GLOB", requires = "entry")]
    include: Vec<String>,
    /// Build for a client profile (`clients/<name>.toml` next to the config): its disabled
    /// features are excluded and outputs go to `dist/<name>/`.
    #[arg(long, value_name = "NAME", conflicts_with = "entry")]
    client: Option<String>,
    /// Glob taken out of what --include matched (repeatable).
    #[arg(long, value_name = "GLOB", requires = "entry")]
    exclude: Vec<String>,
}

impl BuildArgs {
    fn config_path(&self) -> PathBuf {
        self.config
            .clone()
            .unwrap_or_else(|| PathBuf::from("bundle.toml"))
    }

    /// Builds every target and, for a client profile, its one-file package.
    fn build(&self, config: &Config) -> Result<bool> {
        let ok = build_all(config, self.strict, &mut |l| eprintln!("{l}"))?;
        if ok && let Some(name) = &self.client {
            let package = profile::write_package(config, &self.config_path(), name)?;
            eprintln!(
                "✓ package → {} (drop it into ServerScriptService)",
                package.display()
            );
            if let Some(line) = config.place.as_ref().and_then(|p| p.summary()) {
                eprintln!("{line}");
            }
        }
        Ok(ok)
    }

    fn config(&self) -> Result<Config> {
        if let (Some(root), Some(entry), Some(output)) = (&self.root, &self.entry, &self.output) {
            return Ok(Config {
                root: root.clone(),
                sourcemap: None,
                regenerate_sourcemap: self.regenerate_sourcemap,
                minify: self.minify.unwrap_or_default(),
                external: self.external.clone(),
                internal: self.internal.clone(),
                features: None,
                assets: None,
                place: None,
                bundles: vec![Target {
                    entry: entry.clone(),
                    output: output.clone(),
                    rbxmx: self.rbxmx.clone(),
                    minify: None,
                    name: None,
                    external: Vec::new(),
                    internal: Vec::new(),
                    include: self.include.clone(),
                    exclude: self.exclude.clone(),
                    prologue: None,
                }],
            });
        }
        let path = self.config_path();
        let mut config = Config::load(&path)?;
        if let Some(m) = self.minify {
            config.minify = m;
            config.bundles.iter_mut().for_each(|b| b.minify = None);
        }
        config.regenerate_sourcemap |= self.regenerate_sourcemap;
        config.external.extend(self.external.iter().cloned());
        config.internal.extend(self.internal.iter().cloned());
        if let Some(name) = &self.client {
            let profile = profile::load(&path, name)?;
            profile::apply(&mut config, &path, name, &profile)?;
        }
        Ok(config)
    }
}

fn main() {
    // full-moon's parser is recursive; big bundles need more than the default stack.
    let result = std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(run)
        .expect("spawn worker thread")
        .join()
        .unwrap_or_else(|_| Err(anyhow::anyhow!("internal error (panic)")));
    match result {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<bool> {
    match Cli::parse().command {
        Command::Build(args) => args.build(&args.config()?),
        Command::Watch(args) => watch(&args).map(|_| true),
        Command::Trace { map, lines } => {
            let map = trace::load(&map)?;
            if lines.is_empty() {
                let mut text = String::new();
                std::io::stdin().read_to_string(&mut text)?;
                print!("{}", trace::rewrite(&map, &text));
            } else {
                for line in lines {
                    println!("{line} -> {}", trace::describe(&map, line));
                }
            }
            Ok(true)
        }
        Command::Check { files } => {
            let mut ok = true;
            for file in files {
                let source = std::fs::read_to_string(&file)
                    .with_context(|| format!("reading {}", file.display()))?;
                let result = full_moon::parse_fallible(&source, full_moon::LuaVersion::luau());
                for e in result.errors() {
                    ok = false;
                    eprintln!(
                        "{}:{}: {}",
                        file.display(),
                        e.range().0.line(),
                        e.error_message()
                    );
                }
                if result.errors().is_empty() {
                    eprintln!("✓ {}", file.display());
                }
            }
            Ok(ok)
        }
        Command::Tree { root, sourcemap } => {
            let tree = Tree::load(&root, sourcemap.as_deref())?;
            print_tree(&tree, ROOT, 0);
            Ok(true)
        }
        Command::Sourcemap {
            config,
            root,
            output,
        } => {
            let (root, sourcemap) = match root {
                Some(root) => (root.clone(), root.join("sourcemap.json")),
                None => {
                    let path = config.unwrap_or_else(|| PathBuf::from("bundle.toml"));
                    let config = Config::load(&path)?;
                    (config.root.clone(), config.sourcemap_path())
                }
            };
            let tree = Tree::load(&root, Some(&sourcemap))?;
            let target = output.unwrap_or(sourcemap);
            let changed = tree.write_sourcemap(&target)?;
            eprintln!(
                "{} {} ({})",
                if changed {
                    "✓ wrote"
                } else {
                    "✓ up to date:"
                },
                target.display(),
                sourcemap_summary(&tree)
            );
            Ok(true)
        }
    }
}

fn watch(args: &BuildArgs) -> Result<()> {
    let config = args.config()?;
    let root = config
        .root
        .canonicalize()
        .context("root folder not found")?;
    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(tx)?;
    watcher.watch(&root, RecursiveMode::Recursive)?;
    eprintln!("watching {}", root.display());
    if let Err(e) = args.build(&config) {
        eprintln!("error: {e:#}");
    }

    loop {
        let event = rx.recv()?;
        // Re-read the config each time so edits to bundle.toml apply without a restart.
        let config = match args.config() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: {e:#}");
                continue;
            }
        };
        if !relevant(&event, &config) {
            continue;
        }
        // Debounce: Studio writes several files per save.
        let deadline = Instant::now() + Duration::from_millis(150);
        while rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_ok()
        {}
        eprintln!("\nchange detected, rebuilding…");
        if let Err(e) = args.build(&config) {
            eprintln!("error: {e:#}");
        }
    }
}

/// A Luau/sourcemap/.luaurc change that isn't one of our own outputs (which may
/// live inside the sync folder so Studio picks them up).
fn relevant(event: &notify::Result<notify::Event>, config: &Config) -> bool {
    let Ok(event) = event else { return false };
    if event.kind.is_access() {
        return false;
    }
    let same = |a: &PathBuf, b: &PathBuf| a == b || a.canonicalize().ok() == b.canonicalize().ok();
    event.paths.iter().any(|p| {
        let name = p.file_name().unwrap_or_default().to_string_lossy();
        let interesting = name.ends_with(".luau")
            || name.ends_with(".lua")
            || name == "sourcemap.json"
            || name == ".luaurc";
        let ours = config
            .bundles
            .iter()
            .any(|b| same(p, &b.output) || same(p, &output::map_path(&b.output)));
        interesting && !ours
    })
}

fn print_tree(tree: &Tree, id: NodeId, depth: usize) {
    let node = tree.node(id);
    let file = node
        .file
        .as_deref()
        .map(|f| format!("  [{f}]"))
        .unwrap_or_default();
    println!(
        "{}{} ({}){file}",
        "  ".repeat(depth),
        node.name,
        node.class_name
    );
    for &child in &node.children {
        print_tree(tree, child, depth + 1);
    }
}
