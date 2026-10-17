//! Discord 通知：只放寄件人、主旨、AI 摘要和兩個連結，不放內文。
//! 顏色：疑似詐騙＝琥珀色（標題前面加「⚠️ 可能是詐騙」）、重要＝紅、一般＝藍、低＝灰、摘要失敗＝藍（看不出來就當一般）。

use crate::resend::{Email, dashboard_url};
use crate::summary::Summary;
use crate::text::{md_escape, truncate};
use reqwest::Client;
use serde_json::{Value, json};
use std::time::Duration;

pub const COLOR_PHISHING: u32 = 0xFAB219;
pub const COLOR_IMPORTANT: u32 = 0xE03B3B;
pub const COLOR_NORMAL: u32 = 0x3987E5;
pub const COLOR_LOW: u32 = 0x80848E;

pub fn color(s: &Summary) -> u32 {
    if s.phishing {
        COLOR_PHISHING
    } else if !s.ok {
        COLOR_NORMAL
    } else {
        match s.importance.as_deref() {
            Some("重要") => COLOR_IMPORTANT,
            Some("低") => COLOR_LOW,
            _ => COLOR_NORMAL,
        }
    }
}

/// warroom_page：戰情室這封信的網址；None＝沒送進戰情室（連結會 404，改說明去 Resend 看）
pub fn payload(email: &Email, s: &Summary, warroom_page: Option<&str>) -> Value {
    let subject = email.subject_or_default();
    let title = if s.phishing {
        format!("⚠️ 可能是詐騙｜{subject}")
    } else {
        subject
    };
    let mut desc = format!(
        "**寄件人**　{}\n",
        md_escape(&truncate(&email.sender_display(), 200))
    );
    if s.ok {
        desc.push_str(&md_escape(&s.text));
    } else {
        desc.push_str(&format!(
            "**摘要失敗**（{}），請點下面的連結看原信。",
            md_escape(s.error.as_deref().unwrap_or("原因不明"))
        ));
    }
    if s.phishing {
        desc.push_str(&format!(
            "\n⚠️ **為什麼可疑**：{}",
            md_escape(s.phishing_reason.as_deref().unwrap_or("AI 沒寫原因"))
        ));
    }
    let resend = dashboard_url(&email.id);
    desc.push_str("\n\n");
    match warroom_page {
        Some(page) => desc.push_str(&format!(
            "[在戰情室看]({page})　·　[在 Resend 開]({resend})"
        )),
        None => desc.push_str(&format!("戰情室這次沒存到　·　[在 Resend 開]({resend})")),
    }

    let mut foot: Vec<String> = Vec::new();
    if s.ok {
        foot.extend(s.category.clone());
        foot.extend(s.importance.clone());
    }
    let to = email.to_list();
    if !to.is_empty() {
        foot.push(format!("寄到 {}", to.join("、")));
    }

    let mut embed = json!({
        "title": truncate(&title, 256),
        "description": truncate(&desc, 4000),
        "color": color(s),
        "footer": { "text": truncate(&foot.join("・"), 2000) },
    });
    if let Some(page) = warroom_page {
        embed["url"] = json!(page);
    }
    if let Some(t) = &email.created_at {
        embed["timestamp"] = json!(t);
    }
    // 信件內容不准觸發 @everyone 或提及任何人
    json!({ "embeds": [embed], "allowed_mentions": { "parse": [] } })
}

pub async fn send(client: &Client, webhook_url: &str, payload: &Value) -> Result<(), String> {
    let resp = client
        .post(webhook_url)
        .timeout(Duration::from_secs(15))
        .json(payload)
        .send()
        .await
        .map_err(|e| format!("送 Discord 失敗：{}", e.without_url()))?;
    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(format!("Discord 回 {status}：{}", truncate(&body, 300)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn email() -> Email {
        serde_json::from_value(json!({
            "id": "1a066262-13de-4682-b6c6-dce383f35758",
            "from": "noreplay@glowbestwear.com",
            "to": ["team@redia.ai"],
            "subject": "Important Notice: Email Address Verification – redia.ai",
            "created_at": "2026-10-17T20:21:19.746Z",
            "html": "<p>secret body text</p>",
            "headers": { "from": "\"Mail Update redia.ai\" <noreplay@glowbestwear.com>" }
        }))
        .unwrap()
    }

    fn summary(importance: &str, phishing: bool) -> Summary {
        Summary {
            ok: true,
            text: "自稱 redia.ai 管理員，要我們 48 小時內點連結確認信箱。".into(),
            category: Some("資安".into()),
            importance: Some(importance.into()),
            phishing,
            phishing_reason: phishing
                .then(|| "寄件網域是 glowbestwear.com，連結指向 S3 上的 HTML".into()),
            model: Some("gpt-6-luna".into()),
            error: None,
        }
    }

    #[test]
    fn phishing_is_amber_and_labelled() {
        let p = payload(
            &email(),
            &summary("重要", true),
            Some("https://ops.ray-realms.com/mail/1a066262-13de-4682-b6c6-dce383f35758"),
        );
        let e = &p["embeds"][0];
        assert_eq!(e["color"], COLOR_PHISHING);
        assert!(
            e["title"]
                .as_str()
                .unwrap()
                .starts_with("⚠️ 可能是詐騙｜Important Notice")
        );
        let d = e["description"].as_str().unwrap();
        assert!(d.contains("為什麼可疑"));
        assert!(d.contains(
            "[在戰情室看](https://ops.ray-realms.com/mail/1a066262-13de-4682-b6c6-dce383f35758)"
        ));
        assert!(d.contains("[在 Resend 開](https://resend.com/emails/receiving/1a066262-13de-4682-b6c6-dce383f35758)"));
        assert!(!d.contains("secret body text"), "不放內文");
        assert_eq!(e["footer"]["text"], "資安・重要・寄到 team@redia.ai");
        assert_eq!(p["allowed_mentions"]["parse"], json!([]));
    }

    #[test]
    fn colors_by_importance() {
        assert_eq!(color(&summary("重要", false)), COLOR_IMPORTANT);
        assert_eq!(color(&summary("一般", false)), COLOR_NORMAL);
        assert_eq!(color(&summary("低", false)), COLOR_LOW);
        assert_eq!(color(&Summary::failed(None, "x")), COLOR_NORMAL);
    }

    #[test]
    fn failed_summary_still_notifies() {
        let p = payload(
            &email(),
            &Summary::failed(Some("gpt-6-luna"), "OpenAI 逾時"),
            None,
        );
        let d = p["embeds"][0]["description"].as_str().unwrap();
        assert!(d.contains("**摘要失敗**（OpenAI 逾時）"));
        assert!(d.contains("戰情室這次沒存到"));
        assert!(p["embeds"][0].get("url").is_none());
    }
}
