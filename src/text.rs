//! 文字小工具：HTML 轉純文字、截斷、抓連結網域、Discord markdown 跳脫、時間格式。

use regex::Regex;
use std::sync::LazyLock;

static BLOCKS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)<(head|style|script|title)\b.*?</(head|style|script|title)\s*>").unwrap()
});
static BREAKS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<br\s*/?>|</(p|div|tr|li|h[1-6]|table)\s*>").unwrap());
static TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t\u{a0}]+").unwrap());
static BLANK_LINES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n\s*\n\s*\n+").unwrap());
static HREF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\bhref\s*=\s*["']?(https?://[^"'\s>]+)"#).unwrap());
static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s<>"']+"#).unwrap());

/// HTML → 讀得懂的純文字（拿掉 head／style／script，段落換行，空白收斂）
pub fn html_to_text(html: &str) -> String {
    let s = BLOCKS.replace_all(html, "");
    let s = BREAKS.replace_all(&s, "\n");
    let s = TAGS.replace_all(&s, "");
    let s = htmlescape::decode_html(&s).unwrap_or_else(|_| s.to_string());
    let s = SPACES.replace_all(&s, " ");
    let lines: Vec<&str> = s.lines().map(str::trim).collect();
    BLANK_LINES
        .replace_all(&lines.join("\n"), "\n\n")
        .trim()
        .to_string()
}

/// 截到 max 個字（不是位元組），超過補「…」
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

/// 截到 max 位元組以內（不切壞 UTF-8）
pub fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// 信裡的連結指向哪些網域（給 AI 判斷詐騙用），依出現次數排序，最多 12 個
pub fn link_hosts(html: Option<&str>, text: Option<&str>) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    let mut add = |url: &str| {
        let Ok(u) = reqwest::Url::parse(url) else {
            return;
        };
        let Some(host) = u.host_str() else { return };
        let host = host.to_ascii_lowercase();
        match counts.iter_mut().find(|(h, _)| *h == host) {
            Some((_, n)) => *n += 1,
            None => counts.push((host, 1)),
        }
    };
    if let Some(h) = html {
        for c in HREF.captures_iter(h) {
            add(&c[1]);
        }
    }
    if let Some(t) = text {
        for m in URL.find_iter(t) {
            add(m.as_str());
        }
    }
    counts.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    counts.truncate(12);
    counts
}

/// Discord markdown 跳脫：信件內容（寄件人、AI 摘要）不能變成連結、粗體或提及
pub fn md_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '*'
                | '_'
                | '~'
                | '`'
                | '|'
                | '>'
                | '['
                | ']'
                | '('
                | ')'
                | '#'
                | '-'
                | '@'
                | '<'
                | ':'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// 現在的 UTC 時間，ISO 8601（2026-10-18T04:21:00Z）
pub fn iso_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    iso_from_unix(secs)
}

pub fn iso_from_unix(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant 的 civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_to_text_keeps_paragraphs() {
        let html = "<html><head><title>x</title><style>p{color:red}</style></head><body><p>Dear&nbsp;Customer,</p><p>Please   confirm</p><br><a href='https://x.test'>VERIFY</a></body></html>";
        assert_eq!(
            html_to_text(html),
            "Dear Customer,\nPlease confirm\n\nVERIFY"
        );
    }

    #[test]
    fn hosts_counted() {
        let html = r#"<a href="https://secure-1.s3.us-east-1.amazonaws.com/a.html#x">a</a><a href='https://redia.ai/'>b</a><a href="https://redia.ai/x">c</a>"#;
        assert_eq!(
            link_hosts(Some(html), Some("see https://Example.com/x")),
            vec![
                ("redia.ai".into(), 2),
                ("secure-1.s3.us-east-1.amazonaws.com".into(), 1),
                ("example.com".into(), 1)
            ]
        );
    }

    #[test]
    fn escapes_markdown_links_and_mentions() {
        assert_eq!(
            md_escape("[點我](https://evil) @everyone"),
            "\\[點我\\]\\(https\\://evil\\) \\@everyone"
        );
    }

    #[test]
    fn iso_dates() {
        assert_eq!(iso_from_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_from_unix(1_792_268_479), "2026-10-17T20:21:19Z");
        assert_eq!(iso_from_unix(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate("一二三四五", 3), "一二…");
        assert_eq!(truncate_bytes("一二三", 7), "一二");
    }
}
