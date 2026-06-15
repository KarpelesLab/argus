//! Trusted service processes.
//!
//! The net and storage services own the network (rsurl) and disk on the trusted
//! side of the sandbox (see `docs/PROCESS_MODEL.md`) — the sandboxed content
//! process never touches a socket. The **net service** fetches `LoadUrl` requests
//! over rsurl (threading a persistent cookie jar) and serves them through a
//! conservative in-memory HTTP cache that honors `Cache-Control`. The storage
//! service is still a lifecycle skeleton.

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
                let (status, body) = if let Some(body) = cache.get(&url) {
                    log!("GET {url} -> 200 ({} bytes, cached)", body.len());
                    (200, body)
                } else {
                    let (status, headers, body) = fetch(&url, &mut jar);
                    log!("GET {url} -> {status} ({} bytes)", body.len());
                    if let Some(ttl) = cacheable_ttl(status, &headers) {
                        cache.put(url.clone(), body.clone(), ttl);
                    }
                    (status, body)
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
fn fetch(url: &str, jar: &mut rsurl::CookieJar) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let result = rsurl::Request::get(url).and_then(|req| req.send_with_jar(jar));
    match result {
        Ok(resp) => (resp.status, resp.headers, resp.body),
        Err(e) => {
            log!("fetch error for {url}: {e}");
            (0, Vec::new(), Vec::new())
        }
    }
}

/// A conservative in-memory HTTP cache keyed by URL. Entries store the body and an
/// expiry instant; only responses [`cacheable_ttl`] approves are stored.
#[derive(Default)]
struct HttpCache {
    entries: std::collections::HashMap<String, (Vec<u8>, std::time::Instant)>,
}

impl HttpCache {
    /// The body for `url` if a fresh (unexpired) entry exists.
    fn get(&mut self, url: &str) -> Option<Vec<u8>> {
        match self.entries.get(url) {
            Some((body, expiry)) if *expiry > std::time::Instant::now() => Some(body.clone()),
            Some(_) => {
                self.entries.remove(url); // expired
                None
            }
            None => None,
        }
    }

    fn put(&mut self, url: String, body: Vec<u8>, ttl: std::time::Duration) {
        let expiry = std::time::Instant::now() + ttl;
        self.entries.insert(url, (body, expiry));
    }
}

/// How long a response may be cached, per its headers — or `None` to not cache.
///
/// Conservative and spec-aligned for the common case: only `200` responses are
/// cacheable, and only when `Cache-Control: max-age=N` is present (and not
/// `no-store`/`no-cache`/`private`). Responses carrying `Set-Cookie` or `Vary`
/// are never cached (they're personalized or vary by request), since we key only
/// by URL. `Expires`/`ETag` revalidation aren't handled yet.
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
    let cc = get("cache-control")?.to_ascii_lowercase();
    if cc.contains("no-store") || cc.contains("no-cache") || cc.contains("private") {
        return None;
    }
    // Parse `max-age=<seconds>`.
    let secs: u64 = cc
        .split(',')
        .find_map(|d| d.trim().strip_prefix("max-age="))
        .and_then(|v| v.trim().parse().ok())?;
    (secs > 0).then(|| std::time::Duration::from_secs(secs))
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

    #[test]
    fn cache_store_and_expiry() {
        let mut c = HttpCache::default();
        c.put("u".into(), b"body".to_vec(), Duration::from_secs(60));
        assert_eq!(c.get("u"), Some(b"body".to_vec()));
        // An already-expired entry is not served (and is evicted).
        c.put("v".into(), b"x".to_vec(), Duration::from_secs(0));
        assert_eq!(c.get("v"), None);
        assert_eq!(c.get("missing"), None);
    }
}
