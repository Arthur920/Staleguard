//! Types produced by the code extractor.
//!
//! The code index is a flat list of these; doc references resolve against it.

use serde::Serialize;

/// What kind of definition a [`Symbol`] is. Tag kind names differ per grammar;
/// unrecognized ones are preserved in `Other` rather than dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Class,
    Enum,
    Trait,
    Interface,
    Module,
    Constant,
    Field,
    Other(String),
}

/// Best-effort visibility, used by coverage-gaps to scope the "documentable
/// surface" (public surface matters; private internals don't).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Public,
    Internal,
    Private,
}

/// Where a symbol lives. Mirrors the `path` + line shape used by
/// `retrieve::Hit` and `findings::Finding.code_refs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Span {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
}

/// A code definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Symbol {
    /// module path + enclosing scope + name, best-effort (no full scope
    /// resolution yet).
    pub qualified_name: String,
    pub name: String,
    pub kind: SymbolKind,
    pub visibility: Visibility,
    /// file-derived module path (e.g. `src/code/symbol` for this file).
    pub module: String,
    /// Name range (identifier position), used by reports.
    pub span: Span,
}
