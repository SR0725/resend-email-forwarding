# Resend Email Forwarding to Discord

輕量級 Rust 服務，透過 [Resend](https://resend.com) Webhook 接收轉寄的電子郵件，並自動轉發至 Discord 頻道。

A lightweight Rust service that receives inbound emails via [Resend](https://resend.com) webhooks and forwards them to a Discord channel through Discord Webhooks.

## 功能特色 / Features

- 接收 Resend `email.received` webhook 事件
- 透過 Resend API 取得完整郵件內容（主旨、內文、附件）
- 以 Discord Rich Embed 訊息轉發，包含：
  - **主旨 (Subject)** — 顯示為 Embed 標題
  - **內文 (Body)** — 純文字內容（自動去除 HTML 標籤，最多 4000 字）
  - **寄件者 / 收件者 / CC / BCC** — 以 inline fields 呈現
  - **附件 (Attachments)** — 檔名、類型、大小與下載連結
  - **開啟原始郵件 (Open Raw Email)** — 按鈕連結至原始郵件
  - **提取連結 (Extracted Links)** — 自動擷取郵件中所有連結，以 Discord 按鈕呈現
- 健康檢查端點 `GET /health`
- 支援 Docker 部署

## 架構 / Architecture

```
寄件者 ──> Resend 收信 (Inbound Email)
               │
               ▼
         POST /webhook（本服務）
               │
               ├─ GET /emails/receiving/{id}              ← 取得完整郵件內容
               ├─ GET /emails/receiving/{id}/attachments   ← 取得附件詳細資訊
               │
               ▼
         Discord Webhook ──> Discord 頻道
```

Resend Webhook 的 payload 僅包含 metadata（email ID、主旨、寄收件者等）。本服務會額外呼叫 Resend API 取得完整的郵件內文與附件下載連結，再組合成 Discord 訊息發送。

## 前置需求 / Prerequisites

- [Rust](https://rustup.rs/) 1.85+（edition 2024）
- 一個 [Resend](https://resend.com) 帳號，並完成以下設定：
  - 設定接收網域（或使用 Resend 託管的 `@*.resend.app` 地址）
  - 取得具有讀取 inbound email 權限的 API Key
  - 在 Dashboard 建立 Webhook，訂閱 `email.received` 事件
- 一個 [Discord Webhook URL](https://support.discord.com/hc/en-us/articles/228383668-Intro-to-Webhooks)

## 快速開始 / Quick Start

### 1. Clone 並設定環境變數

```bash
git clone <repo-url>
cd resend-email-forwarding
cp .env.example .env
```

編輯 `.env`，填入你的憑證：

```env
RESEND_API_KEY=re_xxxxxxxxxxxxxxxx
DISCORD_WEBHOOK_URL=https://discord.com/api/webhooks/xxxx/xxxx
PORT=3000
```

### 2. 本地執行

```bash
# 載入環境變數
export $(cat .env | xargs)

# 編譯並執行
cargo run --release
```

服務將啟動於 `http://0.0.0.0:3000`。

### 3. 設定 Resend Webhook

1. 前往 [Resend Dashboard](https://resend.com/webhooks) > Webhooks
2. 新增 Webhook 端點：`https://your-domain.com/webhook`
3. 勾選 **email.received** 事件
4. 儲存

## Docker 部署

```bash
# 建置映像
docker build -t resend-email-forwarding .

# 執行容器
docker run -d \
  -e RESEND_API_KEY=re_xxxxxxxxxxxxxxxx \
  -e DISCORD_WEBHOOK_URL=https://discord.com/api/webhooks/xxxx/xxxx \
  -e PORT=3000 \
  -p 3000:3000 \
  resend-email-forwarding
```

## Discord 訊息格式 / Message Format

每封轉發的郵件在 Discord 中的呈現方式：

```
┌─────────────────────────────────────────┐
│  郵件主旨 Subject Line Here             │
│                                         │
│  郵件內文以純文字顯示                      │
│  （最多 4000 字元）...                    │
│                                         │
│  From: sender@example.com  │ To: you@…  │
│  CC: other@example.com                  │
│                                         │
│  Attachments:                           │
│  report.pdf (application/pdf, 1.2 MB)   │
│                                         │
│  Email ID: xxxxxxxx-xxxx-xxxx-xxxx      │
├─────────────────────────────────────────┤
│ [Open Raw Email] [連結1] [連結2] ...     │
└─────────────────────────────────────────┘
```

- 從郵件 HTML 中提取的連結會以可點擊的按鈕顯示（最多 25 個按鈕，每列 5 個）
- 附件下載連結可直接在 Embed 欄位中點擊

## 環境變數 / Environment Variables

| 變數 | 必填 | 預設值 | 說明 |
|---|---|---|---|
| `RESEND_API_KEY` | 是 | - | Resend API Key，用於取得郵件詳細內容 |
| `DISCORD_WEBHOOK_URL` | 是 | - | Discord Webhook URL，指向目標頻道 |
| `PORT` | 否 | `3000` | HTTP 伺服器埠號 |
| `RUST_LOG` | 否 | `info` | 日誌等級（`debug`、`info`、`warn`、`error`） |

## API 端點 / Endpoints

| 方法 | 路徑 | 說明 |
|---|---|---|
| `POST` | `/webhook` | 接收 Resend Webhook 事件 |
| `GET` | `/health` | 健康檢查（回傳 `OK`） |

## License

MIT
