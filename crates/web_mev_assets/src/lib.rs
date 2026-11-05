//! Static files for web apps. Embedded files get digest URLs, so browsers can cache
//! them forever, and text files are compressed once, the first time they're needed.
//! A directory source reads from disk on every request, for development.
//!
//! ```ignore
//! static STATIC: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/static");
//!
//! let assets = Assets::embedded(&STATIC, "/static");
//! assets.url("app.css"); // "/static/app.css?v=3f2a9c1d04be"
//! ```
//!
//! The `axum` feature adds `Assets::respond`, for a handler such as:
//!
//! ```ignore
//! async fn asset(
//!     State(assets): State<Assets>,
//!     Path(path): Path<String>,
//!     uri: Uri,
//!     headers: HeaderMap,
//! ) -> Result<Response, AppError> {
//!     assets.respond(&path, uri.query(), &headers).await.ok_or(AppError::NotFound)
//! }
//! ```

#[cfg(feature = "axum")]
mod respond;

use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    sync::{Arc, OnceLock},
};

use include_dir::{Dir, DirEntry};

/// Hex characters of the digest used in URLs.
const VERSION_LEN: usize = 12;

/// Brotli and gzip encodings of a text file.
#[derive(Debug)]
pub struct Compressed {
    /// Brotli, at quality 11.
    pub br: Vec<u8>,
    /// Gzip, at best compression.
    pub gzip: Vec<u8>,
}

#[derive(Debug)]
struct Entry {
    bytes: &'static [u8],
    version: String,
    compressed: OnceLock<Compressed>,
}

#[derive(Debug)]
enum Source {
    Embedded(HashMap<String, Entry>),
    Directory(PathBuf),
}

/// A set of static files mounted at one URL path. Clones share the same files.
#[derive(Debug, Clone)]
pub struct Assets {
    source: Arc<Source>,
    mount: Arc<str>,
}

impl Assets {
    /// Serves the files of `directory`. `mount` is the URL path they live under,
    /// such as `/static`.
    #[must_use]
    pub fn embedded(directory: &'static Dir<'static>, mount: &str) -> Self {
        let mut entries = HashMap::new();
        collect(directory, &mut entries);
        Self {
            source: Arc::new(Source::Embedded(entries)),
            mount: mount.trim_end_matches('/').into(),
        }
    }

    /// Serves files from `directory` on disk, with no versions, no compression and
    /// no caching, so edits show up at once.
    #[must_use]
    pub fn directory(directory: impl Into<PathBuf>, mount: &str) -> Self {
        Self {
            source: Arc::new(Source::Directory(directory.into())),
            mount: mount.trim_end_matches('/').into(),
        }
    }

    /// The digest of an embedded file.
    #[must_use]
    pub fn version(&self, path: &str) -> Option<&str> {
        match &*self.source {
            Source::Embedded(entries) => entries.get(path).map(|entry| entry.version.as_str()),
            Source::Directory(_) => None,
        }
    }

    /// The URL of a file, versioned when it has a digest. An unknown name still gets
    /// a plain URL, so a typo shows up as a 404 rather than a failed page.
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        match self.version(path) {
            Some(version) => format!("{}/{path}?v={version}", self.mount),
            None => format!("{}/{path}", self.mount),
        }
    }

    /// The bytes of an embedded file.
    #[must_use]
    pub fn file(&self, path: &str) -> Option<&'static [u8]> {
        match &*self.source {
            Source::Embedded(entries) => entries.get(path).map(|entry| entry.bytes),
            Source::Directory(_) => None,
        }
    }

    /// The encodings of an embedded text file, computed on first use. `None` for
    /// binary or unknown files. Brotli at this quality takes a while on a large file,
    /// so call this off the async runtime.
    #[must_use]
    pub fn compressed(&self, path: &str) -> Option<&Compressed> {
        let Source::Embedded(entries) = &*self.source else {
            return None;
        };
        let entry = entries.get(path)?;
        is_compressible(path).then(|| entry.compressed.get_or_init(|| compress(entry.bytes)))
    }
}

fn collect(directory: &'static Dir<'static>, entries: &mut HashMap<String, Entry>) {
    for entry in directory.entries() {
        match entry {
            DirEntry::Dir(subdirectory) => collect(subdirectory, entries),
            DirEntry::File(file) => {
                if let Some(path) = file.path().to_str() {
                    let bytes = file.contents();
                    let mut version = blake3::hash(bytes).to_hex().to_string();
                    version.truncate(VERSION_LEN);
                    entries.insert(
                        path.to_owned(),
                        Entry {
                            bytes,
                            version,
                            compressed: OnceLock::new(),
                        },
                    );
                }
            }
        }
    }
}

fn extension(path: &str) -> Option<&str> {
    Path::new(path).extension().and_then(|ext| ext.to_str())
}

fn is_compressible(path: &str) -> bool {
    matches!(
        extension(path),
        Some("css" | "js" | "mjs" | "svg" | "webmanifest" | "json" | "map" | "txt" | "html")
    )
}

fn compress(bytes: &[u8]) -> Compressed {
    use std::io::Write as _;
    let mut br = Vec::new();
    {
        let mut writer = brotli::CompressorWriter::new(&mut br, 4096, 11, 22);
        // Writing into a Vec can't fail.
        let _ = writer.write_all(bytes);
    }
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    let _ = gzip.write_all(bytes);
    Compressed {
        br,
        gzip: gzip.finish().unwrap_or_default(),
    }
}

/// The `Content-Type` of a static file.
#[must_use]
pub fn content_type(path: &str) -> &'static str {
    match extension(path) {
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("html") => "text/html; charset=utf-8",
        Some("txt") => "text/plain; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("webmanifest") => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

/// Whether a request path stays inside the asset root.
#[must_use]
pub fn is_safe_path(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

/// A compressed encoding a client accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// `br`.
    Brotli,
    /// `gzip`.
    Gzip,
}

/// The best encoding an `Accept-Encoding` value allows. `q=0` means "not this one".
#[must_use]
pub fn accepted_encoding(accept_encoding: Option<&str>) -> Option<Encoding> {
    let accepted = accept_encoding.unwrap_or_default();
    let accepts = |wanted: &str| {
        accepted.split(',').any(|item| {
            let mut parts = item.split(';').map(str::trim);
            parts
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case(wanted))
                && !parts.any(|parameter| {
                    parameter
                        .strip_prefix("q=")
                        .and_then(|q| q.parse::<f32>().ok())
                        .is_some_and(|q| q <= 0.0)
                })
        })
    };
    if accepts("br") {
        Some(Encoding::Brotli)
    } else if accepts("gzip") {
        Some(Encoding::Gzip)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use include_dir::{Dir, include_dir};

    use super::{Assets, Encoding, accepted_encoding, content_type, is_safe_path};

    static FILES: Dir = include_dir!("$CARGO_MANIFEST_DIR/tests/fixtures/static");

    fn assets() -> Assets {
        Assets::embedded(&FILES, "/static/")
    }

    #[test]
    fn urls_carry_a_content_digest() {
        let assets = assets();
        let version = assets.version("app.css").unwrap();
        assert_eq!(version.len(), 12);
        assert_eq!(
            assets.url("app.css"),
            format!("/static/app.css?v={version}")
        );
        assert_ne!(version, assets.version("nested/app.js").unwrap());
        assert_eq!(assets.url("gone.css"), "/static/gone.css");
    }

    #[test]
    fn directory_sources_have_plain_urls() {
        let assets = Assets::directory("/somewhere", "/static");
        assert_eq!(assets.url("app.css"), "/static/app.css");
        assert!(assets.file("app.css").is_none());
    }

    #[test]
    fn text_files_compress_and_decompress_back() {
        let assets = assets();
        let original = assets.file("app.css").unwrap();
        let compressed = assets.compressed("app.css").unwrap();
        assert!(compressed.br.len() < original.len());
        assert!(compressed.gzip.len() < original.len());

        let mut gunzipped = Vec::new();
        flate2::read::GzDecoder::new(compressed.gzip.as_slice())
            .read_to_end(&mut gunzipped)
            .unwrap();
        assert_eq!(gunzipped, original);

        let mut unbrotlied = Vec::new();
        brotli::Decompressor::new(compressed.br.as_slice(), 4096)
            .read_to_end(&mut unbrotlied)
            .unwrap();
        assert_eq!(unbrotlied, original);
    }

    #[test]
    fn binary_and_unknown_files_are_not_compressed() {
        let assets = assets();
        assert!(assets.compressed("nested/logo.png").is_none());
        assert!(assets.compressed("gone.css").is_none());
    }

    #[test]
    fn traversal_is_rejected() {
        for bad in ["../secret", "a/../../b", "/etc/passwd", "./a", ""] {
            assert!(!is_safe_path(bad), "{bad:?}");
        }
        assert!(is_safe_path("nested/app.js"));
    }

    #[test]
    fn encodings_follow_the_header() {
        for (header, expected) in [
            (Some("gzip, br"), Some(Encoding::Brotli)),
            (Some("gzip"), Some(Encoding::Gzip)),
            (Some("BR;q=0.5"), Some(Encoding::Brotli)),
            (Some("br;q=0, gzip"), Some(Encoding::Gzip)),
            (Some("br;q=0, gzip;q=0"), None),
            (Some("identity"), None),
            (None, None),
        ] {
            assert_eq!(accepted_encoding(header), expected, "{header:?}");
        }
    }

    #[test]
    fn content_types_cover_web_files() {
        assert_eq!(content_type("a.css"), "text/css; charset=utf-8");
        assert_eq!(content_type("a.woff2"), "font/woff2");
        assert_eq!(content_type("a.unknown"), "application/octet-stream");
        assert_eq!(content_type("noext"), "application/octet-stream");
    }
}
