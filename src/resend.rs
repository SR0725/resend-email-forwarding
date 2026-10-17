//! Resend：webhook 只帶信的 id 和基本資料，完整內容（HTML、文字、標頭、附件）要另外打 API 拿。

use crate::text::{html_to_text, link_hosts};
use regex::Regex;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::sync::LazyLock;
use std::time::Duration;

static EMAIL_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9-]{7,63}$").unwrap());

pub fn valid_email_id(id: &str) -> bool {
    EMAIL_ID.is_match(id)
}

/// Resend 後台這封信（收到的信在 Emails 的 Receiving 分頁底下）
pub fn dashboard_url(id: &str) -> String {
    format!("https://resend.com/emails/receiving/{id}")
}

#[derive(Debug, Deserialize, Clone)]
pub struct Email {
    pub id: String,
    pub from: Option<String>,
    pub to: Option<Vec<String>>,
    pub cc: Option<Vec<String>>,
    pub reply_to: Option<Vec<String>>,
    pub subject: Option<String>,
    pub html: Option<String>,
    pub text: Option<String>,
    pub created_at: Option<String>,
    pub message_id: Option<String>,
    pub headers: Option<Map<String, Value>>,
    pub authentication: Option<Value>,
    pub attachments: Option<Vec<Attachment>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Attachment {
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub content_disposition: Option<String>,
    pub size: Option<u64>,
}

impl Email {
    /// 標頭的值（Resend 有時候外面多包一層引號，例如 date）
    pub fn header(&self, name: &str) -> Option<String> {
        let v = self.headers.as_ref()?.get(name)?;
        let s = match v {
            Value::String(s) => s.clone(),
            Value::Array(a) => a
                .iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            other => other.to_string(),
        };
        let s = s.trim();
        let s = if s.len() >= 2
            && s.starts_with('"')
            && s.ends_with('"')
            && s.matches('"').count() == 2
        {
            &s[1..s.len() - 1]
        } else {
            s
        };
        (!s.is_empty()).then(|| s.to_string())
    }

    /// 寄件人：From 標頭（含顯示名稱，例如 "Mail Update redia.ai" <noreplay@glowbestwear.com>），沒有才用 Resend 的 from
    pub fn sender_display(&self) -> String {
        self.header("from")
            .or_else(|| self.from.clone())
            .unwrap_or_else(|| "不明寄件人".to_string())
    }

    pub fn to_list(&self) -> Vec<String> {
        self.to.clone().unwrap_or_default()
    }

    pub fn subject_or_default(&self) -> String {
        self.subject
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "（沒有主旨）".to_string())
    }

    /// 純文字版：有 text 用 text，沒有就把 HTML 轉成文字
    pub fn plain_text(&self) -> String {
        match self.text.as_deref().filter(|t| !t.trim().is_empty()) {
            Some(t) => t.trim().to_string(),
            None => self.html.as_deref().map(html_to_text).unwrap_or_default(),
        }
    }

    pub fn link_hosts(&self) -> Vec<(String, usize)> {
        link_hosts(self.html.as_deref(), self.text.as_deref())
    }

    /// SPF／DKIM／DMARC 結果（Resend 的 authentication 欄位）
    pub fn auth_summary(&self) -> Option<String> {
        let a = self.authentication.as_ref()?.as_object()?;
        let parts: Vec<String> = ["spf", "dkim", "dmarc"]
            .iter()
            .filter_map(|k| {
                a.get(*k)
                    .and_then(|v| v.as_str())
                    .map(|v| format!("{} {v}", k.to_uppercase()))
            })
            .collect();
        (!parts.is_empty()).then(|| parts.join("、"))
    }
}

pub async fn fetch_email(client: &Client, api_key: &str, id: &str) -> Result<Email, String> {
    if !valid_email_id(id) {
        return Err(format!("email id 格式不對：{id}"));
    }
    let resp = client
        .get(format!("https://api.resend.com/emails/receiving/{id}"))
        .bearer_auth(api_key)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("打 Resend API 失敗：{e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "Resend API 回 {status}：{}",
            crate::text::truncate(&body, 300)
        ));
    }
    resp.json::<Email>()
        .await
        .map_err(|e| format!("Resend 回傳的格式看不懂：{e}"))
}
