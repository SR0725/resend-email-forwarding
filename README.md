# Resend Email Forwarding

Resend 收到的信（redia.ai 等網域）先讓 AI 讀過，再通知 Discord。Discord 只放寄件人、主旨和摘要，整封信到戰情室（Dashboard）或 Resend 看。

A small Rust service: Resend inbound webhook → AI summary (OpenAI) → stores the full email in the ops dashboard → posts a short summary to Discord.

## 流程

```
寄件人 ──> Resend 收信
              │  email.received webhook（Svix 簽章）
              ▼
        POST /webhook（本服務）── 簽章不對或沒簽章 → 400
              │
              ├─ GET  api.resend.com/emails/receiving/{id}   ← 完整的信（HTML、文字、標頭、附件）
              ├─ POST api.openai.com/v1/responses            ← 摘要、分類、重要程度、是否疑似詐騙（失敗就寫「摘要失敗」，照樣通知）
              ├─ POST ops.ray-realms.com/api/ingest/mail     ← 戰情室存 30 天，/mail/<id> 看渲染後的原信
              ▼
        Discord webhook ──> #電子郵件通知
```

- **不漏信**：拿不到信或 Discord 送不出去就回 500，Resend 會重送（立即、5 秒、5 分鐘、30 分鐘…）。AI 或戰情室失敗不擋通知。
- **不重複**：處理過的信記一天；Resend 重送同一封時直接回 200。超過 12 秒還沒做完先回 503，背景繼續做完，Resend 下次重送時會認出來。
- **AI 讀信**：信件內容標成不可信的資料；寄件網域、回覆地址、SPF／DKIM／DMARC、連結指向的網域、附件名稱一起給 AI 判斷詐騙。OpenAI 端不留存（`store: false`）。

## Discord 格式

| 狀況 | 顏色 | 標題 |
|---|---|---|
| 疑似詐騙 | 琥珀色 `#FAB219` | `⚠️ 可能是詐騙｜<主旨>`，內文多一行「為什麼可疑」 |
| 重要 | 紅 `#E03B3B` | `<主旨>` |
| 一般、摘要失敗 | 藍 `#3987E5` | `<主旨>` |
| 低（DMARC 報告、電子報、行銷） | 灰 `#80848E` | `<主旨>` |

內文只有「寄件人」、AI 摘要和兩個連結：「在戰情室看」（`https://ops.ray-realms.com/mail/<id>`）、「在 Resend 開」。頁尾是分類・重要程度・寄到哪個信箱。信件內容會跳脫 markdown，也不准觸發 @everyone。

## 環境變數

正式環境的值在 AWS Parameter Store `/ray/prod/resend-email-forwarding/`，部署時自動帶進容器（服務設定正本在 SR0725/ray-infra 的 `hosts/prod/resend-email-forwarding/`）。

| 變數 | 必填 | 說明 |
|---|---|---|
| `RESEND_API_KEY` | 是 | 讀收到的信 |
| `RESEND_WEBHOOK_SECRET` | 是 | Resend webhook 的簽章金鑰（`whsec_…`）。沒有就不啟動，部署會保留舊版 |
| `DISCORD_WEBHOOK_URL` | 是 | 通知送到哪個頻道 |
| `OPENAI_API_KEY` | 否 | 沒有的話每封都是「摘要失敗」 |
| `OPENAI_MODEL` | 否 | 預設 `gpt-6-luna` |
| `OPS_INGEST_TOKEN` | 否 | 戰情室 ingest token；沒有的話信不會進戰情室，Discord 改附 Resend 連結 |
| `WARROOM_URL` | 否 | 預設 `https://ops.ray-realms.com` |
| `PORT` | 否 | 預設 `3000`（正式環境 8080） |
| `RUST_LOG` | 否 | 預設 `info` |

## 端點

| 方法 | 路徑 | 說明 |
|---|---|---|
| `POST` | `/webhook` | Resend webhook（驗 `svix-id`、`svix-timestamp`、`svix-signature`，時間差 5 分鐘內） |
| `GET` | `/health` | 健康檢查（回 `OK`） |

## 本機

```bash
cargo test
cargo run --release                      # 要先設好上面的環境變數
```

重跑某幾封信（不用等新信，測試格式用）：

```bash
cargo run -- replay --dry-run <email id>...       # 只印出摘要和 Discord 內容，不送
cargo run -- replay --no-discord <email id>...    # 送進戰情室，不發 Discord
cargo run -- replay --one-message <email id>...   # 合成一則 Discord 訊息（最多 10 封，標明是測試）
```

## 部署

push `main` → GitHub Actions 建 linux/arm64 映像 → 推 ECR → AWS 正式主機上健康檢查通過才切換（`.github/workflows/deploy.yml`）。

## License

MIT
