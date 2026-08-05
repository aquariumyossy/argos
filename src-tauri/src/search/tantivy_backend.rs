use std::collections::HashSet;
use std::path::Path;

use lindera::dictionary::load_dictionary;
use lindera::mode::{Mode, Penalty};
use lindera::segmenter::Segmenter;
use lindera_tantivy::tokenizer::LinderaTokenizer;
use parking_lot::Mutex;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, PhraseQuery, Query, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, STORED, STRING,
};
use tantivy::tokenizer::TokenStream;
use tantivy::{doc, Index, IndexReader, IndexWriter, ReloadPolicy, Term, TantivyDocument};

use crate::pathutil;

use super::morph::{is_noise_highlight_term, MorphAnalyzer};
use super::{ParagraphHit, SearchBackend, SearchHit};

/// Bump when Tantivy on-disk schema changes. Triggers wipe + full reindex.
pub const INDEX_SCHEMA_VERSION: u32 = 3;
const SCHEMA_VERSION_FILE: &str = "argos_schema_version";

/// Nested paragraphs shown under each file hit in the popup list.
const NESTED_PARAGRAPH_LIMIT: usize = 3;
/// Collapse near-duplicate units in the same file (shared label / body overlap).
const DEDUPE_BODY_OVERLAP: f32 = 0.45;

pub struct OpenIndexResult {
    pub backend: TantivyBackend,
    /// True when an older index was wiped; caller should run full reindex.
    pub needs_full_reindex: bool,
}

pub struct TantivyBackend {
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
}

impl TantivyBackend {
    pub fn open(index_dir: &Path) -> Result<OpenIndexResult, String> {
        std::fs::create_dir_all(index_dir).map_err(|e| e.to_string())?;

        let version_path = index_dir.join(SCHEMA_VERSION_FILE);
        let existing_version = std::fs::read_to_string(&version_path)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());
        let has_index = index_dir.join("meta.json").exists();
        let needs_full_reindex = has_index && existing_version != Some(INDEX_SCHEMA_VERSION);

        if needs_full_reindex {
            eprintln!(
                "argos: index schema {:?} -> {}; wiping index for rebuild",
                existing_version, INDEX_SCHEMA_VERSION
            );
            // Remove Tantivy files but keep the directory.
            for entry in std::fs::read_dir(index_dir).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                let path = entry.path();
                if path.file_name().and_then(|n| n.to_str()) == Some(SCHEMA_VERSION_FILE) {
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
        let schema = schema_builder.build();

        let index = if index_dir.join("meta.json").exists() {
            Index::open_in_dir(index_dir).map_err(|e| e.to_string())?
        } else {
            Index::create_in_dir(index_dir, schema.clone()).map_err(|e| e.to_string())?
        };

        std::fs::write(&version_path, INDEX_SCHEMA_VERSION.to_string())
            .map_err(|e| e.to_string())?;

        // Decompose helps split compounds like 損害賠償 -> 損害 + 賠償
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
            let is_noun = morph
                .iter()
                .any(|t| (t.surface == *surface || t.surface.contains(surface.as_str())) && t.major_pos == "名詞");
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

    fn phrase_in_title_or_body(&self, tokens: &[String]) -> Option<Box<dyn Query>> {
        if tokens.is_empty() {
            return None;
        }
        if tokens.len() == 1 {
            return Some(Box::new(BooleanQuery::new(self.term_in_title_or_body(&tokens[0]))));
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

    fn build_parsed_query(
        &self,
        parsed: &ParsedQuery,
        pos_filter: bool,
    ) -> Result<Option<Box<dyn Query>>, String> {
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();

        for raw in &parsed.includes {
            let tokens = self.content_tokens(raw, pos_filter)?;
            for tok in &tokens {
                // Flatten title/body Should into the parent query.
                clauses.extend(self.term_in_title_or_body(tok));
            }
            // Passage match: exact morph token sequence (including particles) finds
            // the selected sentence even when OR-of-content-tokens is too weak alone.
            if raw.chars().count() >= 4 {
                let passage = self.passage_tokens(raw)?;
                if passage.len() >= 2 {
                    if let Some(q) = self.phrase_in_title_or_body(&passage) {
                        clauses.push((Occur::Should, q));
                    }
                }
            }
        }

        for phrase in &parsed.phrases {
            let tokens = self.phrase_tokens(phrase)?;
            if let Some(q) = self.phrase_in_title_or_body(&tokens) {
                // Quoted phrases are required (Google-like).
                clauses.push((Occur::Must, q));
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
        Ok(Some(Box::new(BooleanQuery::new(clauses))))
    }

    fn highlight_terms_for(
        &self,
        parsed: &ParsedQuery,
        pos_filter: bool,
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
        for raw in &parsed.includes {
            let tokens = self.content_tokens(raw, pos_filter)?;
            // Prefer filtered morph tokens for chips. Keep the raw include only when
            // it is itself a single content token (or POS filter is off).
            if !pos_filter {
                push(raw.clone());
            } else if tokens.len() == 1 && tokens[0] == *raw {
                push(raw.clone());
            }
            for tok in tokens {
                push(tok);
            }
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

    fn proximity_tokens_for(
        &self,
        parsed: &ParsedQuery,
        pos_filter: bool,
    ) -> Result<Vec<String>, String> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for raw in &parsed.includes {
            for tok in self.content_tokens(raw, pos_filter)? {
                if seen.insert(tok.clone()) {
                    out.push(tok);
                }
            }
        }
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
        let snippet = make_snippet(&body, query, highlight_terms, 100);
        // Only terms that actually appear in this hit
        let haystack = format!("{title} {body}");
        let mut terms: Vec<String> = Vec::new();
        for t in highlight_terms {
            if !t.is_empty() && haystack.contains(t) {
                terms.push(t.clone());
            }
        }
        terms.sort_by_key(|t| std::cmp::Reverse(t.chars().count()));
        Some(SearchHit {
            id: key,
            title,
            snippet,
            path,
            page,
            chunk_id,
            score,
            source: "local".into(),
            preview_text: body,
            highlight_terms: terms,
            match_count: 1,
            paragraphs: Vec::new(),
            unit_label,
        })
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct ParsedQuery {
    includes: Vec<String>,
    phrases: Vec<String>,
    excludes: Vec<String>,
    exclude_phrases: Vec<String>,
}

/// Short function-word surfaces to skip when guessing snippet anchors.
fn is_stop_token(t: &str) -> bool {
    matches!(
        t,
        "の" | "を" | "に" | "は" | "が" | "も" | "と" | "で" | "へ" | "や" | "か" | "など"
            | "より" | "から" | "まで" | "て" | "た" | "れ" | "せ" | "し" | "さ"
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
fn parse_query_syntax(raw: &str) -> ParsedQuery {
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
    fn search_scored(
        &self,
        query: &str,
        limit: usize,
        path_prefix: Option<&str>,
        pos_filter_enabled: bool,
        exact_path: Option<&str>,
    ) -> Result<Vec<(f32, SearchHit)>, String> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(vec![]);
        }

        let parsed = parse_query_syntax(q);
        if parsed.includes.is_empty() && parsed.phrases.is_empty() {
            return Ok(vec![]);
        }

        let Some(tantivy_q) = self.build_parsed_query(&parsed, pos_filter_enabled)? else {
            return Ok(vec![]);
        };

        let highlight_terms = self.highlight_terms_for(&parsed, pos_filter_enabled)?;
        let proximity_tokens = self.proximity_tokens_for(&parsed, pos_filter_enabled)?;
        let scope = path_prefix
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        // Use path as stored in the index (hit.path), not simplified — STRING TermQuery is exact.
        let exact = exact_path
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        eprintln!(
            "argos: parsed includes={:?} phrases={:?} excludes={:?} exclude_phrases={:?} prox={:?} scope={:?} exact_path={:?} pos_filter={}",
            parsed.includes, parsed.phrases, parsed.excludes, parsed.exclude_phrases, proximity_tokens, scope, exact, pos_filter_enabled
        );
        eprintln!(
            "argos: highlight_terms={:?} content_for_includes={:?}",
            highlight_terms,
            parsed
                .includes
                .iter()
                .map(|r| self.content_tokens(r, pos_filter_enabled))
                .collect::<Result<Vec<_>, _>>()
                .unwrap_or_default()
        );

        let wrap_path = |inner: Box<dyn Query>| -> Box<dyn Query> {
            if let Some(ref p) = exact {
                let path_term = Term::from_field_text(self.fields.path, p);
                let path_q = TermQuery::new(path_term, IndexRecordOption::Basic);
                Box::new(BooleanQuery::new(vec![
                    (Occur::Must, inner),
                    (Occur::Must, Box::new(path_q)),
                ]))
            } else {
                inner
            }
        };

        let searcher = self.reader.searcher();
        // `limit` is the desired scored-unit count. Mild over-fetch absorbs post-filters.
        // (Do not multiply by large factors here — callers already size the unit budget.)
        let fetch_n = if exact.is_some() {
            limit.max(50).min(80)
        } else if scope.is_some() {
            (limit * 5).max(80).min(400)
        } else {
            (limit * 2).max(40).min(200)
        };
        let mut top = searcher
            .search(&*wrap_path(tantivy_q), &TopDocs::with_limit(fetch_n))
            .map_err(|e| e.to_string())?;
        eprintln!("argos: tantivy_raw_hits={}", top.len());

        // POS-filtered TermQuery can miss when the inverted index lacks those exact
        // surfaces (tokenizer drift / partial index) even though the stored body
        // contains the nouns. Fall back to a looser retrieval query, then keep the
        // content proximity filter so chips stay on 光景 / 見慣れ — not そうした / い.
        let mut used_loose_retrieval = false;
        if top.is_empty() && pos_filter_enabled && !proximity_tokens.is_empty() {
            if let Some(loose_q) = self.build_parsed_query(&parsed, false)? {
                top = searcher
                    .search(&*wrap_path(loose_q), &TopDocs::with_limit(fetch_n))
                    .map_err(|e| e.to_string())?;
                used_loose_retrieval = true;
                eprintln!(
                    "argos: pos_filter_raw_empty -> loose_retrieval hits={}",
                    top.len()
                );
            }
        }

        // Phrase-only: require the phrase string; free tokens keep half-overlap rule.
        // With POS filtering, tokens are already contentful — requiring half of them
        // drops useful noun-only hits (e.g. query has 光景+見慣れ but a doc only has 光景).
        // Loose retrieval must still require a content-term substring overlap.
        let min_overlap = if proximity_tokens.is_empty() {
            0
        } else if parsed.includes.is_empty() {
            proximity_tokens.len().min(1)
        } else if pos_filter_enabled || used_loose_retrieval {
            1
        } else {
            ((proximity_tokens.len() + 1) / 2).max(1)
        };

        let mut scored: Vec<(f32, SearchHit)> = Vec::new();
        for (score, addr) in top {
            let doc: TantivyDocument = searcher.doc(addr).map_err(|e| e.to_string())?;
            let Some(mut hit) = self.hit_from_doc(score, &doc, q, &highlight_terms) else {
                continue;
            };
            if let Some(ref prefix) = scope {
                if !pathutil::path_starts_with(&hit.path, prefix) {
                    continue;
                }
            }
            let haystack = format!("{} {}", hit.title, hit.preview_text);

            if parsed
                .exclude_phrases
                .iter()
                .any(|p| haystack_contains_phrase(&haystack, p))
            {
                continue;
            }

            let (overlap, span) = if proximity_tokens.is_empty() {
                (0, 1)
            } else {
                proximity_span(&haystack, &proximity_tokens)
            };
            if min_overlap > 0 && overlap < min_overlap {
                continue;
            }
            let compact = if proximity_tokens.is_empty() {
                1.0
            } else {
                compactness_score(&proximity_tokens, &haystack, span)
            };
            // Prefer many matches that appear close together (contract-friendly).
            let combined = (overlap as f32) * 10.0 * compact + score;
            hit.score = combined;
            scored.push((combined, hit));
        }

        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        Ok(scored)
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
        let mut scored =
            self.search_scored(query, limit, None, pos_filter_enabled, Some(path))?;
        // Fallback if TermQuery missed due to path normalization drift: prefix scope.
        if scored.is_empty() {
            scored = self.search_scored(query, limit, Some(path), pos_filter_enabled, None)?;
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
        pos_filter_enabled: bool,
    ) -> Result<Vec<SearchHit>, String> {
        // Unit budget ≈ pre-paragraph TopDocs size so nesting does not explode fetch_n.
        // search_scored applies only a mild over-fetch on top of this.
        let unit_limit = (limit * 8).max(40);
        let scored =
            self.search_scored(query, unit_limit, path_prefix, pos_filter_enabled, None)?;

        // Group by path; scored is already best-score-first.
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
            // units already score-sorted from global sort; keep that order within path.
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
            let mut best = units.into_iter().next().expect("non-empty");
            best.match_count = match_count;
            best.paragraphs = paragraphs;
            hits.push(best);
            if hits.len() >= limit {
                break;
            }
        }
        eprintln!(
            "argos: final_hits={} sample_terms={:?}",
            hits.len(),
            hits.first().map(|h| &h.highlight_terms)
        );
        Ok(hits)
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
        assert_eq!(kept.len(), 2, "high body overlap merges; different label kept");
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
        assert_eq!(kept.len(), 2, "shared parent label must not merge distinct chunks");
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

        let hits = backend
            .search(good, 10, None, true)
            .expect("search");
        assert!(
            !hits.is_empty(),
            "expected the good doc to match"
        );
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
            !hits[0].highlight_terms.iter().any(|t| t == "そうした" || t == "い"),
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
            .search("そうした光景を見慣れています", 10, None, true)
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
            .index_file(&file, file.to_str().unwrap(), dir.to_str().unwrap(), 1, 1, &extracted)
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
            .search(sentence, 10, None, true)
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

        // Distinct, varied fillers so long-split units are not near-duplicates.
        let filler_a: String = (0..700)
            .map(|i| char::from_u32(0x3042 + (i % 40) as u32).unwrap())
            .collect();
        let filler_b: String = (0..700)
            .map(|i| char::from_u32(0x30a2 + (i % 40) as u32).unwrap())
            .collect();
        let body = format!(
            "損害賠償について。{filler_a}次に損害賠償を述べる。{filler_b}最後に損害賠償。"
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

        let list = backend.search("損害賠償", 10, None, false).expect("search");
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
}
