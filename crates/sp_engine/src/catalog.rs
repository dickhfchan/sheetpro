//! The measurable spec, embedded at build time from `spec/*.toml`.

use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Debug, Clone, Deserialize)]
pub struct CatalogItem {
    pub id: String,
    pub label: String,
    pub loc: String,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Deserialize)]
struct CommandsFile {
    items: Vec<CatalogItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FunctionGroup {
    pub category: String,
    pub names: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct FunctionsFile {
    group: Vec<FunctionGroup>,
}

pub const COMMANDS_TOML: &str = include_str!("../../../spec/commands.toml");
pub const FUNCTIONS_TOML: &str = include_str!("../../../spec/functions.toml");

/// Catalog commands, in file order. An unparseable catalog yields an empty list (the
/// `catalog_parses` test keeps that from ever shipping).
pub fn commands() -> &'static [CatalogItem] {
    static C: OnceLock<Vec<CatalogItem>> = OnceLock::new();
    C.get_or_init(|| {
        toml::from_str::<CommandsFile>(COMMANDS_TOML)
            .map(|f| f.items)
            .unwrap_or_default()
    })
}

pub fn functions() -> &'static [FunctionGroup] {
    static F: OnceLock<Vec<FunctionGroup>> = OnceLock::new();
    F.get_or_init(|| {
        toml::from_str::<FunctionsFile>(FUNCTIONS_TOML)
            .map(|f| f.group)
            .unwrap_or_default()
    })
}

pub fn find(id: &str) -> Option<&'static CatalogItem> {
    commands().iter().find(|c| c.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn catalog_parses() {
        toml::from_str::<CommandsFile>(COMMANDS_TOML).unwrap();
        toml::from_str::<FunctionsFile>(FUNCTIONS_TOML).unwrap();
        assert!(commands().len() > 250, "{}", commands().len());
        assert!(functions().iter().map(|g| g.names.len()).sum::<usize>() > 450);
    }

    #[test]
    fn ids_unique() {
        let mut seen = HashSet::new();
        for c in commands() {
            assert!(seen.insert(c.id.as_str()), "duplicate command id {}", c.id);
        }
        let mut seen = HashSet::new();
        for g in functions() {
            for n in &g.names {
                assert!(seen.insert(n.as_str()), "duplicate function {n}");
            }
        }
    }
}
