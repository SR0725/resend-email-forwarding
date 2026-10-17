//! Resend 收到的信（redia.ai 等網域）→ AI 摘要 → 戰情室存一份（看原信）→ Discord 只發摘要。
//!
//! 流程：Resend 發 email.received webhook（先驗 Svix 簽章）→ 用 Resend API 拿完整的信
//! → OpenAI 寫摘要、分類、重要程度、是否疑似詐騙（失敗就寫「摘要失敗」，照樣通知）
//! → POST 戰情室 /api/ingest/mail → Discord。
//!
//! 也可以在本機重跑某幾封信：`resend-email-forwarding replay [--dry-run] [--no-discord] [--no-warroom] <email id>...`

mod discord;
mod resend;
mod summary;
mod svix;
mod text;
mod warroom;

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use std::env;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

/// Resend（Svix）大約等 15 秒就算失敗；超過 12 秒先回 503 讓它稍後重送，這邊繼續做完（重送時會認出來不重發）
const RESPOND_WITHIN: Duration = Duration::from_secs(12);
/// 處理過的信記一天，Resend 重送同一封時不重複發 Discord
const SEEN_TTL: Duration = Duration::from_secs(24 * 3600);

// ── 設定 ──

struct Config {
    resend_api_key: String,
    discord_webhook_url: String,
    /// Resend webhook 的簽章金鑰（whsec_…）。正式服務必填；本機 replay 不需要。
    webhook_key: Option<Vec<u8>>,
    openai_api_key: Option<String>,
    openai_model: String,
    ops_ingest_token: Option<String>,
    warroom_url: String,
    port: u16,
}

fn opt_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|v| !v.trim().is_empty())
}

impl Config {
    fn from_env() -> Self {
        Self {
            resend_api_key: opt_env("RESEND_API_KEY").expect("RESEND_API_KEY is required"),
            discord_webhook_url: opt_env("DISCORD_WEBHOOK_URL")
                .expect("DISCORD_WEBHOOK_URL is required"),
            webhook_key: opt_env("RESEND_WEBHOOK_SECRET").map(|s| {
                svix::decode_secret(&s)
                    .expect("RESEND_WEBHOOK_SECRET 格式不對（應該是 whsec_ 開頭）")
            }),
            openai_api_key: opt_env("OPENAI_API_KEY"),
            openai_model: opt_env("OPENAI_MODEL").unwrap_or_else(|| "gpt-6-luna".to_string()),
            ops_ingest_token: opt_env("OPS_INGEST_TOKEN"),
            warroom_url: opt_env("WARROOM_URL")
                .unwrap_or_else(|| "https://ops.ray-realms.com".to_string()),
            port: env::var("PORT")
                .unwrap_or_else(|_| "3000".to_string())
                .parse()
                .expect("PORT must be a number"),
        }
    }
}

// ── 狀態 ──

#[derive(Clone, Copy, PartialEq, Debug)]
enum Seen {
    InProgress,
    Done,
}

struct AppState {
    cfg: Config,
    client: Client,
    seen: Mutex<HashMap<String, (Seen, Instant)>>,
}

impl AppState {
    /// 記下「開始處理」。已經在處理或處理完就回目前狀態，不重複做。
    fn claim(&self, id: &str) -> Option<Seen> {
        let mut seen = self.seen.lock().unwrap();
        seen.retain(|_, (_, at)| at.elapsed() < SEEN_TTL);
        if let Some((s, _)) = seen.get(id) {
            return Some(*s);
        }
        seen.insert(id.to_string(), (Seen::InProgress, Instant::now()));
        None
    }

    fn finish(&self, id: &str, ok: bool) {
        let mut seen = self.seen.lock().unwrap();
        if ok {
            seen.insert(id.to_string(), (Seen::Done, Instant::now()));
        } else {
            // 失敗就忘掉，Resend 重送時重做
            seen.remove(id);
        }
    }
}

// ── 處理一封信 ──

#[derive(Default)]
struct Options {
    dry_run: bool,
    no_discord: bool,
    no_warroom: bool,
    /// replay --one-message：先收集起來，最後合成一則 Discord 訊息（測試格式和顏色用）
    collect: Option<Mutex<Vec<serde_json::Value>>>,
}

/// 回傳 Err 代表要 Resend 重送（拿不到信、Discord 沒送出去）。AI 和戰情室失敗不算，照樣通知。
async fn process(st: &AppState, email_id: &str, opt: &Options) -> Result<(), String> {
    let cfg = &st.cfg;
    let email = resend::fetch_email(&st.client, &cfg.resend_api_key, email_id).await?;

    let started = Instant::now();
    let summary = summary::summarize(
        &st.client,
        cfg.openai_api_key.as_deref(),
        &cfg.openai_model,
        &email,
    )
    .await;
    if summary.ok {
        info!(email_id, ms = started.elapsed().as_millis() as u64, importance = ?summary.importance, phishing = summary.phishing, "AI 摘要完成");
    } else {
        warn!(email_id, error = ?summary.error, "AI 摘要失敗，照樣通知");
    }

    let page = warroom::page_url(&cfg.warroom_url, &email.id);
    let stored = if opt.dry_run || opt.no_warroom {
        !opt.no_warroom
    } else {
        match warroom::send(
            &st.client,
            &cfg.warroom_url,
            cfg.ops_ingest_token.as_deref(),
            &email,
            &summary,
        )
        .await
        {
            Ok(()) => true,
            Err(e) => {
                error!(email_id, "送戰情室失敗：{e}");
                false
            }
        }
    };

    let payload = discord::payload(&email, &summary, stored.then_some(page.as_str()));
    if opt.dry_run {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({ "summary": summary, "discord": payload })
            )
            .unwrap()
        );
        return Ok(());
    }
    if let Some(c) = &opt.collect {
        c.lock().unwrap().push(payload["embeds"][0].clone());
    } else if !opt.no_discord {
        discord::send(&st.client, &cfg.discord_webhook_url, &payload).await?;
    }
    info!(email_id, stored, "已通知");
    Ok(())
}

// ── Webhook ──

#[derive(Deserialize)]
struct WebhookEvent {
    #[serde(rename = "type")]
    event_type: String,
    data: Option<WebhookData>,
}

#[derive(Deserialize)]
struct WebhookData {
    email_id: Option<String>,
}

async fn handle_webhook(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, &'static str) {
    let key = st.cfg.webhook_key.as_deref().expect("啟動時已檢查");
    if let Err(e) = svix::verify(key, &headers, &body, text::unix_now()) {
        warn!("拒絕 webhook：{e}");
        return (StatusCode::BAD_REQUEST, "invalid signature");
    }
    let event: WebhookEvent = match serde_json::from_slice(&body) {
        Ok(e) => e,
        Err(e) => {
            warn!("webhook 內容看不懂：{e}");
            return (StatusCode::BAD_REQUEST, "invalid payload");
        }
    };
    if event.event_type != "email.received" {
        info!("略過 {} 事件", event.event_type);
        return (StatusCode::OK, "ignored");
    }
    let Some(id) = event
        .data
        .and_then(|d| d.email_id)
        .filter(|id| resend::valid_email_id(id))
    else {
        warn!("email.received 沒有合法的 email_id");
        return (StatusCode::BAD_REQUEST, "missing email_id");
    };
    info!(email_id = %id, "收到新信");

    match st.claim(&id) {
        Some(Seen::Done) => return (StatusCode::OK, "already processed"),
        Some(Seen::InProgress) => {
            return (StatusCode::SERVICE_UNAVAILABLE, "in progress, retry later");
        }
        None => {}
    }

    // 在獨立的 task 裡做：就算 Resend 先斷線，這封信也會做完
    let task_st = st.clone();
    let task_id = id.clone();
    let task = tokio::spawn(async move {
        let r = process(&task_st, &task_id, &Options::default()).await;
        if let Err(e) = &r {
            error!(email_id = %task_id, "處理失敗，等 Resend 重送：{e}");
        }
        task_st.finish(&task_id, r.is_ok());
        r.is_ok()
    });
    match tokio::time::timeout(RESPOND_WITHIN, task).await {
        Ok(Ok(true)) => (StatusCode::OK, "forwarded"),
        Ok(Ok(false)) => (StatusCode::INTERNAL_SERVER_ERROR, "failed, retry later"),
        Ok(Err(e)) => {
            error!(email_id = %id, "處理中斷：{e}");
            st.finish(&id, false);
            (StatusCode::INTERNAL_SERVER_ERROR, "failed, retry later")
        }
        Err(_) => {
            warn!(email_id = %id, "超過 {} 秒還沒做完，先回 503，背景繼續", RESPOND_WITHIN.as_secs());
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "still processing, retry later",
            )
        }
    }
}

async fn health() -> &'static str {
    "OK"
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
    info!("收到停止訊號，做完手上的請求再結束");
}

fn new_state(cfg: Config) -> Arc<AppState> {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .build()
        .expect("建立 HTTP client");
    Arc::new(AppState {
        cfg,
        client,
        seen: Mutex::new(HashMap::new()),
    })
}

async fn replay(args: &[String]) {
    let mut opt = Options::default();
    let mut ids = Vec::new();
    for a in args {
        match a.as_str() {
            "--dry-run" => opt.dry_run = true,
            "--no-discord" => opt.no_discord = true,
            "--no-warroom" => opt.no_warroom = true,
            "--one-message" => opt.collect = Some(Mutex::new(Vec::new())),
            other => ids.push(other.to_string()),
        }
    }
    if ids.is_empty() {
        eprintln!(
            "用法：resend-email-forwarding replay [--dry-run] [--no-discord] [--no-warroom] [--one-message] <email id>..."
        );
        std::process::exit(2);
    }
    let st = new_state(Config::from_env());
    let mut failed = 0;
    for id in ids.iter().take(if opt.collect.is_some() {
        10
    } else {
        usize::MAX
    }) {
        if let Err(e) = process(&st, id, &opt).await {
            eprintln!("{id}：{e}");
            failed += 1;
        }
    }
    if let Some(c) = opt.collect.take() {
        let embeds = c.into_inner().unwrap();
        if !embeds.is_empty() && !opt.dry_run && !opt.no_discord {
            let payload = serde_json::json!({
                "content": format!("🧪 測試新版信件通知：重跑最近 {} 封信的摘要（不是新信，不用處理）", embeds.len()),
                "embeds": embeds,
                "allowed_mentions": { "parse": [] }
            });
            if let Err(e) = discord::send(&st.client, &st.cfg.discord_webhook_url, &payload).await {
                eprintln!("Discord：{e}");
                failed += 1;
            }
        }
    }
    std::process::exit(if failed > 0 { 1 } else { 0 });
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("replay") {
        return replay(&args[1..]).await;
    }

    let cfg = Config::from_env();
    // 沒有簽章金鑰就不啟動：健康檢查不會過，部署會保留舊版
    assert!(
        cfg.webhook_key.is_some(),
        "RESEND_WEBHOOK_SECRET is required"
    );
    if cfg.openai_api_key.is_none() {
        warn!("沒有設定 OPENAI_API_KEY，每封信都會是「摘要失敗」");
    }
    if cfg.ops_ingest_token.is_none() {
        warn!("沒有設定 OPS_INGEST_TOKEN，信不會送進戰情室");
    }
    let port = cfg.port;
    let st = new_state(cfg);

    let app = Router::new()
        .route("/webhook", post(handle_webhook))
        .route("/health", get(health))
        .with_state(st);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    info!("Listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> Arc<AppState> {
        new_state(Config {
            resend_api_key: "re_test".into(),
            discord_webhook_url: "http://127.0.0.1:9/discord".into(),
            webhook_key: Some(b"test-key".to_vec()),
            openai_api_key: None,
            openai_model: "gpt-6-luna".into(),
            ops_ingest_token: None,
            warroom_url: "http://127.0.0.1:9".into(),
            port: 0,
        })
    }

    #[test]
    fn dedupe_states() {
        let st = state();
        assert_eq!(st.claim("a"), None);
        assert_eq!(st.claim("a"), Some(Seen::InProgress));
        st.finish("a", false);
        assert_eq!(st.claim("a"), None, "失敗的可以重做");
        st.finish("a", true);
        assert_eq!(st.claim("a"), Some(Seen::Done));
    }

    #[tokio::test]
    async fn unsigned_or_bad_signature_is_400() {
        let st = state();
        let body = Bytes::from_static(br#"{"type":"email.received","data":{"email_id":"1a066262-13de-4682-b6c6-dce383f35758"}}"#);
        let (code, _) = handle_webhook(State(st.clone()), HeaderMap::new(), body.clone()).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);

        let mut h = HeaderMap::new();
        h.insert("svix-id", "msg_1".parse().unwrap());
        h.insert(
            "svix-timestamp",
            text::unix_now().to_string().parse().unwrap(),
        );
        h.insert(
            "svix-signature",
            "v1,AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
                .parse()
                .unwrap(),
        );
        let (code, _) = handle_webhook(State(st.clone()), h, body).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert!(st.seen.lock().unwrap().is_empty(), "沒驗過的請求不能佔位");
    }

    #[tokio::test]
    async fn signed_other_events_are_ignored() {
        use base64::Engine;
        use hmac::Mac;
        let st = state();
        let body =
            br#"{"type":"email.sent","data":{"email_id":"1a066262-13de-4682-b6c6-dce383f35758"}}"#;
        let ts = text::unix_now().to_string();
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(b"test-key").unwrap();
        mac.update(format!("msg_1.{ts}.").as_bytes());
        mac.update(body);
        let sig = format!(
            "v1,{}",
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
        );
        let mut h = HeaderMap::new();
        h.insert("svix-id", "msg_1".parse().unwrap());
        h.insert("svix-timestamp", ts.parse().unwrap());
        h.insert("svix-signature", sig.parse().unwrap());
        let (code, msg) = handle_webhook(State(st), h, Bytes::from_static(body)).await;
        assert_eq!((code, msg), (StatusCode::OK, "ignored"));
    }
}
