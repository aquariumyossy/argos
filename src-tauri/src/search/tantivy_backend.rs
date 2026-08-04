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
use super::{SearchBackend, SearchHit};

const CHUNK_SIZE: usize = 800;
const CHUNK_OVERLAP: usize = 100;

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
}

impl TantivyBackend {
    pub fn open(index_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(index_dir).map_err(|e| e.to_string())?;

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
        let schema = schema_builder.build();

        let index = if index_dir.join("meta.json").exists() {
            Index::open_in_dir(index_dir).map_err(|e| e.to_string())?
        } else {
            Index::create_in_dir(index_dir, schema.clone()).map_err(|e| e.to_string())?
        };

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

        Ok(Self {
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
            },
            morph: Mutex::new(morph),
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
        let chunks = crate::extractor::chunk_pages(&extracted.pages, CHUNK_SIZE, CHUNK_OVERLAP);
        let mut writer = self.writer.lock();
        for chunk in &chunks {
            let key = format!("{}#{}", path_str, chunk.chunk_id);
            let page_str = chunk.page.map(|p| p.to_string()).unwrap_or_default();
            writer
                .add_document(doc!(
                    self.fields.title => extracted.title.as_str(),
                    self.fields.body => chunk.text.as_str(),
                    self.fields.path => path_str.as_str(),
                    self.fields.mtime => mtime.to_string().as_str(),
                    self.fields.size => size.to_string().as_str(),
                    self.fields.ext => ext.as_str(),
                    self.fields.folder => folder,
                    self.fields.page => page_str.as_str(),
                    self.fields.chunk_id => chunk.chunk_id.to_string().as_str(),
                    self.fields.doc_key => key.as_str(),
                ))
                .map_err(|e| e.to_string())?;
        }
        writer.commit().map_err(|e| e.to_string())?;
        self.reader.reload().map_err(|e| e.to_string())?;
        Ok(chunks.len())
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
        // Align indexed tokens with morph drop decisions by surface.
        for surface in indexed {
            if surface.trim().is_empty() {
                continue;
            }
            if drop_surfaces.contains(&surface) {
                continue;
            }
            if !pos_filter && super::morph::is_noise_highlight_term(&surface) {
                continue;
            }
            if pos_filter && is_index_symbol_token(&surface) {
                continue;
            }
            if !seen.insert(surface.clone()) {
                continue;
            }
            let is_noun = morph
                .iter()
                .any(|t| t.surface == surface && t.major_pos == "名詞");
            if pos_filter && is_noun {
                nouns.push(surface);
            } else {
                others.push(surface);
            }
        }
        let mut content = Vec::new();
        content.extend(nouns);
        content.extend(others);
        if content.is_empty() {
            // Last resort: morph-only filter (may still help debugging).
            return self.morph.lock().content_surfaces(query, pos_filter);
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
                | '\u{300c}'
                | '\u{300d}'
                | '\u{3001}'
                | '\u{3002}'
                | '\u{30fb}'
                | '"'
                | '\''
                | '\u{ff08}'
                | '\u{ff09}'
                | '、'
                | '。'
                | '，'
                | ','
                | '.'
                | '!'
                | '？'
                | '?'
                | '!'
                | '：'
                | ':'
                | '；'
                | ';'
                | '・'
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

impl SearchBackend for TantivyBackend {
    fn search(
        &self,
        query: &str,
        limit: usize,
        path_prefix: Option<&str>,
        pos_filter_enabled: bool,
    ) -> Result<Vec<SearchHit>, String> {
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
        eprintln!(
            "argos: parsed includes={:?} phrases={:?} excludes={:?} exclude_phrases={:?} prox={:?} scope={:?} pos_filter={}",
            parsed.includes, parsed.phrases, parsed.excludes, parsed.exclude_phrases, proximity_tokens, scope, pos_filter_enabled
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

        let searcher = self.reader.searcher();
        // Over-fetch more when scoping by path so post-filter can still fill `limit`.
        let fetch_n = if scope.is_some() {
            (limit * 40).max(200)
        } else {
            (limit * 8).max(40)
        };
        let top = searcher
            .search(&*tantivy_q, &TopDocs::with_limit(fetch_n))
            .map_err(|e| e.to_string())?;
        eprintln!("argos: tantivy_raw_hits={}", top.len());

        // Phrase-only: require the phrase string; free tokens keep half-overlap rule.
        // With POS filtering, tokens are already contentful — requiring half of them
        // drops useful noun-only hits (e.g. query has 光景+見慣れ but a doc only has 光景).
        let min_overlap = if proximity_tokens.is_empty() {
            0
        } else if parsed.includes.is_empty() {
            proximity_tokens.len().min(1)
        } else if pos_filter_enabled {
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
        let mut hits: Vec<SearchHit> = Vec::new();
        let mut seen_paths = HashSet::new();
        for (_, hit) in scored {
            if !seen_paths.insert(hit.path.clone()) {
                continue;
            }
            hits.push(hit);
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
}
