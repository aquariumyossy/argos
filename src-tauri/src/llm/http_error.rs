//! One HTTP status, one Japanese message. The response body is never shown.
//! A 400 body is read only to match a few fixed phrases.

/// Returned for a 400 whose body says the model cannot call tools.
/// The server body is not part of this string. The tool-free retry matches it exactly.
pub const TOOLS_UNSUPPORTED_ERROR: &str = "このモデルはツール呼び出しに対応していません。";

pub const VISION_UNSUPPORTED_ERROR: &str =
    "このモデルは画像を読めません。vision 対応のモデルを設定してください。";

/// Returned for a 400 whose body says the prompt does not fit the context window.
/// The server body is not part of this string. The smaller-budget retry matches it exactly.
pub const CONTEXT_TOO_LONG_ERROR: &str =
    "この会話はモデルのコンテキストに対して長すぎます。新しい会話に切り替えるか、古い内容を減らしてください。";

/// The question itself, with this turn's attachments, does not fit even with no reserve.
pub const QUESTION_DOES_NOT_FIT_ERROR: &str =
    "この質問はモデルのコンテキストに入りません。短くしてから送ってください。";

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

pub fn is_tools_unsupported_error(error: &str) -> bool {
    error == TOOLS_UNSUPPORTED_ERROR
}

pub fn is_context_too_long_error(error: &str) -> bool {
    error == CONTEXT_TOO_LONG_ERROR
}

/// Chat and OCR. 400 plus a tools phrase becomes [`TOOLS_UNSUPPORTED_ERROR`].
/// 400 plus a context-length phrase becomes [`CONTEXT_TOO_LONG_ERROR`].
/// Every other status is a fixed message. The response body is never copied into it.
pub fn chat_failure(status: u16, body: &str) -> String {
    if status == 400 && body_has(body, TOOLS_PHRASES) {
        return TOOLS_UNSUPPORTED_ERROR.to_string();
    }
    if status == 400 && body_has(body, CONTEXT_PHRASES) {
        return CONTEXT_TOO_LONG_ERROR.to_string();
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
        401 | 403 => "API キーが拒否されました。MTPLX を 0.0.0.0 で起動している場合は、同じキーを Argos の API キー欄に入れてください。".into(),
        404 => "指定したモデルが見つかりません。モデル名を確認してください。".into(),
        413 => "送信内容が大きすぎます（HTTP 413）。".into(),
        429 => "LLM サーバがリクエストを制限しています（HTTP 429）。".into(),
        400..=499 => format!("LLM サーバが要求を拒否しました（HTTP {code}）。"),
        500..=599 => format!("LLM サーバがエラーを返しました（HTTP {code}）。"),
        _ => "LLM の呼び出しに失敗しました。".into(),
    }
}

/// Reqwest failures have no HTTP status. Connection errors keep the fixed connection text.
pub fn transport_failure(raw: &str) -> String {
    let lower = raw.to_lowercase();
    if is_transport_error(&lower) {
        if looks_non_loopback_http(raw) {
            return "LLM サーバのポートに届きません。Mac の MTPLX は既定で 127.0.0.1:8000 だけ待ち受けるため、Tailscale の 100.x からは繋がりません。Mac 側で LAN 公開するか、Tailscale Serve で 8000 を出してください（例: mtplx serve --host 0.0.0.0 --port 8000 --api-key 任意のキー）。Argos の API キー欄に同じキーを入れます。".into();
        }
        return "LLM サーバに接続できません。起動と URL を確認してください。".into();
    }
    "LLM の呼び出しに失敗しました。".into()
}

pub fn is_connection_message(message: &str) -> bool {
    message.contains("接続できません") || message.contains("届きません")
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

fn body_has(body: &str, phrases: &[&str]) -> bool {
    // A JSON body stores `"auto"` as `\"auto\"`. The phrases are the unescaped text.
    let lower = body.to_lowercase().replace("\\\"", "\"");
    phrases.iter().any(|phrase| lower.contains(phrase))
}

fn is_transport_error(lower: &str) -> bool {
    lower.contains("connection refused")
        || lower.contains("connect error")
        || lower.contains("tcp connect error")
        || lower.contains("failed to connect")
        || lower.contains("could not connect")
        || lower.contains("error sending request")
        || lower.contains("dns error")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("network is unreachable")
        || lower.contains("os error 10060")
        || lower.contains("os error 10061")
        || lower.contains("os error 10051")
        || lower.contains("os error 10065")
        || lower.contains("リモート")
        || lower.contains("接続できません")
}

fn looks_non_loopback_http(raw: &str) -> bool {
    raw.contains("100.")
        || raw.contains("192.168.")
        || raw.contains("10.")
        || raw.contains("172.16.")
        || raw.contains("172.17.")
        || raw.contains("172.18.")
        || raw.contains("172.19.")
        || raw.contains("172.2")
        || raw.contains("172.30.")
        || raw.contains("172.31.")
}
