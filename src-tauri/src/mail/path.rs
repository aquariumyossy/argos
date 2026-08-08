//! Virtual document paths for Outlook items: `outlook:{storeId}/{entryId}`.

pub const OUTLOOK_SCHEME: &str = "outlook:";

pub fn is_outlook_path(path: &str) -> bool {
    path.len() > OUTLOOK_SCHEME.len()
        && path
            .get(..OUTLOOK_SCHEME.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(OUTLOOK_SCHEME))
}

pub fn make_outlook_path(store_id: &str, entry_id: &str) -> String {
    format!("{OUTLOOK_SCHEME}{store_id}/{entry_id}")
}

pub fn parse_outlook_path(path: &str) -> Option<(String, String)> {
    if !is_outlook_path(path) {
        return None;
    }
    let rest = &path[OUTLOOK_SCHEME.len()..];
    let (store, entry) = rest.split_once('/')?;
    if store.is_empty() || entry.is_empty() {
        return None;
    }
    Some((store.to_string(), entry.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let p = make_outlook_path("STORE1", "ENTRY2");
        assert!(is_outlook_path(&p));
        assert_eq!(
            parse_outlook_path(&p),
            Some(("STORE1".into(), "ENTRY2".into()))
        );
    }

    #[test]
    fn rejects_fs_path() {
        assert!(!is_outlook_path(r"C:\mail\foo.msg"));
        assert!(parse_outlook_path(r"C:\mail\foo.msg").is_none());
    }
}
