//! 条文番号の漢数字 ↔ アラビア数字変換。
//!
//! 索引は原文のまま。表記ゆれは検索時に吸収する。retrieval は
//! `legal_ref_cite_variants` の別名を隣接フレーズ OR にし、近接スコアは
//! `normalize_legal_refs` でクエリと本文の両側をアラビア正規形に畳む。
//! 経緯は `docs/legal-numeral-search.md`。

const DIGIT_KANJI: [char; 10] = ['〇', '一', '二', '三', '四', '五', '六', '七', '八', '九'];
const PLACE_DIGIT: [&str; 10] = ["", "一", "二", "三", "四", "五", "六", "七", "八", "九"];

fn is_unit(c: char) -> bool {
    matches!(c, '条' | '項' | '号' | '編' | '章' | '節' | '款' | '目')
}

fn is_numeral_char(c: char) -> bool {
    c.is_ascii_digit()
        || ('０'..='９').contains(&c)
        || matches!(
            c,
            '〇' | '零'
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

fn kanji_digit_value(c: char) -> Option<u32> {
    match c {
        '〇' | '零' => Some(0),
        '一' => Some(1),
        '二' => Some(2),
        '三' => Some(3),
        '四' => Some(4),
        '五' => Some(5),
        '六' => Some(6),
        '七' => Some(7),
        '八' => Some(8),
        '九' => Some(9),
        _ => None,
    }
}

fn parse_arabic(s: &str) -> Option<u32> {
    if s.is_empty()
        || !s
            .chars()
            .all(|c| c.is_ascii_digit() || ('０'..='９').contains(&c))
    {
        return None;
    }
    let t: String = s
        .chars()
        .map(|c| {
            if ('０'..='９').contains(&c) {
                char::from(b'0' + (c as u32 - '０' as u32) as u8)
            } else {
                c
            }
        })
        .collect();
    t.parse().ok()
}

fn parse_digit_run(s: &str) -> Option<u32> {
    let mut n: u32 = 0;
    let mut digits = 0u32;
    for c in s.chars() {
        let d = kanji_digit_value(c)?;
        n = n.checked_mul(10)?.checked_add(d)?;
        digits += 1;
        if digits > 8 {
            return None;
        }
    }
    if digits == 0 {
        None
    } else {
        Some(n)
    }
}

fn parse_place_value(s: &str) -> Option<u32> {
    let mut result: u32 = 0;
    let mut current: u32 = 0;
    for c in s.chars() {
        if let Some(d) = kanji_digit_value(c) {
            current = d;
            continue;
        }
        if c == '万' {
            let head = result.checked_add(current)?;
            result = head.max(1).checked_mul(10_000)?;
            current = 0;
            continue;
        }
        let place = match c {
            '十' => 10u32,
            '百' => 100,
            '千' => 1000,
            _ => return None,
        };
        result = result.checked_add(current.max(1).checked_mul(place)?)?;
        current = 0;
    }
    Some(result.checked_add(current)?)
}

pub fn parse_legal_numeral(text: &str) -> Option<u32> {
    let s = text.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(n) = parse_arabic(s) {
        return Some(n);
    }
    if !s.chars().any(|c| matches!(c, '十' | '百' | '千' | '万')) {
        parse_digit_run(s)
    } else {
        parse_place_value(s)
    }
}

fn format_fullwidth(n: u32) -> String {
    n.to_string()
        .chars()
        .map(|c| {
            if c.is_ascii_digit() {
                char::from_u32('０' as u32 + (c as u32 - '0' as u32)).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

fn format_digit_kanji(n: u32, zero: char) -> String {
    if n == 0 {
        return zero.to_string();
    }
    n.to_string()
        .chars()
        .map(|c| {
            let d = (c as u8 - b'0') as usize;
            if d == 0 {
                zero
            } else {
                DIGIT_KANJI[d]
            }
        })
        .collect()
}

fn format_place_below_10000(n: u32) -> String {
    if n == 0 {
        return String::new();
    }
    let mut s = String::new();
    let sen = n / 1000;
    let hyaku = (n / 100) % 10;
    let ju = (n / 10) % 10;
    let ichi = n % 10;
    if sen > 0 {
        if sen > 1 {
            s.push_str(PLACE_DIGIT[sen as usize]);
        }
        s.push('千');
    }
    if hyaku > 0 {
        if hyaku > 1 {
            s.push_str(PLACE_DIGIT[hyaku as usize]);
        }
        s.push('百');
    }
    if ju > 0 {
        if ju > 1 {
            s.push_str(PLACE_DIGIT[ju as usize]);
        }
        s.push('十');
    }
    if ichi > 0 {
        s.push_str(PLACE_DIGIT[ichi as usize]);
    }
    s
}

pub fn format_place_kanji(n: u32) -> String {
    if n == 0 {
        return "〇".into();
    }
    if n < 10_000 {
        return format_place_below_10000(n);
    }
    let man = n / 10_000;
    let rest = n % 10_000;
    let mut s = if man == 1 {
        "万".into()
    } else {
        format!("{}万", format_place_below_10000(man))
    };
    if rest > 0 {
        s.push_str(&format_place_below_10000(rest));
    }
    s
}

#[derive(Clone, Copy)]
enum NumStyle {
    Arabic,
    Fullwidth,
    DigitZero,
    DigitRei,
    Place,
}

fn format_num(n: u32, style: NumStyle) -> String {
    match style {
        NumStyle::Arabic => n.to_string(),
        NumStyle::Fullwidth => format_fullwidth(n),
        NumStyle::DigitZero => format_digit_kanji(n, '〇'),
        NumStyle::DigitRei => format_digit_kanji(n, '零'),
        NumStyle::Place => format_place_kanji(n),
    }
}

struct LegalRef {
    start: usize,
    end: usize,
    main: String,
    unit: char,
    branch: Option<String>,
}

fn take_branch(chars: &[char], i: usize) -> (Option<String>, usize) {
    if i < chars.len() && chars[i] == 'の' {
        let bstart = i + 1;
        let mut j = bstart;
        while j < chars.len() && is_numeral_char(chars[j]) {
            j += 1;
        }
        if j > bstart {
            let b: String = chars[bstart..j].iter().collect();
            if parse_legal_numeral(&b).is_some() {
                return (Some(b), j);
            }
        }
    }
    (None, i)
}

fn find_legal_refs(text: &str) -> Vec<LegalRef> {
    // Runs over every retrieved chunk body; skip the char Vec when no ref can start here.
    if !text
        .chars()
        .any(|c| c == '第' || c.is_ascii_digit() || ('０'..='９').contains(&c))
    {
        return Vec::new();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    let mut out = Vec::new();
    while i < chars.len() {
        let with_dai = chars[i] == '第';
        let arabic_start = chars[i].is_ascii_digit() || ('０'..='９').contains(&chars[i]);
        if !with_dai && !arabic_start {
            i += 1;
            continue;
        }
        let start = i;
        let num_start = if with_dai { i + 1 } else { i };
        if with_dai && num_start >= chars.len() {
            break;
        }
        let mut num_end = num_start;
        while num_end < chars.len() && is_numeral_char(chars[num_end]) {
            num_end += 1;
        }
        if num_end == num_start {
            i = start + 1;
            continue;
        }
        if num_end >= chars.len() || !is_unit(chars[num_end]) {
            i = start + 1;
            continue;
        }
        let main: String = chars[num_start..num_end].iter().collect();
        if parse_legal_numeral(&main).is_none() {
            i = start + 1;
            continue;
        }
        let unit = chars[num_end];
        let after_unit = num_end + 1;
        let (branch, end) = take_branch(&chars, after_unit);
        out.push(LegalRef {
            start,
            end,
            main,
            unit,
            branch,
        });
        i = end;
    }
    out
}

fn format_cite(n: u32, unit: char, branch: Option<u32>, style: NumStyle) -> String {
    let mut s = String::from("第");
    s.push_str(&format_num(n, style));
    s.push(unit);
    if let Some(bn) = branch {
        s.push('の');
        s.push_str(&format_num(bn, style));
    }
    s
}

const NUM_STYLES: [NumStyle; 5] = [
    NumStyle::Arabic,
    NumStyle::Fullwidth,
    NumStyle::DigitZero,
    NumStyle::DigitRei,
    NumStyle::Place,
];

/// True when the text carries a 第…条 / …項 / …号 style citation.
pub fn has_legal_ref(text: &str) -> bool {
    !find_legal_refs(text).is_empty()
}

/// Char ranges `[start, end)` of every citation in `text`.
///
/// Retrieval builds the citation clause from `legal_ref_cite_variants`, so the same
/// characters must not also become free search units — that would inflate the
/// `minimum_number_should_match` denominator with terms the citation clause already
/// requires.
pub fn legal_ref_spans(text: &str) -> Vec<(usize, usize)> {
    find_legal_refs(text)
        .into_iter()
        .map(|r| (r.start, r.end))
        .collect()
}

/// Replace every citation span with spaces, keeping char offsets stable.
pub fn mask_legal_refs(text: &str) -> String {
    let spans = legal_ref_spans(text);
    if spans.is_empty() {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    for (start, end) in spans {
        for slot in chars.iter_mut().take(end.min(len)).skip(start) {
            *slot = ' ';
        }
    }
    chars.into_iter().collect()
}

/// Just the citation strings (第555条 / 第五百五十五条 / …), not the surrounding query.
/// These are meant to be tokenized and OR-ed as adjacent phrases.
pub fn legal_ref_cite_variants(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in find_legal_refs(text) {
        let Some(n) = parse_legal_numeral(&r.main).filter(|n| *n <= 999_999) else {
            continue;
        };
        let branch = r.branch.as_deref().and_then(parse_legal_numeral);
        for style in NUM_STYLES {
            let cite = format_cite(n, r.unit, branch, style);
            if !cite.is_empty() && !out.contains(&cite) {
                out.push(cite);
            }
        }
    }
    out
}

fn rewrite_with(text: &str, style: NumStyle) -> String {
    let refs = find_legal_refs(text);
    if refs.is_empty() {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut at = 0usize;
    for r in refs {
        out.extend(chars[at..r.start].iter());
        let n = match parse_legal_numeral(&r.main) {
            Some(n) if n <= 999_999 => n,
            _ => {
                out.extend(chars[r.start..r.end].iter());
                at = r.end;
                continue;
            }
        };
        out.push('第');
        out.push_str(&format_num(n, style));
        out.push(r.unit);
        if let Some(b) = &r.branch {
            if let Some(bn) = parse_legal_numeral(b) {
                out.push('の');
                out.push_str(&format_num(bn, style));
            }
        }
        at = r.end;
    }
    out.extend(chars[at..].iter());
    out
}

/// Rewrite 第…条/項/号 to Arabic numerals (第五〇九条 → 第509条).
/// Applied to both the query and the candidate body so every spelling of a
/// citation collapses to one form before proximity scoring.
pub fn normalize_legal_refs(text: &str) -> String {
    rewrite_with(text, NumStyle::Arabic)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_digit_kanji_with_zero() {
        assert_eq!(parse_legal_numeral("五〇九"), Some(509));
        assert_eq!(parse_legal_numeral("五五五"), Some(555));
        assert_eq!(parse_legal_numeral("一〇"), Some(10));
        assert_eq!(parse_legal_numeral("一〇〇"), Some(100));
    }

    #[test]
    fn parses_place_value() {
        assert_eq!(parse_legal_numeral("五百九"), Some(509));
        assert_eq!(parse_legal_numeral("五百五十五"), Some(555));
        assert_eq!(parse_legal_numeral("十"), Some(10));
        assert_eq!(parse_legal_numeral("十一"), Some(11));
        assert_eq!(parse_legal_numeral("百"), Some(100));
        assert_eq!(parse_legal_numeral("百一"), Some(101));
        assert_eq!(parse_legal_numeral("二十一"), Some(21));
    }

    #[test]
    fn formats_place_and_digits() {
        assert_eq!(format_place_kanji(555), "五百五十五");
        assert_eq!(format_place_kanji(509), "五百九");
        assert_eq!(format_place_kanji(10), "十");
        assert_eq!(format_place_kanji(11), "十一");
        assert_eq!(format_place_kanji(100), "百");
        assert_eq!(format_digit_kanji(509, '〇'), "五〇九");
        assert_eq!(format_digit_kanji(555, '〇'), "五五五");
        assert_eq!(format_digit_kanji(10, '〇'), "一〇");
    }

    #[test]
    fn arabic_query_expands_to_kanji_forms() {
        let v = legal_ref_cite_variants("第555条");
        assert!(v.contains(&"第555条".into()));
        assert!(v.contains(&"第五五五条".into()), "{v:?}");
        assert!(v.contains(&"第五百五十五条".into()), "{v:?}");
        let v509 = legal_ref_cite_variants("第509条");
        assert!(v509.contains(&"第五〇九条".into()), "{v509:?}");
        assert!(v509.contains(&"第五百九条".into()), "{v509:?}");
    }

    #[test]
    fn kanji_query_normalizes_to_arabic() {
        assert_eq!(normalize_legal_refs("民法第五五五条"), "民法第555条");
        assert_eq!(normalize_legal_refs("第五〇九条"), "第509条");
        assert_eq!(normalize_legal_refs("第五百九条"), "第509条");
        assert_eq!(normalize_legal_refs("第五百五十五条の二"), "第555条の2");
    }

    #[test]
    fn ten_has_both_digit_and_place() {
        let v = legal_ref_cite_variants("第10条");
        assert!(v.contains(&"第一〇条".into()), "{v:?}");
        assert!(v.contains(&"第十条".into()), "{v:?}");
    }

    #[test]
    fn cite_variants_are_short_not_whole_sentence() {
        let v = legal_ref_cite_variants("民法第555条の条文を示して");
        assert!(v.contains(&"第555条".into()), "{v:?}");
        assert!(v.contains(&"第五五五条".into()), "{v:?}");
        assert!(v.contains(&"第五百五十五条".into()), "{v:?}");
        assert!(v.iter().all(|s| !s.contains("民法")), "{v:?}");
        assert!(v.iter().all(|s| !s.contains("条文")), "{v:?}");
    }

    #[test]
    fn arabic_without_dai_still_expands() {
        let v = legal_ref_cite_variants("民法555条");
        assert!(v.contains(&"第555条".into()), "{v:?}");
        assert!(v.contains(&"第五百五十五条".into()), "{v:?}");
    }

    #[test]
    fn kanji_query_expands_to_arabic_and_back() {
        let v = legal_ref_cite_variants("民法第五百五十五条");
        assert!(v.contains(&"第555条".into()), "{v:?}");
        assert!(v.contains(&"第五五五条".into()), "{v:?}");
        assert!(v.contains(&"第五百五十五条".into()), "{v:?}");
    }

    #[test]
    fn variants_are_deduped() {
        // 555 has no zero digit, so DigitZero and DigitRei collapse to one form.
        let v = legal_ref_cite_variants("第555条");
        let mut sorted = v.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), v.len(), "{v:?}");
    }

    #[test]
    fn unit_is_required_so_dates_are_not_citations() {
        assert!(!has_legal_ref("2026年5月15日"));
        assert!(!has_legal_ref("555"));
        assert!(!has_legal_ref("残高は555円です"));
        assert!(has_legal_ref("民法555条"));
        assert!(has_legal_ref("第五百五十五条"));
        assert!(legal_ref_cite_variants("2026年5月15日").is_empty());
    }

    #[test]
    fn normalize_is_identity_without_refs() {
        assert_eq!(normalize_legal_refs("売買契約について"), "売買契約について");
        assert_eq!(normalize_legal_refs(""), "");
    }
}
