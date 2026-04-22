//! Thin wrapper around the `scip` protobuf crate.
//!
//! aide-mcp consumes `.scip` files that `rust-analyzer scip` produces for
//! a given commit snapshot. This crate gives us a small, agent-friendly
//! query surface — documents, symbol search, references — without
//! leaking the protobuf types through the MCP tool layer.

use std::path::Path;

use protobuf::Message;
use scip::types::{Index, SymbolRole};
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
    fn load_missing_file_errors() {
        let err = load(Path::new("/does/not/exist.scip")).unwrap_err();
        assert!(matches!(err, ScipError::Io { .. }));
    }
}
