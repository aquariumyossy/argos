//! Opt-in LAN remote sharing: longest matching registered folder wins.

use std::collections::HashSet;

use crate::db::FolderRow;
use crate::pathutil;

use super::SearchHit;

#[derive(Debug, Clone)]
pub struct RemoteShareFolder {
    pub id: i64,
    pub path: String,
    pub public_path: String,
}

#[derive(Debug, Clone, Default)]
pub struct RemoteShareSnapshot {
    pub folders: Vec<RemoteShareFolder>,
    pub shared_ids: HashSet<i64>,
}

impl RemoteShareSnapshot {
    pub fn from_folders(folders: &[FolderRow]) -> Self {
        let mut shared_ids = HashSet::new();
        let folders = folders
            .iter()
            .filter_map(|f| {
                let path = pathutil::simplify_windows_path(f.path.trim());
                if path.is_empty() {
                    return None;
                }
                if f.share_remote {
                    shared_ids.insert(f.id);
                }
                Some(RemoteShareFolder {
                    id: f.id,
                    path,
                    public_path: pathutil::simplify_windows_path(f.public_path.trim()),
                })
            })
            .collect();
        Self {
            folders,
            shared_ids,
        }
    }

    pub fn has_shared_folders(&self) -> bool {
        !self.shared_ids.is_empty()
    }

    pub fn path_is_shared(&self, path: &str) -> bool {
        path_is_remotely_shared(path, self)
    }

    pub fn filter_hits(&self, hits: Vec<SearchHit>) -> Vec<SearchHit> {
        filter_hits_by_share(hits, self)
    }
}

fn folder_public_root(folder: &RemoteShareFolder) -> String {
    let public = folder.public_path.trim();
    if public.is_empty() {
        folder.path.clone()
    } else {
        pathutil::simplify_windows_path(public)
    }
}

fn equivalent_paths(hit: &str, folders: &[RemoteShareFolder]) -> Vec<String> {
    let hit = pathutil::simplify_windows_path(hit);
    if hit.is_empty() || crate::mail::is_outlook_path(&hit) {
        return Vec::new();
    }
    let mut out = vec![hit.clone()];
    for folder in folders {
        if folder.path.is_empty() {
            continue;
        }
        let public = folder_public_root(folder);
        if public.is_empty() || public.eq_ignore_ascii_case(&folder.path) {
            continue;
        }
        if pathutil::path_starts_with(&hit, &public) {
            let rewritten = pathutil::rewrite_prefix(&hit, &public, &folder.path);
            if !out.iter().any(|p| p.eq_ignore_ascii_case(&rewritten)) {
                out.push(rewritten);
            }
        }
        if pathutil::path_starts_with(&hit, &folder.path) {
            let rewritten = pathutil::rewrite_prefix(&hit, &folder.path, &public);
            if !out.iter().any(|p| p.eq_ignore_ascii_case(&rewritten)) {
                out.push(rewritten);
            }
        }
    }
    out
}

pub fn owning_folder_id(path: &str, snap: &RemoteShareSnapshot) -> Option<i64> {
    let eqs = equivalent_paths(path, &snap.folders);
    if eqs.is_empty() {
        return None;
    }
    let mut best: Option<(usize, i64)> = None;
    for folder in &snap.folders {
        if folder.path.is_empty() {
            continue;
        }
        let public = folder_public_root(folder);
        let matched = eqs.iter().any(|candidate| {
            pathutil::path_starts_with(candidate, &folder.path)
                || (!public.is_empty() && pathutil::path_starts_with(candidate, &public))
        });
        if !matched {
            continue;
        }
        let score = folder.path.len();
        if best.is_none_or(|(len, _)| score > len) {
            best = Some((score, folder.id));
        }
    }
    best.map(|(_, id)| id)
}

/// Whether `path` may appear in a LAN remote response.
pub fn path_is_remotely_shared(path: &str, snap: &RemoteShareSnapshot) -> bool {
    if snap.shared_ids.is_empty() {
        return false;
    }
    if path.trim().is_empty() || crate::mail::is_outlook_path(path) {
        return false;
    }
    let Some(id) = owning_folder_id(path, snap) else {
        return false;
    };
    snap.shared_ids.contains(&id)
}

pub fn filter_hits_by_share(
    hits: Vec<SearchHit>,
    snap: &RemoteShareSnapshot,
) -> Vec<SearchHit> {
    hits.into_iter()
        .filter(|h| path_is_remotely_shared(&h.path, snap))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(folders: &[RemoteShareFolder], shared: &[i64]) -> RemoteShareSnapshot {
        RemoteShareSnapshot {
            folders: folders.to_vec(),
            shared_ids: shared.iter().copied().collect(),
        }
    }

    fn folder(id: i64, path: &str, public_path: &str) -> RemoteShareFolder {
        RemoteShareFolder {
            id,
            path: pathutil::simplify_windows_path(path),
            public_path: pathutil::simplify_windows_path(public_path),
        }
    }

    #[test]
    fn empty_share_set_denies_all() {
        let s = snap(&[folder(1, r"C:\Data", "")], &[]);
        assert!(!path_is_remotely_shared(r"C:\Data\a.txt", &s));
    }

    #[test]
    fn empty_path_is_not_shared() {
        let s = snap(&[folder(1, r"C:\Data", "")], &[1]);
        assert!(!path_is_remotely_shared("", &s));
        assert!(!path_is_remotely_shared("   ", &s));
    }

    #[test]
    fn empty_folder_path_does_not_allow_everything() {
        let s = snap(
            &[RemoteShareFolder {
                id: 1,
                path: String::new(),
                public_path: String::new(),
            }],
            &[1],
        );
        assert!(!path_is_remotely_shared(r"C:\secret\a.txt", &s));
    }

    #[test]
    fn shared_root_keeps_children_and_drops_outsiders() {
        let s = snap(&[folder(1, r"C:\Data", "")], &[1]);
        assert!(path_is_remotely_shared(r"C:\Data\a.txt", &s));
        assert!(path_is_remotely_shared(r"C:\Data", &s));
        assert!(!path_is_remotely_shared(r"C:\Other\a.txt", &s));
    }

    #[test]
    fn data_vs_data2_boundary() {
        let s = snap(&[folder(1, r"C:\Data", "")], &[1]);
        assert!(path_is_remotely_shared(r"C:\Data\file.txt", &s));
        assert!(!path_is_remotely_shared(r"C:\Data2\file.txt", &s));
    }

    #[test]
    fn nested_child_toggle_wins() {
        let s = snap(
            &[
                folder(1, r"C:\Work", ""),
                folder(2, r"C:\Work\secret", ""),
            ],
            &[1],
        );
        assert!(path_is_remotely_shared(r"C:\Work\open.txt", &s));
        assert!(!path_is_remotely_shared(r"C:\Work\secret\note.txt", &s));
    }

    #[test]
    fn unregistered_child_follows_parent_share() {
        let s = snap(&[folder(1, r"C:\Work", "")], &[1]);
        assert!(path_is_remotely_shared(r"C:\Work\secret\note.txt", &s));
    }

    #[test]
    fn public_and_local_paths_both_match() {
        let s = snap(&[folder(1, r"C:\Share", r"\\pc\share")], &[1]);
        assert!(path_is_remotely_shared(r"\\pc\share\a.txt", &s));
        assert!(path_is_remotely_shared(r"C:\Share\a.txt", &s));
    }

    #[test]
    fn nested_child_owns_unc_hit_via_parent_rewrite() {
        let s = snap(
            &[
                folder(1, r"C:\Work", r"\\pc\work"),
                folder(2, r"C:\Work\secret", ""),
            ],
            &[1],
        );
        assert!(path_is_remotely_shared(r"\\pc\work\open.txt", &s));
        assert!(!path_is_remotely_shared(r"\\pc\work\secret\note.txt", &s));
    }

    #[test]
    fn unknown_path_is_denied() {
        let s = snap(&[folder(1, r"C:\Data", "")], &[1]);
        assert!(!path_is_remotely_shared(r"D:\elsewhere\a.txt", &s));
    }

    #[test]
    fn outlook_paths_are_never_shared() {
        let s = snap(&[folder(1, r"C:\Data", "")], &[1]);
        assert!(!path_is_remotely_shared("outlook:store/entry", &s));
    }

    #[test]
    fn from_folders_ignores_stale_share_without_row() {
        let folders = vec![FolderRow {
            id: 3,
            path: r"C:\Docs".into(),
            public_path: String::new(),
            enabled: true,
            indexed_count: 0,
            exists: true,
            share_remote: true,
        }];
        let s = RemoteShareSnapshot::from_folders(&folders);
        assert!(s.shared_ids.contains(&3));
        assert!(!s.shared_ids.contains(&99));
    }

    #[test]
    fn filter_hits_drops_unshared() {
        let s = snap(&[folder(1, r"C:\Data", "")], &[1]);
        let hits = vec![
            SearchHit {
                id: "1".into(),
                title: "a".into(),
                snippet: String::new(),
                path: r"C:\Data\a.txt".into(),
                page: None,
                chunk_id: None,
                score: 1.0,
                source: "local".into(),
                preview_text: String::new(),
                highlight_terms: vec![],
                match_count: 1,
                paragraphs: vec![],
                unit_label: String::new(),
                mail_from: String::new(),
                mail_date: String::new(),
                mail_conversation_id: String::new(),
                mail_folder: String::new(),
                doc_kind: "file".into(),
            },
            SearchHit {
                id: "2".into(),
                title: "b".into(),
                snippet: String::new(),
                path: r"C:\Other\b.txt".into(),
                page: None,
                chunk_id: None,
                score: 1.0,
                source: "local".into(),
                preview_text: String::new(),
                highlight_terms: vec![],
                match_count: 1,
                paragraphs: vec![],
                unit_label: String::new(),
                mail_from: String::new(),
                mail_date: String::new(),
                mail_conversation_id: String::new(),
                mail_folder: String::new(),
                doc_kind: "file".into(),
            },
        ];
        let kept = filter_hits_by_share(hits, &s);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].path, r"C:\Data\a.txt");
    }
}
