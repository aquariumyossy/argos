//! Morphological helpers for query-time POS filtering and user-dictionary matching.
//! Index tokenization stays on Lindera Decompose via lindera-tantivy; this module is
//! query-side only (no Lindera user dictionary on the index).

use std::collections::HashMap;

use lindera::dictionary::load_dictionary;
use lindera::mode::{Mode, Penalty};
use lindera::segmenter::Segmenter;
use lindera::tokenizer::Tokenizer;

/// A surface token with its major POS (IPADIC 大分類).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MorphToken {
    pub surface: String,
    /// e.g. 名詞 / 動詞 / 助詞 / 助動詞 / 記号 / UNK
    pub major_pos: String,
    /// IPADIC 品詞細分類1 (自立 / 非自立 / 接尾 / …)
    pub pos_detail1: String,
}

impl MorphToken {
    pub fn is_particle_or_auxiliary(&self) -> bool {
        matches!(self.major_pos.as_str(), "助詞" | "助動詞")
    }

    pub fn is_symbol(&self) -> bool {
        self.major_pos == "記号"
            || self.surface.chars().all(|c| {
                matches!(
                    c,
                    '(' | ')'
                        | '\u{300c}' // 「
                        | '\u{300d}' // 」
                        | '\u{3001}'
                        | '\u{3002}'
                        | '\u{30fb}'
                        | '"'
                        | '\''
                        | '\u{ff08}' // （
                        | '\u{ff09}' // ）
                )
            })
    }

    /// 動詞の活用断片・付属的動詞（し / いる / れる など）
    pub fn is_light_or_dependent_verb(&self) -> bool {
        if self.major_pos != "動詞" {
            return false;
        }
        matches!(self.pos_detail1.as_str(), "非自立" | "接尾")
            || is_legacy_stop_surface(&self.surface)
            || is_single_hiragana(&self.surface)
    }

    /// Drop for free (non-phrase) content search when POS filter is on.
    pub fn should_drop_for_content(&self) -> bool {
        if self.surface.trim().is_empty() || self.is_symbol() {
            return true;
        }
        if self.is_particle_or_auxiliary() {
            return true;
        }
        // 「し」「さ」「れ」等は動詞扱いのため major_pos だけでは落ちない。
        if self.is_light_or_dependent_verb() || is_legacy_stop_surface(&self.surface) {
            return true;
        }
        // ひらがな1文字は検索ノイズになりやすい（活用語尾・助詞の分解残骸）。
        if is_single_hiragana(&self.surface) {
            return true;
        }
        false
    }

    /// Drop only pure punctuation for phrase token sequences (keep 助詞).
    pub fn should_drop_for_phrase(&self) -> bool {
        self.is_symbol() || self.surface.trim().is_empty()
    }
}

pub struct MorphAnalyzer {
    tokenizer: Tokenizer,
}

impl MorphAnalyzer {
    pub fn new() -> Result<Self, String> {
        let dictionary = load_dictionary("embedded://ipadic").map_err(|e| e.to_string())?;
        let segmenter = Segmenter::new(Mode::Decompose(Penalty::default()), dictionary, None);
        Ok(Self {
            tokenizer: Tokenizer::new(segmenter),
        })
    }

    pub fn analyze(&self, text: &str) -> Result<Vec<MorphToken>, String> {
        let mut tokens = self.tokenizer.tokenize(text).map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity(tokens.len());
        for token in tokens.iter_mut() {
            let surface = token.surface.to_string();
            let major_pos = if let Some(pos) = token.get("part_of_speech") {
                pos.to_string()
            } else if let Some(pos) = token.get("major_pos") {
                pos.to_string()
            } else {
                let details = token.details();
                details
                    .first()
                    .map(|s| (*s).to_string())
                    .unwrap_or_else(|| "UNK".into())
            };
            let pos_detail1 = if let Some(d) = token.get("part_of_speech_subcategory_1") {
                d.to_string()
            } else {
                let details = token.details();
                details
                    .get(1)
                    .map(|s| (*s).to_string())
                    .unwrap_or_default()
            };
            out.push(MorphToken {
                surface,
                major_pos,
                pos_detail1,
            });
        }
        Ok(out)
    }

    /// Surfaces for phrase queries: keep particles, drop symbols.
    pub fn phrase_surfaces(&self, text: &str) -> Result<Vec<String>, String> {
        let tokens = self.analyze(text)?;
        let mut out = Vec::new();
        for t in tokens {
            if t.should_drop_for_phrase() {
                continue;
            }
            out.push(t.surface);
        }
        if out.is_empty() {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                out.push(trimmed.to_string());
            }
        }
        Ok(out)
    }

    /// Surfaces for free OR terms: optionally drop 助詞/助動詞/記号.
    pub fn content_surfaces(
        &self,
        text: &str,
        pos_filter_enabled: bool,
    ) -> Result<Vec<String>, String> {
        let tokens = self.analyze(text)?;
        let mut seen = std::collections::HashSet::new();
        let mut content = Vec::new();
        for t in tokens {
            let drop = if pos_filter_enabled {
                t.should_drop_for_content()
            } else {
                is_legacy_stop_surface(&t.surface) || t.is_symbol()
            };
            if drop {
                continue;
            }
            if seen.insert(t.surface.clone()) {
                content.push(t.surface);
            }
        }
        if content.is_empty() {
            // Do not re-introduce filtered morph debris (e.g. し). Fall back to the
            // raw query only when it itself is not an obvious stop fragment.
            let trimmed = text.trim();
            if !trimmed.is_empty()
                && !is_legacy_stop_surface(trimmed)
                && !is_single_hiragana(trimmed)
            {
                content.push(trimmed.to_string());
            }
        }
        Ok(content)
    }
}

fn is_single_hiragana(s: &str) -> bool {
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => ('\u{3041}'..='\u{3096}').contains(&c),
        _ => false,
    }
}

/// True for surfaces that should not appear as search-result chips.
pub fn is_noise_highlight_term(s: &str) -> bool {
    let t = s.trim();
    t.is_empty() || is_legacy_stop_surface(t) || is_single_hiragana(t)
}

/// Legacy surface stop list (also applied under POS filter for conjugation debris).
fn is_legacy_stop_surface(t: &str) -> bool {
    const STOP: &[&str] = &[
        "\u{306e}", "\u{3092}", "\u{306b}", "\u{306f}", "\u{304c}", "\u{3082}", "\u{3068}", "\u{3067}",
        "\u{3078}", "\u{3084}", "\u{304b}", "\u{306a}\u{3069}", "\u{3088}\u{308a}", "\u{304b}\u{3089}",
        "\u{307e}\u{3067}", "\u{3066}", "\u{305f}", "\u{308c}", "\u{305b}", "\u{3057}", "\u{3055}",
        "\u{3044}\u{308b}", "\u{3042}\u{308b}", "\u{3059}\u{308b}", "\u{306a}\u{308b}",
        "\u{308c}\u{308b}", "\u{3089}\u{308c}\u{308b}", "\u{3067}\u{3059}", "\u{307e}\u{3059}",
        "\u{3067}\u{3057}\u{305f}", "\u{307e}\u{3057}\u{305f}", "\u{3068}\u{3044}\u{3046}",
        "\u{3068}\u{3057}\u{3066}", "\u{306b}\u{3064}\u{3044}\u{3066}",
    ];
    STOP.contains(&t)
}

/// Prefix trie for longest-match user dictionary lookup (char-based).
#[derive(Debug, Default, Clone)]
pub struct UserDictMatcher {
    /// Maps next char -> child; `end` marks a complete dictionary word ending here.
    root: TrieNode,
    /// Longest word length in chars (for bounds).
    max_len: usize,
}

#[derive(Debug, Default, Clone)]
struct TrieNode {
    children: HashMap<char, TrieNode>,
    end: bool,
}

impl UserDictMatcher {
    pub fn from_words<I, S>(words: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut root = TrieNode::default();
        let mut max_len = 0usize;
        for w in words {
            let word = w.as_ref().trim();
            if word.is_empty() {
                continue;
            }
            let chars: Vec<char> = word.chars().collect();
            max_len = max_len.max(chars.len());
            let mut node = &mut root;
            for ch in chars {
                node = node.children.entry(ch).or_default();
            }
            node.end = true;
        }
        Self { root, max_len }
    }

    pub fn is_empty(&self) -> bool {
        self.root.children.is_empty()
    }

    /// Longest dictionary word starting at `chars[from]`, or None.
    pub fn longest_from(&self, chars: &[char], from: usize) -> Option<usize> {
        let mut node = &self.root;
        let mut last_end: Option<usize> = None;
        let limit = (from + self.max_len).min(chars.len());
        for i in from..limit {
            match node.children.get(&chars[i]) {
                Some(next) => {
                    node = next;
                    if node.end {
                        last_end = Some(i + 1);
                    }
                }
                None => break,
            }
        }
        last_end
    }

    pub fn contains_exact(&self, word: &str) -> bool {
        let chars: Vec<char> = word.chars().collect();
        self.longest_from(&chars, 0) == Some(chars.len())
    }
}

fn is_query_delimiter(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\n' | '\r' | '\u{3000}' | ',' | '\u{FF0C}' | '\u{3001}'
    )
}

/// Wrap user-dictionary longest matches in `"..."` so remote/local phrase parsing agrees.
/// Skips already-quoted spans and the term after a leading `-`.
pub fn apply_user_dictionary(query: &str, matcher: &UserDictMatcher) -> String {
    if matcher.is_empty() {
        return query.to_string();
    }
    let chars: Vec<char> = query.chars().collect();
    let mut out = String::new();
    let mut i = 0usize;

    while i < chars.len() {
        // Preserve delimiters as-is.
        if is_query_delimiter(chars[i]) {
            out.push(chars[i]);
            i += 1;
            continue;
        }

        // Exclude marker: copy `-` and then process the following atom specially
        // (still allow dict match inside exclude phrase / term).
        let exclude = chars[i] == '-' && i + 1 < chars.len() && !is_query_delimiter(chars[i + 1]);
        if exclude {
            out.push('-');
            i += 1;
        }

        // Already-quoted phrase: copy verbatim through closing quote.
        if i < chars.len() && chars[i] == '"' {
            out.push('"');
            i += 1;
            while i < chars.len() {
                out.push(chars[i]);
                if chars[i] == '"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }

        // Free atom until delimiter: longest-match rewrite inside.
        let start = i;
        while i < chars.len() && !is_query_delimiter(chars[i]) {
            i += 1;
        }
        let atom: String = chars[start..i].iter().collect();
        out.push_str(&rewrite_atom(&atom, matcher));
    }

    out
}

fn rewrite_atom(atom: &str, matcher: &UserDictMatcher) -> String {
    if atom.is_empty() {
        return String::new();
    }
    // Whole atom is a dictionary word → quote it.
    if matcher.contains_exact(atom) {
        return format!("\"{atom}\"");
    }

    let chars: Vec<char> = atom.chars().collect();
    let mut out = String::new();
    let mut i = 0usize;
    let mut pending = String::new();

    while i < chars.len() {
        if let Some(end) = matcher.longest_from(&chars, i) {
            if !pending.is_empty() {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&pending);
                pending.clear();
            }
            let matched: String = chars[i..end].iter().collect();
            if !out.is_empty() {
                out.push(' ');
            }
            out.push('"');
            out.push_str(&matched);
            out.push('"');
            i = end;
        } else {
            pending.push(chars[i]);
            i += 1;
        }
    }
    if !pending.is_empty() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&pending);
    }
    // If nothing matched, return original atom unchanged.
    if out.is_empty() {
        atom.to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_match_prefers_longer_word() {
        let m = UserDictMatcher::from_words(["損害", "損害賠償", "弁済による代位"]);
        let chars: Vec<char> = "損害賠償請求".chars().collect();
        assert_eq!(m.longest_from(&chars, 0), Some(4)); // 損害賠償 = 4 chars
    }

    #[test]
    fn apply_dict_quotes_compound_with_particle() {
        let m = UserDictMatcher::from_words(["弁済による代位"]);
        let q = apply_user_dictionary("契約 弁済による代位 について", &m);
        assert_eq!(q, "契約 \"弁済による代位\" について");
    }

    #[test]
    fn apply_dict_inside_continuous_japanese() {
        let m = UserDictMatcher::from_words(["弁済による代位"]);
        let q = apply_user_dictionary("契約弁済による代位について", &m);
        assert!(q.contains("\"弁済による代位\""));
        assert!(q.contains("契約"));
        assert!(q.contains("について"));
    }

    #[test]
    fn apply_dict_skips_already_quoted() {
        let m = UserDictMatcher::from_words(["損害賠償"]);
        let q = apply_user_dictionary("\"損害賠償\" -慰謝料", &m);
        assert_eq!(q, "\"損害賠償\" -慰謝料");
    }

    #[test]
    fn content_surfaces_drops_particles_when_filter_on() {
        let morph = MorphAnalyzer::new().expect("morph");
        let tokens = morph
            .content_surfaces("契約についての損害賠償", true)
            .expect("tokenize");
        assert!(tokens.iter().any(|t| t.contains("契約") || t == "契約"));
        assert!(
            !tokens.iter().any(|t| t == "の" || t == "に"),
            "particles should be dropped: {tokens:?}"
        );
        let phrase = morph
            .phrase_surfaces("弁済による代位")
            .expect("phrase");
        assert!(
            phrase.iter().any(|t| t == "による" || t.contains("よる")),
            "phrase must keep particle: {phrase:?}"
        );
    }

    #[test]
    fn content_surfaces_drops_shi_conjugation() {
        let morph = MorphAnalyzer::new().expect("morph");
        let tokens = morph
            .content_surfaces("契約書を作成しました", true)
            .expect("tokenize");
        assert!(
            !tokens.iter().any(|t| t == "し"),
            "conjugation し must be dropped: {tokens:?}"
        );
        assert!(tokens.iter().any(|t| t == "契約" || t == "作成"));
        assert!(!is_noise_highlight_term("契約"));
        assert!(is_noise_highlight_term("し"));
    }
}
