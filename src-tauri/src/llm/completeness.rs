//! After a reply finishes, decide whether the body is usable.
//! A bad body is sent back once, with an instruction to put the conclusion
//! in the answer. A second failure is kept and marked incomplete.
//! Statute-article checks are intentionally not part of this.

/// Added when the body is short or the reasoning hid the conclusion.
pub const CONCLUSION_HINT: &str =
    "結論を回答本文に最後まで書いてください。思考の中だけで述べず、本文が途中で切れないようにしてください。";

/// Added when the body stopped midway. The first answer stays; this asks for the rest.
pub const CONTINUE_HINT: &str =
    "回答本文が途中で切れています。すでに書いた本文はそのまま残し、その続きだけを回答本文に書いてください。最初から書き直さないでください。";

/// Live status while a short or reasoning-heavy answer is rewritten.
pub const RETRY_STATUS: &str = "結論を本文に書き直しています";

/// Live status while a cut-off answer is continued.
pub const CONTINUE_STATUS: &str = "切れた回答の続きを書いています";

/// Shown on an answer that was rewritten and then passed the check.
pub const RETRY_LABEL: &str = "結論を本文に書き直しました";

/// Shown when a cut-off answer was completed by appending the continuation.
pub const CONTINUE_LABEL: &str = "途中で切れた回答の続きを補いました";

/// Shown when the one retry still fails the check.
pub const INCOMPLETE_MARK: &str = "不完全な可能性";

/// Stored on the assistant row. Not a notice: the answer stays in later context.
pub const KIND_RETRIED: &str = "retried";
pub const KIND_CONTINUED: &str = "continued";
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
    // Tools ran, or this send wrote a file or a note. A short report is then enough.
    produced_work: bool,
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
    match assess_answer(question, body, reasoning, finish_reason) {
        Some(AnswerProblem::ShortBody) if produced_work => None,
        other => other,
    }
}

/// Marker placed above a continuation so it stays in the same answer
/// without being spliced into the truncated text.
pub const CONTINUATION_MARK: &str = "（続き）";

/// Keep `original` unchanged and place `continuation` under it.
/// A blank line and 「（続き）」 stop a markdown table or paragraph from merging.
pub fn append_continuation(original: &str, continuation: &str) -> String {
    if continuation.is_empty() {
        return original.to_string();
    }
    let mut out = String::with_capacity(original.len() + continuation.len() + 16);
    out.push_str(original);
    if !(original.ends_with("\n\n") || original.ends_with("\r\n\r\n")) {
        if !original.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(CONTINUATION_MARK);
    out.push('\n');
    out.push_str(continuation);
    out
}

pub fn is_conclusion_prompt(text: &str) -> bool {
    let text = text.trim();
    text.starts_with(CONCLUSION_HINT) || text.starts_with(CONTINUE_HINT)
}

/// The retry instruction. A long reasoning tail is attached so the conclusion
/// can be copied out without another full thinking pass.
pub fn conclusion_retry_prompt(reasoning: &str) -> String {
    prompt_with_tail(CONCLUSION_HINT, reasoning)
}

pub fn continuation_retry_prompt(reasoning: &str) -> String {
    prompt_with_tail(CONTINUE_HINT, reasoning)
}

fn prompt_with_tail(hint: &str, reasoning: &str) -> String {
    let tail = last_chars(reasoning.trim(), HINT_REASONING_TAIL);
    if tail.is_empty() {
        return hint.to_string();
    }
    format!("{hint}\n\nさきほどの思考の末尾を、回答本文の結論に使ってください。思考の末尾:\n{tail}")
}

fn body_is_extremely_short(question: &str, body: &str) -> bool {
    // A finished sentence is a real answer, even when the question was long.
    if ends_with_sentence_end(body) {
        return false;
    }
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
    // Parentheses are left open in titles and asides ("(内容証明"). That is not a cut-off answer.
    let mut corner = 0i32;
    let mut lent = 0i32;
    for ch in text.chars() {
        match ch {
            '「' => corner += 1,
            '」' => corner = (corner - 1).max(0),
            '『' => lent += 1,
            '』' => lent = (lent - 1).max(0),
            _ => {}
        }
    }
    corner > 0 || lent > 0
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
    let Some(last) = last_nonempty_line(text) else {
        return false;
    };
    // A bullet is its own item. " - 通知の時期について" is not a sentence that stopped early.
    if is_bullet_line(last) {
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
    // Connectives that leave the clause unfinished. Topic markers such as について
    // close a heading or a one-line answer and are not a cut.
    const TAILS: &[&str] = &["ので", "ですが", "けれど", "けど"];
    TAILS.iter().any(|tail| t.ends_with(tail))
}

fn last_nonempty_line(text: &str) -> Option<&str> {
    text.lines().rev().find(|line| !line.trim().is_empty())
}

fn is_bullet_line(line: &str) -> bool {
    let t = line.trim();
    if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ") || t.starts_with('・') {
        return true;
    }
    let rest = t.trim_start_matches(|c: char| c.is_ascii_digit());
    rest != t && (rest.starts_with(". ") || rest.starts_with(") ") || rest.starts_with('．'))
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
            assess_answer(&question, "はい", "", "stop"),
            Some(AnswerProblem::ShortBody)
        );
        assert_eq!(
            assess_answer(&question, "はい。", "", "stop"),
            None,
            "a finished sentence is not a short fragment"
        );
        assert_eq!(
            assess_answer(&question, "有効です。公序良俗に反しません。", "", "stop"),
            None
        );
        assert_eq!(
            assess_answer(
                &question,
                "解除できません。根拠は民法第541条です。[1]",
                "",
                "stop"
            ),
            None
        );
        assert_eq!(
            assess_answer(&question, "契約書のWordファイルを作成しました。", "", "stop"),
            None
        );
        assert_eq!(
            assess_answer(&question, "ノートに追記しました。", "", "stop"),
            None
        );
        assert_eq!(assess_answer(&question, &full_answer(), "", "stop"), None);
        assert_eq!(
            finished_answer_problem(&question, "作成しました", "", "stop", false, false, true),
            None,
            "a short report after tool work is not retried for length"
        );
        assert_eq!(
            finished_answer_problem(&question, "作成しました", "", "stop", false, false, false),
            Some(AnswerProblem::ShortBody)
        );
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
        let open_paren = format!("{}(内容証明", "あ".repeat(80));
        assert_eq!(assess_answer(&question, &open_paren, "", "stop"), None);
        assert_eq!(
            assess_answer(&question, "前置きです。続きは（", "", "stop"),
            Some(AnswerProblem::ShortBody),
            "an open parenthesis is not a cut-off answer"
        );
        let bullet = format!("{}\n- 通知の時期について", "あ".repeat(80));
        assert_eq!(assess_answer(&question, &bullet, "", "stop"), None);
        let bullet_clause = format!("{}\n- 契約が重大なので", "あ".repeat(80));
        assert_eq!(
            assess_answer(&question, &bullet_clause, "", "stop"),
            None,
            "the last bullet is not a dangling clause"
        );
        assert_eq!(
            assess_answer("解除できる？", "解除の可否と損害賠償の範囲について", "", "stop"),
            None
        );
        assert_ne!(
            assess_answer(&question, "解除の可否と損害賠償の範囲について", "", "stop"),
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
            assess_answer(&longer_line, "はい", "", "stop"),
            Some(AnswerProblem::ShortBody),
            "a long single line is no longer a one-line question"
        );
        assert_eq!(assess_answer(&longer_line, "はい。", "", "stop"), None);
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
                true,
                false
            ),
            None
        );
        assert_eq!(
            finished_answer_problem(&long_question(), "", "", "stop", false, false, false),
            None,
            "a blank save is the existing empty path"
        );
        let notice = "ツール往復が上限の 8 回に達したため、残りの操作はしていません。";
        assert_eq!(
            finished_answer_problem(&long_question(), notice, "", "stop", true, false, false),
            None
        );
        assert_eq!(
            finished_answer_problem(
                &long_question(),
                notice,
                &"考".repeat(4_000),
                "stop",
                true,
                true,
                false
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
        assert!(is_conclusion_prompt(CONTINUE_HINT));
        assert!(!is_conclusion_prompt("契約を説明してください。"));
        assert_eq!(CONTINUATION_MARK, "（続き）");
        assert_eq!(INCOMPLETE_MARK, "不完全な可能性");
        assert_eq!(CONTINUE_LABEL, "途中で切れた回答の続きを補いました");
        assert_ne!(KIND_RETRIED, "notice");
        assert_ne!(KIND_CONTINUED, "notice");
        assert_ne!(KIND_INCOMPLETE, "notice");
    }

    fn appended(original: &str, continuation: &str) -> String {
        format!("{original}\n\n{mark}\n{continuation}", mark = CONTINUATION_MARK)
    }

    #[test]
    fn append_continuation_leaves_the_original_and_adds_the_marker() {
        let original = "契約は有効で";
        let continuation = "あり、公序良俗に反しません。";
        let joined = append_continuation(original, continuation);
        assert!(joined.starts_with(original), "{joined}");
        assert_eq!(joined, appended(original, continuation));
        assert!(joined.contains("\n\n（続き）\n"));
        assert!(joined.ends_with(continuation));

        let repeated = "前半です、";
        let again = "前半です、\n後半です。";
        let joined = append_continuation(repeated, again);
        assert_eq!(joined, appended(repeated, again));
        assert_eq!(joined.matches("前半です、").count(), 2, "both copies stay: {joined}");
    }

    #[test]
    fn a_cut_continuation_is_still_appended_and_marked_truncated() {
        let question = long_question();
        let original = format!("{}、", "あ".repeat(40));
        let continuation = "通知の期限は";
        let joined = append_continuation(&original, continuation);
        assert!(joined.starts_with(&original));
        assert!(joined.ends_with(continuation));
        assert_eq!(
            assess_answer(&question, continuation, "", "length"),
            Some(AnswerProblem::Truncated)
        );
        assert_eq!(
            assess_answer(&question, continuation, "", "max_tokens"),
            Some(AnswerProblem::Truncated)
        );
        assert_eq!(
            assess_answer(&question, "以上が結論です。解除できます。", "", "stop"),
            None
        );
    }

    #[test]
    fn a_table_cut_mid_row_keeps_both_the_row_and_the_continuation() {
        let original = "| 項目 | 内容 |\n| --- | --- |\n| 損害";
        let continuation = "賠償を請求できます。 |";
        let joined = append_continuation(original, continuation);
        assert!(joined.starts_with(original), "{joined}");
        assert_eq!(joined, appended(original, continuation));
        assert!(joined.contains("| 損害\n\n（続き）\n賠償を請求できます。 |"), "{joined}");
        assert_eq!(joined.matches("| 損害").count(), 1, "{joined}");
        assert_eq!(joined.matches("賠償を請求できます。 |").count(), 1, "{joined}");
    }

    #[test]
    fn a_continuation_that_stops_on_length_is_truncated() {
        let question = long_question();
        let finished = format!("{}。", "あ".repeat(80));
        assert_eq!(
            assess_answer(&question, &finished, "", "length"),
            Some(AnswerProblem::Truncated)
        );
        assert_eq!(
            assess_answer(&question, &finished, "", "max_tokens"),
            Some(AnswerProblem::Truncated)
        );
        assert_eq!(assess_answer(&question, &finished, "", "stop"), None);
    }
}
