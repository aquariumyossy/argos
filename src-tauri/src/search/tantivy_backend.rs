use std::borrow::Cow;
use std::collections::HashSet;
use std::path::Path;

use lindera::dictionary::load_dictionary;
use lindera::mode::{Mode, Penalty};
use lindera::segmenter::Segmenter;
use lindera_tantivy::tokenizer::LinderaTokenizer;
use parking_lot::Mutex;
use tantivy::collector::{DocSetCollector, TopDocs};
use tantivy::query::{BooleanQuery, BoostQuery, Occur, PhraseQuery, Query, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, STORED, STRING,
};
use tantivy::tokenizer::TokenStream;
use tantivy::{doc, DocAddress, Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

use crate::pathutil;

use super::legal_ref::{
    has_legal_ref, legal_ref_cite_variants, mask_legal_refs, normalize_legal_refs,
};
use super::morph::{is_noise_highlight_term, MorphAnalyzer};
use super::{ParagraphHit, RemoteShareSnapshot, SearchBackend, SearchHit, SearchOpts};

/// When a date/from allowlist is larger than this, skip the Tantivy path OR and
/// post-filter instead (recall can drop).
pub const PATH_OR_CAP: usize = 2048;

/// Bump when Tantivy on-disk schema changes. Triggers wipe + full reindex.
pub const INDEX_SCHEMA_VERSION: u32 = 5;
const SCHEMA_VERSION_FILE: &str = "argos_schema_version";

/// Mail index schema / chunking strategy (separate directory: `index-mail/`).
/// Bump to wipe the mail index when on-disk schema or unit granularity changes.
pub const MAIL_INDEX_SCHEMA_VERSION: u32 = 2;
const MAIL_SCHEMA_VERSION_FILE: &str = "argos_mail_schema_version";

/// Nested paragraphs shown under each file hit in the popup list.
const NESTED_PARAGRAPH_LIMIT: usize = 3;
/// Collapse near-duplicate units in the same file (shared label / body overlap).
const DEDUPE_BODY_OVERLAP: f32 = 0.45;

/// `title` holds the file name only. It is a very short field, so BM25 length
/// normalization makes one matching token outscore a body full of matches. Damp it
/// without silencing it: case-law file names carry the court, case number and date.
const TITLE_BOOST: f32 = 0.6;

/// Rescore weights, applied as multipliers on BM25 so they follow the index scale.
/// `coverage` is the share of query units found in the hit, so the score is monotone in
/// the number of matched units — more matches can never rank lower.
const W_COVERAGE: f32 = 1.5;
const W_PROXIMITY: f32 = 1.0;
/// `unit_label` is the paragraph heading (`第N条…` or the first 36 chars).
const W_LABEL: f32 = 0.6;

/// Share of query units a hit must match, tried in order. Precision mode starts strict
/// and relaxes; the popup keeps its historical single-unit recall.
const PRECISION_RATIOS: &[f32] = &[0.7, 0.5, 0.0];
const RECALL_RATIOS: &[f32] = &[0.0];

/// A retrieval query with the `minimum_number_should_match` it settled on. The ladder uses
/// the minimum to skip rungs that would re-run an identical query.
type BuiltQuery = (Box<dyn Query>, usize);

/// Which on-disk index this backend owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexKind {
    File,
    Mail,
}

/// Metadata for Outlook email documents (mail index only).
#[derive(Debug, Clone, Default)]
pub struct EmailDocMeta {
    pub from: String,
    pub date_unix: i64,
    pub conversation_id: String,
    pub folder: String,
}

pub struct OpenIndexResult {
    pub backend: TantivyBackend,
    /// True when an older index was wiped; caller should run full reindex.
    pub needs_full_reindex: bool,
}

pub struct TantivyBackend {
    kind: IndexKind,
    index: Index,
    reader: IndexReader,
    writer: Mutex<IndexWriter>,
    fields: Fields,
    morph: Mutex<MorphAnalyzer>,
}

struct Fields {
    title: Field,
    body: Field,
    path: Field,
    mtime: Field,
    size: Field,
    ext: Field,
    folder: Field,
    page: Field,
    chunk_id: Field,
    doc_key: Field,
    unit_id: Field,
    unit_label: Field,
    unit_kind: Field,
    /// Present only for [`IndexKind::Mail`].
    mail_from: Option<Field>,
    mail_date: Option<Field>,
    mail_conversation_id: Option<Field>,
    mail_folder: Option<Field>,
}

impl TantivyBackend {
    pub fn open(index_dir: &Path) -> Result<OpenIndexResult, String> {
        Self::open_kind(index_dir, IndexKind::File)
    }

    pub fn open_mail(index_dir: &Path) -> Result<OpenIndexResult, String> {
        Self::open_kind(index_dir, IndexKind::Mail)
    }

    pub fn kind(&self) -> IndexKind {
        self.kind
    }

    fn open_kind(index_dir: &Path, kind: IndexKind) -> Result<OpenIndexResult, String> {
        std::fs::create_dir_all(index_dir).map_err(|e| e.to_string())?;

        let (schema_version, version_file) = match kind {
            IndexKind::File => (INDEX_SCHEMA_VERSION, SCHEMA_VERSION_FILE),
            IndexKind::Mail => (MAIL_INDEX_SCHEMA_VERSION, MAIL_SCHEMA_VERSION_FILE),
        };
        let version_path = index_dir.join(version_file);
        let existing_version = std::fs::read_to_string(&version_path)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());
        let has_index = index_dir.join("meta.json").exists();
        let needs_full_reindex = has_index && existing_version != Some(schema_version);

        if needs_full_reindex {
            eprintln!(
                "argos: {:?} index schema {:?} -> {}; wiping index for rebuild",
                kind, existing_version, schema_version
            );
            for entry in std::fs::read_dir(index_dir).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                let path = entry.path();
                if path.file_name().and_then(|n| n.to_str()) == Some(version_file) {
                    continue;
                }
                if path.is_dir() {
                    let _ = std::fs::remove_dir_all(&path);
                } else {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }

        let mut schema_builder = Schema::builder();
        let text_opts = TextOptions::default()
            .set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer("lang_ja")
                    .set_index_option(IndexRecordOption::WithFreqsAndPositions),
            )
            .set_stored();

        let title = schema_builder.add_text_field("title", text_opts.clone());
        let body = schema_builder.add_text_field("body", text_opts);
        let path = schema_builder.add_text_field("path", STRING | STORED);
        let mtime = schema_builder.add_text_field("mtime", STRING | STORED);
        let size = schema_builder.add_text_field("size", STRING | STORED);
        let ext = schema_builder.add_text_field("ext", STRING | STORED);
        let folder = schema_builder.add_text_field("folder", STRING | STORED);
        let page = schema_builder.add_text_field("page", STRING | STORED);
        let chunk_id = schema_builder.add_text_field("chunk_id", STRING | STORED);
        let doc_key = schema_builder.add_text_field("doc_key", STRING | STORED);
        let unit_id = schema_builder.add_text_field("unit_id", STRING | STORED);
        let unit_label = schema_builder.add_text_field("unit_label", STRING | STORED);
        let unit_kind = schema_builder.add_text_field("unit_kind", STRING | STORED);
        let (mail_from, mail_date, mail_conversation_id, mail_folder) = if kind == IndexKind::Mail {
            (
                Some(schema_builder.add_text_field("mail_from", STRING | STORED)),
                Some(schema_builder.add_text_field("mail_date", STRING | STORED)),
                Some(schema_builder.add_text_field("mail_conversation_id", STRING | STORED)),
                Some(schema_builder.add_text_field("mail_folder", STRING | STORED)),
            )
        } else {
            (None, None, None, None)
        };
        let schema = schema_builder.build();

        let index = if index_dir.join("meta.json").exists() {
            Index::open_in_dir(index_dir).map_err(|e| e.to_string())?
        } else {
            Index::create_in_dir(index_dir, schema.clone()).map_err(|e| e.to_string())?
        };

        std::fs::write(&version_path, schema_version.to_string()).map_err(|e| e.to_string())?;

        let dictionary = load_dictionary("embedded://ipadic").map_err(|e| e.to_string())?;
        let segmenter = Segmenter::new(Mode::Decompose(Penalty::default()), dictionary, None);
        let tokenizer = LinderaTokenizer::from_segmenter(segmenter);
        index.tokenizers().register("lang_ja", tokenizer);

        let morph = MorphAnalyzer::new()?;

        let writer = index.writer(50_000_000).map_err(|e| e.to_string())?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .map_err(|e| e.to_string())?;

        Ok(OpenIndexResult {
            backend: Self {
                kind,
                index,
                reader,
                writer: Mutex::new(writer),
                fields: Fields {
                    title,
                    body,
                    path,
                    mtime,
                    size,
                    ext,
                    folder,
                    page,
                    chunk_id,
                    doc_key,
                    unit_id,
                    unit_label,
                    unit_kind,
                    mail_from,
                    mail_date,
                    mail_conversation_id,
                    mail_folder,
                },
                morph: Mutex::new(morph),
            },
            needs_full_reindex,
        })
    }

    pub fn delete_by_path(&self, path: &str) -> Result<(), String> {
        let mut writer = self.writer.lock();
        let term = Term::from_field_text(self.fields.path, path);
        writer.delete_term(term);
        writer.commit().map_err(|e| e.to_string())?;
        self.reader.reload().map_err(|e| e.to_string())?;
        Ok(())
    }

    /// True when at least one document with this `path` field exists.
    pub fn has_path(&self, path: &str) -> Result<bool, String> {
        let searcher = self.reader.searcher();
        let term = Term::from_field_text(self.fields.path, path);
        let query = TermQuery::new(term, IndexRecordOption::Basic);
        let top = searcher
            .search(&query, &TopDocs::with_limit(1))
            .map_err(|e| e.to_string())?;
        Ok(!top.is_empty())
    }

    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    pub fn delete_by_folder(&self, folder: &str) -> Result<u64, String> {
        let mut writer = self.writer.lock();
        let term = Term::from_field_text(self.fields.folder, folder);
        writer.delete_term(term);
        writer.commit().map_err(|e| e.to_string())?;
        self.reader.reload().map_err(|e| e.to_string())?;
        Ok(0)
    }

    pub fn delete_paths(&self, paths: &[String]) -> Result<(), String> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut writer = self.writer.lock();
        for path in paths {
            let term = Term::from_field_text(self.fields.path, path);
            writer.delete_term(term);
        }
        writer.commit().map_err(|e| e.to_string())?;
        self.reader.reload().map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Rewrite `folder` / `path` / `doc_key` for all docs under `old_folder` without re-extracting body.
    /// `old_path_prefix` / `new_path_prefix` are the indexed path roots (`effective_public_root`).
    pub fn remap_folder_prefix(
        &self,
        old_folder: &str,
        new_folder: &str,
        old_path_prefix: &str,
        new_path_prefix: &str,
    ) -> Result<u64, String> {
        if self.kind != IndexKind::File {
            return Err("remap_folder_prefix is only for the file index".into());
        }
        let old_folder = pathutil::simplify_windows_path(old_folder);
        let new_folder = pathutil::simplify_windows_path(new_folder);
        let old_path_prefix = pathutil::simplify_windows_path(old_path_prefix);
        let new_path_prefix = pathutil::simplify_windows_path(new_path_prefix);
        if old_folder.is_empty() || new_folder.is_empty() {
            return Err("フォルダパスが空です".into());
        }
        if old_folder.eq_ignore_ascii_case(&new_folder)
            && old_path_prefix.eq_ignore_ascii_case(&new_path_prefix)
        {
            return Ok(0);
        }

        // IMPORTANT: drop Searcher before commit/reload. Keeping a Searcher across
        // reload is undefined behavior and can STATUS_HEAP_CORRUPTION on Windows.
        let rewritten = {
            let searcher = self.reader.searcher();
            let term = Term::from_field_text(self.fields.folder, &old_folder);
            let query = TermQuery::new(term, IndexRecordOption::Basic);
            let limit = (searcher.num_docs() as usize).max(1);
            let top = searcher
                .search(&query, &TopDocs::with_limit(limit))
                .map_err(|e| e.to_string())?;

            let get = |doc: &TantivyDocument, f: Field| -> String {
                doc.get_first(f)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };

            let mut rewritten: Vec<TantivyDocument> = Vec::with_capacity(top.len());
            for (_score, addr) in top {
                let doc = searcher.doc(addr).map_err(|e| e.to_string())?;
                let old_path = get(&doc, self.fields.path);
                let new_path =
                    pathutil::rewrite_prefix(&old_path, &old_path_prefix, &new_path_prefix);
                let old_key = get(&doc, self.fields.doc_key);
                let new_key = if let Some((path_part, rest)) = old_key.split_once('#') {
                    let rewritten_path =
                        pathutil::rewrite_prefix(path_part, &old_path_prefix, &new_path_prefix);
                    format!("{rewritten_path}#{rest}")
                } else {
                    pathutil::rewrite_prefix(&old_key, &old_path_prefix, &new_path_prefix)
                };

                let mut out = TantivyDocument::default();
                out.add_text(self.fields.title, get(&doc, self.fields.title));
                out.add_text(self.fields.body, get(&doc, self.fields.body));
                out.add_text(self.fields.path, new_path);
                out.add_text(self.fields.mtime, get(&doc, self.fields.mtime));
                out.add_text(self.fields.size, get(&doc, self.fields.size));
                out.add_text(self.fields.ext, get(&doc, self.fields.ext));
                out.add_text(self.fields.folder, &new_folder);
                out.add_text(self.fields.page, get(&doc, self.fields.page));
                out.add_text(self.fields.chunk_id, get(&doc, self.fields.chunk_id));
                out.add_text(self.fields.doc_key, new_key);
                out.add_text(self.fields.unit_id, get(&doc, self.fields.unit_id));
                out.add_text(self.fields.unit_label, get(&doc, self.fields.unit_label));
                out.add_text(self.fields.unit_kind, get(&doc, self.fields.unit_kind));
                rewritten.push(out);
            }
            rewritten
        };

        let count = rewritten.len() as u64;
        {
            let mut writer = self.writer.lock();
            let del = Term::from_field_text(self.fields.folder, &old_folder);
            writer.delete_term(del);
            for doc in rewritten {
                writer.add_document(doc).map_err(|e| e.to_string())?;
            }
            writer.commit().map_err(|e| e.to_string())?;
        }

        // Compact: merge segments so deleted (old-path) docs are physically dropped.
        // Soft-fail — remap already committed; search still works without compaction.
        if let Err(e) = self.compact_segments() {
            eprintln!("argos: post-remap compact failed (index remapped, search OK): {e}");
        }

        self.reader.reload().map_err(|e| e.to_string())?;
        Ok(count)
    }

    /// Merge all searchable segments and garbage-collect obsolete files.
    /// Used after bulk delete+add (e.g. folder path remap) to drop tombstones.
    fn compact_segments(&self) -> Result<(), String> {
        let segment_ids = self
            .index
            .searchable_segment_ids()
            .map_err(|e| e.to_string())?;
        if segment_ids.is_empty() {
            return Ok(());
        }
        {
            let mut writer = self.writer.lock();
            // Even a single segment benefits: merge rewrites without deleted docs.
            writer
                .merge(&segment_ids)
                .wait()
                .map_err(|e| e.to_string())?;
            writer
                .garbage_collect_files()
                .wait()
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn clear_all(&self) -> Result<(), String> {
        let mut writer = self.writer.lock();
        writer.delete_all_documents().map_err(|e| e.to_string())?;
        writer.commit().map_err(|e| e.to_string())?;
        self.reader.reload().map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn index_file(
        &self,
        fs_path: &Path,
        store_path: &str,
        folder: &str,
        mtime: u64,
        size: u64,
        extracted: &crate::extractor::ExtractedDoc,
    ) -> Result<usize, String> {
        let path_str = store_path.to_string();
        self.delete_by_path(&path_str)?;

        let ext = fs_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let units = crate::extractor::segment_pages(&extracted.pages);
        let mut writer = self.writer.lock();
        for unit in &units {
            let key = format!("{}#{}", path_str, unit.unit_id);
            let page_str = unit.page.map(|p| p.to_string()).unwrap_or_default();
            let unit_id_str = unit.unit_id.to_string();
            writer
                .add_document(doc!(
                    self.fields.title => extracted.title.as_str(),
                    self.fields.body => unit.text.as_str(),
                    self.fields.path => path_str.as_str(),
                    self.fields.mtime => mtime.to_string().as_str(),
                    self.fields.size => size.to_string().as_str(),
                    self.fields.ext => ext.as_str(),
                    self.fields.folder => folder,
                    self.fields.page => page_str.as_str(),
                    // chunk_id kept as unit sequence for preview ordering compatibility
                    self.fields.chunk_id => unit_id_str.as_str(),
                    self.fields.doc_key => key.as_str(),
                    self.fields.unit_id => unit_id_str.as_str(),
                    self.fields.unit_label => unit.label.as_str(),
                    self.fields.unit_kind => unit.kind.as_str(),
                ))
                .map_err(|e| e.to_string())?;
        }
        writer.commit().map_err(|e| e.to_string())?;
        self.reader.reload().map_err(|e| e.to_string())?;
        Ok(units.len())
    }

    /// Index an Outlook email into the mail index only.
    pub fn index_email(
        &self,
        store_path: &str,
        title: &str,
        mtime: u64,
        size: u64,
        units: &[crate::extractor::SearchUnit],
        meta: &EmailDocMeta,
    ) -> Result<usize, String> {
        if self.kind != IndexKind::Mail {
            return Err("index_email requires the mail Tantivy backend".into());
        }
        let mail_from = self.fields.mail_from.ok_or("mail_from field missing")?;
        let mail_date = self.fields.mail_date.ok_or("mail_date field missing")?;
        let mail_conversation_id = self
            .fields
            .mail_conversation_id
            .ok_or("mail_conversation_id field missing")?;
        let mail_folder = self.fields.mail_folder.ok_or("mail_folder field missing")?;

        let path_str = store_path.to_string();
        self.delete_by_path(&path_str)?;

        let date_str = meta.date_unix.to_string();
        let mut writer = self.writer.lock();
        for unit in units {
            let key = format!("{}#{}", path_str, unit.unit_id);
            let page_str = unit.page.map(|p| p.to_string()).unwrap_or_default();
            let unit_id_str = unit.unit_id.to_string();
            writer
                .add_document(doc!(
                    self.fields.title => title,
                    self.fields.body => unit.text.as_str(),
                    self.fields.path => path_str.as_str(),
                    self.fields.mtime => mtime.to_string().as_str(),
                    self.fields.size => size.to_string().as_str(),
                    self.fields.ext => "msg",
                    self.fields.folder => meta.folder.as_str(),
                    self.fields.page => page_str.as_str(),
                    self.fields.chunk_id => unit_id_str.as_str(),
                    self.fields.doc_key => key.as_str(),
                    self.fields.unit_id => unit_id_str.as_str(),
                    self.fields.unit_label => unit.label.as_str(),
                    self.fields.unit_kind => unit.kind.as_str(),
                    mail_from => meta.from.as_str(),
                    mail_date => date_str.as_str(),
                    mail_conversation_id => meta.conversation_id.as_str(),
                    mail_folder => meta.folder.as_str(),
                ))
                .map_err(|e| e.to_string())?;
        }
        writer.commit().map_err(|e| e.to_string())?;
        self.reader.reload().map_err(|e| e.to_string())?;
        Ok(units.len())
    }

    pub fn morph_content_surfaces(
        &self,
        text: &str,
        pos_filter: bool,
    ) -> Result<Vec<String>, String> {
        self.morph.lock().content_surfaces(text, pos_filter)
    }

    /// Kept for Tantivy tokenizer parity with the inverted index vocabulary.
    fn tokenize_ja(&self, text: &str) -> Result<Vec<String>, String> {
        let mut tokenizer = self
            .index
            .tokenizers()
            .get("lang_ja")
            .ok_or_else(|| "lang_ja tokenizer missing".to_string())?;
        let mut stream = tokenizer.token_stream(text);
        let mut out = Vec::new();
        while let Some(token) = stream.next() {
            out.push(token.text.to_string());
        }
        Ok(out)
    }

    /// Free OR terms: POS filter drops 助詞/助動詞 when enabled.
    /// Surfaces are taken from the index tokenizer, then filtered by morph POS so
    /// TermQuery values always exist in the inverted index.
    fn content_tokens(&self, query: &str, pos_filter: bool) -> Result<Vec<String>, String> {
        let indexed = self.tokenize_ja(query)?;
        if indexed.is_empty() {
            return self.morph.lock().content_surfaces(query, pos_filter);
        }
        let morph = self.morph.lock().analyze(query)?;
        let drop_surfaces: HashSet<String> = morph
            .iter()
            .filter(|t| {
                if pos_filter {
                    t.should_drop_for_content()
                } else {
                    super::morph::is_noise_highlight_term(&t.surface) || t.is_symbol()
                }
            })
            .map(|t| t.surface.clone())
            .collect();

        let mut seen = HashSet::new();
        let mut nouns = Vec::new();
        let mut others = Vec::new();
        // Align indexed tokens with morph drop decisions by surface / coverage.
        for surface in &indexed {
            if surface.trim().is_empty() {
                continue;
            }
            if drop_surfaces.contains(surface)
                || drop_surfaces
                    .iter()
                    .any(|d| d != surface && d.contains(surface.as_str()))
            {
                continue;
            }
            if is_index_symbol_token(surface) {
                continue;
            }
            if !pos_filter && super::morph::is_noise_highlight_term(surface) {
                continue;
            }
            if !seen.insert(surface.clone()) {
                continue;
            }
            let is_noun = morph.iter().any(|t| {
                (t.surface == *surface || t.surface.contains(surface.as_str()))
                    && t.major_pos == "名詞"
            });
            if pos_filter && is_noun {
                nouns.push(surface.clone());
            } else {
                others.push(surface.clone());
            }
        }

        // If the index tokenizer split a kept morph surface (見慣れ → 見+慣れ), the
        // pieces are already in `others`. Also keep the exact indexed token when it
        // equals a kept morph surface. Never inject morph-only strings that are absent
        // from `indexed` — those TermQueries cannot hit the inverted index.
        let mut content = Vec::new();
        content.extend(nouns);
        content.extend(others);
        if content.is_empty() {
            // Loosen: keep any indexed token that is not a symbol / legacy stop / single kana.
            for surface in indexed {
                if surface.trim().is_empty()
                    || is_index_symbol_token(&surface)
                    || super::morph::is_noise_highlight_term(&surface)
                {
                    continue;
                }
                if seen.insert(surface.clone()) {
                    content.push(surface);
                }
            }
        }
        Ok(content)
    }

    /// Full token sequence for passage PhraseQuery (keep particles, drop symbols).
    fn passage_tokens(&self, query: &str) -> Result<Vec<String>, String> {
        Ok(self
            .tokenize_ja(query)?
            .into_iter()
            .filter(|t| !is_index_symbol_token(t) && !t.trim().is_empty())
            .collect())
    }

    /// Quoted / user-dict phrases: keep particles so index positions align.
    fn phrase_tokens(&self, query: &str) -> Result<Vec<String>, String> {
        // Prefer index tokenizer surfaces; fall back to morph if empty.
        let indexed = self.passage_tokens(query)?;
        if !indexed.is_empty() {
            return Ok(indexed);
        }
        self.morph.lock().phrase_surfaces(query)
    }

    fn term_in_title_or_body(&self, tok: &str) -> Vec<(Occur, Box<dyn Query>)> {
        let mut per_token: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for field in [self.fields.title, self.fields.body] {
            let term = Term::from_field_text(field, tok);
            per_token.push((
                Occur::Should,
                Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs)),
            ));
        }
        per_token
    }

    /// Index-tokenized search units for the free (unquoted) part of the query.
    ///
    /// One unit becomes one Should clause, so `minimum_number_should_match` counts words
    /// and compounds rather than the two title/body alternatives of each word. Tokens
    /// always come from the index tokenizer, so every Term exists in the inverted index.
    fn search_units_for(
        &self,
        parsed: &ParsedQuery,
        pos_filter: bool,
        drop_intent: bool,
    ) -> Result<Vec<Vec<String>>, String> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out: Vec<Vec<String>> = Vec::new();
        for raw in &parsed.includes {
            // `legal_cite_group` already requires these characters; letting them become
            // free units too would inflate the minimum-should denominator.
            let masked = if has_legal_ref(raw) {
                mask_legal_refs(raw)
            } else {
                raw.clone()
            };
            let units = {
                let morph = self.morph.lock();
                morph.query_units(&masked, pos_filter, drop_intent)?
            };
            for unit in units {
                let tokens = self.passage_tokens(&unit.text)?;
                if tokens.is_empty() {
                    continue;
                }
                if !seen.insert(tokens.join("\u{1}")) {
                    continue;
                }
                out.push(tokens);
            }
        }
        Ok(out)
    }

    /// One Should clause: an adjacency phrase for compounds, a term for single words.
    /// `title` is damped because it is only the file name.
    fn unit_clause(&self, tokens: &[String]) -> Option<Box<dyn Query>> {
        if tokens.is_empty() {
            return None;
        }
        let mut per_field: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for field in [self.fields.title, self.fields.body] {
            let inner: Box<dyn Query> = if tokens.len() == 1 {
                Box::new(TermQuery::new(
                    Term::from_field_text(field, &tokens[0]),
                    IndexRecordOption::WithFreqs,
                ))
            } else {
                Box::new(PhraseQuery::new(
                    tokens
                        .iter()
                        .map(|t| Term::from_field_text(field, t))
                        .collect(),
                ))
            };
            let scoped: Box<dyn Query> = if field == self.fields.title {
                Box::new(BoostQuery::new(inner, TITLE_BOOST))
            } else {
                inner
            };
            per_field.push((Occur::Should, scoped));
        }
        Some(Box::new(BooleanQuery::new(per_field)))
    }

    fn phrase_in_title_or_body(&self, tokens: &[String]) -> Option<Box<dyn Query>> {
        if tokens.is_empty() {
            return None;
        }
        if tokens.len() == 1 {
            return Some(Box::new(BooleanQuery::new(
                self.term_in_title_or_body(&tokens[0]),
            )));
        }
        let mut per_field: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for field in [self.fields.title, self.fields.body] {
            let terms: Vec<Term> = tokens
                .iter()
                .map(|t| Term::from_field_text(field, t))
                .collect();
            per_field.push((Occur::Should, Box::new(PhraseQuery::new(terms))));
        }
        Some(Box::new(BooleanQuery::new(per_field)))
    }

    /// 第555条 → (第555条 | 第五百五十五条 | 第五五五条 | …) as adjacent phrases.
    /// The index stores the original spelling, so the alternatives have to come from
    /// the query. Never OR the bare numerals (五 / 百 alone would match all of 民法).
    fn legal_cite_group(&self, parsed: &ParsedQuery) -> Result<Option<Box<dyn Query>>, String> {
        let mut seen = HashSet::new();
        let mut shoulds: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for raw in &parsed.includes {
            for variant in legal_ref_cite_variants(raw) {
                if !seen.insert(variant.clone()) {
                    continue;
                }
                let tokens = self.passage_tokens(&variant)?;
                // A single token would degrade to a TermQuery OR in
                // `phrase_in_title_or_body`; that must not become a Must clause.
                if tokens.len() < 2 {
                    continue;
                }
                if let Some(q) = self.phrase_in_title_or_body(&tokens) {
                    shoulds.push((Occur::Should, q));
                }
            }
        }
        if shoulds.is_empty() {
            return Ok(None);
        }
        Ok(Some(Box::new(BooleanQuery::new(shoulds))))
    }

    /// Build the retrieval query, returning it with the Should minimum it ended up using.
    ///
    /// `min_should_ratio` is the share of search units a document must match. `0.0` means
    /// one unit is enough, which is the historical behaviour used by the popup.
    fn build_parsed_query(
        &self,
        parsed: &ParsedQuery,
        units: &[Vec<String>],
        min_should_ratio: f32,
        pos_filter: bool,
        cite_required: bool,
    ) -> Result<Option<BuiltQuery>, String> {
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        let mut should_units = 0usize;
        let mut has_must = false;

        for tokens in units {
            if let Some(q) = self.unit_clause(tokens) {
                clauses.push((Occur::Should, q));
                should_units += 1;
            }
        }

        // Quoted phrases are left alone: quoting is a request for the exact spelling.
        if let Some(q) = self.legal_cite_group(parsed)? {
            if cite_required {
                clauses.push((Occur::Must, q));
                has_must = true;
            } else {
                clauses.push((Occur::Should, q));
                should_units += 1;
            }
        }

        for phrase in &parsed.phrases {
            let tokens = self.phrase_tokens(phrase)?;
            if let Some(q) = self.phrase_in_title_or_body(&tokens) {
                // Quoted phrases are required (Google-like).
                clauses.push((Occur::Must, q));
                has_must = true;
            }
        }

        for raw in &parsed.excludes {
            let tokens = self.content_tokens(raw, pos_filter)?;
            for tok in &tokens {
                for (_, q) in self.term_in_title_or_body(tok) {
                    clauses.push((Occur::MustNot, q));
                }
            }
        }

        for phrase in &parsed.exclude_phrases {
            let tokens = self.phrase_tokens(phrase)?;
            if let Some(q) = self.phrase_in_title_or_body(&tokens) {
                clauses.push((Occur::MustNot, q));
            }
        }

        let has_positive = !parsed.includes.is_empty() || !parsed.phrases.is_empty();
        if !has_positive {
            return Ok(None);
        }
        if clauses.is_empty() {
            return Ok(None);
        }
        // A citation or a quoted phrase is already the precise part of the query, so the
        // remaining words only rank. Requiring them too turns 民法 第555条 into zero hits
        // against a statute file whose articles never repeat the act's name.
        //
        // Tantivy also answers with an EmptyScorer when the minimum exceeds the number of
        // Should clauses, and when the minimum is positive but there are none at all.
        let min_should = if should_units == 0 || has_must {
            0
        } else {
            (((should_units as f32) * min_should_ratio).ceil() as usize).clamp(1, should_units)
        };
        Ok(Some((
            Box::new(BooleanQuery::with_minimum_required_clauses(
                clauses, min_should,
            )),
            min_should,
        )))
    }

    /// Surface strings of the query's search units, for chips and proximity scoring.
    /// Compounds stay whole (裁判例, not 裁判 + 例).
    fn unit_surfaces_for(
        &self,
        parsed: &ParsedQuery,
        pos_filter: bool,
        drop_intent: bool,
        cite_norm: bool,
    ) -> Result<Vec<String>, String> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for raw in &parsed.includes {
            let raw = if cite_norm {
                normalize_legal_refs(raw)
            } else {
                raw.clone()
            };
            let units = {
                let morph = self.morph.lock();
                morph.query_units(&raw, pos_filter, drop_intent)?
            };
            for unit in units {
                if seen.insert(unit.text.clone()) {
                    out.push(unit.text);
                }
            }
        }
        Ok(out)
    }

    fn highlight_terms_for(
        &self,
        parsed: &ParsedQuery,
        pos_filter: bool,
        drop_intent: bool,
    ) -> Result<Vec<String>, String> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let mut push = |s: String| {
            if s.is_empty() || !seen.insert(s.clone()) {
                return;
            }
            if pos_filter && is_noise_highlight_term(&s) {
                return;
            }
            out.push(s);
        };
        let units = self.unit_surfaces_for(parsed, pos_filter, drop_intent, false)?;
        for raw in &parsed.includes {
            // Keep the raw include only when it is itself a single unit (or POS filter
            // is off), otherwise chips would repeat the whole sentence.
            if !pos_filter || (units.len() == 1 && units[0] == *raw) {
                push(raw.clone());
            }
            // Every spelling of the citation; `hit_from_doc` keeps only the one the
            // document actually uses, and the snippet anchors on it because it is long.
            for cite in legal_ref_cite_variants(raw) {
                push(cite);
            }
        }
        for unit in units {
            push(unit);
        }
        for phrase in &parsed.phrases {
            // Show the registered/quoted phrase as one chip; skip particle pieces.
            push(phrase.clone());
            if !pos_filter {
                for tok in self.phrase_tokens(phrase)? {
                    push(tok);
                }
            }
        }
        Ok(out)
    }

    /// Tokens for the overlap / compactness rescore. These are matched as plain
    /// substrings of the candidate text, so compounds must stay whole — splitting 裁判例
    /// into 裁判 and 例 would count one match as two and wreck the span measurement.
    ///
    /// `cite_norm` folds every citation spelling to the Arabic form. The candidate
    /// text is normalized the same way, so 第五百五十五条 and 第555条 compare equal
    /// here even though the inverted index kept them apart.
    fn proximity_tokens_for(
        &self,
        parsed: &ParsedQuery,
        pos_filter: bool,
        drop_intent: bool,
        cite_norm: bool,
    ) -> Result<Vec<String>, String> {
        let mut out = self.unit_surfaces_for(parsed, pos_filter, drop_intent, cite_norm)?;
        let mut seen: HashSet<String> = out.iter().cloned().collect();
        for phrase in &parsed.phrases {
            // Prefer the whole phrase string for proximity / overlap checks.
            if seen.insert(phrase.clone()) {
                out.push(phrase.clone());
            }
        }
        Ok(out)
    }

    fn hit_from_doc(
        &self,
        score: f32,
        doc: &TantivyDocument,
        query: &str,
        highlight_terms: &[String],
    ) -> Option<SearchHit> {
        let get = |f: Field| -> String {
            doc.get_first(f)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let title = get(self.fields.title);
        let body = get(self.fields.body);
        let path = get(self.fields.path);
        let page = get(self.fields.page).parse().ok();
        let chunk_id = get(self.fields.chunk_id).parse().ok();
        let key = get(self.fields.doc_key);
        let unit_label = get(self.fields.unit_label);
        let mail_from = self.fields.mail_from.map(|f| get(f)).unwrap_or_default();
        let mail_date = self.fields.mail_date.map(|f| get(f)).unwrap_or_default();
        let mail_conversation_id = self
            .fields
            .mail_conversation_id
            .map(|f| get(f))
            .unwrap_or_default();
        let mail_folder = self.fields.mail_folder.map(|f| get(f)).unwrap_or_default();
        let snippet = make_snippet(&body, query, highlight_terms, 100);
        let haystack = format!("{title} {body} {mail_from}");
        let mut terms: Vec<String> = Vec::new();
        for t in highlight_terms {
            if !t.is_empty() && haystack.contains(t) {
                terms.push(t.clone());
            }
        }
        terms.sort_by_key(|t| std::cmp::Reverse(t.chars().count()));
        let (source, doc_kind) = if self.kind == IndexKind::Mail {
            ("outlook".to_string(), "email".to_string())
        } else {
            ("local".to_string(), "file".to_string())
        };
        Some(SearchHit {
            id: key,
            title,
            snippet,
            path,
            page,
            chunk_id,
            score,
            source,
            preview_text: body,
            highlight_terms: terms,
            match_count: 1,
            paragraphs: Vec::new(),
            unit_label,
            mail_from,
            mail_date,
            mail_conversation_id,
            mail_folder,
            doc_kind,
        })
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ParsedQuery {
    pub includes: Vec<String>,
    pub phrases: Vec<String>,
    pub excludes: Vec<String>,
    pub exclude_phrases: Vec<String>,
}

/// Short function-word surfaces to skip when guessing snippet anchors.
fn is_stop_token(t: &str) -> bool {
    matches!(
        t,
        "の" | "を"
            | "に"
            | "は"
            | "が"
            | "も"
            | "と"
            | "で"
            | "へ"
            | "や"
            | "か"
            | "など"
            | "より"
            | "から"
            | "まで"
            | "て"
            | "た"
            | "れ"
            | "せ"
            | "し"
            | "さ"
    )
}

fn is_query_delimiter(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\n' | '\r' | '\u{3000}' | ',' | '\u{FF0C}' | '\u{3001}'
    )
}

fn is_index_symbol_token(t: &str) -> bool {
    t.chars().all(|c| {
        matches!(
            c,
            '(' | ')'
                | '\u{300c}' // 「
                | '\u{300d}' // 」
                | '\u{3001}' // 、
                | '\u{3002}' // 。
                | '\u{30fb}' // ・
                | '"'
                | '\''
                | '\u{ff08}' // （
                | '\u{ff09}' // ）
                | '，'
                | ','
                | '.'
                | '!'
                | '？'
                | '?'
                | '：'
                | ':'
                | '；'
                | ';'
                | '/'
                | '\\'
                | '…'
                | '—'
                | '-'
                | '～'
                | '~'
        ) || c.is_whitespace()
    })
}

/// Parse Google-like syntax: `"phrase"`, `-exclude`, `-"exclude phrase"`.
/// Delimiters: half/full-width space and comma (including `、`).
pub fn parse_query_syntax(raw: &str) -> ParsedQuery {
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0usize;
    let mut out = ParsedQuery::default();

    while i < chars.len() {
        while i < chars.len() && is_query_delimiter(chars[i]) {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }

        let exclude = chars[i] == '-' && i + 1 < chars.len() && !is_query_delimiter(chars[i + 1]);
        if exclude {
            i += 1;
        }

        if i < chars.len() && chars[i] == '"' {
            i += 1; // opening quote
            let start = i;
            let mut closed = false;
            while i < chars.len() {
                if chars[i] == '"' {
                    closed = true;
                    break;
                }
                i += 1;
            }
            if closed {
                let inner: String = chars[start..i].iter().collect();
                i += 1; // closing quote
                let inner = inner.trim();
                if !inner.is_empty() {
                    if exclude {
                        out.exclude_phrases.push(inner.to_string());
                    } else {
                        out.phrases.push(inner.to_string());
                    }
                }
            } else {
                // Unclosed quote: treat remainder as plain text (include quote char).
                let mut plain: String = String::from("\"");
                plain.extend(chars[start..].iter());
                let plain = plain.trim();
                if !plain.is_empty() {
                    if exclude {
                        out.excludes.push(plain.to_string());
                    } else {
                        out.includes.push(plain.to_string());
                    }
                }
                break;
            }
            continue;
        }

        let start = i;
        while i < chars.len() && !is_query_delimiter(chars[i]) {
            i += 1;
        }
        let term: String = chars[start..i].iter().collect();
        let term = term.trim();
        if term.is_empty() {
            continue;
        }
        if exclude {
            out.excludes.push(term.to_string());
        } else {
            out.includes.push(term.to_string());
        }
    }

    out
}

fn haystack_contains_phrase(haystack: &str, phrase: &str) -> bool {
    !phrase.is_empty() && haystack.contains(phrase)
}

/// How strongly the paragraph heading carries a query unit: `1.0` when the heading starts
/// with it, `0.5` when it only mentions it, `0.0` otherwise.
///
/// `unit_label` is the leading `第N条…` for statutes and the first 36 characters
/// otherwise, so it doubles as a heading for ordinary documents (`第3 争点`,
/// `1 事実の概要`). A paragraph that *is* the article beats commentary that merely
/// discusses it, and a heading always begins with its subject. Reads the stored field, so
/// no reindex is involved.
fn label_match_strength(unit_label: &str, tokens: &[String], cite_norm: bool) -> f32 {
    let label = unit_label.trim();
    if label.is_empty() || tokens.is_empty() {
        return 0.0;
    }
    let hay: Cow<str> = if cite_norm {
        Cow::Owned(normalize_legal_refs(label))
    } else {
        Cow::Borrowed(label)
    };
    let mut best = 0.0f32;
    for t in tokens {
        if t.chars().count() < 2 {
            continue;
        }
        if hay.starts_with(t.as_str()) {
            return 1.0;
        }
        if hay.contains(t.as_str()) {
            best = best.max(0.5);
        }
    }
    best
}

/// Char-index ranges `[start, end)` for every occurrence of `needle` in `hay`.
fn find_occurrences(hay: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    let hay_chars: Vec<char> = hay.chars().collect();
    let needle_chars: Vec<char> = needle.chars().collect();
    let nlen = needle_chars.len();
    if nlen == 0 || hay_chars.len() < nlen {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i + nlen <= hay_chars.len() {
        if hay_chars[i..i + nlen] == needle_chars[..] {
            out.push((i, i + nlen));
            i += nlen; // non-overlapping advance; still fine for proximity
        } else {
            i += 1;
        }
    }
    out
}

/// Minimum character span that covers as many distinct query tokens as possible.
/// Returns `(tokens_covered, span_chars)`.
fn proximity_span(text: &str, tokens: &[String]) -> (usize, usize) {
    #[derive(Clone, Copy)]
    struct Hit {
        start: usize,
        end: usize,
        tok: usize,
    }

    let mut hits: Vec<Hit> = Vec::new();
    let mut present = 0usize;
    for (tok, t) in tokens.iter().enumerate() {
        let occ = find_occurrences(text, t);
        if !occ.is_empty() {
            present += 1;
        }
        for (start, end) in occ {
            hits.push(Hit { start, end, tok });
        }
    }
    if present == 0 || hits.is_empty() {
        return (0, usize::MAX / 4);
    }
    hits.sort_by_key(|h| h.start);

    // Sliding window: cover all `present` distinct tokens with minimum span
    let need = present;
    let mut count = vec![0usize; tokens.len()];
    let mut covered = 0usize;
    let mut best_span = usize::MAX / 4;
    let mut left = 0usize;

    for right in 0..hits.len() {
        let r = hits[right];
        if count[r.tok] == 0 {
            covered += 1;
        }
        count[r.tok] += 1;

        while covered == need && left <= right {
            let span = hits[right].end.saturating_sub(hits[left].start);
            if span < best_span {
                best_span = span.max(1);
            }
            let l = hits[left];
            count[l.tok] -= 1;
            if count[l.tok] == 0 {
                covered -= 1;
            }
            left += 1;
        }
    }

    (present, best_span.max(1))
}

/// Compactness in (0, 1]: 1 = tokens packed as tightly as their own lengths.
fn compactness_score(tokens: &[String], text: &str, span: usize) -> f32 {
    let mut ideal = 0usize;
    for t in tokens {
        if !t.is_empty() && text.contains(t) {
            ideal += t.chars().count();
        }
    }
    let ideal = ideal.max(1) as f32;
    let span = span.max(1) as f32;
    (ideal / span).clamp(0.05, 1.0)
}

fn make_snippet(body: &str, query: &str, highlight_terms: &[String], radius: usize) -> String {
    let q = query.trim();
    let chars: Vec<char> = body.chars().collect();
    if chars.is_empty() {
        return String::new();
    }

    let lower_body: String = body.to_lowercase();
    let mut match_start = None;
    let mut match_len = q.chars().count();

    // Prefer a highlight term that appears in the body (longest first)
    let mut terms: Vec<&String> = highlight_terms.iter().collect();
    terms.sort_by_key(|t| std::cmp::Reverse(t.chars().count()));
    for t in &terms {
        if t.is_empty() {
            continue;
        }
        if let Some(byte_idx) = lower_body.find(&t.to_lowercase()) {
            match_start = Some(lower_body[..byte_idx].chars().count());
            match_len = t.chars().count();
            break;
        }
    }

    if match_start.is_none() && !q.is_empty() {
        if let Some(byte_idx) = lower_body.find(&q.to_lowercase()) {
            match_start = Some(lower_body[..byte_idx].chars().count());
            match_len = q.chars().count();
        } else {
            let q_chars: Vec<char> = q.chars().collect();
            'outer: for len in (2..=q_chars.len()).rev() {
                for start in 0..=(q_chars.len() - len) {
                    let sub: String = q_chars[start..start + len].iter().collect();
                    if is_stop_token(&sub) {
                        continue;
                    }
                    if let Some(byte_idx) = lower_body.find(&sub.to_lowercase()) {
                        match_start = Some(lower_body[..byte_idx].chars().count());
                        match_len = len;
                        break 'outer;
                    }
                }
            }
        }
    }

    let (start, end) = if let Some(pos) = match_start {
        let start = pos.saturating_sub(radius / 3);
        let end = (pos + match_len + radius * 2 / 3).min(chars.len());
        (start, end)
    } else {
        (0, radius.min(chars.len()))
    };

    let mut snip: String = chars[start..end].iter().collect();
    if start > 0 {
        snip = format!("\u{2026}{snip}");
    }
    if end < chars.len() {
        snip.push('\u{2026}');
    }
    snip
}

impl TantivyBackend {
    /// Score-ranked chunk hits, optionally scoped by path prefix. Does not dedupe by path.
    /// When `exact_path` is set, AND a path TermQuery so all chunks of that file are retrieved.
    /// When `exts` is set, AND an extension TermQuery (OR across the list).
    /// When `path_allowlist` is set, results are restricted to those paths (Tantivy OR if
    /// small, otherwise a post-filter).
    #[allow(clippy::too_many_arguments)]
    fn search_scored(
        &self,
        query: &str,
        limit: usize,
        path_prefix: Option<&str>,
        exts: Option<&[String]>,
        pos_filter_enabled: bool,
        exact_path: Option<&str>,
        opts: SearchOpts,
        path_allowlist: Option<&[String]>,
        share: Option<&RemoteShareSnapshot>,
    ) -> Result<Vec<(f32, SearchHit)>, String> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(vec![]);
        }

        let parsed = parse_query_syntax(q);
        if parsed.includes.is_empty() && parsed.phrases.is_empty() {
            return Ok(vec![]);
        }

        // A citation in the query means the article number is the point of the search;
        // require it so 第555条 is not buried under every chunk that mentions 民法.
        let cite_norm = parsed.includes.iter().any(|r| has_legal_ref(r));
        // Only the chat tool sends question sentences, and only those carry boilerplate.
        // Words the user typed are meant literally, so nothing is dropped from them.
        let drop_intent = opts.precision;
        // Separating words with a space is a deliberate act: the user listed the terms a
        // document should carry, so requiring most of them is what they asked for. A
        // selected sentence has no separators and stays a single include, keeping the
        // recall the popup depends on.
        let require_most = opts.precision || parsed.includes.len() >= 2;
        let units = self.search_units_for(&parsed, pos_filter_enabled, drop_intent)?;

        let highlight_terms =
            self.highlight_terms_for(&parsed, pos_filter_enabled, drop_intent)?;
        let proximity_tokens =
            self.proximity_tokens_for(&parsed, pos_filter_enabled, drop_intent, cite_norm)?;
        let scope = path_prefix
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let ext_filter: Option<Vec<String>> = exts.filter(|e| !e.is_empty()).map(|e| e.to_vec());
        // Use path as stored in the index (hit.path), not simplified — STRING TermQuery is exact.
        let exact = exact_path
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let allow_or: Option<Vec<String>> = path_allowlist.and_then(|paths| {
            let cleaned: Vec<String> = paths
                .iter()
                .map(|p| p.trim())
                .filter(|p| !p.is_empty())
                .map(|p| p.to_string())
                .collect();
            if cleaned.is_empty() {
                None
            } else if cleaned.len() <= PATH_OR_CAP {
                Some(cleaned)
            } else {
                None
            }
        });
        let allow_set: Option<HashSet<String>> = path_allowlist.map(|paths| {
            let mut set = HashSet::new();
            for p in paths {
                insert_allow_path(&mut set, p);
            }
            set
        });
        if let Some(ref set) = allow_set {
            if set.is_empty() {
                return Ok(Vec::new());
            }
        }
        eprintln!(
            "argos: parsed includes={:?} phrases={:?} excludes={:?} exclude_phrases={:?} prox={:?} scope={:?} exts={:?} exact_path={:?} allow_or={} pos_filter={} precision={} require_most={}",
            parsed.includes, parsed.phrases, parsed.excludes, parsed.exclude_phrases, proximity_tokens, scope, ext_filter, exact, allow_or.as_ref().map(|v| v.len()).unwrap_or(0), pos_filter_enabled, opts.precision, require_most
        );
        eprintln!(
            "argos: highlight_terms={:?} search_units={:?}",
            highlight_terms, units
        );

        let wrap_filters = |inner: Box<dyn Query>| -> Box<dyn Query> {
            let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(Occur::Must, inner)];
            if let Some(ref p) = exact {
                let path_term = Term::from_field_text(self.fields.path, p);
                let path_q = TermQuery::new(path_term, IndexRecordOption::Basic);
                clauses.push((Occur::Must, Box::new(path_q)));
            }
            if let Some(ref paths) = allow_or {
                if paths.len() == 1 {
                    let term = Term::from_field_text(self.fields.path, &paths[0]);
                    clauses.push((
                        Occur::Must,
                        Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
                    ));
                } else {
                    let shoulds: Vec<(Occur, Box<dyn Query>)> = paths
                        .iter()
                        .map(|p| {
                            let term = Term::from_field_text(self.fields.path, p);
                            (
                                Occur::Should,
                                Box::new(TermQuery::new(term, IndexRecordOption::Basic))
                                    as Box<dyn Query>,
                            )
                        })
                        .collect();
                    clauses.push((Occur::Must, Box::new(BooleanQuery::new(shoulds))));
                }
            }
            if let Some(ref list) = ext_filter {
                if list.len() == 1 {
                    let term = Term::from_field_text(self.fields.ext, &list[0]);
                    clauses.push((
                        Occur::Must,
                        Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
                    ));
                } else {
                    let shoulds: Vec<(Occur, Box<dyn Query>)> = list
                        .iter()
                        .map(|e| {
                            let term = Term::from_field_text(self.fields.ext, e);
                            (
                                Occur::Should,
                                Box::new(TermQuery::new(term, IndexRecordOption::Basic))
                                    as Box<dyn Query>,
                            )
                        })
                        .collect();
                    clauses.push((Occur::Must, Box::new(BooleanQuery::new(shoulds))));
                }
            }
            if clauses.len() == 1 {
                clauses.remove(0).1
            } else {
                Box::new(BooleanQuery::new(clauses))
            }
        };

        let searcher = self.reader.searcher();
        // `limit` is the desired scored-unit count. Mild over-fetch absorbs post-filters.
        // (Do not multiply by large factors here — callers already size the unit budget.)
        let fetch_n = if exact.is_some() {
            limit.max(50).min(80)
        } else if allow_or.is_some() {
            (limit * 2).max(40).min(400)
        } else if scope.is_some() || ext_filter.is_some() || allow_set.is_some() || share.is_some() {
            (limit * 5).max(80).min(400)
        } else {
            (limit * 2).max(40).min(200)
        };
        // Scope / extension / exclusion filters plus the proximity rescore. Runs per rung
        // of the ladder, because `min_overlap` can reject everything retrieval found —
        // `find_occurrences` compares characters exactly, so an ASCII term the index
        // matched case-insensitively contributes no overlap here.
        let rescore = |top: Vec<(f32, DocAddress)>,
                       min_overlap: usize|
         -> Result<Vec<(f32, SearchHit)>, String> {
            let mut scored: Vec<(f32, SearchHit)> = Vec::new();
            for (score, addr) in top {
                let doc: TantivyDocument = searcher.doc(addr).map_err(|e| e.to_string())?;
                let Some(mut hit) = self.hit_from_doc(score, &doc, q, &highlight_terms) else {
                    continue;
                };
                if let Some(ref set) = allow_set {
                    if !path_in_allowlist(&hit.path, set) {
                        continue;
                    }
                }
                if let Some(snap) = share {
                    if !snap.path_is_shared(&hit.path) {
                        continue;
                    }
                }
                if let Some(ref prefix) = scope {
                    if let Some(folder) = prefix.strip_prefix("mailfolder:") {
                        if self.kind != IndexKind::Mail {
                            continue;
                        }
                        let folder = folder.trim();
                        if !hit.mail_folder.eq_ignore_ascii_case(folder) {
                            continue;
                        }
                    } else if crate::mail::is_outlook_path(&hit.path)
                        || !pathutil::path_starts_with(&hit.path, prefix)
                    {
                        continue;
                    }
                }
                if let Some(ref list) = ext_filter {
                    let hit_ext = super::path_extension(&hit.path);
                    if !list.iter().any(|e| e == &hit_ext) {
                        continue;
                    }
                }
                let haystack = format!("{} {}", hit.title, hit.preview_text);

                // Exclusions stay on the original text: `-第五百五十五条` means that spelling.
                if parsed
                    .exclude_phrases
                    .iter()
                    .any(|p| haystack_contains_phrase(&haystack, p))
                {
                    continue;
                }

                let prox_hay: Cow<str> = if cite_norm {
                    Cow::Owned(normalize_legal_refs(&haystack))
                } else {
                    Cow::Borrowed(haystack.as_str())
                };
                let (overlap, span) = if proximity_tokens.is_empty() {
                    (0, 1)
                } else {
                    proximity_span(&prox_hay, &proximity_tokens)
                };
                if min_overlap > 0 && overlap < min_overlap {
                    continue;
                }
                let compact = if proximity_tokens.is_empty() {
                    1.0
                } else {
                    compactness_score(&proximity_tokens, &prox_hay, span)
                };
                // Multipliers on BM25, so the boost follows the index scale instead of a
                // fixed constant, and the score is monotone in `overlap`: matching more of
                // the query can never rank a hit lower. Compactness only sweetens a match
                // that is already broad, it cannot outweigh coverage.
                let coverage = if proximity_tokens.is_empty() {
                    0.0
                } else {
                    (overlap as f32) / (proximity_tokens.len() as f32)
                };
                let label_bonus =
                    W_LABEL * label_match_strength(&hit.unit_label, &proximity_tokens, cite_norm);
                let combined = score
                    * (1.0 + W_COVERAGE * coverage + W_PROXIMITY * coverage * compact + label_bonus);
                hit.score = combined;
                scored.push((combined, hit));
            }
            Ok(scored)
        };

        // Post-filter strength follows what the rung actually required. Ratio 0 keeps the
        // historical rules: one content token is enough under POS filtering, half of them
        // otherwise (the popup searches whole selected sentences).
        let min_overlap_for = |ratio: f32, loose: bool| -> usize {
            let n = proximity_tokens.len();
            if n == 0 {
                0
            } else if parsed.includes.is_empty() {
                n.min(1)
            } else if ratio > 0.0 {
                (((n as f32) * ratio).ceil() as usize).clamp(1, n)
            } else if pos_filter_enabled || loose {
                1
            } else {
                n.div_ceil(2).max(1)
            }
        };

        // Retrieval ladder: try the strictest share of search units first and relax until
        // something survives the rescore. The last rung reproduces the historical query,
        // so a stricter rung can only ever return fewer hits — never zero where the old
        // behaviour returned some.
        let ratios = if require_most {
            PRECISION_RATIOS
        } else {
            RECALL_RATIOS
        };
        // Compounds are adjacency phrases. If nothing matches them, split them into
        // single terms as a last resort so a partial word still finds something.
        let flat_units: Vec<Vec<String>> = units
            .iter()
            .flat_map(|u| u.iter().map(|t| vec![t.clone()]))
            .collect();

        // Skip rungs that would re-run an identical query (a Must clause collapses every
        // ratio to the same minimum).
        let mut last_sig: Option<(usize, bool, bool)> = None;
        for stage in 0..(ratios.len() + 2) {
            let (stage_units, ratio, loose) = match stage {
                s if s < ratios.len() => (&units, ratios[s], false),
                // Same units, but let the citation be optional.
                s if s == ratios.len() => (&units, 0.0, false),
                _ => (&flat_units, 0.0, true),
            };
            let cite_required = cite_norm && stage < ratios.len();
            if stage > ratios.len() && flat_units.len() == units.len() {
                break; // No compounds to split; the previous rung already covered this.
            }
            let Some((tantivy_q, min_should)) = self.build_parsed_query(
                &parsed,
                stage_units,
                ratio,
                pos_filter_enabled,
                cite_required,
            )?
            else {
                continue;
            };
            let sig = (min_should, cite_required, loose);
            if last_sig == Some(sig) {
                continue;
            }
            last_sig = Some(sig);
            let top = searcher
                .search(&*wrap_filters(tantivy_q), &TopDocs::with_limit(fetch_n))
                .map_err(|e| e.to_string())?;
            let effective_ratio = if min_should == 0 { 0.0 } else { ratio };
            let mut scored = rescore(top, min_overlap_for(effective_ratio, loose))?;
            eprintln!(
                "argos: retrieval stage={stage} min_should={min_should} cite_required={cite_required} kept={}",
                scored.len()
            );
            if !scored.is_empty() {
                scored.sort_by(|a, b| b.0.total_cmp(&a.0));
                return Ok(scored);
            }
        }
        Ok(Vec::new())
    }

    /// All matching chunks for one file, ordered by chunk_id (for preview navigation).
    pub fn matches_for_path(
        &self,
        query: &str,
        path: &str,
        limit: usize,
        pos_filter_enabled: bool,
    ) -> Result<Vec<SearchHit>, String> {
        let path = path.trim();
        if path.is_empty() {
            return Ok(vec![]);
        }
        let mut scored = self.search_scored(
            query,
            limit,
            None,
            None,
            pos_filter_enabled,
            Some(path),
            SearchOpts::default(),
            None,
            None,
        )?;
        // Fallback if TermQuery missed due to path normalization drift: prefix scope.
        if scored.is_empty() {
            scored = self.search_scored(
                query,
                limit,
                Some(path),
                None,
                pos_filter_enabled,
                None,
                SearchOpts::default(),
                None,
                None,
            )?;
            scored.retain(|(_, hit)| {
                pathutil::simplify_windows_path(&hit.path)
                    .eq_ignore_ascii_case(&pathutil::simplify_windows_path(path))
            });
        }
        scored.sort_by(|a, b| {
            let ca = a.1.chunk_id.unwrap_or(0);
            let cb = b.1.chunk_id.unwrap_or(0);
            ca.cmp(&cb)
                .then_with(|| a.1.page.unwrap_or(0).cmp(&b.1.page.unwrap_or(0)))
        });
        let hits: Vec<SearchHit> = scored.into_iter().map(|(_, hit)| hit).collect();
        let hits = dedupe_path_units_doc_order(hits);
        let hits: Vec<SearchHit> = hits.into_iter().take(limit).collect();
        eprintln!(
            "argos: path_matches={} path={}",
            hits.len(),
            pathutil::simplify_windows_path(path)
        );
        Ok(hits)
    }

    /// Addresses of all indexed units for one path. Does not load stored bodies.
    pub fn unit_addrs_for_path(&self, path: &str) -> Result<HashSet<DocAddress>, String> {
        let path = path.trim();
        if path.is_empty() {
            return Ok(HashSet::new());
        }
        let addrs = self.unit_addrs_for_path_term(path)?;
        if !addrs.is_empty() {
            return Ok(addrs);
        }
        let simplified = pathutil::simplify_windows_path(path);
        if !simplified.eq_ignore_ascii_case(path) {
            return self.unit_addrs_for_path_term(&simplified);
        }
        Ok(addrs)
    }

    fn unit_addrs_for_path_term(&self, path: &str) -> Result<HashSet<DocAddress>, String> {
        let searcher = self.reader.searcher();
        let term = Term::from_field_text(self.fields.path, path);
        let query = TermQuery::new(term, IndexRecordOption::Basic);
        searcher
            .search(&query, &DocSetCollector)
            .map_err(|e| e.to_string())
    }

    /// Load stored hits for already-collected addresses.
    pub fn hits_from_addrs(
        &self,
        addrs: impl IntoIterator<Item = DocAddress>,
    ) -> Result<Vec<SearchHit>, String> {
        let searcher = self.reader.searcher();
        let mut hits = Vec::new();
        for addr in addrs {
            let doc: TantivyDocument = searcher.doc(addr).map_err(|e| e.to_string())?;
            if let Some(hit) = self.hit_from_doc(0.0, &doc, "", &[]) {
                hits.push(hit);
            }
        }
        Ok(hits)
    }

    /// All indexed units for one file, document order. No query scoring.
    pub fn units_for_path(&self, path: &str, limit: usize) -> Result<Vec<SearchHit>, String> {
        let path = path.trim();
        if path.is_empty() {
            return Ok(vec![]);
        }
        let limit = limit.max(1);
        let addrs = self.unit_addrs_for_path(path)?;
        let mut hits = self.hits_from_addrs(addrs)?;
        sort_units_document_order(&mut hits);
        Ok(hits.into_iter().take(limit).collect())
    }

    /// Units whose `chunk_id` is in `chunk_ids` (path ∧ chunk_id OR). Loads those bodies only.
    pub fn units_for_path_chunk_ids(
        &self,
        path: &str,
        chunk_ids: &[u32],
    ) -> Result<Vec<SearchHit>, String> {
        let path = path.trim();
        if path.is_empty() || chunk_ids.is_empty() {
            return Ok(vec![]);
        }
        let mut hits = self.units_for_path_chunk_ids_term(path, chunk_ids)?;
        if hits.is_empty() {
            let simplified = pathutil::simplify_windows_path(path);
            if !simplified.eq_ignore_ascii_case(path) {
                hits = self.units_for_path_chunk_ids_term(&simplified, chunk_ids)?;
            }
        }
        sort_units_document_order(&mut hits);
        Ok(hits)
    }

    fn units_for_path_chunk_ids_term(
        &self,
        path: &str,
        chunk_ids: &[u32],
    ) -> Result<Vec<SearchHit>, String> {
        let mut ids: Vec<u32> = chunk_ids.to_vec();
        ids.sort_unstable();
        ids.dedup();
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let path_q = TermQuery::new(
            Term::from_field_text(self.fields.path, path),
            IndexRecordOption::Basic,
        );
        let shoulds: Vec<(Occur, Box<dyn Query>)> = ids
            .iter()
            .map(|id| {
                let term = Term::from_field_text(self.fields.chunk_id, &id.to_string());
                (
                    Occur::Should,
                    Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as Box<dyn Query>,
                )
            })
            .collect();
        let chunks_q = BooleanQuery::new(shoulds);
        let query = BooleanQuery::new(vec![
            (Occur::Must, Box::new(path_q) as Box<dyn Query>),
            (Occur::Must, Box::new(chunks_q) as Box<dyn Query>),
        ]);
        let searcher = self.reader.searcher();
        let addrs = searcher
            .search(&query, &DocSetCollector)
            .map_err(|e| e.to_string())?;
        self.hits_from_addrs(addrs)
    }

    /// Same as [`SearchBackend::search_opts`] with an optional path allowlist.
    pub fn search_filtered(
        &self,
        query: &str,
        limit: usize,
        path_prefix: Option<&str>,
        exts: Option<&[String]>,
        pos_filter_enabled: bool,
        opts: SearchOpts,
        path_allowlist: Option<&[String]>,
    ) -> Result<Vec<SearchHit>, String> {
        self.search_filtered_share(
            query,
            limit,
            path_prefix,
            exts,
            pos_filter_enabled,
            opts,
            path_allowlist,
            None,
        )
    }

    /// LAN remote search: same retrieval as [`Self::search_filtered`], plus share gating.
    pub fn search_for_remote(
        &self,
        query: &str,
        limit: usize,
        path_prefix: Option<&str>,
        exts: Option<&[String]>,
        pos_filter_enabled: bool,
        share: &RemoteShareSnapshot,
    ) -> Result<Vec<SearchHit>, String> {
        if !share.has_shared_folders() {
            return Ok(Vec::new());
        }
        self.search_filtered_share(
            query,
            limit,
            path_prefix,
            exts,
            pos_filter_enabled,
            SearchOpts::default(),
            None,
            Some(share),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn search_filtered_share(
        &self,
        query: &str,
        limit: usize,
        path_prefix: Option<&str>,
        exts: Option<&[String]>,
        pos_filter_enabled: bool,
        opts: SearchOpts,
        path_allowlist: Option<&[String]>,
        share: Option<&RemoteShareSnapshot>,
    ) -> Result<Vec<SearchHit>, String> {
        if let Some(paths) = path_allowlist {
            if paths.is_empty() {
                return Ok(Vec::new());
            }
        }
        let unit_limit = (limit * 8).max(40);
        let scored = self.search_scored(
            query,
            unit_limit,
            path_prefix,
            exts,
            pos_filter_enabled,
            None,
            opts,
            path_allowlist,
            share,
        )?;

        let mut order: Vec<String> = Vec::new();
        let mut groups: std::collections::HashMap<String, Vec<SearchHit>> =
            std::collections::HashMap::new();
        for (_, hit) in scored {
            let path = hit.path.clone();
            if !groups.contains_key(&path) {
                order.push(path.clone());
            }
            groups.entry(path).or_default().push(hit);
        }

        let mut hits: Vec<SearchHit> = Vec::new();
        for path in order {
            let Some(mut units) = groups.remove(&path) else {
                continue;
            };
            if units.is_empty() {
                continue;
            }
            units.sort_by(|a, b| b.score.total_cmp(&a.score));
            let units = dedupe_path_units(units);
            let match_count = units.len() as u32;
            let paragraphs: Vec<ParagraphHit> = units
                .iter()
                .take(NESTED_PARAGRAPH_LIMIT)
                .map(|u| ParagraphHit {
                    id: u.id.clone(),
                    label: if u.unit_label.is_empty() {
                        u.snippet.chars().take(36).collect()
                    } else {
                        u.unit_label.clone()
                    },
                    snippet: u.snippet.clone(),
                    score: u.score,
                    page: u.page,
                })
                .collect();
            let take = opts.per_file_units.unwrap_or(1).max(1);
            for mut unit in units.into_iter().take(take) {
                unit.match_count = match_count;
                unit.paragraphs = paragraphs.clone();
                hits.push(unit);
                if hits.len() >= limit {
                    break;
                }
            }
            if hits.len() >= limit {
                break;
            }
        }
        if opts.per_file_units.is_some() {
            hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        }
        eprintln!(
            "argos: final_hits={} sample_terms={:?}",
            hits.len(),
            hits.first().map(|h| &h.highlight_terms)
        );
        Ok(super::filter_hits_by_exts(hits, exts))
    }

    /// Load stored units for known paths without a text query (mail listing).
    pub fn hits_for_paths(&self, paths: &[String], limit: usize) -> Result<Vec<SearchHit>, String> {
        let mut hits = Vec::new();
        for path in paths {
            if hits.len() >= limit {
                break;
            }
            let per = if self.kind == IndexKind::Mail { 1 } else { 3 };
            let units = self.units_for_path(path, per)?;
            for hit in units {
                hits.push(hit);
                if hits.len() >= limit {
                    break;
                }
            }
        }
        Ok(hits)
    }
}

fn insert_allow_path(set: &mut HashSet<String>, path: &str) {
    let p = path.trim();
    if p.is_empty() {
        return;
    }
    set.insert(p.to_string());
    set.insert(p.to_ascii_lowercase());
    let simp = pathutil::simplify_windows_path(p);
    set.insert(simp.clone());
    set.insert(simp.to_ascii_lowercase());
}

fn path_in_allowlist(path: &str, set: &HashSet<String>) -> bool {
    if set.contains(path) {
        return true;
    }
    let lower = path.to_ascii_lowercase();
    if set.contains(&lower) {
        return true;
    }
    let simp = pathutil::simplify_windows_path(path);
    set.contains(&simp) || set.contains(&simp.to_ascii_lowercase())
}

fn sort_units_document_order(hits: &mut [SearchHit]) {
    hits.sort_by(|a, b| {
        a.chunk_id
            .unwrap_or(0)
            .cmp(&b.chunk_id.unwrap_or(0))
            .then_with(|| a.page.unwrap_or(0).cmp(&b.page.unwrap_or(0)))
    });
}

/// Prefer higher-score units; drop near-duplicates in the same file.
fn dedupe_path_units(mut units: Vec<SearchHit>) -> Vec<SearchHit> {
    units.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<SearchHit> = Vec::new();
    for u in units {
        if kept.iter().any(|k| units_are_near_duplicate(k, &u)) {
            continue;
        }
        kept.push(u);
    }
    kept
}

/// Dedupe then restore document order (preview occurrence navigation).
fn dedupe_path_units_doc_order(units: Vec<SearchHit>) -> Vec<SearchHit> {
    let mut kept = dedupe_path_units(units);
    kept.sort_by(|a, b| {
        a.chunk_id
            .unwrap_or(0)
            .cmp(&b.chunk_id.unwrap_or(0))
            .then_with(|| a.page.unwrap_or(0).cmp(&b.page.unwrap_or(0)))
    });
    kept
}

fn units_are_near_duplicate(a: &SearchHit, b: &SearchHit) -> bool {
    let overlap = body_overlap_ratio(&a.preview_text, &b.preview_text);
    if overlap >= DEDUPE_BODY_OVERLAP {
        return true;
    }
    // Same label + identical snippet (typical UI-visible duplicate from tiny split overlap).
    // Do not merge on label alone — long articles share a parent label across distinct chunks.
    let la = a.unit_label.trim();
    let lb = b.unit_label.trim();
    if !la.is_empty() && la == lb {
        let sa = normalize_unit_text(&a.snippet);
        let sb = normalize_unit_text(&b.snippet);
        if !sa.is_empty() && sa == sb {
            return true;
        }
    }
    false
}

fn normalize_unit_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase()
}

/// Overlap of two unit bodies in \[0, 1\] via containment or character-trigram Dice.
fn body_overlap_ratio(a: &str, b: &str) -> f32 {
    let na = normalize_unit_text(a);
    let nb = normalize_unit_text(b);
    if na.is_empty() || nb.is_empty() {
        return 0.0;
    }
    if na == nb {
        return 1.0;
    }
    let (shorter, longer) = if na.chars().count() <= nb.chars().count() {
        (na.as_str(), nb.as_str())
    } else {
        (nb.as_str(), na.as_str())
    };
    if longer.contains(shorter) {
        let s = shorter.chars().count().max(1) as f32;
        let l = longer.chars().count().max(1) as f32;
        return (s / l).clamp(0.0, 1.0);
    }
    let ta = char_trigrams(&na);
    let tb = char_trigrams(&nb);
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let mut inter = 0usize;
    for t in &ta {
        if tb.contains(t) {
            inter += 1;
        }
    }
    let dice = (2.0 * inter as f32) / (ta.len() + tb.len()) as f32;
    dice.clamp(0.0, 1.0)
}

fn char_trigrams(s: &str) -> HashSet<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut set = HashSet::new();
    if chars.len() < 3 {
        if !chars.is_empty() {
            set.insert(chars.iter().collect());
        }
        return set;
    }
    for i in 0..=chars.len() - 3 {
        set.insert(chars[i..i + 3].iter().collect());
    }
    set
}

impl SearchBackend for TantivyBackend {
    fn search(
        &self,
        query: &str,
        limit: usize,
        path_prefix: Option<&str>,
        exts: Option<&[String]>,
        pos_filter_enabled: bool,
    ) -> Result<Vec<SearchHit>, String> {
        self.search_opts(
            query,
            limit,
            path_prefix,
            exts,
            pos_filter_enabled,
            SearchOpts::default(),
        )
    }

    fn search_opts(
        &self,
        query: &str,
        limit: usize,
        path_prefix: Option<&str>,
        exts: Option<&[String]>,
        pos_filter_enabled: bool,
        opts: SearchOpts,
    ) -> Result<Vec<SearchHit>, String> {
        self.search_filtered(
            query,
            limit,
            path_prefix,
            exts,
            pos_filter_enabled,
            opts,
            None,
        )
    }

    fn preview(&self, hit_id: &str) -> Result<Option<SearchHit>, String> {
        let searcher = self.reader.searcher();
        let term = Term::from_field_text(self.fields.doc_key, hit_id);
        let query = TermQuery::new(term, IndexRecordOption::Basic);
        let top = searcher
            .search(&query, &TopDocs::with_limit(1))
            .map_err(|e| e.to_string())?;
        if let Some((score, addr)) = top.first() {
            let doc: TantivyDocument = searcher.doc(*addr).map_err(|e| e.to_string())?;
            return Ok(self.hit_from_doc(*score, &doc, "", &[]));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_space_and_comma_delimiters() {
        let a = parse_query_syntax("契約 損害賠償 -慰謝料");
        let b = parse_query_syntax("契約,損害賠償,-慰謝料");
        let c = parse_query_syntax("契約、損害賠償、-慰謝料");
        let d = parse_query_syntax("契約　損害賠償　-慰謝料");
        for p in [&a, &b, &c, &d] {
            assert_eq!(p.includes, vec!["契約", "損害賠償"]);
            assert_eq!(p.excludes, vec!["慰謝料"]);
            assert!(p.phrases.is_empty());
        }
    }

    #[test]
    fn parse_phrase_and_exclude_phrase() {
        let p = parse_query_syntax(r#"契約 "損害賠償" -"慰謝料請求""#);
        assert_eq!(p.includes, vec!["契約"]);
        assert_eq!(p.phrases, vec!["損害賠償"]);
        assert_eq!(p.exclude_phrases, vec!["慰謝料請求"]);
        assert!(p.excludes.is_empty());
    }

    #[test]
    fn parse_unclosed_quote_as_plain() {
        let p = parse_query_syntax(r#"契約 "損害賠償"#);
        assert_eq!(p.includes, vec!["契約", "\"損害賠償"]);
        assert!(p.phrases.is_empty());
    }

    #[test]
    fn parse_hyphen_without_separator_is_not_exclude() {
        let p = parse_query_syntax("契約-慰謝料");
        assert_eq!(p.includes, vec!["契約-慰謝料"]);
        assert!(p.excludes.is_empty());
    }

    /// Documents how `Mode::Decompose` splits domain compounds, which is why a query unit
    /// has to become an adjacency phrase rather than an OR of its pieces.
    #[test]
    fn compound_queries_become_adjacency_units() {
        let dir = std::env::temp_dir().join(format!(
            "argos-tantivy-units-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let backend = TantivyBackend::open(&dir).expect("open index").backend;
        for probe in ["裁判例", "損害賠償", "業務委託契約", "第555条"] {
            eprintln!("{probe} -> {:?}", backend.tokenize_ja(probe).unwrap());
        }

        let parsed = parse_query_syntax("この件に関する裁判例を調べて");
        let units = backend.search_units_for(&parsed, true, true).unwrap();
        eprintln!("units={units:?}");
        assert!(
            units.iter().any(|u| u.concat() == "裁判例"),
            "裁判例 must survive as one unit: {units:?}"
        );
        assert!(
            !units.iter().any(|u| u.concat() == "件"),
            "question boilerplate must be dropped in precision mode: {units:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A citation is enforced by `legal_cite_group`, so it must not also become a free
    /// unit — that would double-count it in the minimum-should denominator.
    #[test]
    fn citation_is_not_also_a_free_unit() {
        let dir = std::env::temp_dir().join(format!(
            "argos-tantivy-cite-unit-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let backend = TantivyBackend::open(&dir).expect("open index").backend;
        let parsed = parse_query_syntax("民法第555条の内容を教えて");
        let units = backend.search_units_for(&parsed, true, true).unwrap();
        eprintln!("units={units:?}");
        assert!(units.iter().any(|u| u.concat() == "民法"));
        assert!(
            !units.iter().any(|u| u.concat().contains("555")),
            "citation chars are masked out of free units: {units:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dedupe_collapses_overlapping_same_label_units() {
        let make = |id: &str, label: &str, body: &str, score: f32| SearchHit {
            id: id.into(),
            title: "t".into(),
            snippet: body.chars().take(40).collect(),
            path: r"C:\a.txt".into(),
            page: Some(1),
            chunk_id: Some(0),
            score,
            source: "local".into(),
            preview_text: body.into(),
            highlight_terms: vec![],
            match_count: 1,
            paragraphs: vec![],
            unit_label: label.into(),
            mail_from: String::new(),
            mail_date: String::new(),
            mail_conversation_id: String::new(),
            mail_folder: String::new(),
            doc_kind: String::new(),
        };
        let shared = "損害賠償について定める。甲は乙に対し損害を賠償する義務を負う。".repeat(3);
        let a = make("a#0", "第12条", &shared, 10.0);
        let mut almost = shared.clone();
        almost.push_str("なお特約がある。");
        let b = make("a#1", "第12条", &almost, 8.0);
        let other = make(
            "a#2",
            "第13条",
            "秘密保持義務について別に定める。開示禁止と例外を列挙する。",
            9.0,
        );
        let kept = dedupe_path_units(vec![a, b, other]);
        assert_eq!(
            kept.len(),
            2,
            "high body overlap merges; different label kept"
        );
        assert!(kept.iter().any(|h| h.id == "a#0"));
        assert!(kept.iter().any(|h| h.id == "a#2"));
        assert!(!kept.iter().any(|h| h.id == "a#1"));
    }

    #[test]
    fn dedupe_keeps_same_label_distinct_bodies() {
        let make = |id: &str, body: &str, score: f32| SearchHit {
            id: id.into(),
            title: "t".into(),
            snippet: body.chars().take(20).collect(),
            path: r"C:\a.txt".into(),
            page: Some(1),
            chunk_id: Some(0),
            score,
            source: "local".into(),
            preview_text: body.into(),
            highlight_terms: vec![],
            match_count: 1,
            paragraphs: vec![],
            unit_label: "第12条（損害賠償）".into(),
            mail_from: String::new(),
            mail_date: String::new(),
            mail_conversation_id: String::new(),
            mail_folder: String::new(),
            doc_kind: String::new(),
        };
        let a = make(
            "a#0",
            "第12条（損害賠償）甲は故意または過失により損害を賠償する。前段の詳細。",
            10.0,
        );
        let b = make(
            "a#1",
            "第12条（損害賠償）乙は前項の請求について異議を述べることができる。後段。",
            9.0,
        );
        let kept = dedupe_path_units(vec![a, b]);
        assert_eq!(
            kept.len(),
            2,
            "shared parent label must not merge distinct chunks"
        );
    }

    #[test]
    fn body_overlap_detects_near_copies() {
        let a = "あいうえおかきくけこさしすせそたちつてとなにぬねの";
        let b = "あいうえおかきくけこさしすせそたちつてとなにぬねの。追加。";
        assert!(body_overlap_ratio(a, b) >= DEDUPE_BODY_OVERLAP);
        let c = "まったく別の文章で契約の解除と損害賠償について述べる。";
        assert!(body_overlap_ratio(a, c) < DEDUPE_BODY_OVERLAP);
    }

    #[test]
    fn loose_fallback_keeps_content_substring_hits() {
        let dir = std::env::temp_dir().join(format!(
            "argos-tantivy-loose-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let backend = TantivyBackend::open(&dir).expect("open index").backend;

        // Junk doc: common debris tokens only (no content nouns from the query).
        let junk = "そうしたことはない。いてもよい。";
        let junk_doc = crate::extractor::ExtractedDoc {
            title: "junk".into(),
            pages: vec![junk.into()],
        };
        let junk_file = dir.join("junk.txt");
        std::fs::write(&junk_file, junk).unwrap();
        backend
            .index_file(
                &junk_file,
                junk_file.to_str().unwrap(),
                dir.to_str().unwrap(),
                1,
                1,
                &junk_doc,
            )
            .expect("index junk");

        // Good doc: full sentence with 光景 / 見慣れ.
        let good = "そうした光景を見慣れています";
        let good_doc = crate::extractor::ExtractedDoc {
            title: "good".into(),
            pages: vec![good.into()],
        };
        let good_file = dir.join("good.txt");
        std::fs::write(&good_file, good).unwrap();
        backend
            .index_file(
                &good_file,
                good_file.to_str().unwrap(),
                dir.to_str().unwrap(),
                1,
                1,
                &good_doc,
            )
            .expect("index good");

        let hits = backend.search(good, 10, None, None, true).expect("search");
        assert!(!hits.is_empty(), "expected the good doc to match");
        assert!(
            hits[0].path.contains("good"),
            "junk-only debris must not outrank content doc: {:?}",
            hits[0].path
        );
        assert!(
            hits[0].highlight_terms.iter().any(|t| t == "光景"),
            "chips must prefer content noun: {:?}",
            hits[0].highlight_terms
        );
        assert!(
            !hits[0]
                .highlight_terms
                .iter()
                .any(|t| t == "そうした" || t == "い"),
            "chips must not be debris: {:?}",
            hits[0].highlight_terms
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sentence_query_hits_doc_with_only_noun() {
        let dir = std::env::temp_dir().join(format!(
            "argos-tantivy-noun-only-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let backend = TantivyBackend::open(&dir).expect("open index").backend;
        let body = "この光景は印象的だった。";
        let extracted = crate::extractor::ExtractedDoc {
            title: "only-noun".into(),
            pages: vec![body.into()],
        };
        let file = dir.join("doc.txt");
        std::fs::write(&file, body).unwrap();
        backend
            .index_file(
                &file,
                file.to_str().unwrap(),
                dir.to_str().unwrap(),
                1,
                1,
                &extracted,
            )
            .expect("index");

        let hits = backend
            .search("そうした光景を見慣れています", 10, None, None, true)
            .expect("search");
        assert!(
            !hits.is_empty(),
            "doc containing only 光景 should still match sentence query"
        );
        assert!(hits[0].highlight_terms.iter().any(|t| t == "光景"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sentence_query_hits_indexed_nouns() {
        let dir = std::env::temp_dir().join(format!(
            "argos-tantivy-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let backend = TantivyBackend::open(&dir).expect("open index").backend;
        let sentence = "そうした光景を見慣れています";
        let indexed = backend.tokenize_ja(sentence).expect("tokenize");
        eprintln!("indexed tokens={indexed:?}");
        let content = backend.content_tokens(sentence, true).expect("content");
        eprintln!("content tokens={content:?}");

        let body = format!("前文。{sentence}。後文。");
        let extracted = crate::extractor::ExtractedDoc {
            title: "scene".into(),
            pages: vec![body.clone()],
        };
        let file = dir.join("doc.txt");
        std::fs::write(&file, &body).unwrap();
        backend
            .index_file(
                &file,
                file.to_str().unwrap(),
                dir.to_str().unwrap(),
                1,
                1,
                &extracted,
            )
            .expect("index");

        // Direct term probe
        for tok in &content {
            let q = BooleanQuery::new(backend.term_in_title_or_body(tok));
            let searcher = backend.reader.searcher();
            let top = searcher
                .search(&q, &TopDocs::with_limit(5))
                .expect("search term");
            eprintln!("term {tok:?} hits={}", top.len());
        }

        let hits = backend
            .search(sentence, 10, None, None, true)
            .expect("search sentence");
        eprintln!(
            "sentence hits={} terms={:?}",
            hits.len(),
            hits.first().map(|h| &h.highlight_terms)
        );
        assert!(
            !hits.is_empty(),
            "expected hits for {sentence}; tokens={content:?}"
        );
        let terms = &hits[0].highlight_terms;
        assert!(
            terms.iter().any(|t| t == "光景"),
            "expected 光景 in highlight_terms: {terms:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn matches_for_path_returns_multiple_chunks() {
        let dir = std::env::temp_dir().join(format!(
            "argos-tantivy-path-matches-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let backend = TantivyBackend::open(&dir).expect("open index").backend;

        // Distinct articles so they stay separate units (not one oversized split).
        let filler_a: String = (0..80)
            .map(|i| char::from_u32(0x3042 + (i % 40) as u32).unwrap())
            .collect();
        let filler_b: String = (0..80)
            .map(|i| char::from_u32(0x30a2 + (i % 40) as u32).unwrap())
            .collect();
        let body = format!(
            "第1条 損害賠償について。{filler_a}\n\n第2条 次に損害賠償を述べる。{filler_b}"
        );
        let path = dir.join("multi.txt");
        let path_str = path.to_str().unwrap().to_string();
        std::fs::write(&path, &body).unwrap();
        backend
            .index_file(
                &path,
                &path_str,
                dir.to_str().unwrap(),
                1,
                body.len() as u64,
                &crate::extractor::ExtractedDoc {
                    title: "multi".into(),
                    pages: vec![body],
                },
            )
            .expect("index");

        let list = backend
            .search("損害賠償", 10, None, None, false)
            .expect("search");
        assert_eq!(list.len(), 1, "list stays one hit per file");

        let matches = backend
            .matches_for_path("損害賠償", &path_str, 50, false)
            .expect("path matches");
        assert!(
            matches.len() >= 2,
            "expected multiple units, got {}",
            matches.len()
        );
        assert!(matches.iter().all(|h| h.path == path_str));
        let chunk_ids: Vec<_> = matches.iter().map(|h| h.chunk_id).collect();
        let mut sorted = chunk_ids.clone();
        sorted.sort();
        assert_eq!(chunk_ids, sorted, "units ordered by chunk_id");

        assert!(
            list[0].match_count >= 2,
            "file hit should report multiple paragraph matches"
        );
        assert!(
            !list[0].paragraphs.is_empty(),
            "file hit should nest paragraph snippets"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn units_for_path_is_document_order_and_keeps_late_units() {
        let dir = std::env::temp_dir().join(format!(
            "argos-tantivy-units-path-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let backend = TantivyBackend::open(&dir).expect("open index").backend;

        let mut body = String::new();
        for i in 0..50 {
            body.push_str(&format!(
                "第{i}条 これはプレビュー用のダミー本文を十分長くした段落です。さらに文字数を稼ぐための追記。番号{i:04}。\n\n"
            ));
        }
        body.push_str(
            "第99条 弁済による代位の要件をここに置く。これは末尾ユニットとして独立させる。\n\n",
        );
        let path = dir.join("civil.md");
        let path_str = path.to_str().unwrap().to_string();
        std::fs::write(&path, &body).unwrap();
        backend
            .index_file(
                &path,
                &path_str,
                dir.to_str().unwrap(),
                1,
                body.len() as u64,
                &crate::extractor::ExtractedDoc {
                    title: "civil".into(),
                    pages: vec![body],
                },
            )
            .expect("index");

        let head = backend.units_for_path(&path_str, 10).expect("head");
        assert_eq!(head.len(), 10);
        let head_chunks: Vec<u32> = head.iter().map(|h| h.chunk_id.unwrap_or(0)).collect();
        assert_eq!(head_chunks, (0..10).collect::<Vec<u32>>());
        assert!(
            !head.iter().any(|h| h.preview_text.contains("弁済による代位")),
            "small window is the start of the file, not score-top docs"
        );

        let all = backend.units_for_path(&path_str, 200).expect("all");
        assert!(
            all.iter().any(|h| h.preview_text.contains("弁済による代位")),
            "late unit must be present once the window covers the file"
        );
        let chunks: Vec<u32> = all.iter().map(|h| h.chunk_id.unwrap_or(0)).collect();
        let mut sorted = chunks.clone();
        sorted.sort();
        assert_eq!(chunks, sorted, "units ordered by chunk_id");

        let _ = std::fs::remove_dir_all(&dir);
    }

    struct TestIndex {
        dir: std::path::PathBuf,
        backend: TantivyBackend,
    }

    impl TestIndex {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "argos-tantivy-{tag}-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let backend = TantivyBackend::open(&dir).expect("open index").backend;
            Self { dir, backend }
        }

        fn add(&self, name: &str, body: &str) {
            let path = self.dir.join(format!("{name}.txt"));
            std::fs::write(&path, body).unwrap();
            self.backend
                .index_file(
                    &path,
                    path.to_str().unwrap(),
                    self.dir.to_str().unwrap(),
                    1,
                    body.len() as u64,
                    &crate::extractor::ExtractedDoc {
                        title: name.into(),
                        pages: vec![body.into()],
                    },
                )
                .expect("index");
        }
    }

    impl Drop for TestIndex {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn arabic_query_finds_place_value_kanji_article() {
        let idx = TestIndex::new("cite-place");
        idx.add(
            "civil",
            "第五百五十五条　売買は、当事者の一方がある財産権を相手方に移転することを約し、\
             相手方がこれに対してその代金を支払うことを約することによって、その効力を生ずる。",
        );

        let hits = idx
            .backend
            .search("民法第555条の条文を示して", 10, None, None, true)
            .expect("search");
        assert!(
            !hits.is_empty(),
            "第555条 must reach 第五百五十五条 without reindexing"
        );
        assert!(
            hits[0].preview_text.contains("第五百五十五条"),
            "unexpected top hit: {:?}",
            hits[0].preview_text
        );
    }

    #[test]
    fn arabic_query_finds_digit_run_kanji_article() {
        let idx = TestIndex::new("cite-digits");
        idx.add(
            "hanrei",
            "本件について、第五〇九条の適用が問題となる。原審の判断は是認できない。",
        );

        let hits = idx
            .backend
            .search("第509条", 10, None, None, true)
            .expect("search");
        assert!(
            !hits.is_empty(),
            "zero-padded digit runs in old case law must match"
        );
        assert!(hits[0].preview_text.contains("第五〇九条"));
    }

    #[test]
    fn kanji_query_still_finds_kanji_article() {
        // Normalizing the proximity haystack must not break the direct spelling.
        let idx = TestIndex::new("cite-kanji-query");
        idx.add("civil", "第五百五十五条　売買は当事者の一方が財産権を移転する。");

        let hits = idx
            .backend
            .search("第五百五十五条", 10, None, None, true)
            .expect("search");
        assert!(!hits.is_empty(), "kanji query must keep matching kanji text");
    }

    #[test]
    fn cite_highlight_uses_the_spelling_in_the_document() {
        let idx = TestIndex::new("cite-highlight");
        idx.add(
            "civil",
            "総則の説明が続く。ここは前置きである。第五百五十五条　売買は当事者の一方が財産権を移転する。",
        );

        let hits = idx
            .backend
            .search("第555条", 10, None, None, true)
            .expect("search");
        assert!(!hits.is_empty());
        let terms = &hits[0].highlight_terms;
        assert!(
            terms.iter().any(|t| t == "第五百五十五条"),
            "chip must be the spelling the document uses: {terms:?}"
        );
        assert!(
            !terms.iter().any(|t| t == "第555条"),
            "absent spellings must be dropped: {terms:?}"
        );
        assert!(
            hits[0].snippet.contains("第五百五十五条"),
            "snippet should anchor on the article: {:?}",
            hits[0].snippet
        );
    }

    #[test]
    fn cite_must_falls_back_when_no_document_has_the_article() {
        let idx = TestIndex::new("cite-fallback");
        idx.add(
            "civil",
            "売買契約の解除について、民法の規定と判例の立場を整理する。",
        );

        let hits = idx
            .backend
            .search("民法第999条", 10, None, None, true)
            .expect("search");
        assert!(
            !hits.is_empty(),
            "a missing article must relax to free-word search, not return nothing"
        );
    }

    #[test]
    fn dates_are_not_expanded_as_articles() {
        let idx = TestIndex::new("cite-date");
        idx.add("memo", "打合せは2026年5月15日に実施した。議事録は別紙のとおり。");
        idx.add("other", "第十五条　この契約は当事者の合意により変更できる。");

        let hits = idx
            .backend
            .search("2026年5月15日", 10, None, None, true)
            .expect("search");
        assert!(!hits.is_empty());
        assert!(
            hits[0].path.contains("memo"),
            "a date must not be read as 第15条: {:?}",
            hits[0].path
        );
    }

    fn llm_opts() -> SearchOpts {
        SearchOpts::for_llm(3)
    }

    /// A compound must not decay into an OR of its pieces: 裁判例 is indexed as adjacent
    /// 裁判 / 例, so a document that only says 例 has to stay out.
    #[test]
    fn compound_query_excludes_partial_token_noise() {
        let idx = TestIndex::new("compound-noise");
        idx.add(
            "guide",
            "記載例を示す。例として次の書式を用いる。作成例は別紙のとおりであり、例外も列挙する。",
        );
        idx.add(
            "hanrei",
            "同種の裁判例では、契約の解除が認められている。裁判例の傾向を整理する。",
        );

        let hits = idx
            .backend
            .search_opts("この件に関する裁判例を調べて", 10, None, None, true, llm_opts())
            .expect("search");
        assert!(!hits.is_empty(), "裁判例 must match the case-law document");
        assert!(
            hits.iter().all(|h| !h.path.contains("guide")),
            "a document matching only 例 must not be returned: {:?}",
            hits.iter().map(|h| &h.path).collect::<Vec<_>>()
        );
    }

    /// The score must be monotone in the number of matched units. The old additive form
    /// let a one-word hit with perfect compactness beat a three-word hit.
    #[test]
    fn more_matched_units_score_higher() {
        let idx = TestIndex::new("score-monotonic");
        idx.add(
            "one",
            "解雇に関する社内規程の運用について、担当部門の記録を残す。",
        );
        idx.add(
            "three",
            "解雇の有効性が争われた裁判例では、就業規則の周知が重視された。",
        );

        let hits = idx
            .backend
            .search_opts("解雇 有効性 裁判例", 10, None, None, true, llm_opts())
            .expect("search");
        assert!(!hits.is_empty());
        assert!(
            hits[0].path.contains("three"),
            "the hit matching all three units must rank first: {:?}",
            hits.iter().map(|h| (&h.path, h.score)).collect::<Vec<_>>()
        );
    }

    /// A statute file is one file with many articles. Collapsing it to a single unit
    /// would hide every article but the best-scoring one.
    #[test]
    fn per_file_units_returns_several_paragraphs_of_one_file() {
        let idx = TestIndex::new("per-file-units");
        // Each article must clear `UNIT_MIN_CHARS` or the blocks merge into one unit, and
        // the wording must differ or `dedupe_path_units` collapses them as near-copies.
        let articles = [
            "第1条　催告による解除について定める。債権者が相当の期間を定めて履行を請求し、\
             その期間内に履行がないときは、解除の意思表示をすることができる。",
            "第2条　無催告解除が認められる場合を列挙する。履行が不能となったとき、\
             債務者が明確に拒絶したとき、定期行為の時期を過ぎたときは直ちに解除できる。",
            "第3条　解除の効果として原状回復の義務を負う。すでに給付を受けた当事者は\
             相手方を元の状態に復させ、金銭には受領の時からの利息を付する。",
            "第4条　解除権の行使期間と消滅を定める。権利者が長期間これを行使しないとき、\
             相手方は期間を定めて確答を求めることができ、返答がなければ解除権は失われる。",
            "第5条　当事者が複数あるときの解除の取扱いを明らかにする。権利は全員から\
             全員に対してのみ行使でき、一人について消滅したときは全員について消滅する。",
            "第6条　解除と損害賠償との関係を確認的に述べる。契約を終了させたことは、\
             債務の不履行によって生じた損害の賠償を請求することを妨げるものではない。",
        ];
        let mut body = String::new();
        for a in articles {
            body.push_str(a);
            body.push_str("\n\n");
        }
        idx.add("civil", &body);

        let one = idx
            .backend
            .search("解除", 10, None, None, true)
            .expect("search");
        assert_eq!(one.len(), 1, "default retrieval aggregates to one unit/file");
        assert!(one[0].match_count > 1, "match_count reports the rest");

        let many = idx
            .backend
            .search_opts("解除", 10, None, None, true, llm_opts())
            .expect("search opts");
        assert!(
            many.len() > 1,
            "LLM retrieval must return several paragraphs of the same file: {}",
            many.len()
        );
        assert!(
            many.len() <= 3,
            "but no more than the per-file cap: {}",
            many.len()
        );
        let ids: HashSet<&str> = many.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids.len(), many.len(), "returned units must be distinct");
    }

    /// `unit_label` is the paragraph heading. A hit whose heading is the thing asked
    /// about beats one that merely mentions it in passing — no reindex involved.
    #[test]
    fn heading_match_outranks_passing_mention() {
        let idx = TestIndex::new("label-bonus");
        idx.add(
            "commentary",
            "解説として、第五百五十五条の趣旨を他の条文と比較しつつ長めに論じる。\
             ここでは制度の沿革と学説の対立、実務上の運用まで幅広く触れておく。",
        );
        idx.add(
            "civil",
            "第五百五十五条　売買は、当事者の一方がある財産権を相手方に移転することを約し、\
             相手方がこれに対してその代金を支払うことを約することによって、その効力を生ずる。",
        );

        let hits = idx
            .backend
            .search_opts("民法 第555条", 10, None, None, true, llm_opts())
            .expect("search");
        assert!(!hits.is_empty());
        assert!(
            hits[0].path.contains("civil"),
            "the article itself must outrank commentary about it: {:?}",
            hits.iter().map(|h| (&h.path, h.score)).collect::<Vec<_>>()
        );
    }

    /// `minimum_number_should_match` yields an EmptyScorer when the minimum is positive
    /// but no Should clause exists. A quoted-only query is all Must, so it must clamp to 0.
    #[test]
    fn quoted_only_query_still_matches() {
        let idx = TestIndex::new("quoted-only");
        idx.add("contract", "本売買契約は、甲乙間の合意により成立する。");

        let hits = idx
            .backend
            .search_opts("\"売買契約\"", 10, None, None, true, llm_opts())
            .expect("search");
        assert!(
            !hits.is_empty(),
            "a phrase-only query must not be clamped into an empty scorer"
        );
    }

    /// Precision mode must not regress the popup, whose queries are whole selected
    /// sentences and where one matching noun is a useful hit.
    #[test]
    fn recall_mode_keeps_single_unit_hits() {
        let idx = TestIndex::new("recall-mode");
        idx.add("scene", "この光景は印象的だった。");

        let hits = idx
            .backend
            .search("そうした光景を見慣れています", 10, None, None, true)
            .expect("search");
        assert!(
            !hits.is_empty(),
            "the popup path must keep matching on a single noun"
        );
    }

    /// Typing words separated by a space is a deliberate list, so the popup requires most
    /// of them. A selected sentence has no separators and must stay loose.
    #[test]
    fn spaced_words_are_and_but_a_sentence_stays_or() {
        let idx = TestIndex::new("spaced-and");
        idx.add(
            "partial",
            "解雇に関する社内規程の運用について、担当部門が保管する記録の一覧を示す。",
        );
        idx.add(
            "both",
            "解雇の有効性が争われた事案について、就業規則の周知の程度が検討された。",
        );

        let spaced = idx
            .backend
            .search("解雇 有効性", 10, None, None, true)
            .expect("spaced");
        assert!(!spaced.is_empty());
        assert!(
            spaced.iter().all(|h| !h.path.contains("partial")),
            "a spaced query must require both words: {:?}",
            spaced.iter().map(|h| &h.path).collect::<Vec<_>>()
        );

        // No separator: one include, so the historical single-unit recall applies.
        let sentence = idx
            .backend
            .search("解雇の有効性について", 10, None, None, true)
            .expect("sentence");
        assert!(
            sentence.iter().any(|h| h.path.contains("partial")),
            "a pasted sentence must keep matching on one noun: {:?}",
            sentence.iter().map(|h| &h.path).collect::<Vec<_>>()
        );
    }

    /// The strictest rung may retrieve documents that the `min_overlap` post-filter then
    /// rejects. The ladder has to relax on the *kept* count, or the query returns nothing
    /// where the old single-word rule returned a hit.
    #[test]
    fn ladder_relaxes_when_the_post_filter_rejects_everything() {
        let idx = TestIndex::new("ladder-relax");
        // The index lowercases ASCII, so retrieval matches `PDF`; the proximity filter
        // compares characters exactly and does not.
        idx.add(
            "spec",
            "契約書の提出方法について、pdf 形式での提出を求める旨をここに定める。",
        );

        let hits = idx
            .backend
            .search("契約書 PDF", 10, None, None, true)
            .expect("search");
        assert!(
            !hits.is_empty(),
            "a term that only the index can match must not empty the result set"
        );
    }

    /// Excludes and quoted phrases must survive the restructured ladder.
    #[test]
    fn spaced_query_still_honours_exclusions() {
        let idx = TestIndex::new("spaced-exclude");
        idx.add(
            "damages",
            "解雇の有効性を争う事案において、慰謝料の請求も併せて行われた。",
        );
        idx.add(
            "rules",
            "解雇の有効性について、就業規則の周知の程度が問題となった事案である。",
        );

        let hits = idx
            .backend
            .search("解雇 有効性 -慰謝料", 10, None, None, true)
            .expect("search");
        assert!(!hits.is_empty());
        assert!(
            hits.iter().all(|h| !h.path.contains("damages")),
            "excluded term must still remove the document: {:?}",
            hits.iter().map(|h| &h.path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn path_allowlist_keeps_low_bm25_in_range_and_drops_out_of_range() {
        let idx = TestIndex::new("date-allow");
        let hot = format!(
            "{} これは高頻度で契約という語が並ぶ文書です。",
            "契約 ".repeat(40)
        );
        let cold =
            "契約について一度だけ触れる。期間内の古いメモであり、関連語は多くない。".to_string();
        idx.add("hot", &hot);
        idx.add("cold", &cold);
        let hot_path = idx.dir.join("hot.txt").to_str().unwrap().to_string();
        let cold_path = idx.dir.join("cold.txt").to_str().unwrap().to_string();

        let unfiltered = idx
            .backend
            .search("契約", 10, None, None, false)
            .expect("search");
        assert!(
            unfiltered.iter().any(|h| h.path == hot_path),
            "hot doc should rank without allowlist: {:?}",
            unfiltered.iter().map(|h| &h.path).collect::<Vec<_>>()
        );

        let filtered = idx
            .backend
            .search_filtered(
                "契約",
                10,
                None,
                None,
                false,
                super::SearchOpts::default(),
                Some(&[cold_path.clone()]),
            )
            .expect("filtered");
        assert_eq!(filtered.len(), 1, "{:?}", filtered.iter().map(|h| &h.path).collect::<Vec<_>>());
        assert_eq!(filtered[0].path, cold_path);
        assert!(!filtered.iter().any(|h| h.path == hot_path));
    }

    #[test]
    fn empty_path_allowlist_returns_no_hits() {
        let idx = TestIndex::new("empty-allow");
        idx.add("doc", "契約についての短いメモです。追加の本文を足して長さを確保する。");
        let hits = idx
            .backend
            .search_filtered(
                "契約",
                10,
                None,
                None,
                false,
                super::SearchOpts::default(),
                Some(&[]),
            )
            .expect("filtered");
        assert!(hits.is_empty());
    }

    #[test]
    fn allowlist_matches_simplified_and_lowercased_paths() {
        let mut set = std::collections::HashSet::new();
        insert_allow_path(&mut set, r"C:\Docs\A.txt");
        assert!(path_in_allowlist(r"C:\Docs\A.txt", &set));
        assert!(path_in_allowlist(r"c:\docs\a.txt", &set));
    }
}
