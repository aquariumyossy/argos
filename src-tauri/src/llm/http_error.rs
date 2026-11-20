//! One HTTP status, one Japanese message.
//! A response body is read only to match fixed phrases (tools, context length, overload).
//! It is not copied into the message. An in-stream `error` object is the exception:
//! its message text is shown, capped, and dropped when it looks like a credential.

use std::time::{Duration, SystemTime};

use serde_json::Value;

/// Returned for a 400 whose body says the model cannot call tools.
/// The server body is not part of this string. The tool-free retry matches it exactly.
pub const TOOLS_UNSUPPORTED_ERROR: &str = "このモデルはツール呼び出しに対応していません。";

pub const VISION_UNSUPPORTED_ERROR: &str =
    "このモデルは画像を読めません。vision 対応のモデルを設定してください。";

/// Returned for a 400 whose body says the prompt does not fit the context window.
/// The server body is not part of this string.
pub const CONTEXT_TOO_LONG_ERROR: &str =
    "この会話はモデルのコンテキストに対して長すぎます。新しい会話に切り替えるか、古い内容を減らしてください。";

/// The system prompt and the typed question do not fit even with no reserve.
pub const QUESTION_DOES_NOT_FIT_ERROR: &str =
    "この質問はモデルのコンテキストに入りません。短くしてから送ってください。";

/// 429, 503, or an overload phrase in the body. Shown when retries are exhausted.
pub(crate) const BUSY_MESSAGE: &str =
    "LLM サーバが混雑しています。しばらくしてからもう一度送ってください。";

/// TCP connect never succeeded. Any local OpenAI-compatible server, not one runtime.
pub(crate) const UNREACHABLE_MESSAGE: &str =
    "LLM サーバに接続できません。サーバが起動していることと、設定の URL が正しいことを確認してください。";

/// The socket dropped before any assistant text, reasoning, or tool call.
pub(crate) const DROPPED_BEFORE_MESSAGE: &str = "応答が届く前に LLM サーバとの接続が切れました。";

/// The socket dropped after the model had started answering. Typical of an engine restart.
pub(crate) const DROPPED_MID_MESSAGE: &str = "応答の途中で LLM サーバとの接続が切れました。";

/// Connect timeout or a transport timeout. The idle-stall sentence stays in the stream loop.
pub(crate) const TIMEOUT_MESSAGE: &str = "LLM サーバへの接続が時間切れになりました。";

pub(crate) const GENERIC_CALL_MESSAGE: &str = "LLM の呼び出しに失敗しました。";

/// In-stream `error` object with no usable message text.
pub(crate) const IN_STREAM_FALLBACK: &str = "LLM サーバがエラーを返しました。";

/// Live status while this call waits to try again. The stop button cancels the wait.
pub(crate) const RETRY_STATUS: &str = "再試行中…";

/// First try plus two retries. Each `stream_chat` call has its own budget.
pub(crate) const MAX_ATTEMPTS: u32 = 3;

/// Engine restart. The middle of the 1–2 minute window, once per retry.
pub(crate) const MID_ANSWER_RETRY_WAIT: Duration = Duration::from_secs(90);

/// `Retry-After` longer than this is capped so one header cannot hold the job.
pub(crate) const RETRY_AFTER_CAP: Duration = Duration::from_secs(120);

const IN_STREAM_MESSAGE_CHARS: usize = 200;

const TOOLS_PHRASES: &[&str] = &["does not support tools", "\"auto\" tool choice requires"];
const VISION_PHRASE: &str = "does not support image input";
/// Fixed phrases. A 400 is a context-length failure only when the body contains one of these.
const CONTEXT_PHRASES: &[&str] = &[
    "context length",
    "context_length_exceeded",
    "maximum context",
    "context window",
    "exceed_context_size",
    "exceeds the available context",
    "n_ctx",
    "prompt is too long",
    "too many tokens",
];

const OVERLOAD_PHRASES: &[&str] = &[
    "overload",
    "server is busy",
    "server busy",
    "too many requests",
    "rate limit",
    "rate_limit",
    "at capacity",
    "out of capacity",
    "engine is busy",
    "model is busy",
    "過負荷",
    "混雑",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureClass {
    Busy,
    Unreachable,
    DroppedBeforeText,
    DroppedMidAnswer,
    InStream,
    Timeout,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClassifiedFailure {
    pub class: FailureClass,
    pub message: String,
    pub retry_after: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetryPlan {
    Stop,
    Wait(Duration),
}

pub fn is_tools_unsupported_error(error: &str) -> bool {
    error == TOOLS_UNSUPPORTED_ERROR
}

pub fn is_context_too_long_error(error: &str) -> bool {
    error == CONTEXT_TOO_LONG_ERROR
}

/// Chat and OCR. 400 plus a tools phrase becomes [`TOOLS_UNSUPPORTED_ERROR`].
/// 400 plus a context-length phrase becomes [`CONTEXT_TOO_LONG_ERROR`].
/// 429, 503, and overload phrases become [`BUSY_MESSAGE`].
/// Every other status is a fixed message. The response body is never copied into it.
pub fn chat_failure(status: u16, body: &str) -> String {
    if status == 400 && body_has(body, TOOLS_PHRASES) {
        return TOOLS_UNSUPPORTED_ERROR.to_string();
    }
    if status == 400 && body_has(body, CONTEXT_PHRASES) {
        return CONTEXT_TOO_LONG_ERROR.to_string();
    }
    if is_busy_failure(status, body) {
        return BUSY_MESSAGE.to_string();
    }
    status_message(status)
}

/// OCR. Vision-unsupported is only a 400 whose body contains the image phrase.
pub fn ocr_failure(status: u16, body: &str) -> String {
    if status == 400 && body_has(body, &[VISION_PHRASE]) {
        return VISION_UNSUPPORTED_ERROR.to_string();
    }
    status_message(status)
}

/// `GET /models`. A 404 is a missing list, not a missing chat model.
pub fn models_failure(status: u16) -> String {
    if status == 404 {
        return "モデル一覧を取得できませんでした（HTTP 404）。".into();
    }
    status_message(status)
}

pub fn status_message(code: u16) -> String {
    match code {
        401 | 403 => "API キーが拒否されました。設定の API キーを確認してください。".into(),
        404 => "指定したモデルが見つかりません。モデル名を確認してください。".into(),
        413 => "送信内容が大きすぎます（HTTP 413）。".into(),
        429 | 503 => BUSY_MESSAGE.into(),
        400..=499 => format!("LLM サーバが要求を拒否しました（HTTP {code}）。"),
        500..=599 => format!("LLM サーバがエラーを返しました（HTTP {code}）。"),
        _ => GENERIC_CALL_MESSAGE.into(),
    }
}

/// True when this status or body should be retried as a busy server.
/// Tools, context length, auth, a missing model, and a too-large body win first.
pub(crate) fn is_busy_failure(status: u16, body: &str) -> bool {
    if status == 400 && (body_has(body, TOOLS_PHRASES) || body_has(body, CONTEXT_PHRASES)) {
        return false;
    }
    if matches!(status, 401 | 403 | 404 | 413) {
        return false;
    }
    status == 429 || status == 503 || body_is_overload(body)
}

/// Reqwest failures have no HTTP status. `saw_text` distinguishes a reset
/// before the answer from one that cut the answer off.
pub fn transport_failure(raw: &str) -> String {
    classify_transport(raw, false).message
}

pub(crate) fn classify_transport(raw: &str, saw_text: bool) -> ClassifiedFailure {
    let (class, message) = match transport_kind(raw) {
        TransportKind::Timeout => (FailureClass::Timeout, TIMEOUT_MESSAGE),
        TransportKind::Unreachable => (FailureClass::Unreachable, UNREACHABLE_MESSAGE),
        TransportKind::Dropped if saw_text => (FailureClass::DroppedMidAnswer, DROPPED_MID_MESSAGE),
        TransportKind::Dropped => (FailureClass::DroppedBeforeText, DROPPED_BEFORE_MESSAGE),
        TransportKind::Other => (FailureClass::Other, GENERIC_CALL_MESSAGE),
    };
    ClassifiedFailure {
        class,
        message: message.to_string(),
        retry_after: None,
    }
}

/// Message text from an in-stream `{"error": ...}` object.
/// The bool is true when the object is an overload and should use [`BUSY_MESSAGE`].
pub(crate) fn in_stream_failure(value: &Value) -> (String, bool) {
    let Some(err) = value.get("error") else {
        return (IN_STREAM_FALLBACK.to_string(), false);
    };
    let raw = error_text(err);
    if body_is_overload(&raw) || field_is_overload(err, "type") || field_is_overload(err, "code") {
        return (BUSY_MESSAGE.to_string(), true);
    }
    let cleaned = collapse_ws(&raw);
    if cleaned.is_empty() || looks_sensitive(&cleaned) {
        return (IN_STREAM_FALLBACK.to_string(), false);
    }
    let clipped: String = cleaned.chars().take(IN_STREAM_MESSAGE_CHARS).collect();
    (format!("LLM サーバがエラーを返しました: {clipped}"), false)
}

/// `attempt` is how many tries have already failed, starting at 1.
/// `Retry-After` replaces the short backoff. A mid-answer drop always waits
/// [`MID_ANSWER_RETRY_WAIT`], because that pause is for the engine to come back.
pub(crate) fn retry_plan(
    class: FailureClass,
    attempt: u32,
    retry_after: Option<Duration>,
) -> RetryPlan {
    if attempt == 0 || attempt >= MAX_ATTEMPTS {
        return RetryPlan::Stop;
    }
    let wait = match class {
        FailureClass::Busy | FailureClass::DroppedBeforeText => match retry_after {
            Some(given) => given.min(RETRY_AFTER_CAP),
            None => backoff(attempt),
        },
        FailureClass::DroppedMidAnswer => MID_ANSWER_RETRY_WAIT,
        FailureClass::Unreachable
        | FailureClass::InStream
        | FailureClass::Timeout
        | FailureClass::Other => return RetryPlan::Stop,
    };
    RetryPlan::Wait(wait)
}

pub(crate) fn retry_after_header(value: Option<&str>) -> Option<Duration> {
    parse_retry_after(value?, SystemTime::now())
}

/// `Retry-After` as delay-seconds or an IMF-fixdate. `None` means the header is absent or unusable.
pub(crate) fn parse_retry_after(raw: &str, now: SystemTime) -> Option<Duration> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    if let Ok(secs) = raw.parse::<f64>() {
        if secs.is_finite() && secs >= 0.0 {
            return Some(Duration::from_secs_f64(secs));
        }
    }
    let dt = chrono::DateTime::parse_from_rfc2822(raw).ok()?;
    if dt.timestamp() <= 0 {
        return Some(Duration::ZERO);
    }
    let target = SystemTime::UNIX_EPOCH + Duration::from_secs(dt.timestamp() as u64);
    match target.duration_since(now) {
        Ok(wait) => Some(wait),
        Err(_) => Some(Duration::ZERO),
    }
}

pub(crate) fn is_connection_message(message: &str) -> bool {
    message.contains("接続できません")
        || message.contains("届きません")
        || message.contains("接続が切れ")
        || message.contains("時間切れ")
}

/// Short clause already written by [`status_message`] or [`models_failure`].
/// Does not look for a status code in an arbitrary server body.
pub fn owned_status_clause(error: &str) -> Option<String> {
    const MARKERS: &[&str] = &[
        "サーバがエラーを返しました（HTTP ",
        "サーバが要求を拒否しました（HTTP ",
        "送信内容が大きすぎます（HTTP ",
        "リクエストを制限しています（HTTP ",
        "モデル一覧を取得できませんでした（HTTP ",
    ];
    for marker in MARKERS {
        let Some(at) = error.find(marker) else {
            continue;
        };
        let rest = &error[at..];
        let end = rest.find('。').unwrap_or(rest.len());
        return Some(rest[..end].to_string());
    }
    None
}

fn backoff(attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(3);
    Duration::from_secs(1_u64 << shift)
}

fn body_is_overload(body: &str) -> bool {
    body_has(body, OVERLOAD_PHRASES)
}

fn field_is_overload(err: &Value, field: &str) -> bool {
    err.get(field)
        .and_then(|value| value.as_str())
        .is_some_and(body_is_overload)
}

fn error_text(err: &Value) -> String {
    if let Some(text) = err.as_str() {
        return text.to_string();
    }
    if let Some(text) = err.get("message").and_then(|value| value.as_str()) {
        return text.to_string();
    }
    if let Some(text) = err.get("error").and_then(|value| value.as_str()) {
        return text.to_string();
    }
    String::new()
}

fn collapse_ws(text: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !out.is_empty() {
                pending_space = true;
            }
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(ch);
    }
    out
}

fn looks_sensitive(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("api_key")
        || lower.contains("api-key")
        || lower.contains("bearer ")
        || lower.contains("sk-")
        || lower.contains("authorization")
}

fn body_has(body: &str, phrases: &[&str]) -> bool {
    // A JSON body stores `"auto"` as `\"auto\"`. The phrases are the unescaped text.
    let lower = body.to_lowercase().replace("\\\"", "\"");
    phrases.iter().any(|phrase| lower.contains(phrase))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TransportKind {
    Timeout,
    Unreachable,
    Dropped,
    Other,
}

/// Dropped and timeout are checked before "could not connect".
/// The Windows reset text contains 「リモート」 and must not become "server is down".
fn transport_kind(raw: &str) -> TransportKind {
    let lower = raw.to_lowercase();
    if is_dropped(&lower) {
        return TransportKind::Dropped;
    }
    if is_timeout(&lower) {
        return TransportKind::Timeout;
    }
    if is_unreachable(&lower) {
        return TransportKind::Unreachable;
    }
    TransportKind::Other
}

fn is_dropped(lower: &str) -> bool {
    lower.contains("connection reset")
        || lower.contains("reset by peer")
        || lower.contains("connection aborted")
        || lower.contains("broken pipe")
        || lower.contains("connection closed")
        || lower.contains("closed connection")
        || lower.contains("forcibly closed")
        || lower.contains("unexpected eof")
        || lower.contains("unexpected end of file")
        || lower.contains("incomplete message")
        || lower.contains("error decoding response body")
        || lower.contains("decoding response body")
        || lower.contains("body decode")
        || lower.contains("os error 10054")
        || lower.contains("os error 10053")
        || lower.contains("強制的に切断")
        || lower.contains("peer closed")
        || lower.contains("early eof")
}

fn is_timeout(lower: &str) -> bool {
    lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("os error 10060")
        || lower.contains("deadline exceeded")
}

fn is_unreachable(lower: &str) -> bool {
    lower.contains("connection refused")
        || lower.contains("connect error")
        || lower.contains("tcp connect error")
        || lower.contains("failed to connect")
        || lower.contains("could not connect")
        || lower.contains("error sending request")
        || lower.contains("dns error")
        || lower.contains("network is unreachable")
        || lower.contains("os error 10061")
        || lower.contains("os error 10051")
        || lower.contains("os error 10065")
        || lower.contains("actively refused")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const WINDOWS_RESET: &str = "error sending request for url (http://100.65.231.111:8000/v1/chat/completions): 既存の接続はリモート ホストに強制的に切断されました (os error 10054)";

    #[test]
    fn windows_10054_remote_is_a_dropped_connection() {
        let before = classify_transport(WINDOWS_RESET, false);
        assert_eq!(before.class, FailureClass::DroppedBeforeText);
        assert_eq!(before.message, DROPPED_BEFORE_MESSAGE);
        assert!(
            !before.message.contains("接続できません"),
            "{}",
            before.message
        );
        assert!(!before.message.contains("MTPLX"), "{}", before.message);
        assert!(!before.message.contains("mtplx"), "{}", before.message);
        assert!(!before.message.contains("127.0.0.1"), "{}", before.message);

        let mid = classify_transport(WINDOWS_RESET, true);
        assert_eq!(mid.class, FailureClass::DroppedMidAnswer);
        assert_eq!(mid.message, DROPPED_MID_MESSAGE);
        assert!(!mid.message.contains("接続できません"), "{}", mid.message);
        assert!(!mid.message.contains("リモート"), "{}", mid.message);

        let bare = transport_failure("リモート");
        assert_eq!(bare, GENERIC_CALL_MESSAGE);
        assert!(!bare.contains("接続できません"), "{bare}");
    }

    #[test]
    fn transport_classes_do_not_name_a_runtime_or_port() {
        let refused = transport_failure(
            "error sending request for url (http://100.65.231.111:8000/v1/models): connection refused",
        );
        assert_eq!(refused, UNREACHABLE_MESSAGE);
        assert!(refused.contains("設定の URL"), "{refused}");
        assert!(!refused.contains("MTPLX"), "{refused}");
        assert!(!refused.contains("mtplx"), "{refused}");
        assert!(!refused.contains("127.0.0.1"), "{refused}");
        assert!(!refused.contains("8000"), "{refused}");

        let timed = classify_transport("operation timed out: os error 10060", false);
        assert_eq!(timed.class, FailureClass::Timeout);
        assert_eq!(timed.message, TIMEOUT_MESSAGE);
        assert!(
            !timed.message.contains("接続できません"),
            "{}",
            timed.message
        );

        let decode = classify_transport(
            "error decoding response body: connection closed before message completed",
            true,
        );
        assert_eq!(decode.class, FailureClass::DroppedMidAnswer);
        let early = classify_transport("error decoding response body", false);
        assert_eq!(early.class, FailureClass::DroppedBeforeText);

        let secret = transport_failure("invalid api key sk-live-secret");
        assert_eq!(secret, GENERIC_CALL_MESSAGE);
        assert!(!secret.contains("sk-live-secret"), "{secret}");
    }

    #[test]
    fn busy_status_and_overload_bodies_share_one_message() {
        for (status, body) in [
            (429, "secret-body"),
            (
                503,
                r#"{"error":{"message":"overloaded","api_key":"sk-live-secret"}}"#,
            ),
            (500, "the server is overloaded"),
            (500, "too many requests, try later"),
        ] {
            let message = chat_failure(status, body);
            assert_eq!(message, BUSY_MESSAGE, "{status} {body}");
            assert!(!message.contains("secret"), "{message}");
            assert!(!message.contains("overload"), "{message}");
            assert!(!message.to_lowercase().contains("api_key"), "{message}");
        }
        let plain = chat_failure(500, r#"{"message":"prompt is 14017 tokens"}"#);
        assert!(plain.contains("HTTP 500"), "{plain}");
        assert_ne!(plain, BUSY_MESSAGE);
        let tools = chat_failure(400, "this model does not support tools");
        assert_eq!(tools, TOOLS_UNSUPPORTED_ERROR);
        let auth = chat_failure(401, "overload is mentioned but this is unauthorized");
        assert!(auth.contains("API キーが拒否"), "{auth}");
        assert!(!auth.contains("MTPLX"), "{auth}");
        assert!(!auth.contains("混雑"), "{auth}");
    }

    #[test]
    fn in_stream_error_shows_the_message_and_overload_stays_busy() {
        let shown = in_stream_failure(
            &json!({"error": {"message": "モデルを\n読み込めません", "api_key": "sk-live-secret"}}),
        );
        assert_eq!(
            shown.0,
            "LLM サーバがエラーを返しました: モデルを 読み込めません"
        );
        assert!(!shown.1);
        assert!(!shown.0.contains("sk-live"), "{}", shown.0);

        let secret = in_stream_failure(&json!({"error": "bad key sk-live-secret"}));
        assert_eq!(secret.0, IN_STREAM_FALLBACK);
        assert!(!secret.0.contains("sk-live"), "{}", secret.0);

        let busy = in_stream_failure(
            &json!({"error": {"message": "The server is overloaded", "type": "server_error"}}),
        );
        assert_eq!(busy.0, BUSY_MESSAGE);
        assert!(busy.1);

        let empty = in_stream_failure(&json!({"error": {}}));
        assert_eq!(empty.0, IN_STREAM_FALLBACK);
        assert!(!empty.1);
    }

    #[test]
    fn retry_plan_backs_off_and_honors_retry_after() {
        assert_eq!(
            retry_plan(FailureClass::Busy, 1, None),
            RetryPlan::Wait(Duration::from_secs(1))
        );
        assert_eq!(
            retry_plan(FailureClass::Busy, 2, None),
            RetryPlan::Wait(Duration::from_secs(2))
        );
        assert_eq!(retry_plan(FailureClass::Busy, 3, None), RetryPlan::Stop);
        assert_eq!(
            retry_plan(FailureClass::Busy, 2, Some(Duration::from_secs(5))),
            RetryPlan::Wait(Duration::from_secs(5))
        );
        assert_eq!(
            retry_plan(FailureClass::Busy, 1, Some(Duration::from_secs(10_000))),
            RetryPlan::Wait(RETRY_AFTER_CAP)
        );
        assert_eq!(
            retry_plan(FailureClass::Busy, 1, Some(Duration::ZERO)),
            RetryPlan::Wait(Duration::ZERO)
        );
        assert_eq!(
            retry_plan(FailureClass::DroppedBeforeText, 1, None),
            RetryPlan::Wait(Duration::from_secs(1))
        );
        assert_eq!(
            retry_plan(
                FailureClass::DroppedMidAnswer,
                1,
                Some(Duration::from_secs(1))
            ),
            RetryPlan::Wait(MID_ANSWER_RETRY_WAIT)
        );
        assert_eq!(
            retry_plan(FailureClass::DroppedMidAnswer, 2, None),
            RetryPlan::Wait(MID_ANSWER_RETRY_WAIT)
        );
        assert_eq!(
            retry_plan(FailureClass::DroppedMidAnswer, MAX_ATTEMPTS, None),
            RetryPlan::Stop
        );
        for class in [
            FailureClass::Unreachable,
            FailureClass::Timeout,
            FailureClass::InStream,
            FailureClass::Other,
        ] {
            assert_eq!(
                retry_plan(class, 1, Some(Duration::from_secs(1))),
                RetryPlan::Stop
            );
        }
    }

    #[test]
    fn retry_after_parses_seconds_and_http_date() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(parse_retry_after("15", now), Some(Duration::from_secs(15)));
        assert_eq!(parse_retry_after("0", now), Some(Duration::ZERO));
        assert_eq!(
            parse_retry_after(" 30 ", now),
            Some(Duration::from_secs(30))
        );
        assert_eq!(parse_retry_after("not-a-header", now), None);

        let now_dt = chrono::DateTime::parse_from_rfc3339("2023-11-14T22:13:20Z").unwrap();
        let later = now_dt + chrono::Duration::seconds(30);
        let header = later.to_rfc2822();
        let parsed = parse_retry_after(
            &header,
            SystemTime::UNIX_EPOCH + Duration::from_secs(now_dt.timestamp() as u64),
        )
        .unwrap();
        assert!(
            parsed >= Duration::from_secs(29) && parsed <= Duration::from_secs(31),
            "{parsed:?}"
        );

        let earlier = now_dt - chrono::Duration::seconds(5);
        assert_eq!(
            parse_retry_after(
                &earlier.to_rfc2822(),
                SystemTime::UNIX_EPOCH + Duration::from_secs(now_dt.timestamp() as u64),
            ),
            Some(Duration::ZERO)
        );
    }
}
