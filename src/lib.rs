pub mod analyze;
pub mod bundle;
pub mod config;
pub mod minify;
pub mod output;
pub mod profile;
pub mod trace;
pub mod tree;

use anyhow::{Context, Result};
use bundle::Bundle;
use bundle::Level;
use config::Config;
use config::Target;
use minify::Minify;
use std::time::Instant;
use tree::Tree;

/// Bundles one target and, if it has no errors, writes its files.
pub fn build_target(
    tree: &Tree,
    aliases: &analyze::Aliases,
    target: &Target,
    default_minify: Minify,
    external: &[String],
    internal: &[String],
) -> Result<Bundle> {
    let entry = tree.find_file(&target.entry)?;
    let name = target.script_name();
    let bundle = bundle::bundle(&bundle::Options {
        tree,
        aliases,
        entry,
        minify: target.minify.unwrap_or(default_minify),
        script_name: name.clone(),
        external,
        internal,
        include: &target.include,
        exclude: &target.exclude,
        prologue: target.prologue.as_deref().unwrap_or(""),
    });
    if !bundle.has_errors() {
        output::write_bundle(&bundle, &target.output)?;
        if let Some(path) = &target.rbxmx {
            output::write_rbxmx(&bundle, tree.node(entry), &name, path)?;
        }
    }
    Ok(bundle)
}

pub fn sourcemap_summary(tree: &Tree) -> String {
    format!(
        "+{} from disk, -{} missing",
        tree.stale_files.len(),
        tree.missing_files.len()
    )
}

/// Builds every target, sending progress lines to `log`; returns false if any target failed.
pub fn build_all(config: &Config, strict: bool, log: &mut dyn FnMut(&str)) -> Result<bool> {
    let tree = Tree::load(&config.root, config.sourcemap.as_deref())?;
    let aliases = config::load_aliases(&tree.root_dir)?;
    if tree.is_stale() {
        let sourcemap = config.sourcemap_path();
        if config.regenerate_sourcemap && sourcemap.parent().is_some_and(|d| d.is_dir()) {
            tree.write_sourcemap(&sourcemap)?;
            log(&format!(
                "note: regenerated {} ({})",
                sourcemap.display(),
                sourcemap_summary(&tree)
            ));
        } else if !tree.stale_files.is_empty() {
            log(&format!(
                "note: sourcemap.json is missing {} script(s) found on disk (e.g. {}); using the folder layout for them (`klsm sourcemap` or `regenerate_sourcemap = true` fixes this)",
                tree.stale_files.len(),
                tree.stale_files[0]
            ));
        }
    }
    let mut ok = true;
    for target in &config.bundles {
        let started = Instant::now();
        let external = target.external(&config.external);
        let internal = target.internal(&config.internal);
        let bundle = build_target(&tree, &aliases, target, config.minify, &external, &internal)
            .with_context(|| format!("bundling {}", target.entry.display()))?;
        for d in &bundle.diagnostics {
            log(&format!("{d}"));
        }
        let warnings = bundle
            .diagnostics
            .iter()
            .filter(|d| d.level == Level::Warning)
            .count();
        if bundle.has_errors() || (strict && warnings > 0) {
            ok = false;
            log(&format!("✗ {} — not written", target.entry.display()));
        } else {
            let external = if bundle.external_count > 0 {
                format!(", {} external", bundle.external_count)
            } else {
                String::new()
            };
            log(&format!(
                "✓ {} → {} ({} modules{external}, {} KB, {warnings} warnings, {} ms)",
                target.entry.display(),
                target.output.display(),
                bundle.module_count,
                bundle.code.len() / 1024,
                started.elapsed().as_millis()
            ));
        }
    }
    Ok(ok)
}
