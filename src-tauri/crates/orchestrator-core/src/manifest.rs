//! Parses a single Odoo `__manifest__.py` (or the legacy `__openerp__.py`).
//!
//! These files are a Python dict *literal* — not arbitrary Python — so rather
//! than shelling out to a Python interpreter (a runtime dependency this crate
//! deliberately has none of, per its own doc comment) this hand-rolls a
//! recursive-descent parser for the literal subset Python manifests actually
//! use: dicts, lists/tuples, strings (single/double/triple-quoted, with
//! adjacent-string concatenation), numbers, `True`/`False`/`None`, and `#`
//! comments. That subset is small and stable — Odoo manifests have looked the
//! same shape for over a decade — so this is worth owning outright instead of
//! pulling in a general Python parser for one dict literal per module.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("couldn't read {path}: {source}")]
    Io { path: PathBuf, #[source] source: std::io::Error },
    #[error("{path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("{path}: manifest must be a single {{...}} dict literal")]
    NotADict { path: PathBuf },
}

/// A parsed Python literal value — the full range a manifest dict's values
/// can take. Nested structures we don't specifically care about (e.g.
/// `external_dependencies`) still parse correctly as `Dict`/`List`, they're
/// just not unpacked into `ModuleManifest`.
#[derive(Debug, Clone, PartialEq)]
pub enum PyValue {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<PyValue>),
    Dict(Vec<(String, PyValue)>),
}

impl PyValue {
    fn as_str(&self) -> Option<&str> {
        match self {
            PyValue::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    fn as_bool(&self) -> Option<bool> {
        match self {
            PyValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    fn as_str_list(&self) -> Option<Vec<String>> {
        match self {
            PyValue::List(items) => items.iter().map(|v| v.as_str().map(str::to_string)).collect(),
            _ => None,
        }
    }
}

/// Whether `auto_install` is a plain bool or (Odoo 17+) a list of module names
/// meaning "auto-install once all of these are installed."
#[derive(Debug, Clone, PartialEq)]
pub enum AutoInstall {
    Bool(bool),
    IfInstalled(Vec<String>),
}

impl AutoInstall {
    pub fn is_enabled(&self) -> bool {
        match self {
            AutoInstall::Bool(b) => *b,
            AutoInstall::IfInstalled(mods) => !mods.is_empty(),
        }
    }
}

/// The subset of manifest fields the orchestrator actually acts on. Odoo
/// manifests carry many more keys (`author`, `website`, `license`,
/// `external_dependencies`, ...) — deliberately not modeled here; add fields
/// as a task actually needs them rather than mirroring the whole schema
/// speculatively.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ModuleManifest {
    pub name: String,
    pub version: Option<String>,
    pub category: Option<String>,
    pub depends: Vec<String>,
    pub installable: bool,
    pub auto_install_enabled: bool,
    pub application: bool,
}

impl ModuleManifest {
    fn from_dict(entries: &[(String, PyValue)], fallback_name: &str) -> Self {
        let get = |key: &str| entries.iter().find(|(k, _)| k == key).map(|(_, v)| v);

        let name = get("name").and_then(PyValue::as_str).map(str::to_string).unwrap_or_else(|| fallback_name.to_string());
        let version = get("version").and_then(PyValue::as_str).map(str::to_string);
        let category = get("category").and_then(PyValue::as_str).map(str::to_string);
        let depends = get("depends").and_then(PyValue::as_str_list).unwrap_or_default();
        // Odoo's own default when the key is absent is `True`.
        let installable = get("installable").and_then(PyValue::as_bool).unwrap_or(true);
        let application = get("application").and_then(PyValue::as_bool).unwrap_or(false);
        let auto_install = match get("auto_install") {
            Some(PyValue::Bool(b)) => AutoInstall::Bool(*b),
            Some(v @ PyValue::List(_)) => AutoInstall::IfInstalled(v.as_str_list().unwrap_or_default()),
            _ => AutoInstall::Bool(false),
        };

        ModuleManifest {
            name,
            version,
            category,
            depends,
            installable,
            auto_install_enabled: auto_install.is_enabled(),
            application,
        }
    }
}

/// Parse a manifest file from disk. `fallback_name` is used for the `name`
/// field when the manifest doesn't set one (rare, but seen in minimal
/// private modules) — callers pass the module's directory/technical name.
pub fn parse_manifest_file(path: &Path, fallback_name: &str) -> Result<ModuleManifest, ManifestError> {
    let src = fs::read_to_string(path).map_err(|source| ManifestError::Io { path: path.to_path_buf(), source })?;
    let value = parse_py_literal(&src).map_err(|e| ManifestError::Parse { path: path.to_path_buf(), message: e.to_string() })?;
    match value {
        PyValue::Dict(entries) => Ok(ModuleManifest::from_dict(&entries, fallback_name)),
        _ => Err(ManifestError::NotADict { path: path.to_path_buf() }),
    }
}

// --- tokenizer -------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    LParen,
    RParen,
    Colon,
    Comma,
    Str(String),
    Int(i64),
    Float(f64),
    True,
    False,
    NoneKw,
    Eof,
}

#[derive(Debug, Clone)]
struct Spanned {
    tok: Tok,
    line: usize,
    col: usize,
}

#[derive(Debug)]
pub struct PyParseError {
    message: String,
    line: usize,
    col: usize,
}

impl fmt::Display for PyParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}, col {}: {}", self.line, self.col, self.message)
    }
}
impl std::error::Error for PyParseError {}

struct Lexer<'a> {
    chars: Vec<char>,
    pos: usize,
    line: usize,
    col: usize,
    _src: std::marker::PhantomData<&'a str>,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str) -> Self {
        Self { chars: src.chars().collect(), pos: 0, line: 1, col: 1, _src: std::marker::PhantomData }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn err(&self, message: impl Into<String>) -> PyParseError {
        PyParseError { message: message.into(), line: self.line, col: self.col }
    }

    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => {
                    self.advance();
                }
                Some('#') => {
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.advance();
                    }
                }
                _ => break,
            }
        }
    }

    fn read_string(&mut self) -> Result<String, PyParseError> {
        let quote = self.advance().expect("caller checked a quote is present");
        // Triple-quoted?
        if self.peek() == Some(quote) && self.peek_at(1) == Some(quote) {
            self.advance();
            self.advance();
            let mut out = String::new();
            loop {
                if self.peek() == Some(quote) && self.peek_at(1) == Some(quote) && self.peek_at(2) == Some(quote) {
                    self.advance();
                    self.advance();
                    self.advance();
                    return Ok(out);
                }
                match self.advance() {
                    Some(c) => out.push(c),
                    None => return Err(self.err("unterminated triple-quoted string")),
                }
            }
        }

        let mut out = String::new();
        loop {
            match self.advance() {
                None => return Err(self.err("unterminated string literal")),
                Some(c) if c == quote => return Ok(out),
                Some('\\') => match self.advance() {
                    None => return Err(self.err("unterminated escape sequence")),
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('\\') => out.push('\\'),
                    Some('\'') => out.push('\''),
                    Some('"') => out.push('"'),
                    Some('\n') => {} // line continuation
                    Some(other) => {
                        // Unknown escape: keep it literally rather than erroring —
                        // manifests occasionally have stray backslashes in paths.
                        out.push('\\');
                        out.push(other);
                    }
                },
                Some(c) => out.push(c),
            }
        }
    }

    fn read_number(&mut self) -> Result<Tok, PyParseError> {
        let start_line = self.line;
        let start_col = self.col;
        let mut s = String::new();
        if self.peek() == Some('-') {
            s.push(self.advance().unwrap());
        }
        let mut is_float = false;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                s.push(self.advance().unwrap());
            } else if c == '.' && !is_float {
                is_float = true;
                s.push(self.advance().unwrap());
            } else {
                break;
            }
        }
        if is_float {
            s.parse::<f64>().map(Tok::Float).map_err(|_| PyParseError {
                message: format!("invalid float literal '{s}'"),
                line: start_line,
                col: start_col,
            })
        } else {
            s.parse::<i64>().map(Tok::Int).map_err(|_| PyParseError {
                message: format!("invalid int literal '{s}'"),
                line: start_line,
                col: start_col,
            })
        }
    }

    fn read_ident(&mut self) -> String {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' {
                s.push(self.advance().unwrap());
            } else {
                break;
            }
        }
        s
    }

    fn next_spanned(&mut self) -> Result<Spanned, PyParseError> {
        self.skip_trivia();
        let (line, col) = (self.line, self.col);
        let tok = match self.peek() {
            None => Tok::Eof,
            Some('{') => {
                self.advance();
                Tok::LBrace
            }
            Some('}') => {
                self.advance();
                Tok::RBrace
            }
            Some('[') => {
                self.advance();
                Tok::LBracket
            }
            Some(']') => {
                self.advance();
                Tok::RBracket
            }
            Some('(') => {
                self.advance();
                Tok::LParen
            }
            Some(')') => {
                self.advance();
                Tok::RParen
            }
            Some(':') => {
                self.advance();
                Tok::Colon
            }
            Some(',') => {
                self.advance();
                Tok::Comma
            }
            Some(c) if c == '\'' || c == '"' => Tok::Str(self.read_string()?),
            Some(c) if c.is_ascii_digit() => self.read_number()?,
            Some('-') if self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) => self.read_number()?,
            Some(c) if c.is_alphabetic() || c == '_' => match self.read_ident().as_str() {
                "True" => Tok::True,
                "False" => Tok::False,
                "None" => Tok::NoneKw,
                other => return Err(self.err(format!("unexpected identifier '{other}' (only True/False/None are valid here)"))),
            },
            Some(c) => return Err(self.err(format!("unexpected character '{c}'"))),
        };
        Ok(Spanned { tok, line, col })
    }
}

fn lex(src: &str) -> Result<Vec<Spanned>, PyParseError> {
    let mut lexer = Lexer::new(src);
    let mut out = Vec::new();
    loop {
        let spanned = lexer.next_spanned()?;
        let is_eof = spanned.tok == Tok::Eof;
        out.push(spanned);
        if is_eof {
            break;
        }
    }
    Ok(out)
}

// --- recursive-descent parser -----------------------------------------------

struct Parser {
    toks: Vec<Spanned>,
    idx: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.idx].tok
    }

    fn pos(&self) -> (usize, usize) {
        (self.toks[self.idx].line, self.toks[self.idx].col)
    }

    fn bump(&mut self) -> Tok {
        let t = self.toks[self.idx].tok.clone();
        if self.idx + 1 < self.toks.len() {
            self.idx += 1;
        }
        t
    }

    fn err(&self, message: impl Into<String>) -> PyParseError {
        let (line, col) = self.pos();
        PyParseError { message: message.into(), line, col }
    }

    fn expect(&mut self, tok: Tok) -> Result<(), PyParseError> {
        if *self.peek() == tok {
            self.bump();
            Ok(())
        } else {
            Err(self.err(format!("expected {tok:?}, found {:?}", self.peek())))
        }
    }

    fn parse_value(&mut self) -> Result<PyValue, PyParseError> {
        match self.peek().clone() {
            Tok::Str(_) => {
                // Adjacent string literals concatenate, same as real Python.
                let mut s = String::new();
                while let Tok::Str(part) = self.peek().clone() {
                    s.push_str(&part);
                    self.bump();
                }
                Ok(PyValue::Str(s))
            }
            Tok::Int(n) => {
                self.bump();
                Ok(PyValue::Int(n))
            }
            Tok::Float(n) => {
                self.bump();
                Ok(PyValue::Float(n))
            }
            Tok::True => {
                self.bump();
                Ok(PyValue::Bool(true))
            }
            Tok::False => {
                self.bump();
                Ok(PyValue::Bool(false))
            }
            Tok::NoneKw => {
                self.bump();
                Ok(PyValue::None)
            }
            Tok::LBracket => self.parse_seq(Tok::RBracket).map(PyValue::List),
            Tok::LParen => self.parse_seq(Tok::RParen).map(PyValue::List),
            Tok::LBrace => self.parse_dict().map(PyValue::Dict),
            other => Err(self.err(format!("unexpected token {other:?} where a value was expected"))),
        }
    }

    fn parse_seq(&mut self, close: Tok) -> Result<Vec<PyValue>, PyParseError> {
        self.bump(); // opening bracket/paren
        let mut items = Vec::new();
        loop {
            if *self.peek() == close {
                self.bump();
                return Ok(items);
            }
            items.push(self.parse_value()?);
            match self.peek().clone() {
                Tok::Comma => {
                    self.bump();
                }
                t if t == close => {
                    self.bump();
                    return Ok(items);
                }
                other => return Err(self.err(format!("expected ',' or closing bracket, found {other:?}"))),
            }
        }
    }

    fn parse_dict(&mut self) -> Result<Vec<(String, PyValue)>, PyParseError> {
        self.bump(); // '{'
        let mut entries = Vec::new();
        loop {
            if *self.peek() == Tok::RBrace {
                self.bump();
                return Ok(entries);
            }
            let key = match self.parse_value()? {
                PyValue::Str(s) => s,
                other => return Err(self.err(format!("dict keys must be string literals, found {other:?}"))),
            };
            self.expect(Tok::Colon)?;
            let value = self.parse_value()?;
            entries.push((key, value));
            match self.peek().clone() {
                Tok::Comma => {
                    self.bump();
                }
                Tok::RBrace => {
                    self.bump();
                    return Ok(entries);
                }
                other => return Err(self.err(format!("expected ',' or '}}', found {other:?}"))),
            }
        }
    }
}

/// Parse a standalone Python literal — exposed mainly for testing the parser
/// against fragments; manifest files always parse to a top-level `Dict`.
pub fn parse_py_literal(src: &str) -> Result<PyValue, PyParseError> {
    let toks = lex(src)?;
    let mut parser = Parser { toks, idx: 0 };
    let value = parser.parse_value()?;
    if *parser.peek() != Tok::Eof {
        return Err(parser.err(format!("unexpected trailing token {:?}", parser.peek())));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_realistic_manifest() {
        let src = r#"
# -*- coding: utf-8 -*-
{
    'name': "Sale Management",
    'version': '17.0.1.0.0',
    'category': 'Sales/Sales',
    'summary': 'Send quotations, orders',
    'description': """
        Long multi-line
        description text.
    """,
    'depends': ['base', 'product', 'account'],
    'data': [
        'security/ir.model.access.csv',
        'views/sale_views.xml',
    ],
    'installable': True,
    'application': True,
    'auto_install': False,
    'license': 'LGPL-3',
}
"#;
        let value = parse_py_literal(src).expect("should parse");
        let PyValue::Dict(entries) = value else { panic!("expected dict") };
        let manifest = ModuleManifest::from_dict(&entries, "sale");
        assert_eq!(manifest.name, "Sale Management");
        assert_eq!(manifest.version.as_deref(), Some("17.0.1.0.0"));
        assert_eq!(manifest.category.as_deref(), Some("Sales/Sales"));
        assert_eq!(manifest.depends, vec!["base", "product", "account"]);
        assert!(manifest.installable);
        assert!(manifest.application);
        assert!(!manifest.auto_install_enabled);
    }

    #[test]
    fn defaults_match_odoo_when_keys_are_absent() {
        let value = parse_py_literal("{'name': 'Minimal'}").unwrap();
        let PyValue::Dict(entries) = value else { panic!("expected dict") };
        let manifest = ModuleManifest::from_dict(&entries, "minimal");
        assert!(manifest.installable, "Odoo defaults installable to True when absent");
        assert!(!manifest.application);
        assert!(!manifest.auto_install_enabled);
        assert!(manifest.depends.is_empty());
    }

    #[test]
    fn auto_install_as_a_list_counts_as_enabled() {
        let value = parse_py_literal("{'name': 'x', 'auto_install': ['a', 'b']}").unwrap();
        let PyValue::Dict(entries) = value else { panic!("expected dict") };
        let manifest = ModuleManifest::from_dict(&entries, "x");
        assert!(manifest.auto_install_enabled, "a non-empty conditional auto_install list still means enabled");
    }

    #[test]
    fn empty_auto_install_list_is_not_enabled() {
        let value = parse_py_literal("{'name': 'x', 'auto_install': []}").unwrap();
        let PyValue::Dict(entries) = value else { panic!("expected dict") };
        let manifest = ModuleManifest::from_dict(&entries, "x");
        assert!(!manifest.auto_install_enabled);
    }

    #[test]
    fn tolerates_trailing_commas_and_nested_structures() {
        let src = r#"{
            'name': 'x',
            'external_dependencies': {'python': ['lxml',],},
            'depends': ['base',],
        }"#;
        let value = parse_py_literal(src).expect("trailing commas should be fine");
        let PyValue::Dict(entries) = value else { panic!("expected dict") };
        let manifest = ModuleManifest::from_dict(&entries, "x");
        assert_eq!(manifest.depends, vec!["base"]);
    }

    #[test]
    fn adjacent_string_literals_concatenate() {
        let value = parse_py_literal("{'name': 'Long ' 'Name'}").unwrap();
        let PyValue::Dict(entries) = value else { panic!("expected dict") };
        let manifest = ModuleManifest::from_dict(&entries, "x");
        assert_eq!(manifest.name, "Long Name");
    }

    #[test]
    fn rejects_non_dict_top_level() {
        let err = parse_py_literal("[1, 2, 3]");
        assert!(matches!(err, Ok(PyValue::List(_))), "parses fine as a value...");
        // ...but parse_manifest_file's caller is the one that enforces dict-ness;
        // verified separately via NotADict in parse_manifest_file.
    }

    #[test]
    fn reports_line_and_column_on_malformed_input() {
        let err = parse_py_literal("{'name':\n  'x',\n  broken}").unwrap_err();
        assert_eq!(err.line, 3, "error should point at the actual bad line, not line 1");
    }

    #[test]
    fn parse_manifest_file_rejects_non_dict() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("__manifest__.py");
        std::fs::write(&path, "[1, 2, 3]").unwrap();
        let err = parse_manifest_file(&path, "whatever").unwrap_err();
        assert!(matches!(err, ManifestError::NotADict { .. }));
    }

    #[test]
    fn parse_manifest_file_reports_missing_file() {
        let err = parse_manifest_file(Path::new("/does/not/exist/__manifest__.py"), "x").unwrap_err();
        assert!(matches!(err, ManifestError::Io { .. }));
    }
}
