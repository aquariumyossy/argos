//! Statute names and abbreviations.
//!
//! The list is `statute_aliases.json` in this directory. The bundler
//! (`scripts/fetch_statutes.py` and `bundle.rs`) and the database open path
//! read that file, so another app that copied this module can copy the same
//! JSON. `bundle: false` only records the name, the same way 金融商品取引法 is
//! shipped.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use serde::Deserialize;

const ALIAS_JSON: &str = include_str!("statute_aliases.json");

#[derive(Debug, Clone, Deserialize)]
struct AliasFile {
    #[serde(default)]
    group_labels: Vec<String>,
    entries: Vec<CatalogEntryRaw>,
}

#[derive(Debug, Clone, Deserialize)]
struct CatalogEntryRaw {
    law_id: String,
    name: String,
    #[serde(default)]
    bundle: bool,
    #[serde(default)]
    aliases: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub law_id: String,
    pub name: String,
    /// Full text is packed into the app. `false` only adds the name so a later
    /// fetch can resolve it.
    pub bundle: bool,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AliasList {
    pub group_labels: Vec<String>,
    pub entries: Vec<CatalogEntry>,
}

static LIST: OnceLock<AliasList> = OnceLock::new();

pub fn alias_list() -> Result<&'static AliasList, String> {
    if let Some(list) = LIST.get() {
        return Ok(list);
    }
    let list = parse_alias_list(ALIAS_JSON)?;
    Ok(LIST.get_or_init(|| list))
}

pub fn catalog() -> Result<&'static [CatalogEntry], String> {
    Ok(alias_list()?.entries.as_slice())
}

pub fn aliases_for(law_id: &str) -> Result<Vec<String>, String> {
    Ok(alias_list()?
        .entries
        .iter()
        .find(|entry| entry.law_id == law_id)
        .map(|entry| entry.aliases.clone())
        .unwrap_or_default())
}

pub fn normalize_alias(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && *c != '　')
        .collect()
}

/// Split an e-Gov `LawTitle@Abbrev` value. ASCII `,` separates aliases.
/// Japanese `、` stays inside a name. Group labels are dropped.
pub fn split_alias(raw: &str, group_labels: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for part in raw.split(',') {
        let norm = normalize_alias(part);
        if norm.chars().count() < 2 {
            continue;
        }
        if group_labels.iter().any(|label| label == &norm) {
            continue;
        }
        if !out.contains(&norm) {
            out.push(norm);
        }
    }
    out
}

pub fn expand_alias(raw: &str) -> Result<Vec<String>, String> {
    Ok(split_alias(raw, &alias_list()?.group_labels))
}

fn parse_alias_list(raw: &str) -> Result<AliasList, String> {
    let file: AliasFile = serde_json::from_str(raw)
        .map_err(|err| format!("法令略称リストを読めませんでした: {err}"))?;
    if file.entries.is_empty() {
        return Err("法令略称リストが空です".into());
    }
    let mut group_labels = Vec::new();
    for label in &file.group_labels {
        let norm = normalize_alias(label);
        if norm.chars().count() < 2 {
            return Err(format!("総称が短すぎます: {label}"));
        }
        if !group_labels.contains(&norm) {
            group_labels.push(norm);
        }
    }
    let mut seen_ids = HashSet::new();
    let mut seen_alias: HashMap<String, String> = HashMap::new();
    let mut entries = Vec::with_capacity(file.entries.len());
    for entry in file.entries {
        let law_id = entry.law_id.trim().to_string();
        let name = normalize_alias(&entry.name);
        if law_id.is_empty() || name.is_empty() {
            return Err("法令略称リストに law_id または name が空の行があります".into());
        }
        if !seen_ids.insert(law_id.clone()) {
            return Err(format!(
                "法令略称リストの law_id が重複しています: {law_id}"
            ));
        }
        if group_labels.iter().any(|label| label == &name) {
            return Err(format!("総称は一つの法令の名称にできません: {name}"));
        }
        let mut aliases = Vec::new();
        {
            let mut push_alias = |norm: String| -> Result<(), String> {
                if norm.chars().count() < 2 {
                    return Err(format!("略称が短すぎます: {norm} ({law_id})"));
                }
                if group_labels.iter().any(|label| label == &norm) {
                    return Err(format!("総称は一つの法令の略称にできません: {norm}"));
                }
                if let Some(other) = seen_alias.get(&norm) {
                    if other != &law_id {
                        return Err(format!(
                            "略称 {norm} が {other} と {law_id} の両方にあります"
                        ));
                    }
                } else {
                    seen_alias.insert(norm.clone(), law_id.clone());
                }
                if !aliases.contains(&norm) {
                    aliases.push(norm);
                }
                Ok(())
            };
            push_alias(name.clone())?;
            for alias in &entry.aliases {
                if alias.contains(',') {
                    return Err(format!(
                        "略称リストに ASCII カンマがあります（分割は保存時に行います）: {alias}"
                    ));
                }
                push_alias(normalize_alias(alias))?;
            }
        }
        entries.push(CatalogEntry {
            law_id,
            name,
            bundle: entry.bundle,
            aliases,
        });
    }
    Ok(AliasList {
        group_labels,
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_list_loads_without_group_labels_or_commas() {
        let list = alias_list().unwrap();
        assert!(list.group_labels.iter().any(|label| label == "労働三法"));
        assert!(list.group_labels.iter().any(|label| label == "組織犯罪対策三法"));
        assert!(list.group_labels.iter().any(|label| label == "保安四法"));
        assert!(list.entries.len() >= 60, "{}", list.entries.len());
        let mut bundled = 0;
        for entry in &list.entries {
            if entry.bundle {
                bundled += 1;
            }
            for alias in &entry.aliases {
                assert!(!alias.contains(','), "{alias}");
                assert!(
                    !list.group_labels.iter().any(|label| label == alias),
                    "{alias}"
                );
            }
        }
        assert!(bundled >= 50, "bundled entries {bundled}");
        let pieces = split_alias(
            "労組法,労働三法,労安衛法,保安四法,安衛法,訪問販売、通信販売、マルチ販売等法",
            &list.group_labels,
        );
        assert_eq!(
            pieces,
            vec![
                "労組法".to_string(),
                "労安衛法".to_string(),
                "安衛法".to_string(),
                "訪問販売、通信販売、マルチ販売等法".to_string(),
            ]
        );
    }
}
