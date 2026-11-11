//! e-Gov 法令API Version 1. No API key.
//!
//! Spec: https://laws.e-gov.go.jp/api/1/ (仕様書 1.4, 2024-07-29).
//! Host `laws.e-gov.go.jp`. Responses are XML.

use std::time::Duration;

#[cfg(test)]
use parking_lot::Mutex;

pub const API_ORIGIN: &str = "https://laws.e-gov.go.jp/api/1";
const USER_AGENT: &str = "Argos/1.18 (statute cache; e-Gov law API v1)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgovFailure {
    Offline,
    NotFound,
    Bad(String),
}

pub trait EgovSource: Send + Sync {
    fn fetch(&self, url: &str) -> Result<String, EgovFailure>;
}

pub fn article_url(law_id: &str, article: &str) -> String {
    format!(
        "{API_ORIGIN}/articles;lawId={};article={}",
        encode(law_id),
        encode(article)
    )
}

pub fn lawdata_url(law_id: &str) -> String {
    format!("{API_ORIGIN}/lawdata/{}", encode(law_id))
}

pub fn update_list_url(yyyymmdd: &str) -> String {
    format!("{API_ORIGIN}/updatelawlists/{yyyymmdd}")
}

pub fn lawlist_url(category: u8) -> String {
    format!("{API_ORIGIN}/lawlists/{category}")
}

fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

pub struct LiveEgov {
    client: reqwest::blocking::Client,
}

impl LiveEgov {
    pub fn new() -> Result<Self, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(5))
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { client })
    }
}

impl EgovSource for LiveEgov {
    fn fetch(&self, url: &str) -> Result<String, EgovFailure> {
        let response = match self.client.get(url).send() {
            Ok(response) => response,
            Err(_) => return Err(EgovFailure::Offline),
        };
        let status = response.status();
        if status.as_u16() == 404 {
            return Err(EgovFailure::NotFound);
        }
        if !status.is_success() {
            return Err(EgovFailure::Bad(format!(
                "法令APIが応答しませんでした（HTTP {}）",
                status.as_u16()
            )));
        }
        match response.text() {
            Ok(body) => {
                if body.trim_start().starts_with("<!DOCTYPE html")
                    || body.contains("Request Rejected")
                {
                    return Err(EgovFailure::Bad("法令APIがリクエストを拒否しました".into()));
                }
                Ok(body)
            }
            Err(_) => Err(EgovFailure::Offline),
        }
    }
}

pub struct OfflineEgov;

impl EgovSource for OfflineEgov {
    fn fetch(&self, _url: &str) -> Result<String, EgovFailure> {
        Err(EgovFailure::Offline)
    }
}

/// Test double. The first needle contained in the URL wins.
#[cfg(test)]
pub struct MapEgov {
    pub routes: Mutex<Vec<(String, Result<String, EgovFailure>)>>,
}

#[cfg(test)]
impl MapEgov {
    pub fn new(routes: Vec<(String, Result<String, EgovFailure>)>) -> Self {
        Self {
            routes: Mutex::new(routes),
        }
    }
}

#[cfg(test)]
impl EgovSource for MapEgov {
    fn fetch(&self, url: &str) -> Result<String, EgovFailure> {
        for (needle, result) in self.routes.lock().iter() {
            if url.contains(needle) {
                return result.clone();
            }
        }
        Err(EgovFailure::Offline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn article_url_encodes_the_japanese_article_number() {
        let url = article_url("129AC0000000089", "第555条");
        assert!(url
            .starts_with("https://laws.e-gov.go.jp/api/1/articles;lawId=129AC0000000089;article="));
        assert!(url.contains("%E7%AC%AC555%E6%9D%A1"));
        assert!(!url.contains("第"));
        assert_eq!(
            update_list_url("20261111"),
            "https://laws.e-gov.go.jp/api/1/updatelawlists/20261111"
        );
    }
}
