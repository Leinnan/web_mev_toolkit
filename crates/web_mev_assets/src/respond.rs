//! Answering requests from axum.

use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, header},
    response::Response,
};

use crate::{Assets, Encoding, Source, accepted_encoding, content_type, is_safe_path};

/// Cache policy for a URL whose digest matches the file: it can never change.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";
/// Cache policy for any other URL of an embedded file.
const SHORT: &str = "public, max-age=3600";

impl Assets {
    /// Answers a request for `path`, the part of the URL after the mount point.
    /// `query` is the URL's query string. Returns `None` for an unknown or unsafe
    /// path, so the app can answer with its own 404.
    ///
    /// Only a `v=` matching the file's digest earns the long cache lifetime. Any
    /// other value could otherwise pin old content in caches under a new URL.
    pub async fn respond(
        &self,
        path: &str,
        query: Option<&str>,
        headers: &HeaderMap,
    ) -> Option<Response> {
        if !is_safe_path(path) {
            return None;
        }
        let (body, encoding, cache_control) = match &*self.source {
            Source::Directory(directory) => {
                let bytes = tokio::fs::read(directory.join(path)).await.ok()?;
                (Body::from(bytes), None, "no-cache")
            }
            Source::Embedded(entries) => {
                let entry = entries.get(path)?;
                let wanted = format!("v={}", entry.version);
                let versioned = query.is_some_and(|query| query.split('&').any(|p| p == wanted));
                let accept = headers
                    .get(header::ACCEPT_ENCODING)
                    .and_then(|value| value.to_str().ok());
                let (body, encoding) = self.encoded(path, accept).await;
                (
                    body.unwrap_or_else(|| Body::from(entry.bytes)),
                    encoding,
                    if versioned { IMMUTABLE } else { SHORT },
                )
            }
        };
        let mut response = Response::new(body);
        let response_headers = response.headers_mut();
        response_headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(content_type(path)),
        );
        response_headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(cache_control),
        );
        if let Some(encoding) = encoding {
            response_headers.insert(
                header::CONTENT_ENCODING,
                HeaderValue::from_static(match encoding {
                    Encoding::Brotli => "br",
                    Encoding::Gzip => "gzip",
                }),
            );
        }
        if matches!(&*self.source, Source::Embedded(_)) && crate::is_compressible(path) {
            response_headers.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
        }
        Some(response)
    }

    /// The compressed body the client accepts, if any. Compressing happens off the
    /// async runtime, because brotli at this quality is slow on a large file.
    async fn encoded(&self, path: &str, accept: Option<&str>) -> (Option<Body>, Option<Encoding>) {
        let Some(encoding) = accepted_encoding(accept).filter(|_| crate::is_compressible(path))
        else {
            return (None, None);
        };
        let assets = self.clone();
        let owned_path = path.to_owned();
        let bytes = tokio::task::spawn_blocking(move || {
            assets
                .compressed(&owned_path)
                .map(|compressed| match encoding {
                    Encoding::Brotli => compressed.br.clone(),
                    Encoding::Gzip => compressed.gzip.clone(),
                })
        })
        .await
        .ok()
        .flatten();
        match bytes {
            Some(bytes) => (Some(Body::from(bytes)), Some(encoding)),
            None => (None, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::{
        body::to_bytes,
        http::{HeaderMap, HeaderValue, header},
        response::Response,
    };
    use include_dir::{Dir, include_dir};

    use crate::Assets;

    static FILES: Dir = include_dir!("$CARGO_MANIFEST_DIR/tests/fixtures/static");

    fn accepting(value: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT_ENCODING, HeaderValue::from_static(value));
        headers
    }

    fn header_of(response: &Response, name: header::HeaderName) -> Option<&str> {
        response.headers().get(name).map(|v| v.to_str().unwrap())
    }

    #[tokio::test]
    async fn versioned_urls_are_immutable_and_others_are_not() {
        let assets = Assets::embedded(&FILES, "/static");
        let query = format!("v={}", assets.version("app.css").unwrap());
        let response = assets
            .respond("app.css", Some(&query), &HeaderMap::new())
            .await
            .unwrap();
        assert_eq!(
            header_of(&response, header::CACHE_CONTROL),
            Some("public, max-age=31536000, immutable")
        );
        assert_eq!(
            header_of(&response, header::CONTENT_TYPE),
            Some("text/css; charset=utf-8")
        );

        for query in [None, Some("v=stale00000000")] {
            let response = assets
                .respond("app.css", query, &HeaderMap::new())
                .await
                .unwrap();
            assert_eq!(
                header_of(&response, header::CACHE_CONTROL),
                Some("public, max-age=3600"),
                "{query:?}"
            );
        }
    }

    #[tokio::test]
    async fn compressed_bodies_match_what_was_accepted() {
        let assets = Assets::embedded(&FILES, "/static");
        let compressed = assets.compressed("app.css").unwrap();
        for (accept, encoding, expected) in [
            ("br, gzip", Some("br"), compressed.br.as_slice()),
            ("gzip", Some("gzip"), compressed.gzip.as_slice()),
            ("identity", None, assets.file("app.css").unwrap()),
        ] {
            let response = assets
                .respond("app.css", None, &accepting(accept))
                .await
                .unwrap();
            assert_eq!(header_of(&response, header::CONTENT_ENCODING), encoding);
            assert_eq!(header_of(&response, header::VARY), Some("accept-encoding"));
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            assert_eq!(&body[..], expected, "{accept}");
        }
    }

    #[tokio::test]
    async fn binary_files_are_sent_as_they_are() {
        let assets = Assets::embedded(&FILES, "/static");
        let response = assets
            .respond("nested/logo.png", None, &accepting("br"))
            .await
            .unwrap();
        assert_eq!(header_of(&response, header::CONTENT_ENCODING), None);
        assert_eq!(header_of(&response, header::VARY), None);
        assert_eq!(
            header_of(&response, header::CONTENT_TYPE),
            Some("image/png")
        );
    }

    #[tokio::test]
    async fn unknown_and_unsafe_paths_are_none() {
        let assets = Assets::embedded(&FILES, "/static");
        assert!(
            assets
                .respond("gone.css", None, &HeaderMap::new())
                .await
                .is_none()
        );
        assert!(
            assets
                .respond("../Cargo.toml", None, &HeaderMap::new())
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn directory_source_reads_fresh_files_and_stays_inside() {
        let root = tempfile::tempdir().unwrap();
        let public = root.path().join("public");
        std::fs::create_dir(&public).unwrap();
        std::fs::write(root.path().join("secret.txt"), b"secret").unwrap();
        std::fs::write(public.join("a.css"), b"one").unwrap();
        let assets = Assets::directory(&public, "/static");

        let response = assets
            .respond("a.css", None, &HeaderMap::new())
            .await
            .unwrap();
        assert_eq!(
            header_of(&response, header::CACHE_CONTROL),
            Some("no-cache")
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"one");

        std::fs::write(public.join("a.css"), b"two").unwrap();
        let response = assets
            .respond("a.css", None, &HeaderMap::new())
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"two");

        assert!(
            assets
                .respond("../secret.txt", None, &HeaderMap::new())
                .await
                .is_none()
        );
    }
}
