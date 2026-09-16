//! Folder picker rows for chat IPC and the local search HTTP API.

use serde::{Deserialize, Serialize};

use crate::db::{Db, FolderRow, Settings};
use crate::pathutil;
use super::{
    run_search, RemoteShareSnapshot, SearchHit, TantivyBackend, UserDictMatcher,
};

/// Wider than typical UI `max_results` so the picker can see more matching folders.
const SCOPE_QUERY_HIT_LIMIT: usize = 200;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchScopeRow {
    pub path: String,
    pub label: String,
    pub is_root: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchScopesResult {
    pub recent: Vec<SearchScopeRow>,
    pub scopes: Vec<SearchScopeRow>,
}

pub struct ScopeListOpts {
    pub include_mail: bool,
    pub share: Option<RemoteShareSnapshot>,
}

fn folder_display_name(path: &str) -> String {
    let simplified = pathutil::simplify_windows_path(path);
    simplified
        .rsplit('\\')
        .find(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

fn parent_dir(path: &str) -> Option<String> {
    let simplified = pathutil::simplify_windows_path(path);
    let truncated = simplified.trim_end_matches('\\');
    let (parent, name) = match truncated.rfind('\\') {
        Some(i) => (&truncated[..i], &truncated[i + 1..]),
        None => return None,
    };
    if name.is_empty() {
        return None;
    }
    // Keep drive root like `C:` as `C:\`
    if parent.len() == 2 && parent.as_bytes()[1] == b':' {
        return Some(format!("{parent}\\"));
    }
    if parent.is_empty() {
        return None;
    }
    Some(parent.to_string())
}

fn relative_label(root: &str, dir: &str) -> String {
    let root = pathutil::simplify_windows_path(root);
    let dir = pathutil::simplify_windows_path(dir);
    if dir.eq_ignore_ascii_case(&root) {
        return folder_display_name(&root);
    }
    if !pathutil::path_starts_with(&dir, &root) {
        return folder_display_name(&dir);
    }
    let rest = dir[root.len()..].trim_start_matches('\\');
    if rest.is_empty() {
        folder_display_name(&root)
    } else {
        rest.replace('\\', "/")
    }
}

/// ChatScopePicker labels: `user@firm / Inbox` → `Inbox（user@firm）`.
pub fn format_mail_scope_label(raw: &str) -> String {
    let parts: Vec<&str> = raw
        .split(['\\', '/'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if parts.len() <= 1 {
        return raw.trim().to_string();
    }
    format!("{}（{}）", parts[1..].join("／"), parts[0])
}

pub fn collect_search_scopes(
    folders: &[FolderRow],
    list_paths: impl Fn(i64) -> Result<Vec<String>, String>,
) -> Result<Vec<SearchScopeRow>, String> {
    use std::collections::BTreeMap;

    let mut out: Vec<SearchScopeRow> = Vec::new();
    for folder in folders.iter().filter(|f| f.enabled) {
        let root = pathutil::effective_public_root(&folder.path, &folder.public_path);
        out.push(SearchScopeRow {
            path: root.clone(),
            label: folder_display_name(&root),
            is_root: true,
        });

        let paths = list_paths(folder.id)?;
        let mut subdirs: BTreeMap<String, String> = BTreeMap::new();
        for file_path in paths {
            let mut current = parent_dir(&file_path);
            while let Some(dir) = current {
                if !pathutil::path_starts_with(&dir, &root) {
                    break;
                }
                if dir.eq_ignore_ascii_case(&root) {
                    break;
                }
                let key = dir.to_ascii_lowercase();
                subdirs.entry(key).or_insert_with(|| dir.clone());
                current = parent_dir(&dir);
            }
        }

        let mut subs: Vec<SearchScopeRow> = subdirs
            .into_values()
            .map(|path| SearchScopeRow {
                label: relative_label(&root, &path),
                path,
                is_root: false,
            })
            .collect();
        subs.sort_by(|a, b| {
            a.label
                .to_ascii_lowercase()
                .cmp(&b.label.to_ascii_lowercase())
        });
        out.extend(subs);
    }
    Ok(out)
}

fn same_scope_path(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// File (and optional mail) scopes, matching `list_search_scopes` plus HTTP extras.
pub fn assemble_search_scopes(
    db: &Db,
    query: Option<&str>,
    settings: &Settings,
    backend: &TantivyBackend,
    mail: Option<&TantivyBackend>,
    user_dict: &UserDictMatcher,
    opts: ScopeListOpts,
) -> Result<SearchScopesResult, String> {
    if let Some(share) = opts.share.as_ref() {
        if !share.has_shared_folders() {
            return Ok(SearchScopesResult {
                recent: Vec::new(),
                scopes: Vec::new(),
            });
        }
    }

    let mut folders = db.list_folders().map_err(|e| e.to_string())?;
    if opts.share.is_some() {
        folders.retain(|f| f.share_remote);
    }

    let all = collect_search_scopes(&folders, |folder_id| {
        db.list_file_paths_by_folder(folder_id)
            .map_err(|e| e.to_string())
    })?;

    let mut recent: Vec<SearchScopeRow> = db
        .list_recent_search_scopes()
        .into_iter()
        .map(|s| SearchScopeRow {
            path: s.path,
            label: s.label,
            is_root: false,
        })
        .collect();
    if let Some(share) = opts.share.as_ref() {
        recent.retain(|r| share.path_is_shared(&r.path));
    }

    let q = query.map(str::trim).filter(|s| !s.is_empty());
    let (filtered, hits) = match q {
        None => (all, Vec::new()),
        Some(q) => {
            let hits = run_search(
                settings,
                backend,
                mail,
                q,
                SCOPE_QUERY_HIT_LIMIT,
                None,
                None,
                user_dict,
            )?;
            if hits.is_empty() {
                (Vec::new(), hits)
            } else {
                let filtered = all
                    .into_iter()
                    .filter(|scope| {
                        hits.iter()
                            .any(|h| pathutil::path_starts_with(&h.path, &scope.path))
                    })
                    .collect();
                (filtered, hits)
            }
        }
    };

    let mut scopes: Vec<SearchScopeRow> = filtered
        .into_iter()
        .filter(|scope| {
            !recent
                .iter()
                .any(|r| same_scope_path(&r.path, &scope.path))
        })
        .collect();

    if opts.include_mail {
        let mail_rows = mail_scope_rows(db, q, &hits)?;
        for row in mail_rows {
            if recent.iter().any(|r| same_scope_path(&r.path, &row.path))
                || scopes.iter().any(|s| same_scope_path(&s.path, &row.path))
            {
                continue;
            }
            scopes.push(row);
        }
    }

    Ok(SearchScopesResult { recent, scopes })
}

fn mail_scope_rows(
    db: &Db,
    query: Option<&str>,
    hits: &[SearchHit],
) -> Result<Vec<SearchScopeRow>, String> {
    let names = db
        .list_indexed_email_folder_names()
        .map_err(|e| e.to_string())?;
    let rows: Vec<SearchScopeRow> = names
        .into_iter()
        .map(|name| SearchScopeRow {
            path: format!("mailfolder:{name}"),
            label: format_mail_scope_label(&name),
            is_root: true,
        })
        .collect();
    if query.is_none() {
        return Ok(rows);
    }
    Ok(rows
        .into_iter()
        .filter(|row| {
            let Some(folder) = row.path.strip_prefix("mailfolder:") else {
                return false;
            };
            hits.iter().any(|h| {
                h.doc_kind == "email" && h.mail_folder.eq_ignore_ascii_case(folder)
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::FolderRow;

    fn folder(id: i64, path: &str, enabled: bool) -> FolderRow {
        FolderRow {
            id,
            path: path.to_string(),
            public_path: String::new(),
            enabled,
            indexed_count: 0,
            exists: false,
            share_remote: false,
        }
    }

    #[test]
    fn collect_search_scopes_keeps_more_than_400_unique_subdirs() {
        let root = r"C:\docs";
        let folders = vec![folder(1, root, true)];
        let mut paths: Vec<String> = (0..449)
            .map(|i| format!(r"{root}\case{i:04}\file.md"))
            .collect();
        paths.push(format!(r"{root}\20260817借地権相談3\note.md"));

        let rows = collect_search_scopes(&folders, |id| {
            assert_eq!(id, 1);
            Ok(paths.clone())
        })
        .expect("scopes");

        let subs: Vec<_> = rows.iter().filter(|r| !r.is_root).collect();
        assert_eq!(subs.len(), 450);
        assert!(rows
            .iter()
            .any(|r| r.is_root && r.path.eq_ignore_ascii_case(root)));
        assert!(
            rows.iter().any(|r| r.path.ends_with("20260817借地権相談3")),
            "newest folder must remain in the picker"
        );
        assert!(
            rows.iter().any(|r| r.label.contains("20260817借地権相談3")),
            "filter by folder name should match the relative label"
        );
    }

    #[test]
    fn collect_search_scopes_includes_nested_parents() {
        let root = r"C:\docs";
        let folders = vec![folder(1, root, true)];
        let paths = vec![format!(r"{root}\a\b\file.md")];
        let rows = collect_search_scopes(&folders, |_| Ok(paths.clone())).expect("scopes");
        let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        assert!(paths.iter().any(|p| p.eq_ignore_ascii_case(root)));
        assert!(paths.iter().any(|p| p.eq_ignore_ascii_case(r"C:\docs\a")));
        assert!(paths.iter().any(|p| p.eq_ignore_ascii_case(r"C:\docs\a\b")));
    }

    #[test]
    fn collect_search_scopes_skips_disabled_folders() {
        let folders = vec![folder(1, r"C:\docs", false)];
        let rows =
            collect_search_scopes(&folders, |_| panic!("disabled folders must not list files"))
                .expect("scopes");
        assert!(rows.is_empty());
    }

    #[test]
    fn format_mail_scope_label_splits_store() {
        assert_eq!(
            format_mail_scope_label("user@firm / 受信トレイ"),
            "受信トレイ（user@firm）"
        );
        assert_eq!(format_mail_scope_label("受信トレイ"), "受信トレイ");
    }
}
