//! Trusted service processes.
//!
//! The net and storage services own the network (rsurl) and disk on the trusted
//! side of the sandbox (see `docs/PROCESS_MODEL.md`) — the sandboxed content
//! process never touches a socket. The **net service** fetches `LoadUrl` requests
//! over rsurl (threading a persistent cookie jar) and serves them through a
//! conservative in-memory HTTP cache. Freshness comes from `Cache-Control: max-age`
//! or `Expires`/`Date`; a stale entry carrying an `ETag`/`Last-Modified` validator
//! is **revalidated** with a conditional request (`If-None-Match`/`If-Modified-Since`)
//! and refreshed in place on a `304`. The storage service is still a lifecycle
//! skeleton.

use argus_ipc::Channel;
use argus_protocol::{self as proto, Msg};
use argus_util::{log, Role};
use std::io;

/// Run a service process (net or storage) to completion over `channel`.
pub fn run(role: Role, channel: Channel) -> io::Result<()> {
    log::set_role(role);
    let _viewport = proto::child_handshake(&channel)?;
    log!("ready");

    // A persistent cookie jar so sessions survive across requests in this process.
    let mut jar = rsurl::CookieJar::new();
    // A conservative in-memory HTTP cache (honors Cache-Control; see `HttpCache`).
    let mut cache = HttpCache::default();

    loop {
        match proto::recv(&channel) {
            Ok((Msg::LoadUrl { url }, _)) if role == Role::NetService => {
                let (status, body) = match cache.lookup(&url) {
                    CacheLookup::Fresh(body) => {
                        log!("GET {url} -> 200 ({} bytes, cached)", body.len());
                        (200, body)
                    }
                    CacheLookup::Stale { validators, body } => {
                        // Revalidate with a conditional request.
                        let (status, headers, new_body) =
                            fetch(&url, &validators.conditional_headers(), &mut jar);
                        if status == 304 || status == 0 {
                            // Not modified (or transport error): serve the stored body.
                            // A 304 may carry fresh caching headers; honor them.
                            if status == 304 {
                                if let Some(ttl) = freshness_from_headers(&headers) {
                                    cache.refresh(&url, ttl);
                                }
                                log!("GET {url} -> 304 ({} bytes, revalidated)", body.len());
                            } else {
                                log!("GET {url} -> stale-served ({} bytes)", body.len());
                            }
                            (200, body)
                        } else {
                            log!("GET {url} -> {status} ({} bytes, refetched)", new_body.len());
                            if let Some(ttl) = cacheable_ttl(status, &headers) {
                                cache.put(
                                    url.clone(),
                                    new_body.clone(),
                                    ttl,
                                    extract_validators(&headers),
                                );
                            }
                            (status, new_body)
                        }
                    }
                    CacheLookup::Miss => {
                        let (status, headers, body) = fetch(&url, &[], &mut jar);
                        log!("GET {url} -> {status} ({} bytes)", body.len());
                        if let Some(ttl) = cacheable_ttl(status, &headers) {
                            cache.put(url.clone(), body.clone(), ttl, extract_validators(&headers));
                        }
                        (status, body)
                    }
                };
                proto::send(&channel, Msg::ResourceLoaded { status, body }, &[])?;
            }
            Ok((Msg::Shutdown, _)) => {
                log!("shutting down");
                return Ok(());
            }
            Ok((other, _)) => log!("ignoring unexpected message {other:?}"),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                // The browser process is gone; exit quietly.
                log!("browser gone; exiting");
                return Ok(());
            }
            Err(e) => return Err(e),
        }
    }
}

/// Fetch `url` over rsurl, threading `jar` so cookies set by responses are sent on
/// subsequent requests (session persistence). Returns `(status, body)`; `status ==
/// 0` on transport error. The net service runs on the trusted side of the sandbox —
/// content never touches a socket (see `docs/PROCESS_MODEL.md`).
fn fetch(
    url: &str,
    extra_headers: &[(String, String)],
    jar: &mut rsurl::CookieJar,
) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let req = match rsurl::Request::get(url) {
        Ok(req) => req,
        Err(e) => {
            log!("fetch error for {url}: {e}");
            return (0, Vec::new(), Vec::new());
        }
    };
    // Apply conditional-request headers (If-None-Match / If-Modified-Since).
    let req = extra_headers
        .iter()
        .fold(req, |req, (k, v)| req.header(k, v));
    match req.send_with_jar(jar) {
        Ok(resp) => (resp.status, resp.headers, resp.body),
        Err(e) => {
            log!("fetch error for {url}: {e}");
            (0, Vec::new(), Vec::new())
        }
    }
}

/// A conservative in-memory HTTP cache keyed by URL. Entries store the body, an
/// expiry instant, and any revalidation validators; only responses [`cacheable_ttl`]
/// approves are stored.
#[derive(Default)]
struct HttpCache {
    entries: std::collections::HashMap<String, CacheEntry>,
}

struct CacheEntry {
    body: Vec<u8>,
    expiry: std::time::Instant,
    validators: Validators,
}

/// The outcome of consulting the cache for a URL.
enum CacheLookup {
    /// A fresh entry: serve its body without a network request.
    Fresh(Vec<u8>),
    /// An expired entry that carries validators: revalidate with a conditional
    /// request, serving the stored body if the origin answers `304`.
    Stale { validators: Validators, body: Vec<u8> },
    /// No usable entry: fetch unconditionally.
    Miss,
}

impl HttpCache {
    /// Consult the cache: fresh hit, stale-but-revalidatable, or miss. An expired
    /// entry without validators is evicted (it can't be revalidated).
    fn lookup(&mut self, url: &str) -> CacheLookup {
        match self.entries.get(url) {
            Some(e) if e.expiry > std::time::Instant::now() => CacheLookup::Fresh(e.body.clone()),
            Some(e) if !e.validators.is_empty() => CacheLookup::Stale {
                validators: e.validators.clone(),
                body: e.body.clone(),
            },
            Some(_) => {
                self.entries.remove(url);
                CacheLookup::Miss
            }
            None => CacheLookup::Miss,
        }
    }

    fn put(&mut self, url: String, body: Vec<u8>, ttl: std::time::Duration, validators: Validators) {
        let expiry = std::time::Instant::now() + ttl;
        self.entries.insert(
            url,
            CacheEntry {
                body,
                expiry,
                validators,
            },
        );
    }

    /// Extend an entry's freshness after a successful `304` revalidation.
    fn refresh(&mut self, url: &str, ttl: std::time::Duration) {
        if let Some(e) = self.entries.get_mut(url) {
            e.expiry = std::time::Instant::now() + ttl;
        }
    }
}

/// How long a response may be cached, per its headers — or `None` to not cache.
///
/// Conservative and spec-aligned for the common case: only `200` responses are
/// cacheable. Freshness comes from `Cache-Control: max-age=N` (preferred) or, when
/// that's absent, the `Expires` header relative to `Date` (HTTP/1.0 servers). A
/// response is never cached if it carries `no-store`/`no-cache`/`private`, or a
/// `Set-Cookie`/`Vary` header (personalized or request-varying — we key only by URL).
fn cacheable_ttl(status: u16, headers: &[(String, String)]) -> Option<std::time::Duration> {
    if status != 200 {
        return None;
    }
    let get = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    };
    if get("set-cookie").is_some() || get("vary").is_some() {
        return None;
    }
    if let Some(cc) = get("cache-control").map(|v| v.to_ascii_lowercase()) {
        if cc.contains("no-store") || cc.contains("no-cache") || cc.contains("private") {
            return None;
        }
    }
    freshness_from_headers(headers)
}

/// The freshness lifetime a response's headers grant, independent of the
/// cacheability gating in [`cacheable_ttl`]: `Cache-Control: max-age` (preferred),
/// else `Expires` minus `Date`. Reused to refresh an entry on a `304` revalidation.
fn freshness_from_headers(headers: &[(String, String)]) -> Option<std::time::Duration> {
    let get = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    };
    if let Some(cc) = get("cache-control").map(|v| v.to_ascii_lowercase()) {
        if let Some(secs) = cc
            .split(',')
            .find_map(|d| d.trim().strip_prefix("max-age="))
            .and_then(|v| v.trim().parse::<u64>().ok())
        {
            return (secs > 0).then(|| std::time::Duration::from_secs(secs));
        }
    }
    if let Some(expires) = get("expires").and_then(parse_http_date) {
        let base = get("date").and_then(parse_http_date).unwrap_or_else(now_unix);
        let ttl = expires - base;
        return (ttl > 0).then(|| std::time::Duration::from_secs(ttl as u64));
    }
    None
}

/// Cache validators extracted from a response, used to revalidate a stale entry
/// with a conditional request (`If-None-Match` / `If-Modified-Since`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Validators {
    etag: Option<String>,
    last_modified: Option<String>,
}

impl Validators {
    fn is_empty(&self) -> bool {
        self.etag.is_none() && self.last_modified.is_none()
    }

    /// The conditional request headers a revalidation should send.
    fn conditional_headers(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if let Some(etag) = &self.etag {
            out.push(("If-None-Match".to_string(), etag.clone()));
        }
        if let Some(lm) = &self.last_modified {
            out.push(("If-Modified-Since".to_string(), lm.clone()));
        }
        out
    }
}

/// Pull `ETag` / `Last-Modified` validators out of response headers.
fn extract_validators(headers: &[(String, String)]) -> Validators {
    let get = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim().to_string())
    };
    Validators {
        etag: get("etag"),
        last_modified: get("last-modified"),
    }
}

/// Parse an RFC 7231 IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`) to Unix epoch
/// seconds. Returns `None` for any malformed field. Only the modern preferred
/// format is accepted (the obsolete RFC 850 / asctime forms are rare in practice).
fn parse_http_date(s: &str) -> Option<i64> {
    // Drop the leading weekday (`Sun, `) and split on ASCII whitespace.
    let rest = s.split_once(',').map(|(_, r)| r).unwrap_or(s).trim();
    let mut it = rest.split_whitespace();
    let day: i64 = it.next()?.parse().ok()?;
    let month = match it.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i64 = it.next()?.parse().ok()?;
    let mut hms = it.next()?.split(':');
    let hh: i64 = hms.next()?.parse().ok()?;
    let mm: i64 = hms.next()?.parse().ok()?;
    let ss: i64 = hms.next()?.parse().ok()?;
    if hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hh * 3_600 + mm * 60 + ss)
}

/// Days since the Unix epoch for a proleptic-Gregorian date (Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Current Unix time in seconds (wall clock), for the rare `Expires`-without-`Date`.
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn h(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn cacheability_rules() {
        // Cacheable: 200 + max-age, no excluding directives/headers.
        assert_eq!(
            cacheable_ttl(200, &h(&[("Cache-Control", "max-age=300")])),
            Some(Duration::from_secs(300))
        );
        // Not cacheable: non-200, no-store/no-cache/private, missing max-age.
        assert_eq!(
            cacheable_ttl(404, &h(&[("Cache-Control", "max-age=300")])),
            None
        );
        assert_eq!(
            cacheable_ttl(200, &h(&[("Cache-Control", "no-store, max-age=300")])),
            None
        );
        assert_eq!(
            cacheable_ttl(200, &h(&[("Cache-Control", "private, max-age=60")])),
            None
        );
        assert_eq!(
            cacheable_ttl(200, &h(&[("Cache-Control", "max-age=0")])),
            None
        );
        assert_eq!(cacheable_ttl(200, &h(&[])), None);
        // Personalized/varying responses are never cached.
        assert_eq!(
            cacheable_ttl(
                200,
                &h(&[("Cache-Control", "max-age=300"), ("Set-Cookie", "s=1")])
            ),
            None
        );
        assert_eq!(
            cacheable_ttl(
                200,
                &h(&[("Cache-Control", "max-age=300"), ("Vary", "Cookie")])
            ),
            None
        );
    }

    fn no_validators() -> Validators {
        Validators::default()
    }

    fn is_fresh(l: &CacheLookup) -> bool {
        matches!(l, CacheLookup::Fresh(_))
    }

    #[test]
    fn cache_store_and_expiry() {
        let mut c = HttpCache::default();
        c.put("u".into(), b"body".to_vec(), Duration::from_secs(60), no_validators());
        assert!(matches!(c.lookup("u"), CacheLookup::Fresh(b) if b == b"body"));
        // An expired entry without validators is not served (and is evicted).
        c.put("v".into(), b"x".to_vec(), Duration::from_secs(0), no_validators());
        assert!(matches!(c.lookup("v"), CacheLookup::Miss));
        assert!(matches!(c.lookup("missing"), CacheLookup::Miss));
    }

    #[test]
    fn expired_entry_with_validators_is_revalidatable() {
        let mut c = HttpCache::default();
        let v = Validators {
            etag: Some("\"abc\"".into()),
            last_modified: None,
        };
        c.put("u".into(), b"cached".to_vec(), Duration::from_secs(0), v);
        // Stale but revalidatable: returned as Stale (not evicted), with its validators.
        match c.lookup("u") {
            CacheLookup::Stale { validators, body } => {
                assert_eq!(body, b"cached");
                assert_eq!(
                    validators.conditional_headers(),
                    vec![("If-None-Match".to_string(), "\"abc\"".to_string())]
                );
            }
            _ => panic!("expected Stale"),
        }
        // A successful revalidation refreshes freshness.
        c.refresh("u", Duration::from_secs(60));
        assert!(is_fresh(&c.lookup("u")));
    }

    #[test]
    fn expires_header_grants_freshness() {
        // No max-age, but Expires is 1h after Date → 3600s TTL.
        let ttl = cacheable_ttl(
            200,
            &h(&[
                ("Date", "Sun, 06 Nov 1994 08:00:00 GMT"),
                ("Expires", "Sun, 06 Nov 1994 09:00:00 GMT"),
            ]),
        );
        assert_eq!(ttl, Some(Duration::from_secs(3600)));
        // Already-expired Expires → not cacheable.
        assert_eq!(
            cacheable_ttl(
                200,
                &h(&[
                    ("Date", "Sun, 06 Nov 1994 09:00:00 GMT"),
                    ("Expires", "Sun, 06 Nov 1994 08:00:00 GMT"),
                ])
            ),
            None
        );
        // max-age wins over Expires when both are present.
        assert_eq!(
            cacheable_ttl(
                200,
                &h(&[
                    ("Cache-Control", "max-age=120"),
                    ("Expires", "Sun, 06 Nov 1994 09:00:00 GMT"),
                ])
            ),
            Some(Duration::from_secs(120))
        );
    }

    #[test]
    fn http_date_parsing() {
        // The canonical RFC 7231 example: 784111777 seconds since the epoch.
        assert_eq!(
            parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(784_111_777)
        );
        assert_eq!(parse_http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(parse_http_date("not a date"), None);
        assert!(parse_http_date("Mon, 32 Jan 2020 00:00:00 GMT").is_some()); // lenient day
    }

    #[test]
    fn validator_extraction_and_conditional_headers() {
        let v = extract_validators(&h(&[
            ("ETag", "\"v1\""),
            ("Last-Modified", "Sun, 06 Nov 1994 08:49:37 GMT"),
        ]));
        assert_eq!(v.etag.as_deref(), Some("\"v1\""));
        assert_eq!(
            v.conditional_headers(),
            vec![
                ("If-None-Match".to_string(), "\"v1\"".to_string()),
                (
                    "If-Modified-Since".to_string(),
                    "Sun, 06 Nov 1994 08:49:37 GMT".to_string()
                ),
            ]
        );
        assert!(extract_validators(&h(&[])).is_empty());
    }
}
