use axum::{Router, extract::Json, http::StatusCode, routing::post};
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::env;
use tracing::{error, info, warn};

// ── Resend Webhook Payload ──

#[derive(Debug, Deserialize)]
struct WebhookEvent {
    #[serde(rename = "type")]
    event_type: String,
    data: WebhookData,
}

#[derive(Debug, Deserialize)]
struct WebhookData {
    email_id: String,
}

// ── Resend GET /emails/receiving/{id} Response ──

#[derive(Debug, Deserialize)]
struct EmailDetail {
    id: String,
    from: Option<String>,
    to: Option<Vec<String>>,
    cc: Option<Vec<String>>,
    bcc: Option<Vec<String>>,
    subject: Option<String>,
    html: Option<String>,
    text: Option<String>,
    raw: Option<RawEmail>,
    attachments: Option<Vec<Attachment>>,
}

#[derive(Debug, Deserialize)]
struct RawEmail {
    download_url: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct Attachment {
    id: String,
    filename: Option<String>,
    content_type: Option<String>,
}

// ── Resend GET /emails/receiving/{email_id}/attachments/{id} Response ──

#[derive(Debug, Deserialize)]
struct AttachmentDetail {
    size: Option<u64>,
    download_url: Option<String>,
}

// ── Discord Webhook Structures ──

#[derive(Debug, Serialize)]
struct DiscordWebhook {
    content: Option<String>,
    embeds: Vec<DiscordEmbed>,
}

#[derive(Debug, Serialize)]
struct DiscordEmbed {
    title: Option<String>,
    description: Option<String>,
    color: Option<u32>,
    fields: Vec<EmbedField>,
    footer: Option<EmbedFooter>,
}

#[derive(Debug, Serialize)]
struct EmbedField {
    name: String,
    value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    inline: Option<bool>,
}

#[derive(Debug, Serialize)]
struct EmbedFooter {
    text: String,
}

// ── Config ──

struct Config {
    resend_api_key: String,
    discord_webhook_url: String,
    port: u16,
}

impl Config {
    fn from_env() -> Self {
        Self {
            resend_api_key: env::var("RESEND_API_KEY").expect("RESEND_API_KEY is required"),
            discord_webhook_url: env::var("DISCORD_WEBHOOK_URL")
                .expect("DISCORD_WEBHOOK_URL is required"),
            port: env::var("PORT")
                .unwrap_or_else(|_| "3000".to_string())
                .parse()
                .expect("PORT must be a number"),
        }
    }
}

// ── Helpers ──

/// Strip HTML tags to get plain text
fn strip_html(html: &str) -> String {
    let tag_re = Regex::new(r"<[^>]+>").unwrap();
    let result = tag_re.replace_all(html, "");
    htmlescape::decode_html(&result).unwrap_or_else(|_| result.to_string())
}

/// Extract all URLs from HTML content (href attributes and plain text URLs)
fn extract_links(html: &str) -> Vec<(String, String)> {
    let mut links = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Extract href links with their anchor text
    let href_re = Regex::new(r#"<a\s[^>]*href\s*=\s*["']([^"']+)["'][^>]*>(.*?)</a>"#).unwrap();
    for cap in href_re.captures_iter(html) {
        let url = cap[1].to_string();
        let label = strip_html(&cap[2]);
        let label = if label.trim().is_empty() {
            truncate_url(&url)
        } else {
            label.trim().to_string()
        };
        if seen.insert(url.clone()) && url.starts_with("http") {
            links.push((label, url));
        }
    }

    // Also extract plain text URLs not already captured
    let url_re = Regex::new(r#"https?://[^\s<>"']+"#).unwrap();
    for m in url_re.find_iter(html) {
        let url = m.as_str().to_string();
        if seen.insert(url.clone()) {
            links.push((truncate_url(&url), url));
        }
    }

    links
}

fn truncate_url(url: &str) -> String {
    if url.len() > 40 {
        format!("{}…", &url[..40])
    } else {
        url.to_string()
    }
}

/// Truncate text to a max char length, appending "…" if needed
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max - 1).collect();
        format!("{t}…")
    }
}

/// Truncate a field value to fit Discord's 1024-char limit
fn truncate_field(s: &str) -> String {
    truncate(s, 1024)
}

/// Build a list of markdown links, accumulating lines until we approach the char budget.
/// This avoids truncating in the middle of a `[label](url)` markdown link.
fn build_link_lines(links: Vec<(String, String)>, budget: usize) -> String {
    let mut result = String::new();
    for (i, (label, url)) in links.into_iter().enumerate() {
        let label = truncate(&label, 60);
        let line = if result.is_empty() {
            format!("{}. [{}]({})", i + 1, label, url)
        } else {
            format!("\n{}. [{}]({})", i + 1, label, url)
        };
        if result.len() + line.len() > budget {
            if result.is_empty() {
                // At least show one truncated URL
                result = truncate(&line, budget);
            }
            break;
        }
        result.push_str(&line);
    }
    result
}

// ── Core Logic ──

async fn fetch_email_detail(
    client: &Client,
    api_key: &str,
    email_id: &str,
) -> Option<EmailDetail> {
    let url = format!("https://api.resend.com/emails/receiving/{email_id}");
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => match r.json::<EmailDetail>().await {
            Ok(detail) => Some(detail),
            Err(e) => {
                error!("Failed to parse email detail: {e}");
                None
            }
        },
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            error!("Resend API returned {status}: {body}");
            None
        }
        Err(e) => {
            error!("Failed to call Resend API: {e}");
            None
        }
    }
}

async fn fetch_attachment_detail(
    client: &Client,
    api_key: &str,
    email_id: &str,
    attachment_id: &str,
) -> Option<AttachmentDetail> {
    let url = format!(
        "https://api.resend.com/emails/receiving/{email_id}/attachments/{attachment_id}"
    );
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => r.json::<AttachmentDetail>().await.ok(),
        _ => None,
    }
}

/// Discord embed limits
const LIMIT_TITLE: usize = 256;
const LIMIT_DESCRIPTION: usize = 4096;
const LIMIT_FIELD_VALUE: usize = 1024;
const LIMIT_FOOTER: usize = 2048;
const LIMIT_EMBED_TOTAL: usize = 6000;

fn build_discord_payload(
    detail: &EmailDetail,
    attachment_details: &[(Attachment, Option<AttachmentDetail>)],
    raw_url: Option<&str>,
) -> DiscordWebhook {
    let subject = detail.subject.as_deref().unwrap_or("(No Subject)");
    let from = detail.from.as_deref().unwrap_or("Unknown");
    let to = detail
        .to
        .as_ref()
        .map(|v| v.join(", "))
        .unwrap_or_else(|| "Unknown".to_string());

    let title = truncate(subject, LIMIT_TITLE);
    let footer_text = truncate(&format!("Email ID: {}", detail.id), LIMIT_FOOTER);

    // Body: prefer text, fallback to stripped HTML
    let body_raw = detail
        .text
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .map(|t| t.to_string())
        .unwrap_or_else(|| {
            detail
                .html
                .as_deref()
                .map(strip_html)
                .unwrap_or_else(|| "(No Body)".to_string())
        });

    // Fields
    let mut fields = vec![
        EmbedField {
            name: "From".to_string(),
            value: truncate_field(from),
            inline: Some(true),
        },
        EmbedField {
            name: "To".to_string(),
            value: truncate_field(&to),
            inline: Some(true),
        },
    ];

    if let Some(cc) = &detail.cc {
        if !cc.is_empty() {
            fields.push(EmbedField {
                name: "CC".to_string(),
                value: truncate_field(&cc.join(", ")),
                inline: Some(true),
            });
        }
    }
    if let Some(bcc) = &detail.bcc {
        if !bcc.is_empty() {
            fields.push(EmbedField {
                name: "BCC".to_string(),
                value: truncate_field(&bcc.join(", ")),
                inline: Some(true),
            });
        }
    }

    // Attachments
    if !attachment_details.is_empty() {
        let mut att_text = String::new();
        for (att, detail_opt) in attachment_details {
            let name = att.filename.as_deref().unwrap_or("unnamed");
            let ctype = att.content_type.as_deref().unwrap_or("unknown");
            let line = if let Some(d) = detail_opt {
                let size = d
                    .size
                    .map(format_size)
                    .unwrap_or_else(|| "?".to_string());
                if let Some(url) = &d.download_url {
                    format!("[{name}]({url}) ({ctype}, {size})")
                } else {
                    format!("{name} ({ctype}, {size})")
                }
            } else {
                format!("{name} ({ctype})")
            };
            let next = if att_text.is_empty() {
                line
            } else {
                format!("\n{line}")
            };
            if att_text.len() + next.len() > LIMIT_FIELD_VALUE {
                break;
            }
            att_text.push_str(&next);
        }

        if !att_text.is_empty() {
            fields.push(EmbedField {
                name: "Attachments".to_string(),
                value: att_text,
                inline: Some(false),
            });
        }
    }

    // Raw email link
    if let Some(url) = raw_url {
        let value = format!("[Open Raw Email]({url})");
        if value.len() <= LIMIT_FIELD_VALUE {
            fields.push(EmbedField {
                name: "Raw Email".to_string(),
                value,
                inline: Some(false),
            });
        }
    }

    // Extract links from HTML body and display as markdown links
    if let Some(html) = &detail.html {
        let links = extract_links(html);
        if !links.is_empty() {
            let link_text = build_link_lines(links, LIMIT_FIELD_VALUE);
            if !link_text.is_empty() {
                fields.push(EmbedField {
                    name: "Links".to_string(),
                    value: link_text,
                    inline: Some(false),
                });
            }
        }
    }

    // Calculate total chars used by non-description parts, then cap description to fit 6000 total
    let fixed_chars: usize = title.chars().count()
        + footer_text.chars().count()
        + fields
            .iter()
            .map(|f| f.name.chars().count() + f.value.chars().count())
            .sum::<usize>();
    let description_budget = LIMIT_EMBED_TOTAL.saturating_sub(fixed_chars).min(LIMIT_DESCRIPTION);
    let description = truncate(body_raw.trim(), description_budget);

    let embed = DiscordEmbed {
        title: Some(title),
        description: Some(description),
        color: Some(0x6C47FF),
        fields,
        footer: Some(EmbedFooter {
            text: footer_text,
        }),
    };

    DiscordWebhook {
        content: None,
        embeds: vec![embed],
    }
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

async fn send_to_discord(client: &Client, webhook_url: &str, payload: &DiscordWebhook) -> bool {
    match client.post(webhook_url).json(payload).send().await {
        Ok(resp) if resp.status().is_success() || resp.status().as_u16() == 204 => true,
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!("Discord webhook returned {status}: {body}");
            false
        }
        Err(e) => {
            error!("Failed to send Discord webhook: {e}");
            false
        }
    }
}

// ── Handler ──

async fn handle_webhook(Json(event): Json<WebhookEvent>) -> StatusCode {
    info!(
        "Received webhook: type={}, email_id={}",
        event.event_type, event.data.email_id
    );

    if event.event_type != "email.received" {
        info!("Ignoring event type: {}", event.event_type);
        return StatusCode::OK;
    }

    let config = Config::from_env();
    let client = Client::new();

    // Step 1: Fetch full email detail
    let detail =
        match fetch_email_detail(&client, &config.resend_api_key, &event.data.email_id).await {
            Some(d) => d,
            None => {
                error!("Could not fetch email detail for {}", event.data.email_id);
                return StatusCode::INTERNAL_SERVER_ERROR;
            }
        };

    // Step 2: Fetch attachment details
    let mut attachment_details = Vec::new();
    if let Some(attachments) = &detail.attachments {
        for att in attachments {
            let att_detail =
                fetch_attachment_detail(&client, &config.resend_api_key, &detail.id, &att.id)
                    .await;
            attachment_details.push((att.clone(), att_detail));
        }
    }

    let raw_url = detail
        .raw
        .as_ref()
        .and_then(|r| r.download_url.as_deref());

    // Step 3: Build and send Discord message
    let payload = build_discord_payload(&detail, &attachment_details, raw_url);

    if send_to_discord(&client, &config.discord_webhook_url, &payload).await {
        info!("Forwarded email {} to Discord", detail.id);
        StatusCode::OK
    } else {
        warn!("Failed to forward email {} to Discord", detail.id);
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

async fn health() -> &'static str {
    "OK"
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = Config::from_env();
    let port = config.port;

    let app = Router::new()
        .route("/webhook", post(handle_webhook))
        .route("/health", axum::routing::get(health));

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    info!("Listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
