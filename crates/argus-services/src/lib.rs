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
                // `ctype`/`cdisp` are only populated on a fresh fetch (downloads aren't
                // cached, so a cache hit serves them empty → renders as a page).
                let (status, body, csp, ctype, cdisp) = match cache.lookup(&url) {
                    CacheLookup::Fresh { body, csp } => {
                        log!("GET {url} -> 200 ({} bytes, cached)", body.len());
                        (200, body, csp, String::new(), String::new())
                    }
                    CacheLookup::Stale {
                        validators,
                        body,
                        csp,
                    } => {
                        // Revalidate with a conditional request.
                        let (status, headers, new_body) =
                            fetch(&url, &validators.conditional_headers(), &mut jar);
                        if status == 304 || status == 0 {
                            // Not modified (or transport error): serve the stored body
                            // and its stored CSP. A 304 may carry fresh caching
                            // headers; honor them.
                            if status == 304 {
                                if let Some(ttl) = freshness_from_headers(&headers) {
                                    cache.refresh(&url, ttl);
                                }
                                log!("GET {url} -> 304 ({} bytes, revalidated)", body.len());
                            } else {
                                log!("GET {url} -> stale-served ({} bytes)", body.len());
                            }
                            (200, body, csp, String::new(), String::new())
                        } else {
                            log!("GET {url} -> {status} ({} bytes, refetched)", new_body.len());
                            let csp = extract_csp(&headers);
                            if let Some(ttl) = cacheable_ttl(status, &headers) {
                                cache.put(
                                    url.clone(),
                                    new_body.clone(),
                                    ttl,
                                    extract_validators(&headers),
                                    csp.clone(),
                                );
                            }
                            let ct = extract_header(&headers, "content-type");
                            let cd = extract_header(&headers, "content-disposition");
                            (status, new_body, csp, ct, cd)
                        }
                    }
                    CacheLookup::Miss => {
                        let (status, headers, body) = fetch(&url, &[], &mut jar);
                        log!("GET {url} -> {status} ({} bytes)", body.len());
                        let csp = extract_csp(&headers);
                        if let Some(ttl) = cacheable_ttl(status, &headers) {
                            cache.put(
                                url.clone(),
                                body.clone(),
                                ttl,
                                extract_validators(&headers),
                                csp.clone(),
                            );
                        }
                        let ct = extract_header(&headers, "content-type");
                        let cd = extract_header(&headers, "content-disposition");
                        (status, body, csp, ct, cd)
                    }
                };
                proto::send(
                    &channel,
                    Msg::ResourceLoaded {
                        status,
                        body,
                        csp,
                        content_type: ctype,
                        content_disposition: cdisp,
                    },
                    &[],
                )?;
            }
            Ok((Msg::PostUrl { url, body }, _)) if role == Role::NetService => {
                // Form POST: never cached (POST is not idempotent), always hits the
                // network. Cookies set by the response thread through the jar.
                let (status, headers, resp) = post(&url, &body, &mut jar);
                log!(
                    "POST {url} ({} bytes) -> {status} ({} bytes)",
                    body.len(),
                    resp.len()
                );
                let csp = extract_csp(&headers);
                proto::send(
                    &channel,
                    Msg::ResourceLoaded {
                        status,
                        body: resp,
                        csp,
                        content_type: extract_header(&headers, "content-type"),
                        content_disposition: extract_header(&headers, "content-disposition"),
                    },
                    &[],
                )?;
            }
            Ok((Msg::StartDownload { url, dir }, _)) if role == Role::NetService => {
                // Stream a download to disk (the net service is trusted — it owns the
                // socket and the filesystem) and report progress/completion to the
                // caller. Runs synchronously: the caller has a dedicated net process.
                download_to(&channel, &url, &dir)?;
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

/// POST `body` (`application/x-www-form-urlencoded`) to `url`, threading `jar` for
/// cookies. Returns `(status, headers, body)`; `status == 0` on transport error.
fn post(
    url: &str,
    body: &[u8],
    jar: &mut rsurl::CookieJar,
) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let req = match rsurl::Request::new("POST", url) {
        Ok(req) => req,
        Err(e) => {
            log!("post error for {url}: {e}");
            return (0, Vec::new(), Vec::new());
        }
    };
    let req = req
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body.to_vec());
    match req.send_with_jar(jar) {
        Ok(resp) => (resp.status, resp.headers, resp.body),
        Err(e) => {
            log!("post error for {url}: {e}");
            (0, Vec::new(), Vec::new())
        }
    }
}

/// Download `url` into directory `dir`, reporting `DownloadStarted`/`DownloadProgress`
/// and a terminal `DownloadDone` to the caller. A `magnet:` link or `.torrent` source
/// goes through BitTorrent; everything else is a streamed HTTP(S) fetch.
fn download_to(channel: &Channel, url: &str, dir: &str) -> io::Result<()> {
    let is_torrent =
        url.starts_with("magnet:") || url.split(['?', '#']).next().unwrap_or(url).ends_with(".torrent");
    let result = if is_torrent {
        torrent_download(channel, url, dir)
    } else {
        http_download(channel, url, dir)
    };
    match result {
        Ok(path) => {
            log!("download complete: {}", path.display());
            proto::send(
                channel,
                Msg::DownloadDone {
                    ok: true,
                    path: path.to_string_lossy().into_owned(),
                    error: String::new(),
                },
                &[],
            )
        }
        Err(e) => {
            log!("download failed for {url}: {e}");
            proto::send(
                channel,
                Msg::DownloadDone {
                    ok: false,
                    path: String::new(),
                    error: e,
                },
                &[],
            )
        }
    }
}

/// Download a `magnet:` link or `.torrent` (an `http(s)` URL or a local/`file://`
/// path) into `dir` via rsurl's BitTorrent engine, emitting `DownloadStarted` (once
/// the metadata names the content) and throttled `DownloadProgress`. Mirrors rsurl's
/// `run_bittorrent` orchestration: obtain the metainfo, gather peers (magnet peers →
/// tracker announce → DHT), then `bittorrent::download` with a progress callback.
/// Returns the saved path (the file for a single-file torrent, else the `dir/<name>`
/// directory). No seeding (`SeedMode::Off`).
fn torrent_download(channel: &Channel, source: &str, dir: &str) -> Result<std::path::PathBuf, String> {
    use rsurl::bittorrent::{self, metadata, Magnet, Metainfo, Progress, SeedMode, TorrentOptions};
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};

    let dir = Path::new(dir);
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let peer_id = bittorrent::generate_peer_id().map_err(|e| e.to_string())?;
    let opts = TorrentOptions {
        peer_id,
        seed: SeedMode::Off,
        ..Default::default()
    };
    let port = opts.listen_port;

    // 1) Obtain the metainfo (and, for a magnet, the peers used to fetch it).
    let mut peers: Vec<SocketAddr> = Vec::new();
    let meta: Metainfo = if source.starts_with("magnet:") {
        let magnet = Magnet::parse(source).map_err(|e| e.to_string())?;
        peers.extend(magnet.peers.iter().copied());
        if peers.is_empty() {
            peers = bt_announce_peers(&magnet.trackers, magnet.info_hash, peer_id, port, 0);
        }
        if peers.is_empty() {
            peers = bt_dht_peers(magnet.info_hash);
        }
        peers.sort();
        peers.dedup();
        if peers.is_empty() {
            return Err("no peers found to fetch magnet metadata".to_string());
        }
        log!("torrent: fetching metadata from {} peers", peers.len());
        let (m, _info) = metadata::fetch_metainfo(
            magnet.info_hash,
            &peers,
            peer_id,
            std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(10),
            false,
        )
        .map_err(|e| e.to_string())?;
        m
    } else {
        let bytes = if source.starts_with("http://") || source.starts_with("https://") {
            let resp = rsurl::Request::get(source)
                .and_then(|r| r.send())
                .map_err(|e| e.to_string())?;
            if resp.status != 200 {
                return Err(format!("fetching torrent: HTTP {}", resp.status));
            }
            resp.body
        } else {
            let path = source.strip_prefix("file://").unwrap_or(source);
            std::fs::read(path).map_err(|e| e.to_string())?
        };
        Metainfo::from_bytes(&bytes).map_err(|e| e.to_string())?
    };

    // 2) Output layout + the path we report to the caller.
    let layout = bittorrent::file_layout(&meta, dir);
    let target: PathBuf = if layout.len() == 1 {
        layout[0].0.clone()
    } else {
        dir.join(&meta.name)
    };
    let _ = proto::send(
        channel,
        Msg::DownloadStarted {
            path: target.to_string_lossy().into_owned(),
        },
        &[],
    );

    // 3) Peers for the download itself (a magnet already has them from step 1).
    if peers.is_empty() {
        peers = bt_announce_peers(&meta.trackers, meta.info_hash, peer_id, port, meta.total_length);
    }
    if peers.is_empty() {
        peers = bt_dht_peers(meta.info_hash);
    }
    peers.sort();
    peers.dedup();
    if peers.is_empty() {
        return Err("no peers found (trackers and DHT returned none)".to_string());
    }
    log!(
        "torrent: {} ({} bytes, {} pieces, {} peers)",
        meta.name,
        meta.total_length,
        meta.num_pieces(),
        peers.len()
    );

    // 4) Download with throttled progress (skip the seeding phase — SeedMode::Off).
    let total = meta.total_length;
    let mut last_sent = 0u64;
    let mut cb = |p: &Progress| {
        if p.downloaded.saturating_sub(last_sent) >= 256 * 1024 || p.downloaded >= total {
            last_sent = p.downloaded;
            let _ = proto::send(channel, Msg::DownloadProgress { done: p.downloaded, total }, &[]);
        }
    };
    bittorrent::download(&meta, layout, &peers, &opts, &mut cb).map_err(|e| e.to_string())?;
    Ok(target)
}

/// Announce the torrent to its trackers (the first responsive one wins) and return
/// the peers. Mirrors rsurl's `bt_announce_peers`.
fn bt_announce_peers(
    trackers: &[String],
    info_hash: [u8; 20],
    peer_id: [u8; 20],
    port: u16,
    left: u64,
) -> Vec<std::net::SocketAddr> {
    use rsurl::bittorrent::tracker::{self, AnnounceParams, Event};
    let params = AnnounceParams {
        info_hash,
        peer_id,
        port,
        uploaded: 0,
        downloaded: 0,
        left,
        event: Event::Started,
        num_want: 100,
        key: 0,
    };
    let mut peers = Vec::new();
    for t in trackers {
        if let Ok(resp) = tracker::announce(t, &params, std::time::Duration::from_secs(10)) {
            peers.extend(resp.peers);
        }
        if !peers.is_empty() {
            break;
        }
    }
    peers
}

/// Find peers for `info_hash` via the BitTorrent DHT. Mirrors rsurl's `bt_dht_peers`.
fn bt_dht_peers(info_hash: [u8; 20]) -> Vec<std::net::SocketAddr> {
    use rsurl::bittorrent::dht;
    let bootstrap = dht::default_bootstrap();
    if bootstrap.is_empty() {
        return Vec::new();
    }
    dht::find_peers(
        info_hash,
        &bootstrap,
        dht::random_node_id(),
        std::time::Duration::from_secs(20),
    )
    .unwrap_or_default()
}

/// Stream an HTTP(S) `url` to a file in `dir` via rsurl, emitting `DownloadStarted`
/// (once the filename is known from the response head) and throttled
/// `DownloadProgress`. Returns the saved path, or an error string. No cookies/resume
/// yet (Slice 1).
fn http_download(channel: &Channel, url: &str, dir: &str) -> Result<std::path::PathBuf, String> {
    use std::cell::RefCell;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    /// Progress reported at most once per this many bytes, to bound IPC chatter.
    const PROGRESS_STEP: u64 = 256 * 1024;

    #[derive(Default)]
    struct DlState {
        file: Option<std::fs::File>,
        path: PathBuf,
        total: u64,
        done: u64,
        last_sent: u64,
        /// Set when the head/open fails; aborts the chunk loop and is the final error.
        err: Option<String>,
    }

    let req = rsurl::Request::get(url).map_err(|e| e.to_string())?;
    let st = RefCell::new(DlState::default());

    let on_head = |head: &rsurl::ResponseHead| {
        let mut s = st.borrow_mut();
        if !(200..300).contains(&head.status) {
            s.err = Some(format!("HTTP {}", head.status));
            return;
        }
        let get = |name: &str| {
            head.headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        };
        s.total = get("content-length").and_then(|v| v.trim().parse().ok()).unwrap_or(0);
        let name = download_filename(get("content-disposition"), url);
        let path = dedupe_path(Path::new(dir), &name, |p| p.exists());
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                s.err = Some(e.to_string());
                return;
            }
        }
        match std::fs::File::create(&path) {
            Ok(f) => {
                s.file = Some(f);
                let _ = proto::send(
                    channel,
                    Msg::DownloadStarted {
                        path: path.to_string_lossy().into_owned(),
                    },
                    &[],
                );
                s.path = path;
            }
            Err(e) => s.err = Some(e.to_string()),
        }
    };

    let on_chunk = |chunk: &[u8]| -> rsurl::Result<()> {
        let mut s = st.borrow_mut();
        if s.err.is_some() {
            // Abort: an error page body / failed open — stop reading.
            return Err(io::Error::other("download aborted").into());
        }
        if let Some(f) = s.file.as_mut() {
            f.write_all(chunk)?;
            s.done += chunk.len() as u64;
            if s.done - s.last_sent >= PROGRESS_STEP {
                s.last_sent = s.done;
                let (done, total) = (s.done, s.total);
                proto::send(channel, Msg::DownloadProgress { done, total }, &[])?;
            }
        }
        Ok(())
    };

    let send_result = req.send_streaming(on_head, on_chunk);
    let s = st.into_inner();

    if let Some(err) = s.err {
        if !s.path.as_os_str().is_empty() {
            let _ = std::fs::remove_file(&s.path);
        }
        return Err(err);
    }
    // A transport error after the head (e.g. a dropped connection) leaves a partial
    // file; treat it as a failure and clean up.
    if let Err(e) = send_result {
        if !s.path.as_os_str().is_empty() {
            let _ = std::fs::remove_file(&s.path);
        }
        return Err(e.to_string());
    }
    if s.file.is_none() {
        return Err("no response".to_string());
    }
    // Final progress so the caller sees 100% (total defaults to the byte count when the
    // server sent no Content-Length).
    let _ = proto::send(
        channel,
        Msg::DownloadProgress {
            done: s.done,
            total: s.total.max(s.done),
        },
        &[],
    );
    Ok(s.path)
}

/// Choose a download filename from a `Content-Disposition` header (`filename*=UTF-8''…`
/// preferred, else `filename="…"`), else the URL's last path segment (percent-decoded),
/// else `"download"`. The result is a bare filename: any path separators are stripped.
fn download_filename(disposition: Option<&str>, url: &str) -> String {
    if let Some(cd) = disposition {
        if let Some(name) = content_disposition_filename(cd) {
            let name = sanitize_filename(&name);
            if !name.is_empty() {
                return name;
            }
        }
    }
    // URL basename: drop query/fragment, strip the scheme+authority (so a host-only
    // URL like `https://h/` has no basename), then take the last path segment.
    let no_qf = url.split(['?', '#']).next().unwrap_or(url);
    let path = match no_qf.split_once("://") {
        Some((_, after)) => match after.find('/') {
            Some(i) => &after[i + 1..],
            None => "",
        },
        None => no_qf,
    };
    let seg = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("");
    let name = sanitize_filename(&percent_decode(seg));
    if name.is_empty() {
        "download".to_string()
    } else {
        name
    }
}

/// Extract the `filename` from a `Content-Disposition` value. Prefers the RFC 5987
/// `filename*=charset'lang'pct-encoded` form, else the quoted/bare `filename=`.
fn content_disposition_filename(cd: &str) -> Option<String> {
    for part in cd.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("filename*=") {
            // charset'lang'value — keep the value after the second quote.
            let value = v.splitn(3, '\'').nth(2).unwrap_or(v);
            return Some(percent_decode(value));
        }
    }
    for part in cd.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("filename=") {
            return Some(v.trim().trim_matches('"').to_string());
        }
    }
    None
}

/// Strip directory separators and trim a candidate filename to a safe bare name.
fn sanitize_filename(name: &str) -> String {
    name.rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim()
        .trim_matches('.')
        .to_string()
}

/// Minimal percent-decoding (`%XX` → byte), lossy UTF-8. Leaves malformed escapes as-is.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Resolve `dir/name`, avoiding collisions: if it exists, try `name (1).ext`,
/// `name (2).ext`, … `exists` is injected so this is unit-testable without the disk.
fn dedupe_path(dir: &std::path::Path, name: &str, exists: impl Fn(&std::path::Path) -> bool) -> std::path::PathBuf {
    let candidate = dir.join(name);
    if !exists(&candidate) {
        return candidate;
    }
    // Split into stem + extension (only a real, short trailing extension).
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() && !e.is_empty() && e.len() <= 16 => (s, Some(e)),
        _ => (name, None),
    };
    for n in 1..10_000 {
        let candidate = match ext {
            Some(e) => dir.join(format!("{stem} ({n}).{e}")),
            None => dir.join(format!("{stem} ({n})")),
        };
        if !exists(&candidate) {
            return candidate;
        }
    }
    dir.join(name)
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
    /// The response's `Content-Security-Policy` header value(s), preserved so a
    /// cache hit enforces the same policy as the original fetch (dropping it would
    /// silently weaken security on repeat visits).
    csp: Vec<String>,
}

/// The outcome of consulting the cache for a URL.
enum CacheLookup {
    /// A fresh entry: serve its body (and stored CSP) without a network request.
    Fresh { body: Vec<u8>, csp: Vec<String> },
    /// An expired entry that carries validators: revalidate with a conditional
    /// request, serving the stored body (and CSP) if the origin answers `304`.
    Stale {
        validators: Validators,
        body: Vec<u8>,
        csp: Vec<String>,
    },
    /// No usable entry: fetch unconditionally.
    Miss,
}

impl HttpCache {
    /// Consult the cache: fresh hit, stale-but-revalidatable, or miss. An expired
    /// entry without validators is evicted (it can't be revalidated).
    fn lookup(&mut self, url: &str) -> CacheLookup {
        match self.entries.get(url) {
            Some(e) if e.expiry > std::time::Instant::now() => CacheLookup::Fresh {
                body: e.body.clone(),
                csp: e.csp.clone(),
            },
            Some(e) if !e.validators.is_empty() => CacheLookup::Stale {
                validators: e.validators.clone(),
                body: e.body.clone(),
                csp: e.csp.clone(),
            },
            Some(_) => {
                self.entries.remove(url);
                CacheLookup::Miss
            }
            None => CacheLookup::Miss,
        }
    }

    fn put(
        &mut self,
        url: String,
        body: Vec<u8>,
        ttl: std::time::Duration,
        validators: Validators,
        csp: Vec<String>,
    ) {
        let expiry = std::time::Instant::now() + ttl;
        self.entries.insert(
            url,
            CacheEntry {
                body,
                expiry,
                validators,
                csp,
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

/// The (trimmed) value of the first header named `name` (case-insensitive), or empty.
fn extract_header(headers: &[(String, String)], name: &str) -> String {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.trim().to_string())
        .unwrap_or_default()
}

/// Every `Content-Security-Policy` response-header value (a response may send more
/// than one; each is an independent policy that must all be satisfied). Empty (not
/// `Content-Security-Policy-Report-Only`, which only reports) values are dropped.
fn extract_csp(headers: &[(String, String)]) -> Vec<String> {
    headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("content-security-policy"))
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .collect()
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
        matches!(l, CacheLookup::Fresh { .. })
    }

    #[test]
    fn cache_store_and_expiry() {
        let mut c = HttpCache::default();
        c.put("u".into(), b"body".to_vec(), Duration::from_secs(60), no_validators(), vec![]);
        assert!(matches!(c.lookup("u"), CacheLookup::Fresh { body, .. } if body == b"body"));
        // An expired entry without validators is not served (and is evicted).
        c.put("v".into(), b"x".to_vec(), Duration::from_secs(0), no_validators(), vec![]);
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
        c.put("u".into(), b"cached".to_vec(), Duration::from_secs(0), v, vec![]);
        // Stale but revalidatable: returned as Stale (not evicted), with its validators.
        match c.lookup("u") {
            CacheLookup::Stale { validators, body, .. } => {
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
    fn extract_csp_collects_all_policy_headers() {
        // Case-insensitive header name; multiple policies; blanks dropped; other
        // headers ignored.
        let headers = h(&[
            ("Content-Security-Policy", "default-src 'self'"),
            ("X-Frame-Options", "DENY"),
            ("content-security-policy", "script-src 'none'"),
            ("Content-Security-Policy", "  "),
        ]);
        assert_eq!(
            extract_csp(&headers),
            vec!["default-src 'self'".to_string(), "script-src 'none'".to_string()]
        );
        assert!(extract_csp(&h(&[("Content-Type", "text/html")])).is_empty());
    }

    #[test]
    fn cache_preserves_csp_on_hit() {
        let mut c = HttpCache::default();
        c.put(
            "u".into(),
            b"body".to_vec(),
            Duration::from_secs(60),
            no_validators(),
            vec!["default-src 'self'".to_string()],
        );
        match c.lookup("u") {
            CacheLookup::Fresh { csp, .. } => {
                assert_eq!(csp, vec!["default-src 'self'".to_string()], "CSP survives a cache hit");
            }
            _ => panic!("expected Fresh"),
        }
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
    fn download_filename_resolution() {
        // Content-Disposition wins; quoted and RFC 5987 forms; path separators stripped.
        assert_eq!(
            download_filename(Some("attachment; filename=\"report.pdf\""), "https://h/x"),
            "report.pdf"
        );
        assert_eq!(
            download_filename(Some("attachment; filename*=UTF-8''na%C3%AFve%20file.txt"), "https://h/x"),
            "naïve file.txt"
        );
        assert_eq!(
            download_filename(Some("attachment; filename=\"../../etc/passwd\""), "https://h/x"),
            "passwd",
            "directory traversal stripped to a bare name"
        );
        // Falls back to the URL basename (percent-decoded), ignoring query/fragment.
        assert_eq!(download_filename(None, "https://h/a/b/file.zip?v=2#frag"), "file.zip");
        assert_eq!(download_filename(None, "https://h/My%20Doc.txt"), "My Doc.txt");
        // No usable name anywhere → a default.
        assert_eq!(download_filename(None, "https://h/"), "download");
        assert_eq!(download_filename(Some("inline"), "https://example.com"), "download");
    }

    #[test]
    fn dedupe_path_avoids_collisions() {
        use std::path::{Path, PathBuf};
        let dir = Path::new("/dl");
        // Nothing exists → the plain name.
        assert_eq!(dedupe_path(dir, "f.bin", |_| false), PathBuf::from("/dl/f.bin"));
        // The base exists → "f (1).bin"; the base and (1) exist → "f (2).bin".
        let taken1 = |p: &Path| p == Path::new("/dl/f.bin");
        assert_eq!(dedupe_path(dir, "f.bin", taken1), PathBuf::from("/dl/f (1).bin"));
        let taken2 = |p: &Path| p == Path::new("/dl/f.bin") || p == Path::new("/dl/f (1).bin");
        assert_eq!(dedupe_path(dir, "f.bin", taken2), PathBuf::from("/dl/f (2).bin"));
        // Extension-less names dedupe without a trailing dot.
        let taken_noext = |p: &Path| p == Path::new("/dl/README");
        assert_eq!(dedupe_path(dir, "README", taken_noext), PathBuf::from("/dl/README (1)"));
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
