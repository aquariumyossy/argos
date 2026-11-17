//! Replace foreign hanzi in a finished chat reply, then optionally rewrite once.
//!
//! Step 1 swaps a simplified or traditional character for the one Japanese kanji
//! the Unihan table maps it to. Step 2 runs only when foreign script is still
//! left, and at most once. Latin and Greek are not foreign script.

#[path = "japanese_han_map.rs"]
mod japanese_han_map;

use std::collections::HashMap;
use std::sync::OnceLock;

use base64::Engine;

use japanese_han_map::HAN_JP_PAIRS;

/// Shown while the one rewrite call is in flight. The answer already on screen stays.
pub const REWRITE_STATUS: &str = "外国文字を書き直しています";

const UNVERIFIED_MARK: &str = "（要確認）";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HanFix {
    /// Unicode scalar index in the text these fixes belong to.
    pub at: usize,
    pub from: char,
    pub to: char,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappedReply {
    /// Text before any correction.
    pub input: String,
    pub text: String,
    pub fixes: Vec<HanFix>,
    /// Foreign script is still in `text`, so one rewrite may run.
    pub needs_rewrite: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyCorrection {
    pub content: String,
    /// Empty when `content` is the reply the model wrote.
    pub original: String,
    pub fixes: Vec<HanFix>,
    pub rewritten: bool,
}

impl ReplyCorrection {
    /// Empty when the chat has nothing to explain. Otherwise JSON for the pane.
    pub fn script_fix_json(&self) -> String {
        script_fix_json(&self.fixes, self.rewritten)
    }
}

pub fn script_fix_json(fixes: &[HanFix], rewritten: bool) -> String {
    if fixes.is_empty() && !rewritten {
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
    serde_json::json!({ "fixes": fixes, "rewritten": rewritten }).to_string()
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

fn has_foreign(text: &str) -> bool {
    !crate::llm::japanese_han::foreign_chars(&[text]).is_empty()
}

pub fn map_reply(text: &str) -> MappedReply {
    let (mapped, fixes) = map_foreign_han(text);
    MappedReply {
        input: text.to_string(),
        needs_rewrite: has_foreign(&mapped),
        text: mapped,
        fixes,
    }
}

/// Instruction for the single local completion. The model returns the whole reply.
pub fn foreign_rewrite_request(text: &str) -> String {
    format!(
        "次の文章は、すでに生成した回答です。日本語で用いない文字（簡体字・繁体字・ハングルなど）が残っています。意味と、それ以外の文字は変えず、その文字を含む箇所だけを自然な日本語（漢字の新字体・ひらがな・カタカナ）に書き直してください。書き直した全文だけを返してください。説明や前置きは不要です。\n\n---\n{text}\n---"
    )
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

/// `rewrite` is `None` when step 2 must not run. `Err` or an empty body keeps step 1.
pub fn apply_rewrite(
    mapped: MappedReply,
    rewrite: Option<Result<String, String>>,
) -> ReplyCorrection {
    if !mapped.needs_rewrite {
        return finish(&mapped.input, mapped.text, mapped.fixes, false);
    }
    let Some(Ok(raw)) = rewrite else {
        return finish(&mapped.input, mapped.text, mapped.fixes, false);
    };
    let next = normalize_rewrite(&raw);
    if next.is_empty() || next == mapped.text {
        return finish(&mapped.input, mapped.text, mapped.fixes, false);
    }
    let fixes = surviving_fixes(&mapped.text, &mapped.fixes, &next);
    finish(&mapped.input, next, fixes, true)
}

fn finish(input: &str, content: String, fixes: Vec<HanFix>, rewritten: bool) -> ReplyCorrection {
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

/// Test helper: map, and call `rewrite` only when foreign script remains.
pub fn correct_reply(
    text: &str,
    rewrite: impl FnOnce(&str) -> Result<String, String>,
) -> ReplyCorrection {
    let mapped = map_reply(text);
    let rewrite = if mapped.needs_rewrite {
        let result = rewrite(&mapped.text);
        Some(result)
    } else {
        None
    };
    apply_rewrite(mapped, rewrite)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(!map.contains_key(&('发' as u32)));
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
        let clean = correct_reply("现在の契約です。", |_| {
            calls += 1;
            Ok("呼ばれない".into())
        });
        assert_eq!(calls, 0);
        assert_eq!(clean.content, "現在の契約です。");
        assert!(!clean.rewritten);
        assert!(clean.script_fix_json().contains("现"));
        assert!(!clean.script_fix_json().contains("\"rewritten\":true"));

        calls = 0;
        let dirty = correct_reply("现在は기타です", |text| {
            calls += 1;
            assert_eq!(text, "現在は기타です");
            assert!(has_foreign(text));
            Ok(format!("{text}기"))
        });
        assert_eq!(
            calls, 1,
            "a residue that survives the rewrite is not sent again"
        );
        assert!(dirty.rewritten);
        assert!(dirty.content.contains('기'));
        assert_eq!(dirty.original, "现在は기타です");
    }

    #[test]
    fn rewrite_failure_keeps_the_mapped_text_and_the_original() {
        let failed = correct_reply("现在は기타です", |_| Err("timeout".into()));
        assert_eq!(failed.content, "現在は기타です");
        assert_eq!(failed.original, "现在は기타です");
        assert!(!failed.rewritten);
        assert_eq!(
            failed.fixes,
            vec![HanFix {
                at: 0,
                from: '现',
                to: '現'
            }]
        );
        let empty = correct_reply("기타", |_| Ok("  \n".into()));
        assert_eq!(empty.content, "기타");
        assert!(empty.original.is_empty());
        assert!(!empty.rewritten);
    }

    #[test]
    fn original_is_empty_when_nothing_changes() {
        let text = "契約を解除できる。α版";
        let kept = correct_reply(text, |_| panic!("no foreign script"));
        assert_eq!(kept.content, text);
        assert!(kept.original.is_empty());
        assert!(kept.script_fix_json().is_empty());
        assert!(!kept.rewritten);
    }

    #[test]
    fn rewrite_keeps_a_mapping_outside_the_edited_span() {
        let fixed = correct_reply("现在は기타です", |_| Ok("現在はその他です".into()));
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
