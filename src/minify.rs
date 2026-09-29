//! Token-level minifier: drops comments and redundant whitespace.
//!
//! `Light` keeps every newline so line numbers (and the source map) stay exact;
//! `Full` also drops newlines, so traces only resolve to the module.

use anyhow::{Result, anyhow};
use full_moon::LuaVersion;
use full_moon::tokenizer::{Lexer, LexerResult, TokenType};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Minify {
    #[default]
    None,
    Light,
    Full,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_op(c: char) -> bool {
    "+-*/%^#=<>~.:&|?!".contains(c)
}

fn needs_space(prev: char, next: char) -> bool {
    (is_word(prev) && is_word(next))
        || (is_op(prev) && is_op(next))
        // `t[ [[x]] ]`, and `{{` / `}}` are rejected inside interpolated strings.
        || (prev == '[' && (next == '[' || next == '='))
        || (prev == '{' && next == '{')
        || (prev == '}' && next == '}')
        // `1 ..x` must not become the malformed number `1..x`.
        || (prev.is_ascii_digit() && next == '.')
}

pub fn minify(source: &str, level: Minify) -> Result<String> {
    if level == Minify::None {
        return Ok(source.to_string());
    }
    let tokens = match Lexer::new(source, LuaVersion::luau()).collect() {
        LexerResult::Ok(tokens) => tokens,
        LexerResult::Recovered(_, errors) | LexerResult::Fatal(errors) => {
            return Err(anyhow!("minify: {}", errors[0]));
        }
    };
    let keep_lines = level == Minify::Light;
    let mut out = String::with_capacity(source.len() / 2);
    let mut separated = false;
    for token in &tokens {
        match token.token_type() {
            TokenType::Whitespace { characters } => {
                separated = true;
                if keep_lines {
                    out.extend(std::iter::repeat_n('\n', characters.matches('\n').count()));
                }
            }
            TokenType::SingleLineComment { .. } => separated = true,
            TokenType::MultiLineComment { comment, .. } => {
                separated = true;
                if keep_lines {
                    out.extend(std::iter::repeat_n('\n', comment.matches('\n').count()));
                }
            }
            TokenType::Eof => {}
            _ => {
                let text = token.to_string();
                if let (Some(prev), Some(next)) = (out.chars().next_back(), text.chars().next())
                    && separated
                    && prev != '\n'
                    && needs_space(prev, next)
                {
                    out.push(' ');
                }
                out.push_str(&text);
                separated = false;
            }
        }
    }
    // Collapse the blank-line runs Light leaves behind at the end.
    while out.ends_with("\n\n") {
        out.pop();
    }
    Ok(out)
}
