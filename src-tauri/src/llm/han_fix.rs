//! Replace foreign hanzi in a finished chat reply, then optionally rewrite once.
//!
//! Step 1 swaps a simplified or traditional character for the one Japanese kanji
//! the Unihan table maps it to. It does not touch code, blockquotes, or text
//! quoted in 「」 / 『』 / "". Step 2 sends only the remaining dirty segments to
//! the local model, once, and splices accepted rewrites back. A segment is
//! rejected when the call did not stop normally, the body is empty, it adds or
//! drops a citation, an article number, a URL, or code, or the length is
//! implausible. Latin and Greek are not foreign script.
//!
//! A named script in the recent user turns (中国語, 韓国語, Chinese, Korean, …)
//! leaves each line that is foreign script with almost no kana untouched, so a
//! Japanese preface or note is still corrected. Generic words (訳して, 翻訳,
//! translate, 和訳, 英訳) skip only when the reply is also mostly foreign. A
//! reply with enough kana is corrected. Foreign text left inside a quote or a
//! code span still gets the warning. A quote may span lines.

#[path = "japanese_han_map.rs"]
mod japanese_han_map;

use std::collections::HashMap;
use std::sync::OnceLock;

use base64::Engine;

use japanese_han_map::HAN_JP_PAIRS;

/// Shown while the one rewrite call is in flight. The answer already on screen stays.
pub const REWRITE_STATUS: &str = "外国文字を書き直しています";

/// Shown under the reply when the user stops during that rewrite. Not an error banner.
pub const REWRITE_STOPPED_NOTE: &str = "書き直しを中止しました";

/// Overall budget for the one rewrite call. A timeout keeps the step-1 text.
pub const REWRITE_TIMEOUT_SECS: u64 = 60;

/// JIS characters that also work as simplified-Chinese function words.
/// A step-1 substitution sitting next to one of these is still residue.
const SUSPECT_JIS: &[char] = &['个', '于', '从', '无'];

const UNVERIFIED_MARK: &str = "（要確認）";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HanFix {
    /// Unicode scalar index in the text these fixes belong to.
    pub at: usize,
    pub from: char,
    pub to: char,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirtySegment {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappedReply {
    /// Text before any correction.
    pub input: String,
    pub text: String,
    pub fixes: Vec<HanFix>,
    /// A dirty segment is still outside quotes and code, so one rewrite may run.
    pub needs_rewrite: bool,
    pub segments: Vec<DirtySegment>,
    /// The user asked for a foreign language. Do not warn about the characters.
    pub suppress_warning: bool,
}

/// One segment returned by the single rewrite call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentRewrite {
    pub text: String,
    pub finish_reason: String,
}

/// What the one rewrite call did. `Stopped` is the user hitting stop, not a failure banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewriteInput {
    NotRun,
    Failed,
    Stopped,
    Pieces(Vec<SegmentRewrite>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyCorrection {
    pub content: String,
    /// Empty when `content` is the reply the model wrote.
    pub original: String,
    pub fixes: Vec<HanFix>,
    pub rewritten: bool,
    pub rewrite_stopped: bool,
    pub suppress_warning: bool,
}

impl ReplyCorrection {
    /// Empty when the chat has nothing to explain. Otherwise JSON for the pane.
    pub fn script_fix_json(&self) -> String {
        script_fix_json(
            &self.fixes,
            self.rewritten,
            self.rewrite_stopped,
            self.suppress_warning,
        )
    }
}

pub fn script_fix_json(
    fixes: &[HanFix],
    rewritten: bool,
    rewrite_stopped: bool,
    suppress_warning: bool,
) -> String {
    if fixes.is_empty() && !rewritten && !rewrite_stopped && !suppress_warning {
        return String::new();
    }
    let fixes: Vec<serde_json::Value> = fixes
        .iter()
        .map(|fix| {
            serde_json::json!({
                "at": fix.at,
                "from": fix.from.to_string(),
                "to": fix.to.to_string(),
            })
        })
        .collect();
    let mut value = serde_json::json!({ "fixes": fixes, "rewritten": rewritten });
    if rewrite_stopped {
        value["rewriteStopped"] = serde_json::json!(true);
    }
    if suppress_warning {
        value["suppressWarning"] = serde_json::json!(true);
    }
    value.to_string()
}

fn han_map() -> &'static HashMap<u32, char> {
    static MAP: OnceLock<HashMap<u32, char>> = OnceLock::new();
    MAP.get_or_init(|| {
        let compact: String = HAN_JP_PAIRS
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(compact)
            .expect("han jp map");
        let mut map = HashMap::with_capacity(bytes.len() / 8);
        for chunk in bytes.chunks_exact(8) {
            let src = u32::from_le_bytes(chunk[0..4].try_into().expect("pair"));
            let dst = u32::from_le_bytes(chunk[4..8].try_into().expect("pair"));
            let Some(to) = char::from_u32(dst) else {
                continue;
            };
            map.insert(src, to);
        }
        map
    })
}

/// One-to-one substitutions. Anything not in the table, including kana, Latin, and Greek, stays.
pub fn map_foreign_han(text: &str) -> (String, Vec<HanFix>) {
    let table = han_map();
    let mut out = String::with_capacity(text.len());
    let mut fixes = Vec::new();
    for (at, ch) in text.chars().enumerate() {
        let cp = ch as u32;
        if let Some(&to) = table.get(&cp) {
            if crate::llm::japanese_han::is_cjk_ideograph(cp)
                && !crate::llm::japanese_han::is_japanese_han(cp)
                && crate::llm::japanese_han::is_japanese_han(to as u32)
            {
                fixes.push(HanFix { at, from: ch, to });
                out.push(to);
                continue;
            }
        }
        out.push(ch);
    }
    (out, fixes)
}

fn is_foreign_char(ch: char) -> bool {
    let cp = ch as u32;
    crate::llm::japanese_han::is_foreign_script(cp)
        || (crate::llm::japanese_han::is_cjk_ideograph(cp)
            && !crate::llm::japanese_han::is_japanese_han(cp))
}

/// How many recent user turns are checked for a translation request.
pub const RECENT_USER_TURNS: usize = 4;

/// A named target script. These skip correction only when the reply has almost no kana.
pub const FOREIGN_SCRIPT_KEYWORDS: &[&str] = &[
    "中国語",
    "韓国語",
    "朝鮮語",
    "中文",
    "한국어",
    "簡体字",
    "繁体字",
    "ハングル",
    "chinese",
    "korean",
];

/// Generic translation words. They skip only together with [`reply_is_mostly_foreign`].
pub const GENERIC_TRANSLATION_KEYWORDS: &[&str] = &["訳して", "翻訳", "translate", "和訳", "英訳"];

/// Thresholds shared with `japaneseHan.ts`.
pub const MOSTLY_FOREIGN_MIN_LETTERS: usize = 24;
pub const MOSTLY_FOREIGN_MIN_FOREIGN: usize = 6;
pub const MOSTLY_FOREIGN_MAX_KANA: usize = 2;

fn contains_keyword(prompt: &str, words: &[&str]) -> bool {
    let lower = prompt.to_lowercase();
    words.iter().any(|word| lower.contains(&word.to_lowercase()))
}

/// The recent user text names Chinese, Korean, or another target script.
pub fn names_foreign_script(prompt: &str) -> bool {
    contains_keyword(prompt, FOREIGN_SCRIPT_KEYWORDS)
}

/// 「訳して」「翻訳」「translate」「和訳」「英訳」 without naming the script.
pub fn asks_generic_translation(prompt: &str) -> bool {
    contains_keyword(prompt, GENERIC_TRANSLATION_KEYWORDS)
}

fn is_kana(ch: char) -> bool {
    matches!(
        ch,
        '\u{3040}'..='\u{309F}' | '\u{30A0}'..='\u{30FF}' | '\u{31F0}'..='\u{31FF}' | '\u{FF66}'..='\u{FF9D}'
    )
}

/// A long reply that is mostly non-Japanese letters and has almost no kana.
///
/// Conservative on purpose: at least 24 letters, at least 6 foreign characters,
/// foreign characters at least one sixth of the letters, and at most 2 kana
/// (under 5%). A Japanese sentence with a few foreign characters still has kana
/// and is corrected.
pub fn reply_is_mostly_foreign(text: &str) -> bool {
    let mut kana = 0usize;
    let mut foreign = 0usize;
    let mut letters = 0usize;
    for ch in text.chars() {
        let cp = ch as u32;
        if is_kana(ch) {
            letters += 1;
            kana += 1;
        } else if is_foreign_char(ch) {
            letters += 1;
            foreign += 1;
        } else if crate::llm::japanese_han::is_cjk_ideograph(cp) || ch.is_alphabetic() {
            letters += 1;
        }
    }
    if letters < MOSTLY_FOREIGN_MIN_LETTERS || foreign < MOSTLY_FOREIGN_MIN_FOREIGN {
        return false;
    }
    if kana > MOSTLY_FOREIGN_MAX_KANA || kana.saturating_mul(20) > letters {
        return false;
    }
    foreign.saturating_mul(6) >= letters
}

/// A normal Japanese reply. More kana than the "almost none" bar above.
pub fn has_enough_kana(text: &str) -> bool {
    let mut kana = 0usize;
    let mut letters = 0usize;
    for ch in text.chars() {
        let cp = ch as u32;
        if is_kana(ch) {
            letters += 1;
            kana += 1;
        } else if is_foreign_char(ch)
            || crate::llm::japanese_han::is_cjk_ideograph(cp)
            || ch.is_alphabetic()
        {
            letters += 1;
        }
    }
    if kana == 0 {
        return false;
    }
    kana > MOSTLY_FOREIGN_MAX_KANA || kana.saturating_mul(20) > letters
}

/// One line of a requested translation: foreign letters and almost no kana.
pub fn line_is_foreign_translation(line: &str) -> bool {
    line.chars().any(is_foreign_char) && !has_enough_kana(line)
}

/// Drop translation lines when a target script was named. Other lines stay.
pub fn foreign_notice_text(prompt: &str, text: &str) -> String {
    if !names_foreign_script(prompt) {
        return text.to_string();
    }
    text.split('\n')
        .filter(|line| !line_is_foreign_translation(line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn mask_translation_lines(text: &str, prompt: &str, mask: &mut [bool]) {
    if !names_foreign_script(prompt) {
        return;
    }
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let start = i;
        while i < chars.len() && chars[i] != '\n' {
            i += 1;
        }
        let line: String = chars[start..i].iter().collect();
        if line_is_foreign_translation(&line) {
            mask[start..i].fill(true);
        }
        if i < chars.len() {
            i += 1;
        }
    }
}

/// Skip correction and the orange warning for this whole text.
///
/// A named script skips a reply that has almost no kana. When the reply mixes
/// a Japanese preface with a translation, each line is judged separately in
/// [`prepare_reply`]. Generic translation words skip only a mostly-foreign reply.
/// A reply with enough kana is corrected either way.
pub fn should_skip_foreign_fix(prompt: &str, text: &str) -> bool {
    if has_enough_kana(text) {
        return false;
    }
    if names_foreign_script(prompt) {
        return true;
    }
    asks_generic_translation(prompt) && reply_is_mostly_foreign(text)
}

/// Code, blockquotes, and quoted spans. Indexes are Unicode scalars.
fn protected_mask(text: &str) -> Vec<bool> {
    let chars: Vec<char> = text.chars().collect();
    let mut mask = vec![false; chars.len()];
    let mut i = 0;
    let mut in_fence = false;
    while i < chars.len() {
        let start = i;
        while i < chars.len() && chars[i] != '\n' {
            i += 1;
        }
        let line_end = i;
        if i < chars.len() {
            i += 1;
        }
        let line: String = chars[start..line_end].iter().collect();
        let trimmed = line.trim_start();
        let fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        if fence {
            mask[start..i].fill(true);
            in_fence = !in_fence;
            continue;
        }
        if in_fence || trimmed.starts_with('>') {
            mask[start..i].fill(true);
        }
    }
    let mut i = 0;
    while i < chars.len() {
        if mask[i] {
            i += 1;
            continue;
        }
        if chars[i] == '`' {
            let mut j = i + 1;
            while j < chars.len() && !mask[j] && chars[j] != '`' && chars[j] != '\n' {
                j += 1;
            }
            if j < chars.len() && chars[j] == '`' {
                mask[i..=j].fill(true);
                i = j + 1;
                continue;
            }
        }
        if chars[i] == '「' || chars[i] == '『' {
            let close = if chars[i] == '「' { '」' } else { '』' };
            if let Some(end) = find_close(&chars, &mask, i + 1, close) {
                mask[i..=end].fill(true);
                i = end + 1;
                continue;
            }
        }
        if chars[i] == '"' {
            if let Some(end) = find_close(&chars, &mask, i + 1, '"') {
                mask[i..=end].fill(true);
                i = end + 1;
                continue;
            }
        }
        i += 1;
    }
    mask
}

/// An unclosed quote, or one longer than this, protects nothing. A statute
/// quotation can span lines, but a missing closer must not swallow the reply.
const QUOTE_MAX_CHARS: usize = 1_500;

fn find_close(chars: &[char], mask: &[bool], from: usize, close: char) -> Option<usize> {
    let mut j = from;
    let mut seen = 0usize;
    while j < chars.len() && !mask[j] && chars[j] != close {
        if seen >= QUOTE_MAX_CHARS {
            return None;
        }
        seen += 1;
        j += 1;
    }
    if j < chars.len() && chars[j] == close {
        Some(j)
    } else {
        None
    }
}

fn map_outside(text: &str, mask: &[bool]) -> (String, Vec<HanFix>) {
    let table = han_map();
    let mut out = String::with_capacity(text.len());
    let mut fixes = Vec::new();
    for (at, ch) in text.chars().enumerate() {
        if mask.get(at) == Some(&true) {
            out.push(ch);
            continue;
        }
        let cp = ch as u32;
        if let Some(&to) = table.get(&cp) {
            if crate::llm::japanese_han::is_cjk_ideograph(cp)
                && !crate::llm::japanese_han::is_japanese_han(cp)
                && crate::llm::japanese_han::is_japanese_han(to as u32)
            {
                fixes.push(HanFix { at, from: ch, to });
                out.push(to);
                continue;
            }
        }
        out.push(ch);
    }
    (out, fixes)
}

fn is_inline_ws(ch: char) -> bool {
    ch != '\n' && ch.is_whitespace()
}

/// Line-leading `#`, `-`, `*`, `+`, `>`, `|`, or `1.` stays outside the segment.
fn skip_markdown_prefix(chars: &[char], start: usize, end: usize) -> usize {
    if start >= end {
        return start;
    }
    let ch = chars[start];
    if ch == '#' {
        let mut k = start;
        while k < end && chars[k] == '#' {
            k += 1;
        }
        return k;
    }
    if matches!(ch, '-' | '*' | '+') {
        if start + 1 < end && chars[start + 1] == ch {
            return start;
        }
        if start + 1 == end || is_inline_ws(chars[start + 1]) {
            return start + 1;
        }
        return start;
    }
    if ch == '>' {
        return start + 1;
    }
    if ch.is_ascii_digit() {
        let mut k = start;
        while k < end && chars[k].is_ascii_digit() {
            k += 1;
        }
        if k < end && chars[k] == '.' {
            return k + 1;
        }
    }
    start
}

fn core_bounds(chars: &[char], start: usize, end: usize, line_start: usize) -> (usize, usize) {
    let mut i = start;
    let mut j = end;
    while j > i && is_inline_ws(chars[j - 1]) {
        j -= 1;
    }
    let lead = i;
    while i < j && is_inline_ws(chars[i]) {
        i += 1;
    }
    let at_line_start = chars[line_start..lead].iter().all(|ch| is_inline_ws(*ch));
    if at_line_start {
        i = skip_markdown_prefix(chars, i, j);
        while i < j && is_inline_ws(chars[i]) {
            i += 1;
        }
    }
    (i, j)
}

fn touches_suspect(chars: &[char], at: usize) -> bool {
    (at > 0 && SUSPECT_JIS.contains(&chars[at - 1]))
        || chars
            .get(at + 1)
            .is_some_and(|ch| SUSPECT_JIS.contains(ch))
}

fn run_is_dirty(chars: &[char], start: usize, end: usize, fixes: &[HanFix]) -> bool {
    for i in start..end {
        if is_foreign_char(chars[i]) {
            return true;
        }
    }
    fixes
        .iter()
        .any(|fix| (start..end).contains(&fix.at) && touches_suspect(chars, fix.at))
}

fn dirty_segments(text: &str, fixes: &[HanFix], mask: &[bool]) -> Vec<DirtySegment> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\n' {
            i += 1;
            continue;
        }
        let line_start = i;
        while i < chars.len() && chars[i] != '\n' {
            i += 1;
        }
        let line_end = i;
        let mut j = line_start;
        while j < line_end {
            if mask.get(j) == Some(&true) {
                j += 1;
                continue;
            }
            let run_start = j;
            while j < line_end && mask.get(j) != Some(&true) {
                j += 1;
            }
            let mut piece = run_start;
            while piece < j {
                if chars[piece] == '|' {
                    piece += 1;
                    continue;
                }
                let cell_start = piece;
                while piece < j && chars[piece] != '|' {
                    piece += 1;
                }
                let (core_start, core_end) = core_bounds(&chars, cell_start, piece, line_start);
                if core_start < core_end && run_is_dirty(&chars, core_start, core_end, fixes) {
                    out.push(DirtySegment {
                        start: core_start,
                        end: core_end,
                        text: chars[core_start..core_end].iter().collect(),
                    });
                }
            }
        }
    }
    out
}

/// Map outside quotes and code. `user_prompt` is the recent user turns joined.
pub fn prepare_reply(user_prompt: &str, text: &str) -> MappedReply {
    if should_skip_foreign_fix(user_prompt, text) {
        return MappedReply {
            input: text.to_string(),
            text: text.to_string(),
            fixes: Vec::new(),
            needs_rewrite: false,
            segments: Vec::new(),
            suppress_warning: true,
        };
    }
    let mut mask = protected_mask(text);
    mask_translation_lines(text, user_prompt, &mut mask);
    let (mapped, fixes) = map_outside(text, &mask);
    let segments = dirty_segments(&mapped, &fixes, &mask);
    MappedReply {
        input: text.to_string(),
        needs_rewrite: !segments.is_empty(),
        segments,
        suppress_warning: false,
        text: mapped,
        fixes,
    }
}

pub fn map_reply(text: &str) -> MappedReply {
    prepare_reply("", text)
}

/// One call, segments only. The model must echo the `<<<n>>>` markers.
pub fn foreign_rewrite_request(segments: &[String]) -> String {
    let mut body = String::from(
        "次の各箇所だけを、意味を保って自然な日本語（漢字の新字体・ひらがな・カタカナ）に書き直してください。出典番号 [n]、条項の番号、URL、コードは変えないでください。説明は書かず、番号の区切りを守って各箇所の書き直しだけを返してください。\n",
    );
    for (i, segment) in segments.iter().enumerate() {
        body.push_str(&format!("\n<<<{}>>>\n{segment}\n", i + 1));
    }
    body
}

/// Bodies in marker order. A single segment may omit the marker.
pub fn parse_rewritten_segments(raw: &str, count: usize) -> Vec<String> {
    let text = normalize_rewrite(raw);
    if count == 0 {
        return Vec::new();
    }
    let mut marks: Vec<(usize, usize, usize)> = Vec::new();
    let mut search = 0;
    while let Some(rel) = text[search..].find("<<<") {
        let at = search + rel;
        let after = &text[at + 3..];
        let Some(rel_end) = after.find(">>>") else {
            break;
        };
        if let Ok(n) = after[..rel_end].trim().parse::<usize>() {
            if (1..=count).contains(&n) {
                let body = at + 3 + rel_end + 3;
                marks.push((n, at, body));
            }
        }
        search = at + 3 + rel_end + 3;
    }
    if marks.is_empty() {
        if count == 1 {
            return vec![strip_preamble(&text)];
        }
        return vec![String::new(); count];
    }
    let mut out = vec![String::new(); count];
    for (i, (n, _marker, body)) in marks.iter().enumerate() {
        let end = marks.get(i + 1).map(|(_, next, _)| *next).unwrap_or(text.len());
        if *n >= 1 && *n <= count {
            out[*n - 1] = strip_preamble(text[*body..end].trim());
        }
    }
    out
}

/// Drop a fence or a copied `---` wrapper. Empty means the rewrite failed.
pub fn normalize_rewrite(raw: &str) -> String {
    let trimmed = raw.trim();
    let unfenced = unwrap_fence(trimmed).unwrap_or_else(|| trimmed.to_string());
    strip_hr_lines(unfenced.trim())
}

fn unwrap_fence(text: &str) -> Option<String> {
    let mut lines = text.lines();
    let first = lines.next()?.trim();
    let lang = first.strip_prefix("```")?;
    if !lang.is_empty() && !lang.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        return None;
    }
    let mut body: Vec<&str> = lines.collect();
    if body.last()?.trim() != "```" {
        return None;
    }
    body.pop();
    Some(body.join("\n"))
}

fn strip_hr_lines(text: &str) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    if lines.first().is_some_and(|line| line.trim() == "---") {
        lines.remove(0);
    }
    if lines.last().is_some_and(|line| line.trim() == "---") {
        lines.pop();
    }
    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

fn strip_preamble(text: &str) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines.remove(0);
    }
    if let Some(first) = lines.first().copied() {
        let trimmed = first.trim();
        if is_preamble_line(trimmed) {
            if let Some(tail) = clause_after_colon(trimmed) {
                if !tail.is_empty() && lines.len() == 1 {
                    return tail.to_string();
                }
            }
            lines.remove(0);
        }
    }
    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines.remove(0);
    }
    lines.join("\n").trim().to_string()
}

fn is_preamble_line(line: &str) -> bool {
    let head = line.trim_end_matches(['：', ':', '。', ' ']);
    ["書き直", "以下", "次の", "説明", "修正", "翻訳", "回答", "結果"]
        .iter()
        .any(|prefix| head.starts_with(prefix))
}

fn clause_after_colon(line: &str) -> Option<&str> {
    line.rsplit_once(['：', ':']).map(|(_, tail)| tail.trim())
}

fn is_normal_stop(reason: &str) -> bool {
    matches!(reason.trim().to_ascii_lowercase().as_str(), "stop" | "end_turn")
}

fn bracket_cites(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' {
            let start = i + 1;
            let mut j = start;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            if j > start && j < chars.len() && chars[j] == ']' {
                out.push(chars[start..j].iter().collect());
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out.sort();
    out
}

fn is_article_digit(ch: char) -> bool {
    ch.is_ascii_digit()
        || matches!(
            ch,
            '０'..='９'
                | '一'
                | '二'
                | '三'
                | '四'
                | '五'
                | '六'
                | '七'
                | '八'
                | '九'
                | '十'
                | '百'
                | '千'
                | '万'
        )
}

fn article_marks(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '第' {
            let start = i;
            i += 1;
            let num = i;
            while i < chars.len() && is_article_digit(chars[i]) {
                i += 1;
            }
            if i > num && i < chars.len() && matches!(chars[i], '条' | '項' | '号') {
                i += 1;
                out.push(chars[start..i].iter().collect());
                continue;
            }
            i = start + 1;
            continue;
        }
        i += 1;
    }
    out.sort();
    out
}

fn urls(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let rest: String = chars[i..].iter().collect();
        let https = rest.starts_with("https://");
        let http = !https && rest.starts_with("http://");
        if https || http {
            let mut j = i + if https { 8 } else { 7 };
            while j < chars.len()
                && !chars[j].is_whitespace()
                && !matches!(chars[j], '」' | '』' | '"' | '<' | '>')
            {
                j += 1;
            }
            out.push(chars[i..j].iter().collect());
            i = j;
            continue;
        }
        i += 1;
    }
    out.sort();
    out
}

fn code_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        if let Some(end) = after.find('`') {
            let inner = after[..end].trim();
            if !inner.is_empty() {
                out.push(inner.to_string());
            }
            rest = &after[end + 1..];
            continue;
        }
        break;
    }
    let mut word = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            word.push(ch);
        } else {
            push_code_word(&mut out, &word);
            word.clear();
        }
    }
    push_code_word(&mut out, &word);
    out.sort();
    out
}

fn push_code_word(out: &mut Vec<String>, word: &str) {
    // Identifiers only. Ordinary words such as "OK" may be rewritten.
    let ident = word.contains('_')
        || (word.chars().any(|ch| ch.is_ascii_alphabetic())
            && word.chars().any(|ch| ch.is_ascii_digit()));
    if ident {
        out.push(word.to_string());
    }
}

fn same_anchors(source: &str, next: &str) -> bool {
    bracket_cites(source) == bracket_cites(next)
        && article_marks(source) == article_marks(next)
        && urls(source) == urls(next)
        && code_tokens(source) == code_tokens(next)
}

fn plausible_len(source: &str, next: &str) -> bool {
    let before = source.chars().count().max(1) as f32;
    let after = next.chars().count() as f32;
    let ratio = after / before;
    if before <= 4.0 {
        (0.25..=8.0).contains(&ratio)
    } else {
        (0.4..=3.0).contains(&ratio)
    }
}

/// `None` keeps the step-1 text of this segment.
pub fn accept_segment(source: &str, raw: &str, finish_reason: &str) -> Option<String> {
    if !is_normal_stop(finish_reason) {
        return None;
    }
    let text = strip_preamble(&normalize_rewrite(raw));
    // A segment is one line. A newline in the rewrite would join or split lines.
    if text.is_empty() || text == source || text.contains('\n') {
        return None;
    }
    if !same_anchors(source, &text) || !plausible_len(source, &text) {
        return None;
    }
    Some(text)
}

fn splice(mapped: &MappedReply, replacements: &[Option<String>]) -> (String, Vec<HanFix>, bool) {
    let chars: Vec<char> = mapped.text.chars().collect();
    let mut out = String::new();
    let mut fixes = Vec::new();
    let mut cursor = 0usize;
    let mut wrote = 0usize;
    let mut any = false;
    for (segment, replacement) in mapped.segments.iter().zip(replacements.iter()) {
        append_gap(&chars, cursor, segment.start, &mapped.fixes, wrote, &mut out, &mut fixes);
        wrote += segment.start - cursor;
        if let Some(text) = replacement {
            let local: Vec<HanFix> = mapped
                .fixes
                .iter()
                .filter(|fix| (segment.start..segment.end).contains(&fix.at))
                .map(|fix| HanFix {
                    at: fix.at - segment.start,
                    from: fix.from,
                    to: fix.to,
                })
                .collect();
            for fix in surviving_fixes(&segment.text, &local, text) {
                fixes.push(HanFix {
                    at: wrote + fix.at,
                    from: fix.from,
                    to: fix.to,
                });
            }
            out.push_str(text);
            wrote += text.chars().count();
            any = true;
        } else {
            append_gap(&chars, segment.start, segment.end, &mapped.fixes, wrote, &mut out, &mut fixes);
            wrote += segment.end - segment.start;
        }
        cursor = segment.end;
    }
    append_gap(&chars, cursor, chars.len(), &mapped.fixes, wrote, &mut out, &mut fixes);
    (out, fixes, any)
}

fn append_gap(
    chars: &[char],
    from: usize,
    to: usize,
    fixes: &[HanFix],
    wrote: usize,
    out: &mut String,
    kept: &mut Vec<HanFix>,
) {
    if from >= to {
        return;
    }
    out.extend(chars[from..to].iter().copied());
    for fix in fixes {
        if (from..to).contains(&fix.at) {
            kept.push(HanFix {
                at: wrote + fix.at - from,
                from: fix.from,
                to: fix.to,
            });
        }
    }
}

/// Splice accepted segment rewrites. A rejected segment stays at the step-1 text.
pub fn apply_rewrite(mapped: MappedReply, rewrite: RewriteInput) -> ReplyCorrection {
    if mapped.suppress_warning {
        return finish(&mapped.input, mapped.text, Vec::new(), false, false, true);
    }
    if !mapped.needs_rewrite {
        return finish(&mapped.input, mapped.text, mapped.fixes, false, false, false);
    }
    let RewriteInput::Pieces(pieces) = rewrite else {
        let stopped = matches!(rewrite, RewriteInput::Stopped);
        return finish(&mapped.input, mapped.text, mapped.fixes, false, stopped, false);
    };
    if pieces.len() != mapped.segments.len() {
        return finish(&mapped.input, mapped.text, mapped.fixes, false, false, false);
    }
    let accepted: Vec<Option<String>> = mapped
        .segments
        .iter()
        .zip(pieces.iter())
        .map(|(segment, piece)| accept_segment(&segment.text, &piece.text, &piece.finish_reason))
        .collect();
    if accepted.iter().all(|item| item.is_none()) {
        return finish(&mapped.input, mapped.text, mapped.fixes, false, false, false);
    }
    let (content, fixes, rewritten) = splice(&mapped, &accepted);
    finish(&mapped.input, content, fixes, rewritten, false, false)
}

fn finish(
    input: &str,
    content: String,
    fixes: Vec<HanFix>,
    rewritten: bool,
    rewrite_stopped: bool,
    suppress_warning: bool,
) -> ReplyCorrection {
    let original = if content == input {
        String::new()
    } else {
        input.to_string()
    };
    ReplyCorrection {
        content,
        original,
        fixes,
        rewritten,
        rewrite_stopped,
        suppress_warning,
    }
}

/// Fixes that still sit in an unchanged head or tail. The edited middle is not underlined.
fn surviving_fixes(before: &str, fixes: &[HanFix], after: &str) -> Vec<HanFix> {
    if before == after {
        return fixes.to_vec();
    }
    let a: Vec<char> = before.chars().collect();
    let b: Vec<char> = after.chars().collect();
    let mut prefix = 0;
    while prefix < a.len() && prefix < b.len() && a[prefix] == b[prefix] {
        prefix += 1;
    }
    let mut a_suf = a.len();
    let mut b_suf = b.len();
    while a_suf > prefix && b_suf > prefix && a[a_suf - 1] == b[b_suf - 1] {
        a_suf -= 1;
        b_suf -= 1;
    }
    let shift = b_suf as isize - a_suf as isize;
    let mut kept = Vec::new();
    for fix in fixes {
        let at = if fix.at < prefix {
            Some(fix.at)
        } else if fix.at >= a_suf {
            let next = fix.at as isize + shift;
            if next >= 0 {
                Some(next as usize)
            } else {
                None
            }
        } else {
            None
        };
        let Some(at) = at else {
            continue;
        };
        if b.get(at) == Some(&fix.to) {
            kept.push(HanFix {
                at,
                from: fix.from,
                to: fix.to,
            });
        }
    }
    kept
}

/// `after` is `before` with `（要確認）` inserted. Fix indexes move past those inserts.
pub fn shift_fixes_past_unverified_marks(
    before: &str,
    fixes: &[HanFix],
    after: &str,
) -> Vec<HanFix> {
    if before == after || fixes.is_empty() {
        return fixes.to_vec();
    }
    let a: Vec<char> = before.chars().collect();
    let b: Vec<char> = after.chars().collect();
    let mark: Vec<char> = UNVERIFIED_MARK.chars().collect();
    let mut index_of = vec![None; a.len()];
    let mut i = 0;
    let mut j = 0;
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            index_of[i] = Some(j);
            i += 1;
            j += 1;
            continue;
        }
        if j + mark.len() <= b.len() && b[j..j + mark.len()] == mark[..] {
            j += mark.len();
            continue;
        }
        break;
    }
    fixes
        .iter()
        .filter_map(|fix| {
            let at = index_of.get(fix.at).copied().flatten()?;
            Some(HanFix {
                at,
                from: fix.from,
                to: fix.to,
            })
        })
        .collect()
}

/// Test helper. `rewrite` is called once with the dirty segments, or not at all.
pub fn correct_reply(
    user_prompt: &str,
    text: &str,
    rewrite: impl FnOnce(&[String]) -> Result<Vec<SegmentRewrite>, String>,
) -> ReplyCorrection {
    let mapped = prepare_reply(user_prompt, text);
    let input = if !mapped.needs_rewrite {
        RewriteInput::NotRun
    } else {
        match rewrite(
            &mapped
                .segments
                .iter()
                .map(|segment| segment.text.clone())
                .collect::<Vec<_>>(),
        ) {
            Ok(pieces) => RewriteInput::Pieces(pieces),
            Err(error) if error == "rewrite-stopped" => RewriteInput::Stopped,
            Err(_) => RewriteInput::Failed,
        }
    };
    apply_rewrite(mapped, input)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stop_piece(text: impl Into<String>) -> SegmentRewrite {
        SegmentRewrite {
            text: text.into(),
            finish_reason: "stop".into(),
        }
    }

    #[test]
    fn maps_xianzai_and_leaves_japanese_latin_and_greek() {
        let (text, fixes) = map_foreign_han("现在");
        assert_eq!(text, "現在");
        assert_eq!(
            fixes,
            vec![HanFix {
                at: 0,
                from: '现',
                to: '現'
            }]
        );
        assert_eq!(map_foreign_han("说").0, "説");
        let japanese = "第1条　甲は乙に対し、瑕疵を理由に契約を解除できる。";
        assert_eq!(map_foreign_han(japanese).0, japanese);
        assert!(map_foreign_han(japanese).1.is_empty());
        let latin = "α版　μm　OK 123 https://example.com";
        assert_eq!(map_foreign_han(latin).0, latin);
        assert!(map_foreign_han(latin).1.is_empty());
        assert_eq!(map_foreign_han("发").0, "发");
        assert_eq!(map_foreign_han("国学个干后面里").0, "国学个干后面里");
    }

    #[test]
    fn every_pair_targets_one_allowlisted_kanji() {
        let map = han_map();
        assert!(map.len() > 1000, "{}", map.len());
        for (&src, &dst) in map {
            assert!(
                crate::llm::japanese_han::is_cjk_ideograph(src)
                    && !crate::llm::japanese_han::is_japanese_han(src),
                "U+{src:04X}"
            );
            assert!(
                crate::llm::japanese_han::is_japanese_han(dst as u32),
                "U+{:04X}",
                dst as u32
            );
        }
        assert_eq!(map[&('现' as u32)], '現');
        assert_eq!(map[&('说' as u32)], '説');
        assert_eq!(map[&('线' as u32)], '線');
        assert_eq!(map[&('举' as u32)], '挙');
        assert_eq!(map[&('鉴' as u32)], '鑑');
        assert_eq!(map[&('净' as u32)], '浄');
        assert_eq!(map[&('账' as u32)], '帳');
        assert_eq!(map[&('谭' as u32)], '譚');
        assert_eq!(map[&('岗' as u32)], '崗');
        assert_ne!(map[&('岗' as u32)], '岡');
        assert_eq!(map[&('箫' as u32)], '簫');
        assert_ne!(map.get(&('箫' as u32)), Some(&'焼'));
        assert_eq!(map[&('鲶' as u32)], '鯰');
        assert_ne!(map.get(&('鲶' as u32)), Some(&'鮎'));
        assert!(!map.contains_key(&('发' as u32)));
        assert!(!map.contains_key(&('恶' as u32)));
        assert!(!map.contains_key(&('脏' as u32)));
        assert!(!map.contains_key(&('够' as u32)));
        assert!(!map.contains_key(&('夠' as u32)));
    }

    #[test]
    fn typescript_pairs_match_the_rust_ones() {
        let ts = include_str!("../../../src/features/chat/japaneseHanMap.ts");
        let marker = "HAN_JP_PAIRS = `";
        let start = ts.find(marker).unwrap() + marker.len();
        let end = ts[start..].find('`').unwrap();
        let ts_body: String = ts[start..start + end]
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        let rs_body: String = HAN_JP_PAIRS
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        assert_eq!(ts_body, rs_body);
    }

    #[test]
    fn step_two_runs_only_when_residue_remains_and_only_once() {
        let mut calls = 0;
        let clean = correct_reply("", "现在の契約です。", |_| {
            calls += 1;
            Ok(vec![stop_piece("呼ばれない")])
        });
        assert_eq!(calls, 0);
        assert_eq!(clean.content, "現在の契約です。");
        assert!(!clean.rewritten);
        assert!(clean.script_fix_json().contains("现"));
        assert!(!clean.script_fix_json().contains("\"rewritten\":true"));

        calls = 0;
        let dirty = correct_reply("", "现在は기타です", |segments| {
            calls += 1;
            assert_eq!(segments, ["現在は기타です"]);
            Ok(vec![stop_piece(format!("{}기", segments[0]))])
        });
        assert_eq!(calls, 1, "a residue that survives the rewrite is not sent again");
        assert!(dirty.rewritten);
        assert!(dirty.content.contains('기'));
        assert_eq!(dirty.original, "现在は기타です");
    }

    #[test]
    fn rewrite_failure_keeps_the_mapped_text_and_the_original() {
        let failed = correct_reply("", "现在は기타です", |_| Err("timeout".into()));
        assert_eq!(failed.content, "現在は기타です");
        assert_eq!(failed.original, "现在は기타です");
        assert!(!failed.rewritten);
        assert!(!failed.rewrite_stopped);
        assert_eq!(
            failed.fixes,
            vec![HanFix {
                at: 0,
                from: '现',
                to: '現'
            }]
        );
        let empty = correct_reply("", "기타", |_| Ok(vec![stop_piece("  \n")]));
        assert_eq!(empty.content, "기타");
        assert!(empty.original.is_empty());
        assert!(!empty.rewritten);
    }

    #[test]
    fn original_is_empty_when_nothing_changes() {
        let text = "契約を解除できる。α版";
        let kept = correct_reply("", text, |_| panic!("no foreign script"));
        assert_eq!(kept.content, text);
        assert!(kept.original.is_empty());
        assert!(kept.script_fix_json().is_empty());
        assert!(!kept.rewritten);
    }

    #[test]
    fn rewrite_keeps_a_mapping_outside_the_edited_span() {
        let fixed = correct_reply("", "现在は기타です", |_| {
            Ok(vec![stop_piece("現在はその他です")])
        });
        assert_eq!(fixed.content, "現在はその他です");
        assert_eq!(fixed.original, "现在は기타です");
        assert!(fixed.rewritten);
        assert_eq!(
            fixed.fixes,
            vec![HanFix {
                at: 0,
                from: '现',
                to: '現'
            }]
        );
    }

    #[test]
    fn truncated_rewrite_keeps_the_step1_segment() {
        let kept = correct_reply("", "现在は기타です", |_| {
            Ok(vec![SegmentRewrite {
                text: "現在はその他です".into(),
                finish_reason: "length".into(),
            }])
        });
        assert_eq!(kept.content, "現在は기타です");
        assert!(!kept.rewritten);
    }

    #[test]
    fn preamble_is_stripped_before_the_segment_is_spliced() {
        let fixed = correct_reply("", "现在は기타です", |_| {
            Ok(vec![stop_piece(
                "書き直した文は次のとおりです。\n現在はその他です",
            )])
        });
        assert_eq!(fixed.content, "現在はその他です");
        assert!(fixed.rewritten);
    }

    #[test]
    fn dropped_citation_keeps_the_step1_segment() {
        let source = "根拠は [1] の기타です。";
        let kept = correct_reply("", source, |_| Ok(vec![stop_piece("根拠は以下です。")]));
        assert_eq!(kept.content, source);
        assert!(!kept.rewritten);
        let changed_url = correct_reply("", "詳細は https://example.com/a と기타です。", |_| {
            Ok(vec![stop_piece("詳細はサイトと、その他です。")])
        });
        assert!(changed_url.content.contains("https://example.com/a"));
        assert!(changed_url.content.contains('기'));
        let changed_article = correct_reply("", "第1条の기타です。", |_| {
            Ok(vec![stop_piece("条のその他です。")])
        });
        assert!(changed_article.content.contains("第1条"));
        let changed_code = correct_reply("", "値は foo_bar と기타です。", |_| {
            Ok(vec![stop_piece("値はその他です。")])
        });
        assert!(changed_code.content.contains("foo_bar"));
        let plain_word = correct_reply("", "状態は OK だが기타です。", |_| {
            Ok(vec![stop_piece("状態は良いが、その他です。")])
        });
        assert_eq!(plain_word.content, "状態は良いが、その他です。");
        assert!(accept_segment("値は `now` と기타", "値はその他です", "stop").is_none());
    }

    #[test]
    fn chinese_function_word_next_to_a_substitution_triggers_step_two() {
        let mut calls = 0;
        let out = correct_reply("", "这个合同现在生效", |segments| {
            calls += 1;
            assert_eq!(segments, ["這个合同現在生效"]);
            Ok(vec![stop_piece("この契約は今効力を生じる")])
        });
        assert_eq!(calls, 1);
        assert_eq!(out.content, "この契約は今効力を生じる");
        assert!(out.rewritten);

        let japanese = "第1条　甲は乙に対し、瑕疵を理由に契約を解除できる。";
        let kept = correct_reply("", japanese, |_| panic!("ordinary Japanese is not rewritten"));
        assert_eq!(kept.content, japanese);
        assert!(!kept.rewritten);
    }

    #[test]
    fn quotes_code_and_translation_requests_are_left_alone() {
        let quoted = correct_reply("", "定義は「现在」です。", |_| {
            panic!("a quoted clause is not rewritten")
        });
        assert_eq!(quoted.content, "定義は「现在」です。");
        assert!(!quoted.suppress_warning);

        let fenced = correct_reply("", "```\n现在\n```", |_| panic!("code is not rewritten"));
        assert_eq!(fenced.content, "```\n现在\n```");

        let inline = correct_reply("", "用語 `现在` を使う。", |_| panic!("inline code"));
        assert_eq!(inline.content, "用語 `现在` を使う。");

        let block = correct_reply("", "> 现在\n", |_| panic!("a blockquote is not rewritten"));
        assert_eq!(block.content, "> 现在\n");

        let named = correct_reply("", "名前は\"现在\"です。", |_| panic!("an ascii quote"));
        assert_eq!(named.content, "名前は\"现在\"です。");

        let lines = "## 关于本件\n本文です。";
        let heading = correct_reply("", lines, |segments| {
            assert_eq!(segments.len(), 1);
            assert!(!segments[0].contains('\n'));
            assert!(!segments[0].contains('#'));
            Ok(vec![stop_piece("本件について")])
        });
        assert_eq!(heading.content, "## 本件について\n本文です。");

        let bullets = "- 关于本件\n  - 关于子项\n1. 关于本件\n";
        let listed = correct_reply("", bullets, |segments| {
            assert!(segments.len() >= 3);
            assert!(segments.iter().all(|segment| {
                !segment.contains('\n') && !segment.starts_with('-') && !segment.starts_with('#')
            }));
            Ok(segments
                .iter()
                .map(|_| stop_piece("項目"))
                .collect())
        });
        assert!(listed.content.starts_with("- 項目\n"));
        assert!(listed.content.contains("\n  - 項目\n"));
        assert!(listed.content.contains("\n1. 項目\n"));

        let quoted = "前置き。\n「现在\n生效」\n本文は现在です。";
        let kept_quote = correct_reply("", quoted, |_| panic!("a multi-line quote is not rewritten"));
        assert!(kept_quote.content.contains("「现在\n生效」"));
        assert!(kept_quote.content.contains("現在"));
        assert!(!kept_quote.suppress_warning);

        let open = "「现在\n本文は现在です。";
        let unclosed = correct_reply("", open, |_| panic!("an unclosed quote protects nothing"));
        assert!(!unclosed.content.contains('现'));
        assert!(unclosed.content.contains('現'));

        let zh = "本合同自签订之日起生效，双方应当遵守本合同的全部条款并及时履行各自的义务。";
        let follow = correct_reply("第1条を中国語に訳して\n第2条も同様に", zh, |_| {
            panic!("a translation follow-up is not corrected")
        });
        assert_eq!(follow.content, zh);
        assert!(follow.suppress_warning);
        let unspoken = correct_reply("第2条も同様に", zh, |segments| {
            Ok(segments.iter().map(|segment| stop_piece(segment.clone())).collect())
        });
        assert!(!unspoken.suppress_warning);
        assert!(names_foreign_script("簡体字で書いて"));
        assert!(names_foreign_script("繁体字に直して"));
        assert!(names_foreign_script("ハングルで"));
        assert!(names_foreign_script("Please write this in Chinese"));
        assert!(!names_foreign_script("翻訳会社との契約"));
        assert!(asks_generic_translation("和訳して"));
        assert!(asks_generic_translation("英訳"));

        let japanese = "现在の契約は有効です。甲は乙に対し、金銭を支払う義務を負います。";
        let kana = correct_reply("第2条も同様に", japanese, |_| {
            panic!("a Japanese reply with kana is not a translation")
        });
        assert!(kana.content.starts_with("現在"));
        assert!(!kana.suppress_warning);

        let stopped = correct_reply("", "现在は기타です", |_| Err("rewrite-stopped".into()));
        assert_eq!(stopped.content, "現在は기타です");
        assert!(stopped.rewrite_stopped);
        assert!(!stopped.rewritten);
        assert!(stopped.script_fix_json().contains("rewriteStopped"));

        let asked = correct_reply("中国語に訳して", "这个合同现在生效", |_| {
            panic!("a translation request is not corrected")
        });
        assert_eq!(asked.content, "这个合同现在生效");
        assert!(asked.suppress_warning);
        assert!(asked.script_fix_json().contains("suppressWarning"));
        assert!(!asked.rewritten);
    }

    #[test]
    fn translation_skip_keeps_a_japanese_reply() {
        let reply = "第5条は、契约が现在も有効であることを定めています。甲は乙に対し通知しなければなりません。";
        let translated = correct_reply("この英文契約の第5条を和訳して", reply, |_| {
            panic!("a Japanese 和訳 is still corrected")
        });
        assert!(translated.content.contains('約'));
        assert!(translated.content.contains('現'));
        assert!(!translated.suppress_warning);

        let follow = "有効期間は契約締結日から3年です。ただし现在の特約があるときは、この限りではありません。";
        let next = correct_reply(
            "この英文契約の第5条を和訳して\nこの契約の有効期間は？",
            follow,
            |_| panic!("the next Japanese question is corrected"),
        );
        assert!(next.content.contains('現'));
        assert!(!next.suppress_warning);

        let note = "秘密保持と现在の再委託制限を確認してください。甲は乙に対し報告します。";
        let company = correct_reply("翻訳会社との業務委託契約の注意点は？", note, |_| {
            panic!("翻訳会社 is not a request to write in a foreign script")
        });
        assert!(company.content.contains('現'));
        assert!(!company.suppress_warning);

        let zh = "本合同自签订之日起生效，双方应当遵守本合同的全部条款并及时履行各自的义务。";
        let skipped = correct_reply("第1条を中国語に訳して\n第2条も同様に", zh, |_| {
            panic!("a Chinese follow-up stays uncorrected")
        });
        assert_eq!(skipped.content, zh);
        assert!(skipped.suppress_warning);

        let explained = correct_reply("中国語に訳して", reply, |_| {
            panic!("a kana reply is corrected even after 中国語")
        });
        assert!(explained.content.contains('現'));
        assert!(!explained.suppress_warning);
    }

    #[test]
    fn a_named_translation_keeps_foreign_lines_and_checks_japanese_ones() {
        let chinese = "本合同自签订之日起生效，双方应当遵守本合同的全部条款并及时履行各自的义务。";
        let mixed = format!("以下が第5条の中国語訳です。现在も確認します。\n{chinese}");
        let out = correct_reply("第5条を中国語に訳して", &mixed, |_| {
            panic!("the Chinese lines are not rewritten")
        });
        assert!(out.content.contains(chinese));
        assert!(out.content.contains("現在も確認します。"));
        assert!(!out.content.contains('现'));
        assert!(!out.suppress_warning);
        let notice = foreign_notice_text("第5条を中国語に訳して", &out.content);
        assert!(!notice.contains("本合同"));
        assert!(notice.contains("以下が第5条"));

        let korean = "韓国語訳は次のとおりです。现在の確認です。\n안녕하세요";
        let ko = correct_reply("韓国語に訳して", korean, |_| panic!("Korean is not rewritten"));
        assert!(ko.content.contains("안녕하세요"));
        assert!(ko.content.contains("現在の確認です。"));
        assert!(!foreign_notice_text("韓国語に訳して", &ko.content).contains('안'));

        let noted = format!("{chinese}\n（注：现在の訳は参考です。）");
        let note = correct_reply("中国語に訳して", &noted, |_| panic!("the note is not a translation line"));
        assert!(note.content.starts_with(chinese));
        assert!(note.content.contains("（注：現在の訳は参考です。）"));
        assert!(!foreign_notice_text("中国語に訳して", &note.content).contains("本合同"));
    }

    #[test]
    fn gou_stays_unmapped_and_goes_to_step_two() {
        assert_eq!(map_foreign_han("足够").0, "足够");
        assert_eq!(map_foreign_han("能够").0, "能够");
        assert_eq!(map_foreign_han("恶").0, "恶");
        assert_eq!(map_foreign_han("脏").0, "脏");
        let mut calls = 0;
        let out = correct_reply("", "足够です。", |segments| {
            calls += 1;
            assert!(segments.iter().any(|segment| segment.contains('够')));
            Ok(vec![stop_piece("十分です。")])
        });
        assert_eq!(calls, 1);
        assert!(out.rewritten);
        assert!(out.content.contains("十分"));
    }

    #[test]
    fn typescript_skip_rules_match_rust() {
        let ts = include_str!("../../../src/features/chat/japaneseHan.ts");
        for word in FOREIGN_SCRIPT_KEYWORDS
            .iter()
            .chain(GENERIC_TRANSLATION_KEYWORDS.iter())
        {
            assert!(
                ts.contains(&format!("\"{word}\"")),
                "{word} missing from japaneseHan.ts"
            );
        }
        for needle in [
            "MOSTLY_FOREIGN_MIN_LETTERS = 24",
            "MOSTLY_FOREIGN_MIN_FOREIGN = 6",
            "MOSTLY_FOREIGN_MAX_KANA = 2",
            "kana * 20",
            "foreign * 6",
        ] {
            assert!(ts.contains(needle), "{needle}");
        }
    }

    #[test]
    fn unverified_marks_shift_later_fixes() {
        let before = "現在は民法第1条です";
        let after = "現在は民法第1条（要確認）です";
        let fixes = vec![HanFix {
            at: 0,
            from: '现',
            to: '現',
        }];
        assert_eq!(
            shift_fixes_past_unverified_marks(before, &fixes, after),
            fixes
        );
        let before = "民法第1条の現在";
        let after = "民法第1条（要確認）の現在";
        let at = before.chars().position(|ch| ch == '現').unwrap();
        let fixes = vec![HanFix {
            at,
            from: '现',
            to: '現',
        }];
        let shifted = shift_fixes_past_unverified_marks(before, &fixes, after);
        assert_eq!(shifted[0].at, at + UNVERIFIED_MARK.chars().count());
        assert_eq!(after.chars().nth(shifted[0].at), Some('現'));
    }

    #[test]
    fn normalize_rewrite_unwraps_a_fence_and_the_prompt_rule() {
        assert_eq!(normalize_rewrite("  現在 \n"), "現在");
        assert_eq!(
            normalize_rewrite("```text\n現在は契約です。\n```"),
            "現在は契約です。"
        );
        assert_eq!(normalize_rewrite("---\n現在\n---"), "現在");
        assert!(normalize_rewrite("```\n```").is_empty());
    }
}
