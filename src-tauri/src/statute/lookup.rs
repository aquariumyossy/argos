//! DB first, then e-Gov. Offline uses the saved text and does not fail the call.

use crate::statute::cite::ToolArticle;
use crate::statute::egov::{
    article_url, lawdata_url, lawlist_url, update_list_url, EgovFailure, EgovSource,
};
use crate::statute::refresh::{
    compact_ymd, format_ymd, parse_ymd, revision_action, updates_cover, walk_start, RevisionAction,
    RevisionInput, UPDATE_WALK_DAYS,
};
use crate::statute::store::{LawRef, StatuteDb};
use crate::statute::xmlparse::{self, parse_article_query, ParsedArticle, ParsedLaw};

pub struct ArticleQuery {
    pub law: String,
    pub article: String,
}

pub fn resolve_articles(
    db: &StatuteDb,
    client: &dyn EgovSource,
    queries: &[ArticleQuery],
    today: &str,
) -> Vec<ToolArticle> {
    let offline = sync_updates(db, client, today);
    let mut fetched_catalog = db.meta("lawlist_full").unwrap_or_default() == "1";
    let mut reloaded = Vec::new();
    let mut out = Vec::new();
    for query in queries {
        out.push(resolve_one(
            db,
            client,
            query,
            today,
            offline,
            &mut fetched_catalog,
            &mut reloaded,
        ));
    }
    out
}

fn resolve_one(
    db: &StatuteDb,
    client: &dyn EgovSource,
    query: &ArticleQuery,
    today: &str,
    offline: bool,
    fetched_catalog: &mut bool,
    reloaded: &mut Vec<String>,
) -> ToolArticle {
    let law_query = query.law.trim().to_string();
    let article_query = query.article.trim().to_string();
    let Some((key, display)) = parse_article_query(&article_query) else {
        return miss(&law_query, &article_query, "条番号を読めませんでした。");
    };
    let law = match resolve_law(db, client, &law_query, fetched_catalog) {
        Ok(Some(law)) => law,
        Ok(None) => return miss(&law_query, &article_query, "法令名を特定できませんでした。"),
        Err(EgovFailure::Offline) | Err(EgovFailure::NotFound) => {
            return miss(
                &law_query,
                &article_query,
                "オフラインで法令名を確認できませんでした。",
            );
        }
        Err(EgovFailure::Bad(message)) => {
            return miss(&law_query, &article_query, &message);
        }
    };
    let cached = db.lookup_article(&law.law_id, &key).ok().flatten();
    if cached.is_none() {
        return fetch_missing(
            db,
            client,
            &law,
            &key,
            &display,
            &law_query,
            &article_query,
            today,
            offline,
        );
    }
    let cached = cached.expect("cached");
    let remote = db.remote_as_of(&law.law_id).ok().flatten();
    let (from, through, count) = db.update_span().unwrap_or((None, None, 0));
    let covered = updates_cover(
        &law.as_of,
        today,
        from.as_deref(),
        through.as_deref(),
        count,
    );
    let action = revision_action(&RevisionInput {
        stored_as_of: &law.as_of,
        today,
        remote_as_of: remote.as_deref(),
        covered,
        offline,
        cached_articles: law.article_count,
        probed_on: &cached.probed_on,
    });
    match action {
        RevisionAction::Keep { offline } => hit_from_cache(&db, &law, &cached, offline),
        RevisionAction::ProbeArticle => probe_article(
            db,
            client,
            &law,
            &cached,
            &display,
            today,
            remote.as_deref(),
            &law_query,
            &article_query,
        ),
        RevisionAction::ReloadLaw { as_of } => {
            if reloaded.iter().any(|id| id == &law.law_id) {
                let refreshed = db.lookup_article(&law.law_id, &key).ok().flatten();
                if let Some(row) = refreshed {
                    return hit_from_cache(db, &law, &row, false);
                }
            }
            match reload_law(db, client, &law, &as_of, today) {
                Ok(()) => {
                    reloaded.push(law.law_id.clone());
                    let Some(row) = db.lookup_article(&law.law_id, &key).ok().flatten() else {
                        return probe_article(
                            db,
                            client,
                            &law,
                            &cached,
                            &display,
                            today,
                            Some(&as_of),
                            &law_query,
                            &article_query,
                        );
                    };
                    hit_from_cache(db, &law, &row, false)
                }
                Err(EgovFailure::Offline) | Err(EgovFailure::NotFound) => {
                    hit_from_cache(db, &law, &cached, true)
                }
                Err(EgovFailure::Bad(_)) => probe_article(
                    db,
                    client,
                    &law,
                    &cached,
                    &display,
                    today,
                    Some(&as_of),
                    &law_query,
                    &article_query,
                ),
            }
        }
    }
}

fn fetch_missing(
    db: &StatuteDb,
    client: &dyn EgovSource,
    law: &LawRef,
    key: &str,
    display: &str,
    law_query: &str,
    article_query: &str,
    today: &str,
    offline: bool,
) -> ToolArticle {
    if offline {
        return miss(
            law_query,
            article_query,
            "法令DBに無く、オフラインのため e-Gov から取得できませんでした。",
        );
    }
    match fetch_article(client, &law.law_id, display) {
        Ok(parsed) => {
            let mut articles = parsed.articles;
            let article = if let Some(pos) = articles.iter().position(|a| a.key == key) {
                articles.swap_remove(pos)
            } else if articles.len() == 1 {
                let mut article = articles.pop().expect("one article");
                article.key = key.to_string();
                if article.num.is_empty() {
                    article.num = display.to_string();
                }
                article
            } else {
                return miss(
                    law_query,
                    article_query,
                    "e-Gov の応答にこの条がありません。",
                );
            };
            store_fetched(db, law, article, today, law_query, article_query, false)
        }
        Err(EgovFailure::Offline) => miss(
            law_query,
            article_query,
            "法令DBに無く、オフラインのため e-Gov から取得できませんでした。",
        ),
        Err(EgovFailure::NotFound) => miss(
            law_query,
            article_query,
            "e-Gov 法令APIにこの条はありません。",
        ),
        Err(EgovFailure::Bad(message)) => miss(law_query, article_query, &message),
    }
}

fn store_fetched(
    db: &StatuteDb,
    law: &LawRef,
    article: ParsedArticle,
    today: &str,
    law_query: &str,
    article_query: &str,
    offline: bool,
) -> ToolArticle {
    let name = if law.name.is_empty() {
        law_query.to_string()
    } else {
        law.name.clone()
    };
    let _ = db.ensure_law(&law.law_id, &name, &law.law_num, "egov");
    let as_of = if law.as_of.is_empty() {
        today
    } else {
        law.as_of.as_str()
    };
    // A newly fetched article is current as of today unless the law already has a revision date
    // that is still the one we just confirmed.
    let as_of = if as_of.is_empty() {
        today.to_string()
    } else {
        as_of.to_string()
    };
    let stored_as_of = today.to_string();
    if db
        .upsert_article(&law.law_id, &article, &stored_as_of)
        .is_err()
    {
        return miss(
            law_query,
            article_query,
            "取得した条文を保存できませんでした。",
        );
    }
    let _ = db.set_law_revision(&law.law_id, &stored_as_of, today);
    let _ = as_of;
    let mut names = db.aliases_of(&law.law_id).unwrap_or_default();
    if !names.iter().any(|n| n == &name) {
        names.insert(0, name.clone());
    }
    ToolArticle {
        names,
        article_num: article.num,
        heading: article.heading,
        body: article.body,
        as_of: stored_as_of,
        verified: true,
        offline,
        note: String::new(),
        law_query: law_query.to_string(),
        article_query: article_query.to_string(),
    }
}

fn probe_article(
    db: &StatuteDb,
    client: &dyn EgovSource,
    law: &LawRef,
    cached: &crate::statute::store::ArticleRow,
    display: &str,
    today: &str,
    remote_as_of: Option<&str>,
    law_query: &str,
    article_query: &str,
) -> ToolArticle {
    match fetch_article(client, &law.law_id, display) {
        Ok(parsed) => {
            let mut articles = parsed.articles;
            let fresh = if let Some(pos) = articles
                .iter()
                .position(|article| article.key == cached.key)
            {
                Some(articles.swap_remove(pos))
            } else if articles.len() == 1 {
                articles.pop()
            } else {
                None
            };
            let Some(mut fresh) = fresh else {
                let _ = db.mark_probed(&law.law_id, &cached.key, today);
                return hit_from_cache(db, law, cached, false);
            };
            fresh.key = cached.key.clone();
            if fresh.body.trim() == cached.body.trim() {
                let _ = db.mark_probed(&law.law_id, &cached.key, today);
                return hit_from_cache(db, law, cached, false);
            }
            let as_of = remote_as_of
                .map(|s| s.to_string())
                .unwrap_or_else(|| today.to_string());
            if db.upsert_article(&law.law_id, &fresh, &as_of).is_err() {
                return hit_from_cache(db, law, cached, false);
            }
            let _ = db.mark_probed(&law.law_id, &cached.key, today);
            let mut names = db.aliases_of(&law.law_id).unwrap_or_default();
            if names.is_empty() {
                names.push(law.name.clone());
            }
            ToolArticle {
                names,
                article_num: if fresh.num.is_empty() {
                    cached.num.clone()
                } else {
                    fresh.num
                },
                heading: fresh.heading,
                body: fresh.body,
                as_of,
                verified: true,
                offline: false,
                note: String::new(),
                law_query: law_query.to_string(),
                article_query: article_query.to_string(),
            }
        }
        Err(_) => hit_from_cache(db, law, cached, true),
    }
}

fn reload_law(
    db: &StatuteDb,
    client: &dyn EgovSource,
    law: &LawRef,
    as_of: &str,
    today: &str,
) -> Result<(), EgovFailure> {
    let xml = client.fetch(&lawdata_url(&law.law_id))?;
    let parsed = xmlparse::parse_law_xml(&xml)
        .map_err(|_| EgovFailure::Bad("法令XMLを読めませんでした".into()))?;
    let name = if parsed.name.is_empty() {
        law.name.clone()
    } else {
        parsed.name
    };
    let law_num = if parsed.law_num.is_empty() {
        law.law_num.clone()
    } else {
        parsed.law_num
    };
    let mut aliases = db.aliases_of(&law.law_id).unwrap_or_default();
    if !parsed.abbrev.is_empty() {
        aliases.push(parsed.abbrev);
    }
    db.replace_law(
        &law.law_id,
        &name,
        &law_num,
        as_of,
        today,
        "egov",
        &aliases,
        &parsed.articles,
    )
    .map_err(|e| EgovFailure::Bad(e))
}

fn fetch_article(
    client: &dyn EgovSource,
    law_id: &str,
    display: &str,
) -> Result<ParsedLaw, EgovFailure> {
    let xml = client.fetch(&article_url(law_id, display))?;
    xmlparse::parse_law_xml(&xml).map_err(|_| EgovFailure::Bad("条文XMLを読めませんでした".into()))
}

fn hit_from_cache(
    db: &StatuteDb,
    law: &LawRef,
    row: &crate::statute::store::ArticleRow,
    offline: bool,
) -> ToolArticle {
    let mut names = db.aliases_of(&law.law_id).unwrap_or_default();
    if !law.name.is_empty() && !names.iter().any(|n| n == &law.name) {
        names.insert(0, law.name.clone());
    }
    if names.is_empty() {
        names.push(law.law_id.clone());
    }
    ToolArticle {
        names,
        article_num: row.num.clone(),
        heading: row.heading.clone(),
        body: row.body.clone(),
        as_of: row.as_of.clone(),
        verified: true,
        offline,
        note: String::new(),
        law_query: law.name.clone(),
        article_query: row.num.clone(),
    }
}

fn miss(law: &str, article: &str, note: &str) -> ToolArticle {
    ToolArticle {
        names: Vec::new(),
        article_num: String::new(),
        heading: String::new(),
        body: String::new(),
        as_of: String::new(),
        verified: false,
        offline: false,
        note: note.to_string(),
        law_query: law.to_string(),
        article_query: article.to_string(),
    }
}

fn resolve_law(
    db: &StatuteDb,
    client: &dyn EgovSource,
    name: &str,
    fetched_catalog: &mut bool,
) -> Result<Option<LawRef>, EgovFailure> {
    if let Some(law) = db.resolve_name(name).map_err(EgovFailure::Bad)? {
        return Ok(Some(law));
    }
    if *fetched_catalog {
        return Ok(None);
    }
    let xml = client.fetch(&lawlist_url(1))?;
    let names = xmlparse::parse_law_list(&xml)
        .map_err(|_| EgovFailure::Bad("法令名一覧を読めませんでした".into()))?;
    let rows: Vec<(String, String, String)> = names
        .into_iter()
        .map(|row| (row.law_id, row.name, row.law_num))
        .collect();
    db.upsert_names(&rows).map_err(EgovFailure::Bad)?;
    let _ = db.set_meta("lawlist_full", "1");
    *fetched_catalog = true;
    db.resolve_name(name).map_err(EgovFailure::Bad)
}

fn sync_updates(db: &StatuteDb, client: &dyn EgovSource, today: &str) -> bool {
    let through = db.meta("updates_through").unwrap_or_default();
    let Some(mut day) = walk_start(&through, today) else {
        return false;
    };
    let Some(today_d) = parse_ymd(today) else {
        return false;
    };
    let mut fetched = 0i64;
    while day <= today_d && fetched < UPDATE_WALK_DAYS {
        let url = update_list_url(&compact_ymd(day));
        match client.fetch(&url) {
            Ok(xml) => match xmlparse::parse_update_list(&xml) {
                Ok(rows) => {
                    let pairs = rows
                        .into_iter()
                        .map(|row| (row.law_id, row.as_of))
                        .collect::<Vec<_>>();
                    if db.store_updates(&format_ymd(day), &pairs).is_err() {
                        return true;
                    }
                }
                Err(_) => return true,
            },
            Err(EgovFailure::NotFound) => {
                if db.store_updates(&format_ymd(day), &[]).is_err() {
                    return true;
                }
            }
            Err(_) => return true,
        }
        fetched += 1;
        day = match day.succ_opt() {
            Some(next) => next,
            None => break,
        };
    }
    if day > today_d {
        let _ = db.set_meta("updates_through", today);
    } else if fetched > 0 {
        if let Some(last) = day.pred_opt() {
            let _ = db.set_meta("updates_through", &format_ymd(last));
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::statute::egov::{MapEgov, OfflineEgov};
    use crate::statute::xmlparse::ParsedArticle;

    fn temp_db() -> StatuteDb {
        let path = std::env::temp_dir().join(format!(
            "argos-lookup-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_file(&path);
        StatuteDb::open_empty(&path).unwrap()
    }

    fn seed_minpo(db: &StatuteDb, body: &str, as_of: &str) {
        db.replace_law(
            "129AC0000000089",
            "民法",
            "明治二十九年法律第八十九号",
            as_of,
            as_of,
            "bundle",
            &["民法".into()],
            &[ParsedArticle {
                key: "555".into(),
                num: "第555条".into(),
                heading: "売買".into(),
                body: body.into(),
                suppl: false,
            }],
        )
        .unwrap();
    }

    #[test]
    fn offline_returns_the_cached_article_without_failing() {
        let db = temp_db();
        seed_minpo(
            &db,
            "売買は、当事者の一方がある財産権を相手方に移転することを約する。",
            "2024-04-01",
        );
        let hits = resolve_articles(
            &db,
            &OfflineEgov,
            &[ArticleQuery {
                law: "民法".into(),
                article: "555".into(),
            }],
            "2026-11-11",
        );
        assert_eq!(hits.len(), 1);
        assert!(hits[0].verified);
        assert!(hits[0].offline);
        assert!(hits[0].body.contains("財産権"));
        assert_eq!(hits[0].heading, "売買");
        assert_eq!(hits[0].as_of, "2024-04-01");
    }

    #[test]
    fn missing_article_is_fetched_and_cached() {
        let db = temp_db();
        db.set_meta("updates_through", "2026-11-11").unwrap();
        let xml = include_str!("fixtures/article_555.xml").to_string();
        let client = MapEgov::new(vec![("articles;lawId=129AC0000000089".into(), Ok(xml))]);
        db.ensure_law(
            "129AC0000000089",
            "民法",
            "明治二十九年法律第八十九号",
            "bundle",
        )
        .unwrap();
        db.replace_law(
            "129AC0000000089",
            "民法",
            "明治二十九年法律第八十九号",
            "2026-11-11",
            "2026-11-11",
            "bundle",
            &["民法".into()],
            &[],
        )
        .unwrap();
        let hits = resolve_articles(
            &db,
            &client,
            &[ArticleQuery {
                law: "民法".into(),
                article: "第555条".into(),
            }],
            "2026-11-11",
        );
        assert!(hits[0].verified, "{:?}", hits[0].note);
        assert!(hits[0].body.contains("代金を支払う"));
        assert_eq!(hits[0].heading, "売買");
        let stored = db
            .lookup_article("129AC0000000089", "555")
            .unwrap()
            .unwrap();
        assert!(stored.body.contains("代金を支払う"));
    }

    #[test]
    fn newer_revision_replaces_the_cached_body() {
        let db = temp_db();
        seed_minpo(&db, "古い本文", "2020-04-01");
        let updates = include_str!("fixtures/update_list.xml").to_string();
        let article = include_str!("fixtures/article_555.xml").to_string();
        let client = MapEgov::new(vec![
            ("updatelawlists/".into(), Ok(updates)),
            ("articles;".into(), Ok(article)),
        ]);
        let hits = resolve_articles(
            &db,
            &client,
            &[ArticleQuery {
                law: "民法".into(),
                article: "555".into(),
            }],
            "2026-11-11",
        );
        assert!(hits[0].verified, "{:?}", hits[0].note);
        assert!(
            hits[0].body.contains("財産権を相手方に移転"),
            "{}",
            hits[0].body
        );
        assert_ne!(hits[0].body, "古い本文");
        let stored = db
            .lookup_article("129AC0000000089", "555")
            .unwrap()
            .unwrap();
        assert!(stored.body.contains("財産権"));
        assert_eq!(stored.as_of, "2026-04-01");
    }

    #[test]
    fn unknown_article_offline_is_unverified() {
        let db = temp_db();
        let hits = resolve_articles(
            &db,
            &OfflineEgov,
            &[ArticleQuery {
                law: "存在しない法".into(),
                article: "1".into(),
            }],
            "2026-11-11",
        );
        assert!(!hits[0].verified);
        assert!(hits[0].note.contains("オフライン") || hits[0].note.contains("特定できません"));
    }

    #[test]
    fn same_body_is_not_rewritten_when_the_revision_is_not_newer() {
        let db = temp_db();
        let body = "売買は、当事者の一方がある財産権を相手方に移転することを約し、相手方がこれに対してその代金を支払うことを約することによって、その効力を生ずる。";
        seed_minpo(&db, body, "2026-11-11");
        db.set_meta("updates_through", "2026-11-11").unwrap();
        let hits = resolve_articles(
            &db,
            &OfflineEgov,
            &[ArticleQuery {
                law: "民法".into(),
                article: "555".into(),
            }],
            "2026-11-11",
        );
        // updates_through is today, so the walk does not touch the network.
        // stored as_of is today, so the saved text is kept.
        assert!(hits[0].verified);
        assert!(!hits[0].offline);
        assert_eq!(hits[0].body, body);
    }
}
