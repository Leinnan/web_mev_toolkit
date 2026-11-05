//! Reloading a directory source when its files change.

use std::{
    path::Path,
    sync::mpsc::{RecvTimeoutError, channel},
    time::Duration,
};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::{TemplateSource, Templates};

/// An editor's save is several file events, so wait for them to settle.
const SETTLE: Duration = Duration::from_millis(150);

/// Keeps a watch running. Dropping it stops the watcher and its thread.
#[must_use = "dropping the watch stops reloading"]
#[derive(Debug)]
pub struct TemplateWatch {
    _watcher: RecommendedWatcher,
}

impl Templates {
    /// Reloads whenever a file under a directory source changes. A failed reload is
    /// logged, and the last good templates stay in use. An embedded source can't
    /// change, so this returns `None` for one, which lets the same startup code run
    /// everywhere.
    ///
    /// # Errors
    ///
    /// Returns the watcher's error when the directory can't be watched.
    pub fn watch(&self) -> notify::Result<Option<TemplateWatch>> {
        let TemplateSource::Directory(directory) = &self.source else {
            return Ok(None);
        };
        Ok(Some(self.watch_directory(directory)?))
    }

    fn watch_directory(&self, directory: &Path) -> notify::Result<TemplateWatch> {
        let (changes, events) = channel();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                // The receiver only goes away during shutdown.
                let _ = changes.send(event);
            })?;
        watcher.watch(directory, RecursiveMode::Recursive)?;
        let templates = self.clone();
        std::thread::spawn(move || {
            while let Ok(event) = events.recv() {
                let relevant = match event {
                    Ok(event) => !matches!(event.kind, EventKind::Access(_)),
                    Err(error) => {
                        tracing::warn!(%error, "template watcher error");
                        false
                    }
                };
                if !relevant {
                    continue;
                }
                loop {
                    match events.recv_timeout(SETTLE) {
                        Ok(_) => {}
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                match templates.reload() {
                    Ok(()) => tracing::info!("templates reloaded"),
                    Err(error) => tracing::error!(%error, "template reload failed"),
                }
            }
        });
        Ok(TemplateWatch { _watcher: watcher })
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use include_dir::{Dir, include_dir};
    use tera::Context;

    use crate::{TemplateSource, Templates};

    static FIXTURES: Dir = include_dir!("$CARGO_MANIFEST_DIR/tests/fixtures/templates");

    fn wait_for(templates: &Templates, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let page = templates.render("page.html", &Context::new()).unwrap();
            if page == expected {
                return;
            }
            assert!(Instant::now() < deadline, "still {page:?}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn edits_are_picked_up_and_broken_ones_ignored() {
        let dir = tempfile::tempdir().unwrap();
        // Canonical, because macOS reports temporary paths through a symlink.
        let root = dir.path().canonicalize().unwrap();
        let file = root.join("page.html");
        std::fs::write(&file, "one").unwrap();
        let templates =
            Templates::new(TemplateSource::Directory(root), &["page.html"], |_| {}).unwrap();
        let watch = templates.watch().unwrap().unwrap();

        std::fs::write(&file, "two").unwrap();
        wait_for(&templates, "two");

        std::fs::write(&file, "{% if %}").unwrap();
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(
            templates.render("page.html", &Context::new()).unwrap(),
            "two"
        );

        std::fs::write(&file, "three").unwrap();
        wait_for(&templates, "three");

        drop(watch);
        std::thread::sleep(Duration::from_millis(100));
        std::fs::write(&file, "four").unwrap();
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(
            templates.render("page.html", &Context::new()).unwrap(),
            "three",
            "dropping the watch stops reloading"
        );
    }

    #[test]
    fn embedded_sources_have_nothing_to_watch() {
        let templates = Templates::new(TemplateSource::Embedded(&FIXTURES), &[], |_| {}).unwrap();
        assert!(templates.watch().unwrap().is_none());
    }
}
