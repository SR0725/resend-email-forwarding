//! 驗證 Resend webhook 的簽章（Resend 用 Svix 送 webhook）。
//!
//! 簽的內容是 `{svix-id}.{svix-timestamp}.{原始 body}`，用 HMAC-SHA256（金鑰是 `whsec_` 後面那段 base64 解開），
//! 結果 base64 後放在 `svix-signature`，格式 `v1,<簽章>`，可能有好幾個（換金鑰期間），用空白隔開。
//! 時間戳超過 5 分鐘的一律拒絕，避免有人錄下舊的請求重送。

use axum::http::HeaderMap;
use base64::{Engine, engine::general_purpose::STANDARD};
use hmac::{Hmac, Mac};
use sha2::Sha256;

const TOLERANCE_SECS: i64 = 5 * 60;

#[derive(Debug, PartialEq, Eq)]
pub enum VerifyError {
    MissingHeaders,
    BadTimestamp,
    Expired,
    NoMatch,
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            VerifyError::MissingHeaders => {
                "沒有簽章標頭（svix-id／svix-timestamp／svix-signature）"
            }
            VerifyError::BadTimestamp => "svix-timestamp 不是數字",
            VerifyError::Expired => "簽章時間跟現在差超過 5 分鐘",
            VerifyError::NoMatch => "簽章不對",
        })
    }
}

/// `whsec_xxx` → HMAC 金鑰。格式不對回 None。
pub fn decode_secret(secret: &str) -> Option<Vec<u8>> {
    let s = secret.trim();
    let raw = s.strip_prefix("whsec_").unwrap_or(s);
    STANDARD.decode(raw).ok().filter(|k| !k.is_empty())
}

fn header<'a>(h: &'a HeaderMap, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|n| h.get(*n).and_then(|v| v.to_str().ok()))
}

pub fn verify(
    key: &[u8],
    headers: &HeaderMap,
    body: &[u8],
    now_secs: i64,
) -> Result<(), VerifyError> {
    let id = header(headers, &["svix-id", "webhook-id"]).ok_or(VerifyError::MissingHeaders)?;
    let ts_raw = header(headers, &["svix-timestamp", "webhook-timestamp"])
        .ok_or(VerifyError::MissingHeaders)?;
    let sigs = header(headers, &["svix-signature", "webhook-signature"])
        .ok_or(VerifyError::MissingHeaders)?;
    let ts: i64 = ts_raw
        .trim()
        .parse()
        .map_err(|_| VerifyError::BadTimestamp)?;
    if (now_secs - ts).abs() > TOLERANCE_SECS {
        return Err(VerifyError::Expired);
    }

    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC 接受任何長度的金鑰");
    mac.update(id.as_bytes());
    mac.update(b".");
    mac.update(ts_raw.as_bytes());
    mac.update(b".");
    mac.update(body);

    for part in sigs.split_whitespace() {
        let Some(sig) = part.strip_prefix("v1,") else {
            continue;
        };
        let Ok(sig) = STANDARD.decode(sig) else {
            continue;
        };
        // verify_slice 是固定時間比較
        if mac.clone().verify_slice(&sig).is_ok() {
            return Ok(());
        }
    }
    Err(VerifyError::NoMatch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(id: &str, ts: &str, sig: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("svix-id", HeaderValue::from_str(id).unwrap());
        h.insert("svix-timestamp", HeaderValue::from_str(ts).unwrap());
        h.insert("svix-signature", HeaderValue::from_str(sig).unwrap());
        h
    }

    fn sign(key: &[u8], id: &str, ts: &str, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
        mac.update(format!("{id}.{ts}.").as_bytes());
        mac.update(body);
        format!("v1,{}", STANDARD.encode(mac.finalize().into_bytes()))
    }

    // Svix 文件上的範例（https://docs.svix.com/receiving/verifying-payloads/how-manual）
    #[test]
    fn svix_official_example() {
        let key = decode_secret("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw").unwrap();
        let h = headers(
            "msg_p5jXN8AQM9LWM0D4loKWxJek",
            "1614265330",
            "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=",
        );
        assert_eq!(
            verify(&key, &h, br#"{"test": 2432232314}"#, 1614265330),
            Ok(())
        );
    }

    #[test]
    fn accepts_any_matching_signature() {
        let key = decode_secret("whsec_c2VjcmV0LWtleS1mb3ItdGVzdHM=").unwrap();
        let body = br#"{"type":"email.received","data":{"email_id":"abc"}}"#;
        let good = sign(&key, "msg_1", "1700000000", body);
        let h = headers("msg_1", "1700000000", &format!("v1,AAAA {good} v2,zzz"));
        assert_eq!(verify(&key, &h, body, 1700000100), Ok(()));
    }

    #[test]
    fn rejects_tampered_old_or_unsigned() {
        let key = decode_secret("whsec_c2VjcmV0LWtleS1mb3ItdGVzdHM=").unwrap();
        let body = br#"{"type":"email.received","data":{"email_id":"abc"}}"#;
        let sig = sign(&key, "msg_1", "1700000000", body);
        let h = headers("msg_1", "1700000000", &sig);
        assert_eq!(
            verify(
                &key,
                &h,
                br#"{"type":"email.received","data":{"email_id":"evil"}}"#,
                1700000000
            ),
            Err(VerifyError::NoMatch)
        );
        assert_eq!(
            verify(&key, &h, body, 1700000000 + 301),
            Err(VerifyError::Expired)
        );
        assert_eq!(
            verify(b"other-key", &h, body, 1700000000),
            Err(VerifyError::NoMatch)
        );
        assert_eq!(
            verify(&key, &HeaderMap::new(), body, 1700000000),
            Err(VerifyError::MissingHeaders)
        );
        assert_eq!(
            verify(&key, &headers("msg_1", "abc", &sig), body, 1700000000),
            Err(VerifyError::BadTimestamp)
        );
    }

    #[test]
    fn secret_format() {
        assert!(decode_secret("whsec_").is_none());
        assert!(decode_secret("whsec_???").is_none());
        assert!(decode_secret("c2VjcmV0").is_some());
    }
}
