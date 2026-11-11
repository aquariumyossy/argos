//! Download the bundled statutes from the e-Gov law API and write the seed database.

use std::path::Path;
use std::thread;
use std::time::Duration;

use crate::statute::catalog::{aliases_for, CATALOG};
use crate::statute::egov::{lawdata_url, lawlist_url, EgovSource, LiveEgov};
use crate::statute::refresh::format_ymd;
use crate::statute::store::{gzip_file, StatuteDb, BUNDLE_VERSION};
use crate::statute::xmlparse::{self, ParsedLaw};

pub fn build_bundle(dest: &Path) -> Result<(), String> {
    let client = LiveEgov::new()?;
    let today = format_ymd(chrono::Local::now().date_naive());
    let scratch = dest.with_extension("build.db");
    let _ = std::fs::remove_file(&scratch);
    let db = StatuteDb::open_empty(&scratch)?;
    eprintln!("argos statute bundle: law list");
    let list = client
        .fetch(&lawlist_url(2))
        .map_err(|e| format!("law list: {e:?}"))?;
    let names = xmlparse::parse_law_list(&list)?;
    let rows = names
        .into_iter()
        .map(|row| (row.law_id, row.name, row.law_num))
        .collect::<Vec<_>>();
    eprintln!("argos statute bundle: {} names", rows.len());
    db.upsert_names(&rows)?;
    for entry in CATALOG {
        if !entry.bundle {
            let name = entry.aliases.first().copied().unwrap_or(entry.law_id);
            db.ensure_law(entry.law_id, name, "", "alias")?;
            let aliases = entry
                .aliases
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<_>>();
            // ensure_law stores the first name. Replace aliases without articles.
            if db.article_count_for(entry.law_id)? == 0 {
                db.replace_law(entry.law_id, name, "", "", "", "alias", &aliases, &[])?;
            }
            continue;
        }
        eprintln!("argos statute bundle: {}", entry.law_id);
        thread::sleep(Duration::from_millis(350));
        let xml = client
            .fetch(&lawdata_url(entry.law_id))
            .map_err(|e| format!("{}: {e:?}", entry.law_id))?;
        let parsed = xmlparse::parse_law_xml(&xml).map_err(|e| format!("{}: {e}", entry.law_id))?;
        store_bundled(&db, &parsed, entry.law_id, &today)?;
    }
    db.set_meta("bundle_version", BUNDLE_VERSION)?;
    let articles = db.article_count()?;
    let laws = db.bundled_law_count()?;
    gzip_file(&scratch, dest)?;
    let _ = std::fs::remove_file(&scratch);
    let bytes = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
    eprintln!(
        "argos statute bundle: laws={laws} articles={articles} gzip_bytes={bytes} -> {}",
        dest.display()
    );
    Ok(())
}

fn store_bundled(
    db: &StatuteDb,
    parsed: &ParsedLaw,
    law_id: &str,
    today: &str,
) -> Result<(), String> {
    let id = if parsed.law_id.is_empty() {
        law_id.to_string()
    } else {
        parsed.law_id.clone()
    };
    let mut aliases = aliases_for(&id)
        .iter()
        .map(|s| (*s).to_string())
        .collect::<Vec<_>>();
    if !parsed.abbrev.is_empty() {
        aliases.push(parsed.abbrev.clone());
    }
    if !parsed.name.is_empty() {
        aliases.push(parsed.name.clone());
    }
    let name = if parsed.name.is_empty() {
        aliases.first().cloned().unwrap_or_else(|| id.clone())
    } else {
        parsed.name.clone()
    };
    db.replace_law(
        &id,
        &name,
        &parsed.law_num,
        today,
        today,
        "bundle",
        &aliases,
        &parsed.articles,
    )
}

pub fn bundle_main() -> Result<(), String> {
    let dest = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "src-tauri/resources/statutes.db.gz".into());
    build_bundle(Path::new(&dest))
}
