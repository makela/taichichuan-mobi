use std::collections::HashMap;
use std::hash::{DefaultHasher, Hasher};
use std::path::Path;
use std::sync::OnceLock;

/// Static files whose URLs carry a content hash, so browsers fetch them
/// again whenever they change.
const VERSIONED_FILES: &[&str] = &["styles.css", "img/profile.jpg", "img/profile-432.jpg"];

static VERSIONS: OnceLock<HashMap<&'static str, String>> = OnceLock::new();

fn content_hash(bytes: &[u8]) -> String {
    let mut hasher = DefaultHasher::new();
    hasher.write(bytes);
    format!("{:016x}", hasher.finish())
}

/// Hashes the versioned files in `static_dir`. Only the first call has an effect.
/// A missing file is skipped, and its URL is then left unversioned.
pub fn init(static_dir: &Path) {
    VERSIONS.get_or_init(|| {
        VERSIONED_FILES
            .iter()
            .filter_map(|&name| {
                let bytes = std::fs::read(static_dir.join(name)).ok()?;
                Some((name, content_hash(&bytes)))
            })
            .collect()
    });
}

/// Whether `version` is the current content hash of the static file `name`.
#[must_use]
pub fn is_current(name: &str, version: &str) -> bool {
    VERSIONS
        .get()
        .and_then(|versions| versions.get(name))
        .is_some_and(|current| current == version)
}

/// URL of a static file, e.g. `/static/styles.css?v=1a2b3c4d5e6f7a8b`.
#[must_use]
pub fn url(name: &str) -> String {
    match VERSIONS.get().and_then(|versions| versions.get(name)) {
        Some(version) => format!("/static/{name}?v={version}"),
        None => format!("/static/{name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_changes_with_content() {
        assert_eq!(content_hash(b"a"), content_hash(b"a"));
        assert_ne!(content_hash(b"a"), content_hash(b"b"));
    }

    #[test]
    fn versioned_url_carries_current_hash() {
        init(Path::new("dist"));
        let url = url("styles.css");
        let version = url
            .strip_prefix("/static/styles.css?v=")
            .unwrap_or_default();
        assert_eq!(version.len(), 16, "{url}");
        assert!(is_current("styles.css", version));
        assert!(!is_current("styles.css", "0000000000000000"));
        assert!(!is_current("favicon.ico", version));
    }

    #[test]
    fn unversioned_file_keeps_plain_url() {
        init(Path::new("dist"));
        assert_eq!(
            url("fonts/nunito-latin.woff2"),
            "/static/fonts/nunito-latin.woff2"
        );
    }
}
