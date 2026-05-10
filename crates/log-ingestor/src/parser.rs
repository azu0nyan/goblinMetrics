use anyhow::{Context, Result};
use chrono::DateTime;
use serde::Deserialize;
use serde_json::Value;

/// Parsed from one JSON log line produced by the goblin_json nginx format.
#[derive(Debug)]
pub struct LogEntry {
    pub timestamp_ms: i64,
    pub url:          String,
    pub ip:           String,
    pub host:         String,
    pub user_agent:   Option<String>,
    pub status_code:  i64,
    pub headers:      String,
}

/// Raw struct matching the JSON fields emitted by nginx.
#[derive(Debug, Deserialize)]
struct RawEntry {
    remote_addr:     String,
    time_local:      String,
    request:         String,
    status:          Value,
    #[serde(default)]
    user_agent:      String,
    #[serde(default)]
    referer:         String,
    #[serde(default)]
    accept:          String,
    #[serde(default)]
    accept_language: String,
    #[serde(default)]
    x_forwarded_for: String,
    #[serde(default)]
    content_type:    String,
    #[serde(default)]
    host:            String,
}

pub fn parse_line(line: &str) -> Result<LogEntry> {
    let raw: RawEntry =
        serde_json::from_str(line).with_context(|| format!("invalid JSON: {line:.120}"))?;

    let timestamp_ms = parse_nginx_time(&raw.time_local)
        .with_context(|| format!("bad time_local: {}", raw.time_local))?;

    let url = extract_url(&raw.request);

    let status_code = raw
        .status
        .as_i64()
        .or_else(|| {
            raw.status
                .as_str()
                .and_then(|s| s.parse::<i64>().ok())
        })
        .context("status is not a number")?;

    let ua = if raw.user_agent.is_empty() {
        None
    } else {
        Some(raw.user_agent.clone())
    };

    let headers = build_headers_json(&raw);

    Ok(LogEntry {
        timestamp_ms,
        url,
        ip: raw.remote_addr,
        host: raw.host,
        user_agent: ua,
        status_code,
        headers,
    })
}

/// nginx time_local: "09/May/2026:17:05:56 +0200"
fn parse_nginx_time(s: &str) -> Result<i64> {
    let dt = DateTime::parse_from_str(s, "%d/%b/%Y:%H:%M:%S %z")
        .with_context(|| format!("chrono parse: {s}"))?;
    Ok(dt.timestamp_millis())
}

/// Extract URL from "GET /path HTTP/2.0" → "/path"
fn extract_url(request: &str) -> String {
    let parts: Vec<&str> = request.splitn(3, ' ').collect();
    if parts.len() >= 2 {
        parts[1].to_string()
    } else {
        request.to_string()
    }
}

fn build_headers_json(raw: &RawEntry) -> String {
    let mut map = serde_json::Map::new();
    if !raw.referer.is_empty()         { map.insert("referer".into(),         raw.referer.clone().into()); }
    if !raw.user_agent.is_empty()      { map.insert("user-agent".into(),      raw.user_agent.clone().into()); }
    if !raw.accept.is_empty()          { map.insert("accept".into(),           raw.accept.clone().into()); }
    if !raw.accept_language.is_empty() { map.insert("accept-language".into(), raw.accept_language.clone().into()); }
    if !raw.x_forwarded_for.is_empty() { map.insert("x-forwarded-for".into(), raw.x_forwarded_for.clone().into()); }
    if !raw.content_type.is_empty()    { map.insert("content-type".into(),    raw.content_type.clone().into()); }
    serde_json::to_string(&map).unwrap_or_else(|_| "{}".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"{"remote_addr":"1.2.3.4","time_local":"09/May/2026:17:05:56 +0200","request":"GET /goblin-lore HTTP/2.0","status":200,"body_bytes_sent":2426,"referer":"https://goblin.geno.su/","user_agent":"Mozilla/5.0","accept":"text/html","accept_language":"en-US","x_forwarded_for":"","content_type":"","host":"goblin.geno.su"}"#;

    #[test]
    fn parse_valid_line() {
        let e = parse_line(VALID).expect("should parse");
        assert_eq!(e.ip, "1.2.3.4");
        assert_eq!(e.url, "/goblin-lore");
        assert_eq!(e.status_code, 200);
        assert_eq!(e.host, "goblin.geno.su");
        assert!(e.user_agent.as_deref() == Some("Mozilla/5.0"));
        let h: serde_json::Value = serde_json::from_str(&e.headers).unwrap();
        assert_eq!(h["accept"], "text/html");
    }

    #[test]
    fn parse_line_without_host_defaults_empty() {
        let line = r#"{"remote_addr":"1.2.3.4","time_local":"09/May/2026:17:05:56 +0200","request":"GET / HTTP/1.1","status":200,"body_bytes_sent":0,"referer":"","user_agent":"","accept":"","accept_language":"","x_forwarded_for":"","content_type":""}"#;
        let e = parse_line(line).expect("should parse without host");
        assert_eq!(e.host, "");
    }

    #[test]
    fn parse_malformed_line_returns_error() {
        assert!(parse_line("not json at all").is_err());
    }

    #[test]
    fn extract_url_normal() {
        assert_eq!(extract_url("GET /path HTTP/1.1"), "/path");
    }

    #[test]
    fn extract_url_degenerate() {
        assert_eq!(extract_url("/just-path"), "/just-path");
    }
}
