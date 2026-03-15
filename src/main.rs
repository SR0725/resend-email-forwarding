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
    #[serde(skip_serializing_if = "Option::is_none")]
    components: Option<Vec<ActionRow>>,
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

#[derive(Debug, Serialize)]
struct ActionRow {
    #[serde(rename = "type")]
    component_type: u8,
    components: Vec<ButtonComponent>,
}

#[derive(Debug, Serialize, Clone)]
struct ButtonComponent {
    #[serde(rename = "type")]
    component_type: u8,
    style: u8,
    label: String,
    url: String,
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

/// Truncate text to a max length, appending "…" if needed
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max - 1).collect();
        format!("{t}…")
    }
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
    let body = truncate(body_raw.trim(), 4000);

    // Fields
    let mut fields = vec![
        EmbedField {
            name: "From".to_string(),
            value: from.to_string(),
            inline: Some(true),
        },
        EmbedField {
            name: "To".to_string(),
            value: to,
            inline: Some(true),
        },
    ];

    if let Some(cc) = &detail.cc {
        if !cc.is_empty() {
            fields.push(EmbedField {
                name: "CC".to_string(),
                value: cc.join(", "),
                inline: Some(true),
            });
        }
    }
    if let Some(bcc) = &detail.bcc {
        if !bcc.is_empty() {
            fields.push(EmbedField {
                name: "BCC".to_string(),
                value: bcc.join(", "),
                inline: Some(true),
            });
        }
    }

    // Attachments
    if !attachment_details.is_empty() {
        let att_lines: Vec<String> = attachment_details
            .iter()
            .map(|(att, detail_opt)| {
                let name = att.filename.as_deref().unwrap_or("unnamed");
                let ctype = att.content_type.as_deref().unwrap_or("unknown");
                if let Some(d) = detail_opt {
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
                }
            })
            .collect();

        fields.push(EmbedField {
            name: "Attachments".to_string(),
            value: att_lines.join("\n"),
            inline: Some(false),
        });
    }

    let embed = DiscordEmbed {
        title: Some(truncate(subject, 256)),
        description: Some(body),
        color: Some(0x6C47FF),
        fields,
        footer: Some(EmbedFooter {
            text: format!("Email ID: {}", detail.id),
        }),
    };

    // Build link buttons
    let mut buttons: Vec<ButtonComponent> = Vec::new();

    // "Open Raw Email" button
    if let Some(url) = raw_url {
        buttons.push(ButtonComponent {
            component_type: 2,
            style: 5, // Link style
            label: "Open Raw Email".to_string(),
            url: url.to_string(),
        });
    }

    // Extract links from HTML body and add as buttons
    if let Some(html) = &detail.html {
        let links = extract_links(html);
        for (label, url) in links {
            if buttons.len() >= 25 {
                break;
            }
            buttons.push(ButtonComponent {
                component_type: 2,
                style: 5,
                label: truncate(&label, 80),
                url,
            });
        }
    }

    // Organize buttons into action rows (max 5 per row, max 5 rows)
    let components = if buttons.is_empty() {
        None
    } else {
        let rows: Vec<ActionRow> = buttons
            .chunks(5)
            .take(5)
            .map(|chunk| ActionRow {
                component_type: 1,
                components: chunk.to_vec(),
            })
            .collect();
        Some(rows)
    };

    DiscordWebhook {
        content: None,
        embeds: vec![embed],
        components,
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
