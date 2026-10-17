//! 送進戰情室（Dashboard）：POST {WARROOM_URL}/api/ingest/mail，Bearer OPS_INGEST_TOKEN。
//! 戰情室存 30 天，網頁 /mail/<id> 上面是摘要、下面是原信（sandbox iframe）。格式見 ray-infra 的 docs/contracts.md「信件」。

use crate::resend::{Email, dashboard_url};
use crate::summary::Summary;
use crate::text::{iso_now, truncate};
use reqwest::Client;
use serde_json::{Map, Value, json};
use std::time::Duration;

/// 原信 HTML 的上限（跟戰情室一樣 512 KB），超過只送文字
pub const HTML_MAX: usize = 512 * 1024;
/// 純文字版上限（字數）
pub const TEXT_MAX: usize = 100_000;
const HEADERS_MAX: usize = 32 * 1024;
const KEY_HEADERS: &[&str] = &[
    "from",
    "to",
    "cc",
    "reply-to",
    "date",
    "subject",
    "message-id",
    "return-path",
    "received-spf",
    "authentication-results",
    "x-ses-spam-verdict",
    "x-ses-virus-verdict",
    "list-unsubscribe",
    "priority",
];

pub fn page_url(base: &str, id: &str) -> String {
    format!("{}/mail/{id}", base.trim_end_matches('/'))
}

fn headers_for(email: &Email) -> Option<Value> {
    let h = email.headers.as_ref()?;
    let all = Value::Object(h.clone());
    if all.to_string().len() <= HEADERS_MAX {
        return Some(all);
    }
    let kept: Map<String, Value> = KEY_HEADERS
        .iter()
        .filter_map(|k| h.get(*k).map(|v| (k.to_string(), v.clone())))
        .collect();
    Some(Value::Object(kept))
}

pub fn payload(email: &Email, summary: &Summary) -> Value {
    let (html, html_note) = match email.html.as_deref().filter(|h| !h.trim().is_empty()) {
        Some(h) if h.len() > HTML_MAX => (
            None,
            Some(format!(
                "原信 HTML 有 {} KB，超過 {} KB，只存文字版；完整原信請在 Resend 開",
                h.len() / 1000,
                HTML_MAX / 1024
            )),
        ),
        Some(h) => (Some(h.to_string()), None),
        None => (None, None),
    };
    let text = email.plain_text();
    let text = if text.chars().count() > TEXT_MAX {
        text.chars().take(TEXT_MAX).collect()
    } else {
        text
    };
    let attachments: Vec<Value> = email
        .attachments
        .clone()
        .unwrap_or_default()
        .iter()
        .map(|a| json!({ "filename": a.filename, "content_type": a.content_type, "size": a.size, "inline": a.content_disposition.as_deref() == Some("inline") }))
        .collect();
    json!({
        "id": email.id,
        "received_at": email.created_at.clone().unwrap_or_else(iso_now),
        "from": email.sender_display(),
        "from_address": email.from,
        "to": email.to_list(),
        "cc": email.cc.clone().unwrap_or_default(),
        "reply_to": email.reply_to.clone().unwrap_or_default(),
        "subject": email.subject,
        "message_id": email.message_id,
        "summary": summary,
        "authentication": email.authentication,
        "headers": headers_for(email),
        "attachments": attachments,
        "text": text,
        "html": html,
        "html_note": html_note,
        "resend_url": dashboard_url(&email.id),
    })
}

pub async fn send(
    client: &Client,
    base: &str,
    token: Option<&str>,
    email: &Email,
    summary: &Summary,
) -> Result<(), String> {
    let Some(token) = token.filter(|t| !t.trim().is_empty()) else {
        return Err("沒有設定 OPS_INGEST_TOKEN".to_string());
    };
    let url = format!("{}/api/ingest/mail", base.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .timeout(Duration::from_secs(15))
        .json(&payload(email, summary))
        .send()
        .await
        .map_err(|e| format!("送戰情室失敗：{e}"))?;
    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(format!("戰情室回 {status}：{}", truncate(&body, 300)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn email(html: &str) -> Email {
        serde_json::from_value(json!({
            "id": "1a066262-13de-4682-b6c6-dce383f35758",
            "from": "noreplay@glowbestwear.com",
            "to": ["team@redia.ai"],
            "subject": "Important Notice",
            "created_at": "2026-10-17T20:21:19.746Z",
            "html": html,
            "text": "",
            "headers": { "from": "\"Mail Update redia.ai\" <noreplay@glowbestwear.com>", "received": ["a", "b"] },
            "attachments": [{ "id": "x", "filename": "a.zip", "content_type": "application/zip", "content_disposition": "attachment", "size": 733 }]
        }))
        .unwrap()
    }

    #[test]
    fn payload_has_summary_and_body() {
        let p = payload(
            &email("<p>VERIFY HERE</p>"),
            &Summary::failed(Some("gpt-6-luna"), "逾時"),
        );
        assert_eq!(
            p["from"],
            "\"Mail Update redia.ai\" <noreplay@glowbestwear.com>"
        );
        assert_eq!(p["summary"]["ok"], false);
        assert_eq!(p["summary"]["text"], "摘要失敗");
        assert_eq!(p["text"], "VERIFY HERE");
        assert_eq!(p["html"], "<p>VERIFY HERE</p>");
        assert_eq!(
            p["attachments"][0],
            json!({ "filename": "a.zip", "content_type": "application/zip", "size": 733, "inline": false })
        );
        assert_eq!(p["received_at"], "2026-10-17T20:21:19.746Z");
    }

    #[test]
    fn big_html_sends_text_only() {
        let big = format!("<p>{}</p>", "x".repeat(HTML_MAX + 1));
        let p = payload(&email(&big), &Summary::failed(None, "x"));
        assert!(p["html"].is_null());
        assert!(p["html_note"].as_str().unwrap().contains("只存文字版"));
        assert!(p["text"].as_str().unwrap().starts_with("xxx"));
    }
}
