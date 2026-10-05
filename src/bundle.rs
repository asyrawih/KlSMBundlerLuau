//! Walks the require graph from an entry script and emits one self-contained file.

use crate::analyze::{self, Aliases, Analysis, Target};
use crate::minify::{Minify, glue, minify, rename_locals};
use crate::tree::{NodeId, Tree, lua_string};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;

const RUNTIME: &str = include_str!("runtime.luau");
const PROXY_MARKER: &str = "--@@SCRIPT_PROXY@@\n";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub level: Level,
    pub file: String,
    pub line: usize,
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let level = match self.level {
            Level::Warning => "warning",
            Level::Error => "error",
        };
        if self.line > 0 {
            write!(f, "{level}: {}:{}: {}", self.file, self.line, self.message)
        } else {
            write!(f, "{level}: {}: {}", self.file, self.message)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub file: String,
    pub instance: String,
    /// First and last output line of the module body (1-based, inclusive).
    pub out_start: usize,
    pub out_end: usize,
    /// False when minification merged lines, so only the module is known.
    pub exact: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceMap {
    pub version: u32,
    pub script_name: String,
    pub entry: String,
    pub segments: Vec<Segment>,
}

impl SourceMap {
    /// Maps an output line back to `(file, line)`; `line` is 0 when inexact.
    pub fn lookup(&self, line: usize) -> Option<(&Segment, usize)> {
        let seg = self
            .segments
            .iter()
            .find(|s| (s.out_start..=s.out_end).contains(&line))?;
        Some((
            seg,
            if seg.exact {
                line - seg.out_start + 1
            } else {
                0
            },
        ))
    }
}

#[derive(Debug)]
pub struct Bundle {
    pub code: String,
    pub map: SourceMap,
    pub diagnostics: Vec<Diagnostic>,
    pub module_count: usize,
    /// Requires left pointing at modules in the game (see `Options::external`).
    pub external_count: usize,
    /// Every source file that went in, for watch mode.
    pub inputs: Vec<String>,
}

impl Bundle {
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.level == Level::Error)
    }
}

struct Unit {
    node: NodeId,
    file: String,
    source: String,
    analysis: Analysis,
    /// Module id; the entry script has none unless it is itself a ModuleScript.
    id: Option<usize>,
    deps: Vec<NodeId>,
}

pub struct Options<'a> {
    pub tree: &'a Tree,
    pub aliases: &'a Aliases,
    pub entry: NodeId,
    pub minify: Minify,
    pub script_name: String,
    /// Instance paths (`ReplicatedStorage`, `ReplicatedStorage.Packages`) whose modules stay
    /// in the game: requires of them are rewritten to absolute paths instead of bundled.
    pub external: &'a [String],
    /// Paths under `external` that are bundled anyway.
    pub internal: &'a [String],
    /// Globs of module files bundled even when nothing requires them statically; at
    /// runtime they are reached through the `script` stand-in (`GetChildren`, `require`).
    pub include: &'a [String],
    /// Globs removed from what `include` matched.
    pub exclude: &'a [String],
    /// Luau emitted right after the `--!` directives, before the runtime.
    pub prologue: &'a str,
}

impl Options<'_> {
    fn is_external(&self, id: NodeId) -> bool {
        self.tree.is_under(id, self.external) && !self.tree.is_under(id, self.internal)
    }
}

/// Scripts whose file matches `pattern`, sorted by file; warns when there are none (a typo).
fn matching(
    tree: &Tree,
    key: &str,
    pattern: &str,
    diags: &mut Vec<Diagnostic>,
) -> Vec<(String, NodeId)> {
    let glob = match globset::GlobBuilder::new(&crate::tree::normalize(pattern))
        .literal_separator(true)
        .build()
    {
        Ok(g) => g.compile_matcher(),
        Err(e) => {
            diags.push(error(
                "bundle.toml",
                0,
                format!("bad {key} glob {pattern:?}: {e}"),
            ));
            return Vec::new();
        }
    };
    let mut matched: Vec<(String, NodeId)> = tree
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(id, n)| n.file.as_ref().map(|f| (f.clone(), id)))
        .filter(|(f, _)| glob.is_match(f))
        .collect();
    matched.sort();
    if matched.is_empty() {
        diags.push(warning(
            "bundle.toml",
            0,
            format!("{key} {pattern:?} matches no script"),
        ));
    }
    matched
}

/// Module nodes matched by `include` and not by `exclude`, sorted by file per glob.
fn resolve_include(
    tree: &Tree,
    include: &[String],
    exclude: &[String],
    diags: &mut Vec<Diagnostic>,
) -> Vec<NodeId> {
    let excluded: Vec<NodeId> = exclude
        .iter()
        .flat_map(|p| matching(tree, "exclude", p, diags))
        .map(|(_, id)| id)
        .collect();
    let mut found = Vec::new();
    for pattern in include {
        for (file, id) in matching(tree, "include", pattern, diags) {
            if excluded.contains(&id) {
                continue;
            }
            if !tree.node(id).is_module() {
                diags.push(error(
                    &file,
                    0,
                    format!(
                        "included by {pattern:?} but is a {}; only ModuleScripts can be included",
                        tree.node(id).class_name
                    ),
                ));
            } else if !found.contains(&id) {
                found.push(id);
            }
        }
    }
    found
}

pub fn bundle(opts: &Options) -> Bundle {
    let tree = opts.tree;
    let mut diags = Vec::new();
    let entry = tree.node(opts.entry);
    let entry_file = entry.file.clone().unwrap_or_default();
    if !entry.is_module() && !entry.is_runnable() {
        diags.push(Diagnostic {
            level: Level::Error,
            file: entry_file.clone(),
            line: 0,
            message: format!(
                "entry must be a Script, LocalScript or ModuleScript, found {}",
                entry.class_name
            ),
        });
    }

    // BFS over the require graph. Unit index 0 is the entry; module ids are the unit
    // index, shifted by one when the entry is itself a ModuleScript (id 0 is unused).
    let mut units: Vec<Unit> = Vec::new();
    let mut order: Vec<NodeId> = vec![opts.entry];
    let mut seen: HashMap<NodeId, usize> = HashMap::from([(opts.entry, 0)]);
    let id_shift = usize::from(entry.is_module());
    let mut external_count = 0;
    for id in resolve_include(tree, opts.include, opts.exclude, &mut diags) {
        if opts.is_external(id) {
            diags.push(warning(
                tree.node(id).file.as_deref().unwrap_or_default(),
                0,
                "included but under `external`; list it in `internal` to bundle it".into(),
            ));
            continue;
        }
        seen.entry(id).or_insert_with(|| {
            order.push(id);
            order.len() - 1
        });
    }

    while units.len() < order.len() {
        let index = units.len();
        let node = order[index];
        let file = tree.node(node).file.clone().unwrap_or_default();
        let id = (index > 0 || entry.is_module()).then_some(index + id_shift);
        let mut unit = Unit {
            node,
            file: file.clone(),
            source: String::new(),
            analysis: Analysis::default(),
            id,
            deps: vec![],
        };
        match fs::read_to_string(tree.abs_file(&file)) {
            Ok(s) => unit.source = s.replace("\r\n", "\n"),
            Err(e) => {
                diags.push(error(&file, 0, format!("cannot read file: {e}")));
                units.push(unit);
                continue;
            }
        }
        match analyze::analyze(&unit.source, tree, node, opts.aliases) {
            Ok(a) => unit.analysis = a,
            Err(errors) => {
                for e in errors {
                    diags.push(error(&file, e.line, e.message));
                }
                units.push(unit);
                continue;
            }
        }
        analyze::attach_text(&unit.source, &mut unit.analysis);

        for site in unit.analysis.requires.iter().filter(|r| !r.never_runs) {
            let targets = match &site.target {
                Target::Node(target) => vec![*target],
                // Left as a real `require` call; the runtime routes stand-ins to the bundle.
                Target::OneOf(targets) => targets.clone(),
                // Roblox only fails a bad require when it runs, and guarded fallbacks
                // (`game and Enum or require("../test/mock")`) never do.
                Target::Missing(why) => {
                    diags.push(warning(
                        &file,
                        site.line,
                        format!(
                            "cannot resolve {}: {why}; left as a real require (errors if it runs)",
                            site.text
                        ),
                    ));
                    continue;
                }
                Target::Dynamic => {
                    diags.push(warning(
                        &file,
                        site.line,
                        format!(
                            "{} is not statically resolvable; left as a real require",
                            site.text
                        ),
                    ));
                    continue;
                }
                Target::Asset => continue,
            };
            for target in &targets {
                let t = tree.node(*target);
                if !t.is_module() {
                    diags.push(error(
                        &file,
                        site.line,
                        format!(
                            "{} resolves to {} {}; only ModuleScripts can be required",
                            site.text,
                            t.class_name,
                            tree.full_name(*target)
                        ),
                    ));
                } else if opts.is_external(*target) {
                    external_count += 1;
                } else if t.file.is_none() {
                    diags.push(error(
                        &file,
                        site.line,
                        format!(
                            "{} resolves to {}, which has no synced source file",
                            site.text,
                            tree.full_name(*target)
                        ),
                    ));
                } else {
                    seen.entry(*target).or_insert_with(|| {
                        order.push(*target);
                        order.len() - 1
                    });
                    unit.deps.push(*target);
                }
            }
        }
        units.push(unit);
    }

    let id_of: HashMap<NodeId, usize> = units
        .iter()
        .filter_map(|u| u.id.map(|id| (u.node, id)))
        .collect();
    // `exclude` only filters `include`; a static require still pulls an excluded module in.
    for (pattern, file, id) in opts.exclude.iter().flat_map(|p| {
        matching(tree, "exclude", p, &mut Vec::new())
            .into_iter()
            .map(move |(f, id)| (p, f, id))
    }) {
        if id == opts.entry || !id_of.contains_key(&id) {
            continue;
        }
        let by: Vec<&str> = units
            .iter()
            .filter(|u| u.deps.contains(&id))
            .map(|u| u.file.as_str())
            .collect();
        diags.push(warning(
            &file,
            0,
            format!(
                "excluded by {pattern:?} but still bundled: required by {}",
                by.join(", ")
            ),
        ));
    }
    // A `script` path is fine in a module when the stand-in can find bundled modules there.
    let mut around_bundled = HashSet::new();
    for &node in id_of.keys() {
        let mut cur = Some(node);
        while let Some(c) = cur.filter(|c| around_bundled.insert(*c)) {
            cur = tree.node(c).parent;
        }
    }
    for unit in &units {
        let mut lines = unit.analysis.script_uses.clone();
        lines.extend(
            unit.analysis
                .script_paths
                .iter()
                .filter(|(_, n)| !around_bundled.contains(n))
                .map(|(l, _)| *l),
        );
        lines.sort();
        lines.dedup();
        let Some(&first) = lines.first() else {
            continue;
        };
        let lines = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let message = if unit.id.is_some() {
            format!(
                "`script` used at runtime (lines {lines}); in the bundle it is a stand-in that only knows its Name, Parent and the bundled modules around it"
            )
        } else {
            format!(
                "`script` used at runtime (lines {lines}); in the bundle it is the bundle Script, not {}",
                tree.full_name(unit.node)
            )
        };
        diags.push(warning(&unit.file, first, message));
    }
    report_cycles(&units, tree, &id_of, &mut diags);

    let (mut code, segments) = emit(&units, opts, &id_of, &mut diags);
    if opts.minify == Minify::Max {
        match rename_locals(&code) {
            Ok(renamed) => code = renamed,
            Err(e) => diags.push(error(&entry_file, 0, e.to_string())),
        }
    }
    diags.sort_by(|a, b| {
        b.level
            .cmp(&a.level)
            .then(a.file.cmp(&b.file))
            .then(a.line.cmp(&b.line))
    });
    Bundle {
        code,
        map: SourceMap {
            version: 1,
            script_name: opts.script_name.clone(),
            entry: entry_file,
            segments,
        },
        diagnostics: diags,
        module_count: id_of.len(),
        external_count,
        inputs: units.iter().map(|u| u.file.clone()).collect(),
    }
}

fn error(file: &str, line: usize, message: String) -> Diagnostic {
    Diagnostic {
        level: Level::Error,
        file: file.to_string(),
        line,
        message,
    }
}

fn warning(file: &str, line: usize, message: String) -> Diagnostic {
    Diagnostic {
        level: Level::Warning,
        file: file.to_string(),
        line,
        message,
    }
}

/// Static cycles are legal when at least one require is lazy (inside a function),
/// so they are warnings; the runtime raises Roblox's own error if one actually loops.
fn report_cycles(
    units: &[Unit],
    tree: &Tree,
    id_of: &HashMap<NodeId, usize>,
    diags: &mut Vec<Diagnostic>,
) {
    let by_id: HashMap<usize, &Unit> = units
        .iter()
        .filter_map(|u| u.id.map(|id| (id, u)))
        .collect();
    let mut state: HashMap<usize, u8> = HashMap::new(); // 1 = visiting, 2 = done
    let mut stack: Vec<usize> = Vec::new();
    fn visit(
        id: usize,
        by_id: &HashMap<usize, &Unit>,
        id_of: &HashMap<NodeId, usize>,
        state: &mut HashMap<usize, u8>,
        stack: &mut Vec<usize>,
        found: &mut Vec<Vec<usize>>,
    ) {
        state.insert(id, 1);
        stack.push(id);
        for dep in by_id[&id].deps.iter().filter_map(|n| id_of.get(n).copied()) {
            match state.get(&dep) {
                Some(1) => {
                    let start = stack.iter().position(|&s| s == dep).unwrap();
                    let mut cycle = stack[start..].to_vec();
                    cycle.push(dep);
                    found.push(cycle);
                }
                Some(_) => {}
                None => visit(dep, by_id, id_of, state, stack, found),
            }
        }
        stack.pop();
        state.insert(id, 2);
    }
    let mut found = Vec::new();
    let mut ids: Vec<usize> = by_id.keys().copied().collect();
    ids.sort();
    for id in ids {
        if !state.contains_key(&id) {
            visit(id, &by_id, id_of, &mut state, &mut stack, &mut found);
        }
    }
    for cycle in found {
        let names: Vec<String> = cycle
            .iter()
            .map(|id| tree.node(by_id[id].node).name.clone())
            .collect();
        diags.push(warning(
            &by_id[&cycle[0]].file,
            0,
            format!(
                "circular require {}; fine only if one of these requires runs lazily",
                names.join(" -> ")
            ),
        ));
    }
}

/// Rewrites resolved requires and strips `export` from type declarations, keeping
/// the line count of every replaced span so the source map stays exact.
fn rewrite(unit: &Unit, tree: &Tree, id_of: &HashMap<NodeId, usize>, opts: &Options) -> String {
    let annotate = opts.minify == Minify::None;
    let mut edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    for site in unit.analysis.requires.iter().filter(|r| !r.never_runs) {
        let Target::Node(n) = site.target else {
            continue;
        };
        let pad = "\n".repeat(site.text.matches('\n').count());
        let Some(id) = id_of.get(&n) else {
            // External module: `script`-relative paths mean nothing inside the bundle,
            // so point at the real instance from `game`.
            if opts.is_external(n) && tree.node(n).is_module() {
                edits.push((
                    site.range.clone(),
                    format!("require({}){pad}", tree.runtime_path(n)),
                ));
            }
            continue;
        };
        let text = if annotate {
            format!(
                "__KLSM_require({id} --[[{}]]){pad}",
                tree.full_name(n).replace("]]", "] ]")
            )
        } else {
            format!("__KLSM_require({id}){pad}")
        };
        edits.push((site.range.clone(), text));
    }
    // In the entry `script` is the bundle Script, which lives somewhere else; paths that
    // start at it (`script.Parent.Parent.Addon`) go through a stand-in at the original place.
    if unit.id.is_none() && !unit.analysis.script_reroutes.is_empty() {
        let segs: Vec<String> = tree
            .segments(unit.node)
            .iter()
            .map(|s| lua_string(s))
            .collect();
        let standin = format!("__KLSM_script({{{}}})", segs.join(","));
        for &byte in &unit.analysis.script_reroutes {
            edits.push((byte..byte + "script".len(), standin.clone()));
        }
    }
    if unit.id.is_some() {
        for range in &unit.analysis.exports {
            edits.push((range.clone(), String::new()));
        }
    }
    edits.sort_by_key(|(r, _)| std::cmp::Reverse(r.start));
    let mut out = unit.source.clone();
    for (range, text) in edits {
        out.replace_range(range, &text);
    }
    out
}

fn emit(
    units: &[Unit],
    opts: &Options,
    id_of: &HashMap<NodeId, usize>,
    diags: &mut Vec<Diagnostic>,
) -> (String, Vec<Segment>) {
    let tree = opts.tree;
    let entry = &units[0];
    let mut out = String::new();
    let mut segments = Vec::new();
    let mut lines = LineCount::default();
    // `none` keeps the file readable; `light`/`full` drop every comment and space the
    // bundler adds itself, and `full` also drops the newlines between the module wrappers
    // (one newline per module stays, so `klsm trace` can still name the module).
    let annotate = opts.minify == Minify::None;
    let full = matches!(opts.minify, Minify::Full | Minify::Max);
    let exact = !full;
    let (eq, nl) = if annotate {
        (" = ", "\n")
    } else {
        ("=", if full { "" } else { "\n" })
    };
    let mut squash = |text: &str, file: &str| match minify(text, opts.minify) {
        Ok(s) => s,
        Err(e) => {
            diags.push(error(file, 0, e.to_string()));
            text.to_string()
        }
    };

    out.push_str("--!nocheck\n");
    for d in &entry.analysis.directives {
        if d == "native" || d.starts_with("optimize") {
            out.push_str(&format!("--!{d}\n"));
        }
    }
    if annotate {
        out.push_str(&format!(
            "-- Bundled by KlSMBundlerLuau {VERSION} from {} ({} modules). Generated file: edit the sources, not this.\n",
            entry.file,
            id_of.len()
        ));
    }

    if !opts.prologue.is_empty() {
        glue(
            &mut out,
            squash(opts.prologue, "<prologue>").trim_end_matches('\n'),
        );
        out.push('\n');
    }

    let needs_proxy = units.iter().any(|u| {
        if u.id.is_some() {
            u.analysis.uses_script()
        } else {
            !u.analysis.script_reroutes.is_empty()
        }
    });
    let runtime = match RUNTIME.split_once(PROXY_MARKER) {
        Some((core, proxy)) if needs_proxy => format!("{core}{proxy}"),
        Some((core, _)) => core.to_string(),
        None => RUNTIME.to_string(),
    };
    glue(
        &mut out,
        squash(&runtime, "<runtime>").trim_end_matches('\n'),
    );
    out.push_str(nl);

    let mut modules: Vec<&Unit> = units.iter().filter(|u| u.id.is_some()).collect();
    modules.sort_by_key(|u| u.id);
    for unit in modules {
        let id = unit.id.unwrap();
        let body = squash(&rewrite(unit, tree, id_of, opts), &unit.file);
        let proxy = if !unit.analysis.uses_script() {
            String::new()
        } else {
            let segs: Vec<String> = tree
                .segments(unit.node)
                .iter()
                .map(|s| lua_string(s))
                .collect();
            let sep = if annotate { ", " } else { "," };
            format!(" local script{eq}__KLSM_script({{{}}})", segs.join(sep))
        };
        glue(
            &mut out,
            &format!(
                "__KLSM_names[{id}]{eq}{}",
                lua_string(&tree.full_name(unit.node))
            ),
        );
        out.push_str(nl);
        let comment = if annotate {
            format!(" -- {}", unit.file)
        } else {
            String::new()
        };
        glue(
            &mut out,
            &format!("__KLSM_modules[{id}]{eq}function(...){proxy}{comment}"),
        );
        out.push_str(nl);
        push_body(
            &mut out,
            &mut segments,
            &mut lines,
            unit,
            tree,
            &body,
            exact,
        );
        glue(&mut out, "end\n");
    }

    if let Some(id) = entry.id {
        glue(&mut out, &format!("return __KLSM_require({id})\n"));
    } else {
        let body = squash(&rewrite(entry, tree, id_of, opts), &entry.file);
        if annotate {
            out.push_str(&format!("do -- {}\n", entry.file));
        } else {
            glue(&mut out, "do");
            out.push_str(nl);
        }
        push_body(
            &mut out,
            &mut segments,
            &mut lines,
            entry,
            tree,
            &body,
            exact,
        );
        glue(&mut out, "end\n");
    }
    (out, segments)
}

/// Newlines in `out` so far, counted only over what was appended since the last call
/// (recounting the whole bundle per module is quadratic).
#[derive(Default)]
struct LineCount {
    scanned: usize,
    lines: usize,
}

impl LineCount {
    fn of(&mut self, out: &str) -> usize {
        self.lines += out.as_bytes()[self.scanned..]
            .iter()
            .filter(|&&b| b == b'\n')
            .count();
        self.scanned = out.len();
        self.lines
    }
}

fn push_body(
    out: &mut String,
    segments: &mut Vec<Segment>,
    lines: &mut LineCount,
    unit: &Unit,
    tree: &Tree,
    body: &str,
    exact: bool,
) {
    let out_start = lines.of(out) + 1;
    let out_end = if exact {
        out.push_str(body);
        if !body.ends_with('\n') {
            out.push('\n');
        }
        lines.of(out)
    } else {
        glue(out, body.trim_end_matches('\n'));
        lines.of(out) + 1
    };
    segments.push(Segment {
        file: unit.file.clone(),
        instance: tree.full_name(unit.node),
        out_start,
        out_end: out_end.max(out_start),
        exact,
    });
}
