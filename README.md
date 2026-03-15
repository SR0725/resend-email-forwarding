# Resend Email Forwarding to Discord

A lightweight Rust service that receives inbound emails via [Resend](https://resend.com) webhooks and forwards them to a Discord channel through Discord Webhooks.

## Features

- Receives Resend `email.received` webhook events
- Fetches full email content (subject, body, attachments) via Resend API
- Forwards to Discord as a rich Embed message, including:
  - **Subject** - displayed as embed title
  - **Body** - plain text content (HTML is automatically stripped)
  - **From / To / CC / BCC** - shown as inline fields
  - **Attachments** - file name, type, size, and download link
  - **Open Raw Email** - button to view the original email
  - **Extracted Links** - all links in the email body are displayed as clickable Discord buttons
- Health check endpoint at `GET /health`
- Docker support for easy deployment

## Architecture

```
Sender ──> Resend Inbound Email
               │
               ▼
         POST /webhook  (this service)
               │
               ├─ GET /emails/receiving/{id}              ← fetch full email
               ├─ GET /emails/receiving/{id}/attachments   ← fetch attachment details
               │
               ▼
         Discord Webhook ──> Discord Channel
```

The Resend webhook payload only contains metadata (email ID, subject, from/to). The service makes additional API calls to retrieve the full email body and attachment download URLs before composing the Discord message.

## Prerequisites

- [Rust](https://rustup.rs/) 1.85+ (edition 2024)
- A [Resend](https://resend.com) account with:
  - A receiving domain (or Resend-managed `@*.resend.app` address)
  - An API key with permission to read inbound emails
  - A webhook configured to send `email.received` events to your endpoint
- A [Discord Webhook URL](https://support.discord.com/hc/en-us/articles/228383668-Intro-to-Webhooks)

## Quick Start

### 1. Clone and configure

```bash
git clone <repo-url>
cd resend-email-forwarding
cp .env.example .env
```

Edit `.env` with your credentials:

```env
RESEND_API_KEY=re_xxxxxxxxxxxxxxxx
DISCORD_WEBHOOK_URL=https://discord.com/api/webhooks/xxxx/xxxx
PORT=3000
```

### 2. Run locally

```bash
# Load environment variables
export $(cat .env | xargs)

# Build and run
cargo run --release
```

The server will start on `http://0.0.0.0:3000`.

### 3. Configure Resend Webhook

1. Go to [Resend Dashboard](https://resend.com/webhooks) > Webhooks
2. Add a new webhook endpoint: `https://your-domain.com/webhook`
3. Select the **email.received** event
4. Save

## Docker

### Build and run

```bash
docker build -t resend-email-forwarding .

docker run -d \
  -e RESEND_API_KEY=re_xxxxxxxxxxxxxxxx \
  -e DISCORD_WEBHOOK_URL=https://discord.com/api/webhooks/xxxx/xxxx \
  -e PORT=3000 \
  -p 3000:3000 \
  resend-email-forwarding
```

## Discord Message Format

Each forwarded email appears in Discord as:

```
┌─────────────────────────────────────────┐
│  Subject Line Here                      │
│                                         │
│  Email body content displayed as        │
│  plain text (up to 4000 chars)...       │
│                                         │
│  From: sender@example.com  │ To: you@…  │
│  CC: other@example.com                  │
│                                         │
│  Attachments:                           │
│  report.pdf (application/pdf, 1.2 MB)   │
│                                         │
│  Email ID: xxxxxxxx-xxxx-xxxx-xxxx      │
├─────────────────────────────────────────┤
│ [Open Raw Email] [Link 1] [Link 2] ... │
└─────────────────────────────────────────┘
```

- Links extracted from the email HTML are displayed as clickable buttons (up to 25 buttons, 5 per row)
- Attachment download links are clickable in the embed field

## Environment Variables

| Variable | Required | Default | Description |
|---|---|---|---|
| `RESEND_API_KEY` | Yes | - | Resend API key for fetching email details |
| `DISCORD_WEBHOOK_URL` | Yes | - | Discord webhook URL for the target channel |
| `PORT` | No | `3000` | HTTP server port |
| `RUST_LOG` | No | `info` | Log level (`debug`, `info`, `warn`, `error`) |

## API Endpoints

| Method | Path | Description |
|---|---|---|
| `POST` | `/webhook` | Receives Resend webhook events |
| `GET` | `/health` | Health check (returns `OK`) |

## License

MIT
