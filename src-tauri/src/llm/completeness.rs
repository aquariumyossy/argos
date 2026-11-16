//! After a reply finishes, decide whether the body is usable.
//! A bad body is sent back once, with an instruction to put the conclusion
//! in the answer. A second failure is kept and marked incomplete.
//! Statute-article checks are intentionally not part of this.

/// Added on the single retry. The model must land the conclusion in the body.
pub const CONCLUSION_HINT: &str =
    "結論を回答本文に最後まで書いてください。思考の中だけで述べず、本文が途中で切れないようにしてください。";

/// Live status while that retry streams. The saved row uses a shorter label.
pub const RETRY_STATUS: &str = "結論を本文に書き直しています";

/// Shown on an answer that was rewritten and then passed the check.
pub const RETRY_LABEL: &str = "結論を本文に書き直しました";

/// Shown when the one retry still fails the check.
pub const INCOMPLETE_MARK: &str = "不完全な可能性";

/// Stored on the assistant row. Not a notice: the answer stays in later context.
pub const KIND_RETRIED: &str = "retried";
pub const KIND_INCOMPLETE: &str = "incomplete";

/// How much of the reasoning tail is pasted into the retry instruction.
const HINT_REASONING_TAIL: usize = 1_200;

/// One line at or under this length may be answered in a few words.
const BRIEF_QUESTION_CHARS: usize = 80;

/// A substantive question expects at least this many characters, and no more than this.
const SHORT_FLOOR_MIN: usize = 24;
const SHORT_FLOOR_MAX: usize = 64;

/// Reasoning shorter than this is a scratch note, not a hidden argument.
const REASONING_HEAVY_MIN: usize = 1_500;

/// A body at or above this is already a real answer, even next to long reasoning.
const NEARLY_EMPTY_BODY: usize = 160;

/// Reasoning must be more than this many times the body before it "holds" the answer.
const REASONING_TO_BODY: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnswerProblem {
    /// The body is far shorter than the question calls for.
    ShortBody,
    /// The body is nearly empty and the reasoning holds the argument.
    ReasoningHeavy,
    /// The body stops midway (length cap, open fence, dangling clause).
    Truncated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletenessStep {
    /// Save this attempt. The caller labels a retry that reached here.
    Keep,
    /// Ask once more. Only the first failing attempt returns this.
    RetryOnce,
    /// The retry was already used and this attempt still fails.
    Incomplete,
}

/// One retry, then stop. A later failure is marked, not sent again.
pub struct CompletenessRetry {
    used: bool,
}

impl CompletenessRetry {
    pub fn new() -> Self {
        Self { used: false }
    }

    pub fn used(&self) -> bool {
        self.used
    }

    pub fn observe(&mut self, problem: bool) -> CompletenessStep {
        if !self.used {
            if !problem {
                return CompletenessStep::Keep;
            }
            self.used = true;
            return CompletenessStep::RetryOnce;
        }
        if problem {
            CompletenessStep::Incomplete
        } else {
            CompletenessStep::Keep
        }
    }
}

/// What is wrong with a finished model body. `None` means it can be saved.
pub fn assess_answer(
    question: &str,
    body: &str,
    reasoning: &str,
    finish_reason: &str,
) -> Option<AnswerProblem> {
    let body = body.trim();
    if looks_cut_off(body, finish_reason) {
        return Some(AnswerProblem::Truncated);
    }
    if reasoning_hides_the_answer(question, body, reasoning) {
        return Some(AnswerProblem::ReasoningHeavy);
    }
    if body_is_extremely_short(question, body) {
        return Some(AnswerProblem::ShortBody);
    }
    None
}

/// The send loop's policy. An app notice is not a model answer.
/// An empty body is retried only when the reasoning holds the argument.
/// A blank save (no notice, no reasoning) stays the caller's existing path.
pub fn finished_answer_problem(
    question: &str,
    body: &str,
    reasoning: &str,
    finish_reason: &str,
    app_notice: bool,
    empty_model_body: bool,
) -> Option<AnswerProblem> {
    if app_notice && !empty_model_body {
        return None;
    }
    if empty_model_body {
        return assess_answer(question, "", reasoning, finish_reason)
            .filter(|problem| *problem == AnswerProblem::ReasoningHeavy);
    }
    if body.trim().is_empty() {
        return None;
    }
    assess_answer(question, body, reasoning, finish_reason)
}

pub fn is_conclusion_prompt(text: &str) -> bool {
    text.trim().starts_with(CONCLUSION_HINT)
}

/// The retry instruction. A long reasoning tail is attached so the conclusion
/// can be copied out without another full thinking pass.
pub fn conclusion_retry_prompt(reasoning: &str) -> String {
    let tail = last_chars(reasoning.trim(), HINT_REASONING_TAIL);
    if tail.is_empty() {
        return CONCLUSION_HINT.to_string();
    }
    format!("{CONCLUSION_HINT}\n\nさきほどの思考の末尾を、回答本文の結論に使ってください。思考の末尾:\n{tail}")
}

fn body_is_extremely_short(question: &str, body: &str) -> bool {
    chars(body) < short_body_limit(question)
}

/// Greetings and one-line questions accept a short reply.
/// A longer question raises the floor, and never above its own length.
fn short_body_limit(question: &str) -> usize {
    if allows_brief_answer(question) {
        return 1;
    }
    let n = chars(question.trim());
    let scaled = (n / 8).clamp(SHORT_FLOOR_MIN, SHORT_FLOOR_MAX);
    scaled.min(n.max(1))
}

fn allows_brief_answer(question: &str) -> bool {
    let q = question.trim();
    if q.is_empty() || is_greeting(q) {
        return true;
    }
    let one_line = !q.contains('\n') && !q.contains('\r');
    one_line && chars(q) <= BRIEF_QUESTION_CHARS
}

fn is_greeting(question: &str) -> bool {
    const GREETINGS: &[&str] = &[
        "こんにちは",
        "こんばんは",
        "おはよう",
        "おはようございます",
        "ありがとう",
        "ありがとうございます",
        "どうもありがとう",
        "どうもありがとうございます",
        "よろしく",
        "よろしくお願いします",
        "よろしくお願いいたします",
        "お疲れ",
        "お疲れ様",
        "お疲れ様です",
        "おつかれ",
        "おつかれさま",
        "おつかれさまです",
        "さようなら",
        "さよなら",
        "はじめまして",
        "初めまして",
        "hello",
        "hi",
        "hey",
        "thanks",
        "thank you",
        "good morning",
        "good evening",
        "good night",
    ];
    let key = greeting_key(question);
    GREETINGS.iter().any(|greeting| key == *greeting)
}

fn greeting_key(question: &str) -> String {
    question
        .trim()
        .to_lowercase()
        .trim_matches(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    '!' | '！'
                        | '?'
                        | '？'
                        | '。'
                        | '、'
                        | '.'
                        | ','
                        | '〜'
                        | '~'
                        | 'ー'
                        | '…'
                        | '　'
                )
        })
        .to_string()
}

fn reasoning_hides_the_answer(question: &str, body: &str, reasoning: &str) -> bool {
    let body_n = chars(body);
    let reason_n = chars(reasoning.trim());
    if reason_n < REASONING_HEAVY_MIN || body_n >= NEARLY_EMPTY_BODY {
        return false;
    }
    if body_n.saturating_mul(REASONING_TO_BODY) >= reason_n {
        return false;
    }
    // A greeting or one-liner is allowed to stay short. Long thinking beside
    // that reply is not a missing conclusion.
    if allows_brief_answer(question) && body_n > 0 {
        return false;
    }
    true
}

fn looks_cut_off(body: &str, finish_reason: &str) -> bool {
    if body.is_empty() {
        return false;
    }
    if stopped_for_length(finish_reason) {
        return true;
    }
    unclosed_fence(body) || unclosed_bracket(body) || ends_on_heading(body) || ends_mid_clause(body)
}

fn stopped_for_length(finish_reason: &str) -> bool {
    let reason = finish_reason.trim().to_ascii_lowercase();
    reason == "length" || reason == "max_tokens"
}

fn unclosed_fence(text: &str) -> bool {
    text.lines()
        .filter(|line| line.trim_start().starts_with("```"))
        .count()
        % 2
        == 1
}

fn unclosed_bracket(text: &str) -> bool {
    if ends_with_sentence_end(text) {
        return false;
    }
    let mut round = 0i32;
    let mut paren = 0i32;
    let mut corner = 0i32;
    let mut lent = 0i32;
    for ch in text.chars() {
        match ch {
            '(' => round += 1,
            ')' => round = (round - 1).max(0),
            '（' => paren += 1,
            '）' => paren = (paren - 1).max(0),
            '「' => corner += 1,
            '」' => corner = (corner - 1).max(0),
            '『' => lent += 1,
            '』' => lent = (lent - 1).max(0),
            _ => {}
        }
    }
    round > 0 || paren > 0 || corner > 0 || lent > 0
}

fn ends_on_heading(text: &str) -> bool {
    let Some(last) = text.lines().rev().find(|line| !line.trim().is_empty()) else {
        return false;
    };
    let t = last.trim();
    let hashes = t.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&hashes) {
        return false;
    }
    let after: String = t.chars().skip(hashes).collect();
    after.starts_with(' ') || after.starts_with('　')
}

fn ends_mid_clause(text: &str) -> bool {
    if ends_with_sentence_end(text) {
        return false;
    }
    let t = text.trim_end();
    if t.chars()
        .last()
        .is_some_and(|c| matches!(c, '、' | '，' | ',' | '：' | ':' | '・'))
    {
        return true;
    }
    if chars(t) < 12 {
        return false;
    }
    const TAILS: &[&str] = &[
        "ので",
        "ですが",
        "しかし",
        "また",
        "および",
        "について",
        "により",
        "として",
        "けれど",
        "だから",
        "そして",
        "または",
        "けど",
    ];
    TAILS.iter().any(|tail| t.ends_with(tail))
}

fn ends_with_sentence_end(text: &str) -> bool {
    let mut t = text.trim_end();
    loop {
        let bytes = t.as_bytes();
        if bytes.last() != Some(&b']') {
            break;
        }
        let Some(open) = t.rfind('[') else {
            break;
        };
        let inside = &t[open + 1..t.len() - 1];
        if inside.is_empty() || !inside.bytes().all(|c| c.is_ascii_digit()) {
            break;
        }
        t = t[..open].trim_end();
    }
    matches!(
        t.chars().last(),
        Some('。' | '！' | '？' | '!' | '?' | '」' | '』' | '）' | ')' | '.')
    )
}

fn chars(text: &str) -> usize {
    text.chars().count()
}

fn last_chars(text: &str, max_chars: usize) -> String {
    let count = chars(text);
    if count <= max_chars {
        return text.to_string();
    }
    text.chars().skip(count - max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn long_question() -> String {
        format!(
            "賃貸借契約の解除について説明してください。{}\n通知の期限と損害賠償の有無も書いてください。",
            "要点を落とさず、根拠を分けて書いてください。".repeat(3)
        )
    }

    fn full_answer() -> String {
        let sentence = "解除できるのは、相手の債務不履行が契約の目的を達せないほど重大な場合です。事前に通知したうえで解除し、生じた損害は賠償を請求できます。軽微な不履行だけでは解除できません。";
        format!("{sentence}{sentence}")
    }

    #[test]
    fn a_short_body_on_a_substantive_question_is_a_problem() {
        let question = long_question();
        assert!(chars(&question) > BRIEF_QUESTION_CHARS);
        assert_eq!(
            assess_answer(&question, "はい。", "", "stop"),
            Some(AnswerProblem::ShortBody)
        );
        assert_eq!(assess_answer(&question, &full_answer(), "", "stop"), None);
    }

    #[test]
    fn an_empty_body_with_a_long_reasoning_is_reasoning_heavy() {
        let question = long_question();
        let reasoning = "考".repeat(34_000);
        assert_eq!(
            assess_answer(&question, "", &reasoning, "stop"),
            Some(AnswerProblem::ReasoningHeavy)
        );
        let benchmark = "あ".repeat(137);
        assert_eq!(
            assess_answer(&question, &benchmark, &reasoning, "stop"),
            Some(AnswerProblem::ReasoningHeavy),
            "137 characters against 34k of reasoning is the hidden-argument case"
        );
        assert_eq!(
            assess_answer(&question, &full_answer(), &reasoning, "stop"),
            None,
            "a full body is kept even when reasoning is long"
        );
    }

    #[test]
    fn a_cut_off_answer_is_truncated() {
        let question = long_question();
        let finished = full_answer();
        assert_eq!(
            assess_answer(&question, &finished, "", "length"),
            Some(AnswerProblem::Truncated)
        );
        assert_eq!(
            assess_answer(&question, &finished, "", "max_tokens"),
            Some(AnswerProblem::Truncated)
        );
        let dangling = format!("{}、", "あ".repeat(80));
        assert_eq!(
            assess_answer(&question, &dangling, "", "stop"),
            Some(AnswerProblem::Truncated)
        );
        let open_fence = format!("{finished}\n```\nflowchart LR\nA-->B");
        assert_eq!(
            assess_answer(&question, &open_fence, "", "stop"),
            Some(AnswerProblem::Truncated)
        );
        let heading = format!("{finished}\n## 結論");
        assert_eq!(
            assess_answer(&question, &heading, "", "stop"),
            Some(AnswerProblem::Truncated)
        );
        let closed = format!("{finished}\n```\nflowchart LR\nA-->B\n```");
        assert_eq!(assess_answer(&question, &closed, "", "stop"), None);
        assert_eq!(
            assess_answer(&question, "前置きです。続きは（", "", "stop"),
            Some(AnswerProblem::Truncated)
        );
    }

    #[test]
    fn a_short_greeting_or_one_line_question_is_fine() {
        assert_eq!(
            assess_answer("こんにちは", "こんにちは！", "", "stop"),
            None
        );
        assert_eq!(assess_answer("こんにちは！", "はい", "", "stop"), None);
        assert_eq!(assess_answer("Hello!", "Hi.", "", "stop"), None);
        assert_eq!(
            assess_answer("今日の天気は？", "晴れです。", "", "stop"),
            None
        );
        assert_eq!(
            assess_answer("調べて", "根拠は [9] です。", "", "stop"),
            None
        );
        let thought = "考".repeat(4_000);
        assert_eq!(
            assess_answer("こんにちは", "こんにちは！", &thought, "stop"),
            None,
            "a finished greeting stays put even when thinking ran long"
        );
        let one_line = "あ".repeat(40);
        assert_eq!(assess_answer(&one_line, "はい。", "", "stop"), None);
        let longer_line = "あ".repeat(90);
        assert_eq!(
            assess_answer(&longer_line, "はい。", "", "stop"),
            Some(AnswerProblem::ShortBody),
            "a long single line is no longer a one-line question"
        );
    }

    #[test]
    fn an_empty_notice_is_not_retried_unless_reasoning_holds_the_answer() {
        assert_eq!(
            finished_answer_problem(
                "調べて",
                "モデルが本文も操作も返さずに終わりました。",
                "",
                "stop",
                true,
                true
            ),
            None
        );
        assert_eq!(
            finished_answer_problem(&long_question(), "", "", "stop", false, false),
            None,
            "a blank save is the existing empty path"
        );
        let notice = "ツール往復が上限の 8 回に達したため、残りの操作はしていません。";
        assert_eq!(
            finished_answer_problem(&long_question(), notice, "", "stop", true, false),
            None
        );
        assert_eq!(
            finished_answer_problem(
                &long_question(),
                notice,
                &"考".repeat(4_000),
                "stop",
                true,
                true
            ),
            Some(AnswerProblem::ReasoningHeavy)
        );
    }

    #[test]
    fn a_failing_answer_is_retried_once_and_a_second_failure_is_marked() {
        let mut gate = CompletenessRetry::new();
        assert_eq!(gate.observe(true), CompletenessStep::RetryOnce);
        assert!(gate.used());
        assert_eq!(gate.observe(true), CompletenessStep::Incomplete);
        assert_eq!(
            gate.observe(true),
            CompletenessStep::Incomplete,
            "a third attempt must not ask again"
        );
    }

    #[test]
    fn a_retry_that_passes_is_kept_and_not_sent_again() {
        let mut gate = CompletenessRetry::new();
        assert_eq!(gate.observe(false), CompletenessStep::Keep);
        assert!(!gate.used());

        let mut gate = CompletenessRetry::new();
        assert_eq!(gate.observe(true), CompletenessStep::RetryOnce);
        assert_eq!(gate.observe(false), CompletenessStep::Keep);
        assert_eq!(
            gate.observe(true),
            CompletenessStep::Incomplete,
            "passing the retry does not open another send"
        );
    }

    #[test]
    fn the_retry_prompt_carries_the_instruction_and_the_reasoning_tail() {
        assert_eq!(conclusion_retry_prompt(""), CONCLUSION_HINT);
        assert!(is_conclusion_prompt(CONCLUSION_HINT));
        let reasoning = format!("{}{}", "頭", "尾".repeat(2_000));
        let prompt = conclusion_retry_prompt(&reasoning);
        assert!(prompt.starts_with(CONCLUSION_HINT));
        assert!(prompt.contains("尾"));
        assert!(!prompt.contains('頭'));
        assert!(is_conclusion_prompt(&prompt));
        assert!(!is_conclusion_prompt("契約を説明してください。"));
        assert_eq!(INCOMPLETE_MARK, "不完全な可能性");
        assert_ne!(KIND_RETRIED, "notice");
        assert_ne!(KIND_INCOMPLETE, "notice");
    }
}
