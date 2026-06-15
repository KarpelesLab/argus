//! On-disk location for persisted `localStorage`, so it survives browser restarts
//! (not just navigations within a session). The sandboxed content process can't
//! touch the filesystem, so the trusted browser process owns the file: it reads it
//! at startup to seed each content process (`ProvideStorage`), and rewrites it
//! whenever a content process reports a change (`StorageChanged`). The on-disk text
//! is the content process's own serialization (`argus_protocol::encode_storage`),
//! written verbatim, so no parsing happens here.

use std::path::PathBuf;

/// Where the store lives: `$ARGUS_STORAGE` if set, else a stable per-user file.
pub(crate) fn path() -> PathBuf {
    if let Ok(p) = std::env::var("ARGUS_STORAGE") {
        return PathBuf::from(p);
    }
    let base = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(base).join(".argus_localstorage")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_honors_the_env_override() {
        // Note: env is process-global; this test sets and clears its own key.
        std::env::set_var("ARGUS_STORAGE", "/tmp/argus_store_override");
        assert_eq!(path(), PathBuf::from("/tmp/argus_store_override"));
        std::env::remove_var("ARGUS_STORAGE");
        // Falls back to a file under HOME (or /tmp) — just check it ends right.
        assert!(path().ends_with(".argus_localstorage"));
    }
}
