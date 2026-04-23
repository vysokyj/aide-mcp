//! Thin wrapper around the `scip` protobuf crate.
//!
//! aide-mcp consumes `.scip` files that `rust-analyzer scip` produces for
//! a given commit snapshot. This crate gives us a small, agent-friendly
//! query surface — documents, symbol search, references — without
//! leaking the protobuf types through the MCP tool layer.

use std::path::Path;

use protobuf::Message;
use scip::types::{Document, Index, SymbolRole};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ScipError {
    #[error("I/O error reading {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("protobuf decode error: {0}")]
    Decode(#[from] protobuf::Error),
}

/// Read `path` and decode it as a SCIP [`Index`].
pub fn load(path: &Path) -> Result<Index, ScipError> {
    let bytes = std::fs::read(path).map_err(|e| ScipError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    Ok(Index::parse_from_bytes(&bytes)?)
}

/// Relative paths of every document covered by `index`, in insertion order.
pub fn documents(index: &Index) -> Vec<String> {
    index
        .documents
        .iter()
        .map(|d| d.relative_path.clone())
        .collect()
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SymbolHit {
    /// SCIP symbol identifier (scheme + manager + package + descriptor).
    pub symbol: String,
    pub display_name: String,
    pub kind: String,
    pub relative_path: String,
    pub documentation: Vec<String>,
}

/// Find every [`SymbolInformation`](scip::types::SymbolInformation) whose
/// `display_name` or symbol id contains `query`, case-insensitively. An
/// empty query returns all symbols.
pub fn find_symbols(index: &Index, query: &str) -> Vec<SymbolHit> {
    let needle = query.to_lowercase();
    let match_all = needle.is_empty();
    let mut out = Vec::new();
    for doc in &index.documents {
        for sym in &doc.symbols {
            if match_all
                || sym.display_name.to_lowercase().contains(&needle)
                || sym.symbol.to_lowercase().contains(&needle)
            {
                out.push(SymbolHit {
                    symbol: sym.symbol.clone(),
                    display_name: sym.display_name.clone(),
                    kind: format!("{:?}", sym.kind.enum_value_or_default()),
                    relative_path: doc.relative_path.clone(),
                    documentation: sym.documentation.clone(),
                });
            }
        }
    }
    out
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OccurrenceHit {
    pub relative_path: String,
    /// `[start_line, start_col, end_line, end_col]` or
    /// `[start_line, start_col, end_col]` per the SCIP spec.
    pub range: Vec<i32>,
    pub is_definition: bool,
}

/// For the document in `index` whose `relative_path == path`, find the
/// most recent definition occurrence whose range starts at or before
/// `line_0based` and return its display name (or the raw symbol id if
/// no matching [`SymbolInformation`] ships with a display name).
///
/// Lines in SCIP are 0-indexed; callers with 1-indexed line numbers
/// (e.g. grep output) must subtract one before calling.
///
/// Returns `None` if the document is not present, if no definition
/// occurrence precedes the line, or if the occurrence refers to an
/// unknown symbol.
pub fn enclosing_definition(index: &Index, path: &str, line_0based: i32) -> Option<String> {
    let doc = index.documents.iter().find(|d| d.relative_path == path)?;
    enclosing_definition_in_doc(doc, line_0based)
}

fn enclosing_definition_in_doc(doc: &Document, line_0based: i32) -> Option<String> {
    let mut best: Option<(i32, &str)> = None;
    for occ in &doc.occurrences {
        if (occ.symbol_roles & SymbolRole::Definition as i32) == 0 {
            continue;
        }
        let Some(start_line) = occ.range.first().copied() else {
            continue;
        };
        if start_line > line_0based {
            continue;
        }
        if best.is_none_or(|(b, _)| start_line >= b) {
            best = Some((start_line, occ.symbol.as_str()));
        }
    }
    let (_, symbol_id) = best?;
    Some(
        doc.symbols
            .iter()
            .find(|s| s.symbol == symbol_id)
            .map_or_else(
                || symbol_id.to_string(),
                |s| {
                    if s.display_name.is_empty() {
                        s.symbol.clone()
                    } else {
                        s.display_name.clone()
                    }
                },
            ),
    )
}

/// Every occurrence of `symbol` (exact match on the SCIP symbol id) across
/// the index. The `is_definition` flag is derived from the occurrence's
/// `symbol_roles` bitmask.
pub fn references(index: &Index, symbol: &str) -> Vec<OccurrenceHit> {
    let mut out = Vec::new();
    for doc in &index.documents {
        for occ in &doc.occurrences {
            if occ.symbol == symbol {
                let is_def = (occ.symbol_roles & SymbolRole::Definition as i32) != 0;
                out.push(OccurrenceHit {
                    relative_path: doc.relative_path.clone(),
                    range: occ.range.clone(),
                    is_definition: is_def,
                });
            }
        }
    }
    out
}

/// Call sites of `symbol`: every occurrence except the definition.
/// Thin wrapper over [`references`] that makes "who uses this?" a
/// one-call question for an agent. Shape mirrors [`references`] so
/// existing consumers can swap freely.
pub fn callers(index: &Index, symbol: &str) -> Vec<OccurrenceHit> {
    references(index, symbol)
        .into_iter()
        .filter(|o| !o.is_definition)
        .collect()
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ModuleEntry {
    pub relative_path: String,
    pub symbols: Vec<MapSymbol>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MapSymbol {
    /// SCIP symbol id (stable, can be fed back into [`references`]
    /// / [`callers`]).
    pub symbol: String,
    pub display_name: String,
    pub kind: String,
    /// Zero-indexed start line of the definition occurrence in
    /// `relative_path`. `None` when no definition occurrence was
    /// recorded in this document — usually means the indexer emitted
    /// the symbol information but not a matching occurrence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<i32>,
}

/// Top-level symbol digest per document — a map of "what's actually
/// defined here" for an agent that would otherwise grep for `pub fn`
/// / `class` / `interface`. Filters symbols by `kind`: pass an empty
/// slice to keep everything, or a curated list to narrow to function
/// / type / trait shapes.
pub fn project_map(index: &Index, kinds: &[&str]) -> Vec<ModuleEntry> {
    index
        .documents
        .iter()
        .map(|doc| ModuleEntry {
            relative_path: doc.relative_path.clone(),
            symbols: document_map_symbols(doc, kinds),
        })
        .filter(|m| !m.symbols.is_empty())
        .collect()
}

fn document_map_symbols(doc: &Document, kinds: &[&str]) -> Vec<MapSymbol> {
    doc.symbols
        .iter()
        .filter_map(|sym| {
            let kind_str = format!("{:?}", sym.kind.enum_value_or_default());
            if !kinds.is_empty() && !kinds.iter().any(|k| *k == kind_str) {
                return None;
            }
            let line = doc
                .occurrences
                .iter()
                .find(|o| {
                    o.symbol == sym.symbol && (o.symbol_roles & SymbolRole::Definition as i32) != 0
                })
                .and_then(|o| o.range.first().copied());
            Some(MapSymbol {
                symbol: sym.symbol.clone(),
                display_name: if sym.display_name.is_empty() {
                    sym.symbol.clone()
                } else {
                    sym.display_name.clone()
                },
                kind: kind_str,
                line,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use protobuf::{EnumOrUnknown, MessageField};
    use scip::types::{
        symbol_information::Kind, Document, Index as ScipIndex, Metadata, Occurrence,
        SymbolInformation,
    };
    use tempfile::TempDir;

    fn fake_index() -> ScipIndex {
        let main_sym = SymbolInformation {
            symbol: "rust-analyzer rust aide-mcp . `main`().".into(),
            display_name: "main".into(),
            kind: EnumOrUnknown::new(Kind::Function),
            documentation: vec!["entry point".into()],
            ..Default::default()
        };
        let helper_sym = SymbolInformation {
            symbol: "rust-analyzer rust aide-mcp . `helper`().".into(),
            display_name: "helper".into(),
            kind: EnumOrUnknown::new(Kind::Function),
            documentation: vec![],
            ..Default::default()
        };
        let main_doc = Document {
            relative_path: "src/main.rs".into(),
            symbols: vec![main_sym.clone()],
            occurrences: vec![
                Occurrence {
                    symbol: main_sym.symbol.clone(),
                    range: vec![0, 3, 7],
                    symbol_roles: SymbolRole::Definition as i32,
                    ..Default::default()
                },
                Occurrence {
                    symbol: helper_sym.symbol.clone(),
                    range: vec![2, 4, 10],
                    symbol_roles: 0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let helper_doc = Document {
            relative_path: "src/helper.rs".into(),
            symbols: vec![helper_sym.clone()],
            occurrences: vec![Occurrence {
                symbol: helper_sym.symbol.clone(),
                range: vec![0, 3, 9],
                symbol_roles: SymbolRole::Definition as i32,
                ..Default::default()
            }],
            ..Default::default()
        };
        ScipIndex {
            metadata: MessageField::some(Metadata::default()),
            documents: vec![main_doc, helper_doc],
            ..Default::default()
        }
    }

    #[test]
    fn load_roundtrips_encoded_index() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("index.scip");
        std::fs::write(&path, fake_index().write_to_bytes().unwrap()).unwrap();
        let back = load(&path).unwrap();
        assert_eq!(back.documents.len(), 2);
    }

    #[test]
    fn documents_returns_relative_paths() {
        let idx = fake_index();
        let docs = documents(&idx);
        assert_eq!(docs, vec!["src/main.rs", "src/helper.rs"]);
    }

    #[test]
    fn find_symbols_filters_by_display_name() {
        let idx = fake_index();
        let hits = find_symbols(&idx, "help");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].display_name, "helper");
    }

    #[test]
    fn find_symbols_empty_query_returns_all() {
        let idx = fake_index();
        let hits = find_symbols(&idx, "");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn references_matches_by_exact_symbol_id() {
        let idx = fake_index();
        let helper_id = "rust-analyzer rust aide-mcp . `helper`().";
        let hits = references(&idx, helper_id);
        assert_eq!(hits.len(), 2);
        let defs: Vec<_> = hits.iter().filter(|h| h.is_definition).collect();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].relative_path, "src/helper.rs");
    }

    #[test]
    fn enclosing_definition_returns_enclosing_fn() {
        let idx = fake_index();
        assert_eq!(
            enclosing_definition(&idx, "src/main.rs", 0),
            Some("main".to_string())
        );
    }

    #[test]
    fn enclosing_definition_ignores_non_definition_occurrences() {
        // Line 2 of src/main.rs is where helper is *referenced* (not
        // defined). The enclosing definition is still main.
        let idx = fake_index();
        assert_eq!(
            enclosing_definition(&idx, "src/main.rs", 2),
            Some("main".to_string())
        );
    }

    #[test]
    fn enclosing_definition_picks_most_recent_preceding_def() {
        let outer_sym = SymbolInformation {
            symbol: "s outer".into(),
            display_name: "outer".into(),
            kind: EnumOrUnknown::new(Kind::Function),
            ..Default::default()
        };
        let inner_sym = SymbolInformation {
            symbol: "s inner".into(),
            display_name: "inner".into(),
            kind: EnumOrUnknown::new(Kind::Function),
            ..Default::default()
        };
        let doc = Document {
            relative_path: "src/nested.rs".into(),
            symbols: vec![outer_sym.clone(), inner_sym.clone()],
            occurrences: vec![
                Occurrence {
                    symbol: outer_sym.symbol.clone(),
                    range: vec![0, 3, 8],
                    symbol_roles: SymbolRole::Definition as i32,
                    ..Default::default()
                },
                Occurrence {
                    symbol: inner_sym.symbol.clone(),
                    range: vec![4, 7, 12],
                    symbol_roles: SymbolRole::Definition as i32,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let idx = ScipIndex {
            metadata: MessageField::some(Metadata::default()),
            documents: vec![doc],
            ..Default::default()
        };
        assert_eq!(
            enclosing_definition(&idx, "src/nested.rs", 3),
            Some("outer".to_string())
        );
        // Line 4 onwards belongs to `inner` — it's the most recent
        // preceding definition.
        assert_eq!(
            enclosing_definition(&idx, "src/nested.rs", 5),
            Some("inner".to_string())
        );
    }

    #[test]
    fn enclosing_definition_returns_none_for_unknown_path() {
        let idx = fake_index();
        assert_eq!(enclosing_definition(&idx, "src/missing.rs", 0), None);
    }

    #[test]
    fn load_missing_file_errors() {
        let err = load(Path::new("/does/not/exist.scip")).unwrap_err();
        assert!(matches!(err, ScipError::Io { .. }));
    }

    #[test]
    fn callers_drops_the_definition_occurrence() {
        let idx = fake_index();
        let helper_id = "rust-analyzer rust aide-mcp . `helper`().";
        let hits = callers(&idx, helper_id);
        assert_eq!(hits.len(), 1);
        assert!(!hits[0].is_definition);
        assert_eq!(hits[0].relative_path, "src/main.rs");
    }

    #[test]
    fn project_map_groups_defined_symbols_by_document() {
        let idx = fake_index();
        let map = project_map(&idx, &[]);
        assert_eq!(map.len(), 2);
        // Only the document that *defines* helper lists helper — not
        // the one that merely references it.
        let helper_docs: Vec<_> = map
            .iter()
            .filter(|e| e.symbols.iter().any(|s| s.display_name == "helper"))
            .map(|e| e.relative_path.clone())
            .collect();
        assert_eq!(helper_docs, vec!["src/helper.rs"]);
    }

    #[test]
    fn project_map_filters_by_kind() {
        let idx = fake_index();
        let map = project_map(&idx, &["Function"]);
        // Both symbols are Functions → both docs survive.
        assert_eq!(map.len(), 2);
        let map = project_map(&idx, &["Class"]);
        // Nothing matches → empty map.
        assert!(map.is_empty());
    }

    #[test]
    fn project_map_fills_line_from_definition_occurrence() {
        let idx = fake_index();
        let map = project_map(&idx, &[]);
        let main_entry = map
            .iter()
            .find(|e| e.relative_path == "src/main.rs")
            .unwrap();
        assert_eq!(main_entry.symbols[0].line, Some(0));
    }
}
