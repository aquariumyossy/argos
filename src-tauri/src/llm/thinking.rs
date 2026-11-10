//! MTPLX thinking budget. The token budget stays on the server.
//! Argos only stops a thinking stream that runs past the safety cap
//! (budget tokens × 8 characters) and asks once more with thinking off.
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

/// Shown after a capped thinking stream was cut and the one retry answered.
pub const THINKING_CUT_NOTICE: &str =
    "考えが長くなったため途中で打ち切り、集めた資料から答えています";

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
        "MTPLX に思考の上限（--agent-thinking-budget）が設定されていません。上限がないと思考が長く続くことがあるため、チャットは思考が {} 字を超えたところでターンを止めます。MTPLX の起動設定で上限を指定してください。",
        format_ja_int(runaway_thinking_chars(None) as u64)
    )
}

pub fn runaway_thinking_error(chars: usize) -> String {
    format!(
        "思考が {chars} 字に達しても答えが出ないため止めました。MTPLX に思考の上限が設定されているか確かめ、指示を分けて送り直してください。"
    )
}

/// The safety-cap error from a stream, as the existing user-facing failure.
pub fn finish_runaway(error: &str) -> String {
    let n = error
        .strip_prefix(THINKING_RUNAWAY_PREFIX)
        .and_then(|rest| rest.parse().ok())
        .unwrap_or(0);
    runaway_thinking_error(n)
}

/// What the in-flight completion asked for while recovering from the cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CutRequest {
    pub thinking_off: bool,
    pub tools: bool,
}

/// What that completion came back with. The caller classifies the stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutReply {
    /// Thinking ran past the character cap. The stream has already stopped.
    OverCap,
    /// The model asked for another tool call.
    NeedsTools,
    /// The server rejected the request because tools are enabled.
    ToolsRejected,
    /// A finished answer with no tool call.
    Answer,
    /// Any other failure. Existing error handling applies.
    Other,
}

/// The next step after one completion. A cut does not consume a tool round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutStep {
    /// Ask again. The session's [`ThinkingCut::request`] matches this.
    Again(CutRequest),
    /// The retry produced an answer. `tools` is false when tool calls must not run.
    Answer { tools: bool },
    /// Give up. `over_cap` selects the existing runaway error.
    Stop { over_cap: bool },
    /// The first call did not hit the cap. The caller keeps its existing path.
    Pass,
}

impl CutStep {
    /// The chat notice for a cut that actually answered. A failed retry has none.
    pub fn notice(self) -> Option<&'static str> {
        match self {
            CutStep::Answer { .. } => Some(THINKING_CUT_NOTICE),
            _ => None,
        }
    }
}

/// One thinking-off retry, then at most one tools-off fallback.
pub struct ThinkingCut {
    thinking_was_on: bool,
    request: CutRequest,
    /// 0 = first call, 1 = thinking-off retry, 2 = tools-off fallback.
    stage: u8,
}

impl ThinkingCut {
    pub fn start(thinking_on: bool, tools: bool) -> Self {
        Self {
            thinking_was_on: thinking_on,
            request: CutRequest {
                thinking_off: false,
                tools,
            },
            stage: 0,
        }
    }

    /// A later pass after this turn already cut thinking. A new cap stops.
    /// It does not turn thinking back on.
    pub fn held(tools: bool) -> Self {
        Self {
            thinking_was_on: false,
            request: CutRequest {
                thinking_off: true,
                tools,
            },
            stage: 0,
        }
    }

    pub fn request(&self) -> CutRequest {
        self.request
    }

    pub fn in_recovery(&self) -> bool {
        self.stage > 0
    }

    pub fn observe(&mut self, reply: CutReply) -> CutStep {
        if self.stage == 0 {
            if reply != CutReply::OverCap {
                return CutStep::Pass;
            }
            if !self.thinking_was_on {
                return CutStep::Stop { over_cap: true };
            }
            self.stage = 1;
            self.request.thinking_off = true;
            return CutStep::Again(self.request);
        }
        if self.stage == 1 {
            return match reply {
                CutReply::OverCap => CutStep::Stop { over_cap: true },
                CutReply::NeedsTools | CutReply::ToolsRejected if self.request.tools => {
                    self.stage = 2;
                    self.request.tools = false;
                    CutStep::Again(self.request)
                }
                CutReply::Answer => CutStep::Answer {
                    tools: self.request.tools,
                },
                CutReply::NeedsTools => CutStep::Answer { tools: false },
                CutReply::ToolsRejected | CutReply::Other => CutStep::Stop { over_cap: false },
            };
        }
        match reply {
            CutReply::Answer | CutReply::NeedsTools => CutStep::Answer { tools: false },
            CutReply::OverCap => CutStep::Stop { over_cap: true },
            CutReply::ToolsRejected | CutReply::Other => CutStep::Stop { over_cap: false },
        }
    }
}

/// What stays off for the rest of a turn after thinking was cut.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CutHold {
    pub thinking_off: bool,
    /// Set when the cut itself answered with tools disabled.
    pub tools_off: bool,
}

impl CutHold {
    pub fn observe_answer(&mut self, tools_still_on: bool) {
        self.thinking_off = true;
        if !tools_still_on {
            self.tools_off = true;
        }
    }

    /// A later pass may ask for thinking and tools again. A cut keeps thinking
    /// off, and a cut that forced tools off keeps them off.
    pub fn allow(self, settings_thinking: bool, tools_requested: bool) -> (bool, bool) {
        (
            settings_thinking && !self.thinking_off,
            tools_requested && !self.tools_off,
        )
    }

    /// The chat notice only when the kept text is the model's own answer.
    pub fn notice(self, saved_is_notice: bool, text: &str) -> Option<&'static str> {
        if !self.thinking_off || saved_is_notice || text.trim().is_empty() {
            None
        } else {
            Some(THINKING_CUT_NOTICE)
        }
    }
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
        assert_eq!(runaway_thinking_chars(Some(4_096)), 32_768);
        assert_eq!(runaway_thinking_chars(None), 24_576);
        let notice = thinking_budget_unset_notice();
        assert!(notice.contains("24,576"));
        assert!(notice.contains("チャット"));
        assert!(!notice.contains("作業ウィンドウ"));
    }

    /// Drive the same decisions the send loop makes, and record each request.
    fn replay(thinking_on: bool, tools: bool, script: &[CutReply]) -> (Vec<CutRequest>, CutStep) {
        let mut cut = ThinkingCut::start(thinking_on, tools);
        let mut seen = vec![cut.request()];
        let mut last = CutStep::Pass;
        for reply in script {
            last = cut.observe(*reply);
            match last {
                CutStep::Again(req) => seen.push(req),
                _ => break,
            }
        }
        (seen, last)
    }

    #[test]
    fn cap_exceeded_retries_with_thinking_off() {
        let (seen, step) = replay(true, true, &[CutReply::OverCap, CutReply::Answer]);
        assert_eq!(
            seen,
            vec![
                CutRequest {
                    thinking_off: false,
                    tools: true
                },
                CutRequest {
                    thinking_off: true,
                    tools: true
                },
            ]
        );
        assert_eq!(step, CutStep::Answer { tools: true });
        assert_eq!(step.notice(), Some(THINKING_CUT_NOTICE));
        assert_eq!(
            THINKING_CUT_NOTICE,
            "考えが長くなったため途中で打ち切り、集めた資料から答えています"
        );
    }

    #[test]
    fn runaway_is_retried_once_and_a_second_cap_fails_without_a_notice() {
        let (seen, step) = replay(
            true,
            true,
            &[CutReply::OverCap, CutReply::OverCap, CutReply::Answer],
        );
        assert_eq!(seen.len(), 2, "a third call must not be made");
        assert_eq!(
            seen[1],
            CutRequest {
                thinking_off: true,
                tools: true
            }
        );
        assert_eq!(step, CutStep::Stop { over_cap: true });
        assert!(step.notice().is_none());
        let err = finish_runaway(&format!("{THINKING_RUNAWAY_PREFIX}32768"));
        assert!(err.contains("32,768") || err.contains("32768"));
        assert!(err.contains("字に達しても答えが出ない"));
    }

    #[test]
    fn tools_off_fallback_when_the_thinking_off_retry_still_needs_tools() {
        let (seen, step) = replay(
            true,
            true,
            &[CutReply::OverCap, CutReply::NeedsTools, CutReply::Answer],
        );
        assert_eq!(
            seen,
            vec![
                CutRequest {
                    thinking_off: false,
                    tools: true
                },
                CutRequest {
                    thinking_off: true,
                    tools: true
                },
                CutRequest {
                    thinking_off: true,
                    tools: false
                },
            ]
        );
        assert_eq!(step, CutStep::Answer { tools: false });
        assert_eq!(step.notice(), Some(THINKING_CUT_NOTICE));
    }

    #[test]
    fn tools_off_fallback_when_the_thinking_off_retry_is_rejected() {
        let (seen, step) = replay(
            true,
            true,
            &[CutReply::OverCap, CutReply::ToolsRejected, CutReply::Answer],
        );
        assert_eq!(seen.len(), 3);
        assert_eq!(
            *seen.last().unwrap(),
            CutRequest {
                thinking_off: true,
                tools: false
            }
        );
        assert_eq!(step, CutStep::Answer { tools: false });
        assert!(step.notice().is_some());
    }

    #[test]
    fn tools_off_fallback_does_not_retry_when_it_also_hits_the_cap() {
        let (seen, step) = replay(
            true,
            true,
            &[
                CutReply::OverCap,
                CutReply::NeedsTools,
                CutReply::OverCap,
                CutReply::Answer,
            ],
        );
        assert_eq!(seen.len(), 3, "the tools-off call is the last attempt");
        assert_eq!(step, CutStep::Stop { over_cap: true });
        assert!(step.notice().is_none());
    }

    #[test]
    fn other_failure_on_the_single_retry_keeps_the_error() {
        let (seen, step) = replay(true, true, &[CutReply::OverCap, CutReply::Other]);
        assert_eq!(seen.len(), 2);
        assert_eq!(step, CutStep::Stop { over_cap: false });
        assert!(step.notice().is_none());
    }

    #[test]
    fn thinking_already_off_does_not_retry_the_cap() {
        let (seen, step) = replay(false, true, &[CutReply::OverCap, CutReply::Answer]);
        assert_eq!(seen.len(), 1);
        assert_eq!(step, CutStep::Stop { over_cap: true });
        assert!(step.notice().is_none());
    }

    #[test]
    fn later_pass_after_a_cut_stays_thinking_off() {
        let mut hold = CutHold::default();
        let (thinking, tools) = hold.allow(true, true);
        assert!(thinking && tools);
        let mut cut = ThinkingCut::start(thinking, tools);
        assert_eq!(
            cut.observe(CutReply::OverCap),
            CutStep::Again(CutRequest {
                thinking_off: true,
                tools: true
            })
        );
        assert_eq!(cut.observe(CutReply::Answer), CutStep::Answer { tools: true });
        hold.observe_answer(true);
        let (thinking, tools) = hold.allow(true, true);
        assert!(!thinking, "a source-block retry must not think again");
        assert!(tools, "tools stay available when the cut did not force them off");
        let later = ThinkingCut::held(tools);
        assert_eq!(
            later.request(),
            CutRequest {
                thinking_off: true,
                tools: true
            }
        );
        let mut later = later;
        assert_eq!(
            later.observe(CutReply::OverCap),
            CutStep::Stop { over_cap: true },
            "a later cap must not start another thinking-on retry"
        );
    }

    #[test]
    fn tools_forced_off_by_a_cut_stay_off_when_a_phantom_retry_asks() {
        let mut hold = CutHold::default();
        hold.observe_answer(false);
        let (thinking, tools) = hold.allow(true, true);
        assert!(!thinking);
        assert!(!tools, "phantom must not open a new tool round");
        assert_eq!(
            ThinkingCut::held(tools).request(),
            CutRequest {
                thinking_off: true,
                tools: false
            }
        );
    }

    #[test]
    fn cut_notice_is_only_for_a_non_empty_model_answer() {
        let mut hold = CutHold::default();
        assert!(hold.notice(false, "答え").is_none());
        hold.observe_answer(false);
        assert_eq!(hold.notice(false, "集めた結論"), Some(THINKING_CUT_NOTICE));
        assert!(hold.notice(false, "").is_none());
        assert!(hold.notice(false, " \n").is_none());
        let empty = crate::llm::context::silent_end_notice("stop");
        assert!(
            hold.notice(true, &empty).is_none(),
            "an empty finish must not also store the cut notice"
        );
        let tools_only = crate::llm::context::silent_end_notice("tool_calls");
        assert!(hold.notice(true, &tools_only).is_none());
    }

    #[test]
    fn a_tools_rejection_before_the_cap_is_not_a_cut() {
        let mut cut = ThinkingCut::start(true, true);
        assert_eq!(cut.observe(CutReply::ToolsRejected), CutStep::Pass);
        assert!(!cut.in_recovery());
        assert_eq!(
            cut.request(),
            CutRequest {
                thinking_off: false,
                tools: true
            }
        );
    }
}
