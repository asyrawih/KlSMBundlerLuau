//! Per-file analysis: finds `require` calls and statically resolves their argument
//! to a node in the sync tree, tracking simple local aliases such as
//! `local ReplicatedStorage = game:GetService("ReplicatedStorage")`.

use crate::tree::{NodeId, ROOT, Tree};
use full_moon::LuaVersion;
use full_moon::ast::luau::{ExportedTypeDeclaration, ExportedTypeFunction, TypeInfo};
use full_moon::ast::{
    BinOp, Call, Expression, FunctionArgs, FunctionCall, Index, LocalAssignment, Prefix, Suffix,
    Var, VarExpression,
};
use full_moon::node::Node;
use full_moon::tokenizer::{TokenReference, TokenType};
use full_moon::visitors::Visitor;
use std::collections::HashMap;
use std::ops::Range;

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// Resolved to a node in the tree.
    Node(NodeId),
    /// Statically shaped but points at something that isn't in the tree.
    Missing(String),
    /// Not statically resolvable (variables, computed names, ...).
    Dynamic,
    /// `require(123456)` — a published asset; left untouched.
    Asset,
}

#[derive(Debug, Clone)]
pub struct RequireSite {
    /// Byte range of `require(...)` in the source.
    pub range: Range<usize>,
    pub line: usize,
    pub text: String,
    pub target: Target,
    /// Never executed in Roblox: inside a type annotation (`typeof(require(...))`) or
    /// behind a `game and x or require(...)` fallback for non-Roblox runtimes.
    pub never_runs: bool,
}

#[derive(Debug, Default)]
pub struct Analysis {
    pub requires: Vec<RequireSite>,
    /// `export` keywords, which are illegal once the module body is wrapped in a function.
    pub exports: Vec<Range<usize>>,
    /// Lines where `script` is used for something other than a resolvable path.
    pub script_uses: Vec<usize>,
    /// `--!` directives at the top of the file.
    pub directives: Vec<String>,
}

#[derive(Debug)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

/// `.luaurc` aliases: lower-cased name → path relative to the tree root.
pub type Aliases = HashMap<String, String>;

pub fn analyze(
    source: &str,
    tree: &Tree,
    node: NodeId,
    aliases: &Aliases,
) -> Result<Analysis, Vec<ParseError>> {
    let result = full_moon::parse_fallible(source, LuaVersion::luau());
    if !result.errors().is_empty() {
        return Err(result
            .errors()
            .iter()
            .map(|e| ParseError {
                line: e.range().0.line(),
                message: e.error_message().into_owned(),
            })
            .collect());
    }
    let mut visitor = Analyzer {
        tree,
        node,
        aliases,
        env: HashMap::new(),
        type_depth: 0,
        out: Analysis::default(),
        script_tokens: Vec::new(),
        path_ranges: Vec::new(),
        dead_ranges: Vec::new(),
    };
    visitor.visit_ast(result.ast());

    let Analyzer {
        mut out,
        script_tokens,
        path_ranges,
        dead_ranges,
        ..
    } = visitor;
    for (byte, line) in script_tokens {
        let covered = out.requires.iter().any(|r| r.range.contains(&byte))
            || path_ranges.iter().any(|r| r.contains(&byte))
            || dead_ranges.iter().any(|r| r.contains(&byte));
        if !covered {
            out.script_uses.push(line);
        }
    }
    out.script_uses.dedup();
    out.directives = source
        .lines()
        .take_while(|l| l.trim().is_empty() || l.trim_start().starts_with("--"))
        .filter_map(|l| l.trim().strip_prefix("--!").map(|d| d.trim().to_string()))
        .collect();
    Ok(out)
}

struct Analyzer<'a> {
    tree: &'a Tree,
    node: NodeId,
    aliases: &'a Aliases,
    env: HashMap<String, Target>,
    type_depth: usize,
    out: Analysis,
    script_tokens: Vec<(usize, usize)>,
    /// Alias definitions that resolved to a tree node (`local UI = script.Parent.UI`).
    path_ranges: Vec<Range<usize>>,
    /// Right-hand sides of `game and x or <here>` fallbacks.
    dead_ranges: Vec<Range<usize>>,
}

fn unparen(expr: &Expression) -> &Expression {
    match expr {
        Expression::Parentheses { expression, .. } => unparen(expression),
        _ => expr,
    }
}

fn string_literal(expr: &Expression) -> Option<String> {
    match expr {
        Expression::String(tok) => token_string(tok),
        Expression::Parentheses { expression, .. } => string_literal(expression),
        _ => None,
    }
}

fn token_string(tok: &TokenReference) -> Option<String> {
    match tok.token_type() {
        TokenType::StringLiteral { literal, .. } => Some(literal.to_string()),
        _ => None,
    }
}

fn first_arg(args: &FunctionArgs) -> Option<&Expression> {
    match args {
        FunctionArgs::Parentheses { arguments, .. } => arguments.iter().next(),
        _ => None,
    }
}

fn arg_string(args: &FunctionArgs) -> Option<String> {
    match args {
        FunctionArgs::String(tok) => token_string(tok),
        _ => first_arg(args).and_then(string_literal),
    }
}

fn ident(tok: &TokenReference) -> Option<&str> {
    match tok.token_type() {
        TokenType::Identifier { identifier } => Some(identifier.as_str()),
        _ => None,
    }
}

impl Analyzer<'_> {
    fn child(&self, base: NodeId, name: &str) -> Target {
        match self.tree.child(base, name) {
            Some(c) => Target::Node(c),
            None => Target::Missing(format!(
                "{} has no child \"{name}\"",
                self.tree.full_name(base)
            )),
        }
    }

    fn lookup(&self, name: &str) -> Target {
        if let Some(t) = self.env.get(name) {
            return t.clone();
        }
        match name {
            "script" => Target::Node(self.node),
            "game" => Target::Node(ROOT),
            "workspace" => self.child(ROOT, "Workspace"),
            _ => Target::Dynamic,
        }
    }

    fn eval(&self, expr: &Expression) -> Target {
        match expr {
            Expression::Parentheses { expression, .. } => self.eval(expression),
            Expression::TypeAssertion { expression, .. } => self.eval(expression),
            Expression::Var(Var::Name(tok)) => {
                ident(tok).map_or(Target::Dynamic, |n| self.lookup(n))
            }
            Expression::Var(Var::Expression(ve)) => self.eval_chain(ve.prefix(), ve.suffixes()),
            Expression::FunctionCall(fc) => self.eval_chain(fc.prefix(), fc.suffixes()),
            Expression::Number(_) => Target::Asset,
            _ => Target::Dynamic,
        }
    }

    fn eval_chain<'s>(
        &self,
        prefix: &Prefix,
        suffixes: impl Iterator<Item = &'s Suffix>,
    ) -> Target {
        let mut cur = match prefix {
            Prefix::Name(tok) => ident(tok).map_or(Target::Dynamic, |n| self.lookup(n)),
            Prefix::Expression(e) => self.eval(e),
            _ => Target::Dynamic,
        };
        for suffix in suffixes {
            let Target::Node(base) = cur else { return cur };
            cur = match suffix {
                Suffix::Index(Index::Dot { name, .. }) => match ident(name) {
                    Some("Parent") => self
                        .tree
                        .node(base)
                        .parent
                        .map_or(Target::Missing("game has no Parent".into()), Target::Node),
                    Some(n) => self.child(base, n),
                    None => Target::Dynamic,
                },
                Suffix::Index(Index::Brackets { expression, .. }) => {
                    match string_literal(expression) {
                        Some(n) => self.child(base, &n),
                        None => Target::Dynamic,
                    }
                }
                Suffix::Call(Call::MethodCall(mc)) => {
                    let method = ident(mc.name()).unwrap_or("");
                    match (method, arg_string(mc.args())) {
                        (
                            "WaitForChild" | "FindFirstChild" | "GetService" | "FindService",
                            Some(n),
                        ) => self.child(base, &n),
                        ("FindFirstAncestor", Some(n)) => {
                            match self.tree.ancestor_named(base, &n) {
                                Some(a) => Target::Node(a),
                                None => Target::Missing(format!(
                                    "{} has no ancestor \"{n}\"",
                                    self.tree.full_name(base)
                                )),
                            }
                        }
                        _ => Target::Dynamic,
                    }
                }
                Suffix::TypeInstantiation(_) => Target::Node(base),
                _ => Target::Dynamic,
            };
        }
        cur
    }

    /// Luau string requires: `./x`, `../x`, `@self/x`, `@game/...`, `.luaurc` aliases.
    fn resolve_string(&self, path: &str) -> Target {
        let path = path.trim_end_matches(".luau").trim_end_matches(".lua");
        let mut parts = path.split('/').filter(|p| !p.is_empty()).peekable();
        let Some(first) = parts.next() else {
            return Target::Missing("empty require path".into());
        };
        let parent = self.tree.node(self.node).parent.unwrap_or(ROOT);
        let mut cur = match first {
            "." => parent,
            ".." => self.tree.node(parent).parent.unwrap_or(ROOT),
            "@self" => self.node,
            "@game" => ROOT,
            alias if alias.starts_with('@') => {
                let key = alias[1..].to_ascii_lowercase();
                let Some(base) = self.aliases.get(&key) else {
                    return Target::Missing(format!("unknown alias {alias} (add it to .luaurc)"));
                };
                let rest: Vec<&str> = parts.collect();
                let full = if rest.is_empty() {
                    base.clone()
                } else {
                    format!("{base}/{}", rest.join("/"))
                };
                return self.node_for_path(&full);
            }
            other => {
                return Target::Missing(format!(
                    "require(\"{other}...\") must start with ./, ../, @self or an alias"
                ));
            }
        };
        for part in parts {
            cur = match part {
                "." => cur,
                ".." => self.tree.node(cur).parent.unwrap_or(ROOT),
                name => match self.tree.child(cur, name) {
                    Some(c) => c,
                    None => {
                        return Target::Missing(format!(
                            "{} has no child \"{name}\"",
                            self.tree.full_name(cur)
                        ));
                    }
                },
            };
        }
        Target::Node(cur)
    }

    fn node_for_path(&self, rel: &str) -> Target {
        let rel = rel.trim_end_matches('/');
        for candidate in [
            rel.to_string(),
            format!("{rel}.luau"),
            format!("{rel}.lua"),
            format!("{rel}/init.luau"),
            format!("{rel}/init.lua"),
        ] {
            if let Some(id) = self.tree.by_file(&candidate) {
                return Target::Node(id);
            }
        }
        Target::Missing(format!("no script at {rel}"))
    }
}

impl Visitor for Analyzer<'_> {
    fn visit_function_call(&mut self, call: &FunctionCall) {
        self.require_call(call.prefix(), call.suffixes().next());
    }

    /// `require(x).Field` parses as a var expression, not a call.
    fn visit_var_expression(&mut self, var: &VarExpression) {
        self.require_call(var.prefix(), var.suffixes().next());
    }

    fn visit_local_assignment_end(&mut self, assignment: &LocalAssignment) {
        let exprs: Vec<&Expression> = assignment.expressions().iter().collect();
        for (i, name) in assignment.names().iter().enumerate() {
            let Some(name) = ident(name) else { continue };
            match exprs.get(i).map(|e| (e, self.eval(e))) {
                Some((expr, target @ Target::Node(_))) => {
                    if let Some((s, e)) = expr.range() {
                        self.path_ranges.push(s.bytes()..e.bytes());
                    }
                    self.env.insert(name.to_string(), target);
                }
                Some((_, target @ Target::Missing(_))) => {
                    self.env.insert(name.to_string(), target);
                }
                _ => {
                    self.env.remove(name);
                }
            }
        }
    }

    fn visit_prefix(&mut self, prefix: &Prefix) {
        if let Prefix::Name(tok) = prefix {
            self.note_script(tok);
        }
    }

    fn visit_var(&mut self, var: &Var) {
        if let Var::Name(tok) = var {
            self.note_script(tok);
        }
    }

    /// `game and x or y`: `game` is always truthy in Roblox, so `y` never runs there.
    fn visit_expression(&mut self, expr: &Expression) {
        let Expression::BinaryOperator {
            lhs,
            binop: BinOp::Or(_),
            rhs,
        } = expr
        else {
            return;
        };
        let Expression::BinaryOperator {
            lhs: guard,
            binop: BinOp::And(_),
            ..
        } = unparen(lhs)
        else {
            return;
        };
        if matches!(unparen(guard), Expression::Var(Var::Name(tok)) if ident(tok) == Some("game"))
            && let Some((s, e)) = rhs.range()
        {
            self.dead_ranges.push(s.bytes()..e.bytes());
        }
    }

    fn visit_type_info(&mut self, _: &TypeInfo) {
        self.type_depth += 1;
    }

    fn visit_type_info_end(&mut self, _: &TypeInfo) {
        self.type_depth -= 1;
    }

    fn visit_exported_type_declaration(&mut self, decl: &ExportedTypeDeclaration) {
        self.push_export(decl.export_token());
    }

    fn visit_exported_type_function(&mut self, decl: &ExportedTypeFunction) {
        self.push_export(decl.export_token());
    }
}

impl Analyzer<'_> {
    fn require_call(&mut self, prefix: &Prefix, first: Option<&Suffix>) {
        let Prefix::Name(name) = prefix else { return };
        if ident(name) != Some("require") {
            return;
        }
        let Some(first @ Suffix::Call(Call::AnonymousCall(args))) = first else {
            return;
        };
        let target = match (args, first_arg(args)) {
            (FunctionArgs::String(tok), _) => {
                token_string(tok).map_or(Target::Dynamic, |s| self.resolve_string(&s))
            }
            (_, Some(expr)) => match string_literal(expr) {
                Some(s) => self.resolve_string(&s),
                None => self.eval(expr),
            },
            _ => Target::Dynamic,
        };
        let start = name.start_position().unwrap().bytes();
        let end = first.end_position().map_or(start, |p| p.bytes());
        self.out.requires.push(RequireSite {
            range: start..end,
            line: name.start_position().unwrap().line(),
            text: String::new(),
            target,
            never_runs: self.type_depth > 0 || self.dead_ranges.iter().any(|r| r.contains(&start)),
        });
    }

    fn note_script(&mut self, tok: &TokenReference) {
        if self.type_depth == 0 && ident(tok) == Some("script") && !self.env.contains_key("script")
        {
            let pos = tok.start_position().unwrap();
            self.script_tokens.push((pos.bytes(), pos.line()));
        }
    }

    fn push_export(&mut self, tok: &TokenReference) {
        let (s, e) = (tok.start_position().unwrap(), tok.end_position().unwrap());
        self.out.exports.push(s.bytes()..e.bytes());
    }
}

/// Fills `RequireSite::text` from the source (kept out of the visitor to avoid
/// threading the source through it).
pub fn attach_text(source: &str, analysis: &mut Analysis) {
    for r in &mut analysis.requires {
        r.text = source.get(r.range.clone()).unwrap_or_default().to_string();
    }
}
