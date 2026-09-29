pub mod analyze;
pub mod bundle;
pub mod config;
pub mod minify;
pub mod output;
pub mod trace;
pub mod tree;

use anyhow::Result;
use bundle::Bundle;
use config::Target;
use minify::Minify;
use tree::Tree;

/// Bundles one target and, if it has no errors, writes its files.
pub fn build_target(
    tree: &Tree,
    aliases: &analyze::Aliases,
    target: &Target,
    default_minify: Minify,
    external: &[String],
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
    });
    if !bundle.has_errors() {
        output::write_bundle(&bundle, &target.output)?;
        if let Some(path) = &target.rbxmx {
            output::write_rbxmx(&bundle, tree.node(entry), &name, path)?;
        }
    }
    Ok(bundle)
}
