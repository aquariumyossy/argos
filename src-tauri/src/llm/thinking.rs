//! MTPLX thinking budget. Argos does not count or cut the budget itself.
//! Twin of LexCrew-Doc `src/shared/thinking.ts` and `src-tauri/src/thinking.rs`.

use parking_lot::Mutex;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KnownBudget {
    Unknown,
    Unset,
    Tokens(u32),
}

struct BudgetCache {
    base: String,
    budget: KnownBudget,
}

static BUDGET_CACHE: Mutex<BudgetCache> = Mutex::new(BudgetCache {
    base: String::new(),
    budget: KnownBudget::Unknown,
});

pub fn known_budget_for(base: &str) -> KnownBudget {
    let cache = BUDGET_CACHE.lock();
    if cache.base == base {
        cache.budget
    } else {
        KnownBudget::Unknown
    }
}

pub fn remember_budget(base: &str, budget: KnownBudget) {
    if matches!(budget, KnownBudget::Unknown) || base.is_empty() {
        return;
    }
    let mut cache = BUDGET_CACHE.lock();
    cache.base = base.to_string();
    cache.budget = budget;
}

use crate::db::Settings;

/// The budget assumed when MTPLX shows none: what its "auto" mode gives medium.
pub const FALLBACK_THINKING_BUDGET: u32 = 3_072;

/// Observed thinking ran up to 5 characters a token; 8 keeps a capped call clear of the cut.
const RUNAWAY_CHARS_PER_TOKEN: u32 = 8;

/// MTPLX's default bridge, written just before it force-closes thinking.
pub const THINKING_BRIDGE: &str = "I have enough analysis — deciding now and acting.";

pub const THINKING_BRIDGE_ERROR: &str = "thinking-bridge";
pub const THINKING_RUNAWAY_PREFIX: &str = "thinking-runaway:";

/// `Some(Some(tokens))` when a budget is set, `Some(None)` when none is set,
/// `None` when the records do not say.
pub fn budget_from_metrics(metrics: &Value, request_id: Option<&str>) -> Option<Option<u32>> {
    let recent = metrics.get("recent")?.as_array()?;
    recent
        .iter()
        .rev()
        .filter(|record| record.is_object())
        .filter(|record| request_id.is_none_or(|id| record["request_id"] == id))
        .find_map(budget_of_record)
}

fn budget_of_record(record: &Value) -> Option<Option<u32>> {
    let guard = &record["thinking_guard"];
    if guard["enabled"] == true {
        if let Some(budget) = guard["budget_tokens"].as_u64().filter(|n| *n > 0) {
            return Some(Some(budget.min(u32::MAX as u64) as u32));
        }
    }
    let tools = record["request_tool_count"].as_u64().unwrap_or(0);
    (record["request_enable_thinking"] == true && tools > 0).then_some(None)
}

pub fn runaway_thinking_chars(budget: Option<u32>) -> usize {
    (budget.unwrap_or(FALLBACK_THINKING_BUDGET) as usize) * (RUNAWAY_CHARS_PER_TOKEN as usize)
}

pub fn thinking_enabled(settings: &Settings) -> bool {
    settings.llm_thinking.trim() != "off"
}

pub fn thinking_budget_unset_notice() -> String {
    format!(
        "MTPLX に思考の上限（--agent-thinking-budget）が設定されていません。上限がないと思考が長く続くことがあるため、作業ウィンドウは思考が {} 字を超えたところでターンを止めます。MTPLX の起動設定で上限を指定してください。",
        format_ja_int(runaway_thinking_chars(None) as u64)
    )
}

pub fn runaway_thinking_error(chars: usize) -> String {
    format!(
        "思考が {chars} 字に達しても答えが出ないため止めました。MTPLX に思考の上限が設定されているか確かめ、指示を分けて送り直してください。"
    )
}

pub fn thinking_bridge_error() -> String {
    "MTPLX が思考を閉じられず、同じ文を繰り返したため止めました。もう一度送ってください。続くときは MTPLX を再起動してください。"
        .into()
}

pub fn format_ja_int(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn guarded(id: &str, budget: u64) -> Value {
        json!({
            "request_id": id,
            "request_tool_count": 3,
            "request_enable_thinking": true,
            "thinking_guard": { "enabled": true, "budget_tokens": budget, "think_tokens": 900 }
        })
    }

    fn unguarded(id: &str) -> Value {
        json!({
            "request_id": id,
            "request_tool_count": 3,
            "request_enable_thinking": true,
            "thinking_guard": {}
        })
    }

    #[test]
    fn budget_comes_off_the_record_of_the_request() {
        let metrics = json!({ "recent": [guarded("a", 2048), guarded("b", 4096)] });
        assert_eq!(budget_from_metrics(&metrics, Some("a")), Some(Some(2048)));
        assert_eq!(
            budget_from_metrics(&json!({ "recent": [unguarded("a")] }), Some("a")),
            Some(None)
        );
        assert_eq!(budget_from_metrics(&metrics, Some("z")), None);
    }

    #[test]
    fn budget_is_unknown_when_the_guard_never_covered_the_request() {
        let plain = json!({ "recent": [{
            "request_id": "a",
            "request_tool_count": 0,
            "request_enable_thinking": true,
            "thinking_guard": {}
        }] });
        assert_eq!(budget_from_metrics(&plain, Some("a")), None);
        assert_eq!(budget_from_metrics(&json!("not metrics"), Some("a")), None);
    }

    #[test]
    fn budget_without_a_request_takes_the_newest_covered_one() {
        let plain = json!({ "request_id": "c", "request_tool_count": 0, "thinking_guard": {} });
        let unset = json!({ "recent": [guarded("a", 2048), unguarded("b"), plain.clone()] });
        assert_eq!(budget_from_metrics(&unset, None), Some(None));
        let set = json!({ "recent": [unguarded("a"), guarded("b", 2048), plain] });
        assert_eq!(budget_from_metrics(&set, None), Some(Some(2048)));
        assert_eq!(budget_from_metrics(&json!({ "recent": [] }), None), None);
    }

    #[test]
    fn runaway_uses_eight_chars_per_token_or_the_fallback() {
        assert_eq!(runaway_thinking_chars(Some(2048)), 16_384);
        assert_eq!(runaway_thinking_chars(None), 24_576);
        assert!(thinking_budget_unset_notice().contains("24,576"));
    }
}
