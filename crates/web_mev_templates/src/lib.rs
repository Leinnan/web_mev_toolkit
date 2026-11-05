//! Tera 2 setup shared by web apps. It has no web framework in it, so any app can
//! use it.
//!
//! The app says where templates come from and which ones it can't run without,
//! and registers its filters before they load, because Tera rejects a template
//! that names an unknown filter:
//!
//! ```ignore
//! static TEMPLATES: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/templates");
//!
//! let templates = Templates::new(TemplateSource::Embedded(&TEMPLATES), &["base.html"], |tera| {
//!     register_filters(tera, &FilterOptions::default());
//!     register_static_url(tera, move |name| assets.url(name));
//! })?;
//! let page = templates.render("base.html", &context)?;
//! ```
//!
//! With the `reload` feature, [`Templates::watch`] picks up edits to a directory
//! source while the app runs.

pub mod filters;
#[cfg(feature = "reload")]
pub mod reload;

use std::{
    path::PathBuf,
    sync::{Arc, PoisonError, RwLock},
};

pub use filters::{FilterOptions, register_filters, register_static_url, relative_age};
use include_dir::{Dir, DirEntry};
#[cfg(feature = "reload")]
pub use reload::TemplateWatch;
use tera::{Context, Tera};

/// Why templates couldn't be loaded.
#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    /// A template didn't parse, or names something unknown.
    #[error(transparent)]
    Tera(#[from] tera::Error),
    /// Templates the app can't run without aren't there.
    #[error("required templates are missing: {}", .0.join(", "))]
    Missing(Vec<String>),
}

/// Where templates come from.
#[derive(Debug, Clone)]
pub enum TemplateSource {
    /// Inside the binary, so a deployment only needs the executable.
    Embedded(&'static Dir<'static>),
    /// `*.html` files under a directory, for development and theming.
    Directory(PathBuf),
}

/// Every `.html` file in `directory` as `(name, source)` pairs, named relative to
/// its root, such as `fragments/flash.html`.
#[must_use]
pub fn html_templates(directory: &'static Dir<'static>) -> Vec<(&'static str, &'static str)> {
    fn collect(directory: &'static Dir<'static>, found: &mut Vec<(&'static str, &'static str)>) {
        for entry in directory.entries() {
            match entry {
                DirEntry::Dir(subdirectory) => collect(subdirectory, found),
                DirEntry::File(file) => {
                    if file.path().extension().is_some_and(|ext| ext == "html")
                        && let (Some(name), Some(source)) =
                            (file.path().to_str(), file.contents_utf8())
                    {
                        found.push((name, source));
                    }
                }
            }
        }
    }
    let mut found = Vec::new();
    collect(directory, &mut found);
    found
}

type Setup = dyn Fn(&mut Tera) + Send + Sync;

/// A loaded template set that can be replaced while the app runs. Clones share the
/// same set. A replacement only takes effect once it has fully loaded, so a broken
/// edit leaves the last good templates in place.
#[derive(Clone)]
pub struct Templates {
    current: Arc<RwLock<Arc<Tera>>>,
    source: TemplateSource,
    required: Arc<[String]>,
    setup: Arc<Setup>,
}

impl std::fmt::Debug for Templates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Templates")
            .field("source", &self.source)
            .field("required", &self.required)
            .finish_non_exhaustive()
    }
}

impl Templates {
    /// Loads templates from `source`. `setup` registers filters and functions, and
    /// runs again for every reload. `required` lists names that must exist, so a
    /// missing template stops startup rather than a page.
    ///
    /// # Errors
    ///
    /// Returns [`TemplateError`] when a template doesn't parse or a required one is
    /// missing.
    pub fn new(
        source: TemplateSource,
        required: &[&str],
        setup: impl Fn(&mut Tera) + Send + Sync + 'static,
    ) -> Result<Self, TemplateError> {
        let setup: Arc<Setup> = Arc::new(setup);
        let required: Arc<[String]> = required.iter().map(|name| (*name).to_owned()).collect();
        let tera = load(&source, &required, &setup)?;
        Ok(Self {
            current: Arc::new(RwLock::new(Arc::new(tera))),
            source,
            required,
            setup,
        })
    }

    /// The current template set.
    #[must_use]
    pub fn current(&self) -> Arc<Tera> {
        self.current
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Renders a template from the current set.
    ///
    /// # Errors
    ///
    /// Returns the Tera error when rendering fails.
    pub fn render(&self, name: &str, context: &Context) -> Result<String, tera::Error> {
        self.current().render(name, context)
    }

    /// Loads the templates again and swaps them in. On failure the current set stays
    /// in place. Requests in flight finish on the set they started with.
    ///
    /// # Errors
    ///
    /// Returns [`TemplateError`] when the new set can't be used.
    pub fn reload(&self) -> Result<(), TemplateError> {
        let tera = load(&self.source, &self.required, &self.setup)?;
        *self.current.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(tera);
        Ok(())
    }
}

fn load(
    source: &TemplateSource,
    required: &[String],
    setup: &Arc<Setup>,
) -> Result<Tera, TemplateError> {
    let mut tera = Tera::new();
    setup(&mut tera);
    match source {
        TemplateSource::Embedded(directory) => tera.add_raw_templates(html_templates(directory))?,
        TemplateSource::Directory(directory) => {
            tera.load_from_glob(&format!("{}/**/*.html", directory.display()))?;
        }
    }
    let missing: Vec<String> = required
        .iter()
        .filter(|name| !tera.contains_template(name))
        .cloned()
        .collect();
    if missing.is_empty() {
        Ok(tera)
    } else {
        Err(TemplateError::Missing(missing))
    }
}

#[cfg(test)]
mod tests {
    use include_dir::{Dir, include_dir};
    use tera::Context;

    use super::{TemplateError, TemplateSource, Templates, html_templates};

    static FIXTURES: Dir = include_dir!("$CARGO_MANIFEST_DIR/tests/fixtures/templates");

    fn title(value: &str) -> Context {
        let mut context = Context::new();
        context.insert("title", value);
        context
    }

    #[test]
    fn embedded_templates_are_named_from_their_root() {
        let mut names: Vec<&str> = html_templates(&FIXTURES)
            .iter()
            .map(|(name, _)| *name)
            .collect();
        names.sort_unstable();
        assert_eq!(names, ["base.html", "fragments/flash.html"]);
    }

    #[test]
    fn embedded_source_renders_and_escapes() {
        let templates =
            Templates::new(TemplateSource::Embedded(&FIXTURES), &["base.html"], |_| {}).unwrap();
        assert_eq!(
            templates.render("base.html", &title("<b>")).unwrap().trim(),
            "<h1>&lt;b&gt;</h1>"
        );
    }

    #[test]
    fn missing_required_templates_stop_startup() {
        let error = Templates::new(
            TemplateSource::Embedded(&FIXTURES),
            &["base.html", "gone.html"],
            |_| {},
        )
        .unwrap_err();
        assert!(
            matches!(&error, TemplateError::Missing(names) if names == &["gone.html"]),
            "{error}"
        );
    }

    #[test]
    fn a_failed_reload_keeps_the_last_good_templates() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("page.html");
        std::fs::write(&file, "one {{ title }}").unwrap();
        let templates = Templates::new(
            TemplateSource::Directory(dir.path().to_owned()),
            &["page.html"],
            |_| {},
        )
        .unwrap();
        assert_eq!(templates.render("page.html", &title("x")).unwrap(), "one x");

        std::fs::write(&file, "two {{ title }}").unwrap();
        assert_eq!(
            templates.render("page.html", &title("x")).unwrap(),
            "one x",
            "nothing changes until a reload"
        );
        templates.reload().unwrap();
        assert_eq!(templates.render("page.html", &title("x")).unwrap(), "two x");

        std::fs::write(&file, "{% if %}").unwrap();
        assert!(templates.reload().is_err());
        assert_eq!(templates.render("page.html", &title("x")).unwrap(), "two x");
    }

    #[test]
    fn a_reload_runs_setup_again_for_filters() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("page.html"), "{{ title | shout }}").unwrap();
        let templates = Templates::new(
            TemplateSource::Directory(dir.path().to_owned()),
            &[],
            |tera| {
                tera.register_filter("shout", |value: &str, _: tera::Kwargs, _: &tera::State| {
                    value.to_uppercase()
                });
            },
        )
        .unwrap();
        templates.reload().unwrap();
        assert_eq!(templates.render("page.html", &title("hi")).unwrap(), "HI");
    }
}
