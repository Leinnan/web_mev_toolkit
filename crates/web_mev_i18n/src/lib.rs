//! CSV translation tables with plural rules, and locale negotiation for web
//! requests. It uses no web framework, so any app can use it.
//!
//! # CSV format
//!
//! The first column holds the translation key, and every following header names a
//! locale. Columns headed `Context`, `Comment` or `Notes`, or with an empty header,
//! are documentation and are ignored. Any other header that isn't a locale is an
//! error, so a typo can't silently drop a language. Rows with an empty key are
//! ignored. Cell values are trimmed, and an empty cell means "no translation here".
//!
//! ```csv
//! key,Context,en,pl
//! nav_gallery,Main menu,Gallery,Galeria
//! photos_one,,{n} photo,{n} zdjęcie
//! photos_other,,{n} photos,
//! photos_few,,,{n} zdjęcia
//! photos_many,,,{n} zdjęć
//! ```
//!
//! A table is parsed once at startup, and pages look values up with a [`Lang`]:
//!
//! ```ignore
//! let table = TranslationsAsset::from_csv_str(include_str!("../locales/i18n.csv"))?;
//! let locale = negotiate(cookie, accept_language, &supported, &default);
//! let lang = Lang { catalog: &table, locale };
//! lang.t("nav_gallery");
//! lang.t_n("photos", count.unsigned_abs(), &[]);
//! ```
//!
//! Tables can be merged on top of one another, with later tables adding keys and
//! locales and overriding values. See [`TranslationsAsset::merge_csv_str`].

pub mod localize;
pub mod negotiate;

pub use localize::{Lang, Localize};
pub use negotiate::{negotiate, safe_return_path};

use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};
use std::{
    collections::{HashMap, HashSet},
    fmt,
};

/// Controls what happens to an existing translation when a merged table leaves
/// the matching value cell empty.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EmptyValue {
    /// Keep the value loaded earlier; empty cells never override.
    #[default]
    Skip,
    /// Clear the value loaded earlier, removing the translation.
    Clear,
}

/// Return `true` when the segment is exactly two ASCII alphabetic characters.
fn is_two_alpha(value: &str) -> bool {
    value.len() == 2 && value.bytes().all(|byte| byte.is_ascii_alphabetic())
}

/// A helper struct for storing the two segments commonly used to denote a locale and region.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LocaleCode {
    lang: String,
    region: Option<String>,
}

impl Default for LocaleCode {
    fn default() -> Self {
        Self {
            lang: "en".to_owned(),
            region: None,
        }
    }
}

impl LocaleCode {
    /// The delimiter expected and produced when combining language and region codes in a `LocaleCode`
    pub const REGION_DELIMITER: &'static str = "-";

    /// Return locale code without Region.
    #[must_use]
    pub fn without_region(&self) -> LocaleCode {
        LocaleCode {
            lang: self.lang.clone(),
            region: None,
        }
    }

    /// Return the lowercase language segment, for example `en`.
    #[must_use]
    pub fn lang(&self) -> &str {
        self.lang.as_str()
    }

    /// Return the lowercase region segment when one is present, for example `us`.
    #[must_use]
    pub fn region(&self) -> Option<&str> {
        self.region.as_deref()
    }

    /// Return the canonical `lang` or `lang-REGION` form of this code.
    ///
    /// The library stores both segments lowercased, so the produced region is
    /// lowercased as well.
    #[must_use]
    pub fn code(&self) -> String {
        match &self.region {
            Some(region) => format!("{}{}{}", self.lang.as_str(), Self::REGION_DELIMITER, region),
            None => self.lang.as_str().to_owned(),
        }
    }
}

impl fmt::Display for LocaleCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.code())
    }
}

impl TryFrom<&str> for LocaleCode {
    type Error = &'static str;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let code = value.trim().to_ascii_lowercase();
        if code.is_empty() {
            return Err("Invalid locale code");
        }
        let (lang, region) = match code.split_once(['-', '_']) {
            Some((lang, region)) => (lang, Some(region)),
            None => (code.as_str(), None),
        };
        if !is_two_alpha(lang) {
            return Err("Invalid locale language");
        }
        if let Some(region) = region
            && !is_two_alpha(region)
        {
            return Err("Invalid locale region");
        }
        Ok(LocaleCode {
            lang: lang.to_owned(),
            region: region.map(str::to_owned),
        })
    }
}

/// Whether a header names a column of notes for translators.
fn is_documentation_column(header: &str) -> bool {
    ["context", "comment", "notes"]
        .iter()
        .any(|name| header.eq_ignore_ascii_case(name))
}

/// Errors produced while parsing a translation CSV table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranslationError {
    /// The CSV reader rejected the input, for example because of malformed quoting.
    InvalidCsv(String),
    /// A header is neither a locale nor a documentation column.
    InvalidHeader(String),
    /// The header row did not contain any parseable locale columns.
    NoLocales,
    /// A locale column defined the same key more than once.
    DuplicateKey {
        /// The canonical code of the locale that repeated the key.
        locale: String,
        /// The duplicated key.
        key: String,
    },
}

impl fmt::Display for TranslationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCsv(error) => write!(formatter, "invalid translation CSV: {error}"),
            Self::InvalidHeader(header) => {
                write!(
                    formatter,
                    "translation CSV header `{header}` is not a locale"
                )
            }
            Self::NoLocales => formatter.write_str("translation CSV has no locale columns"),
            Self::DuplicateKey { locale, key } => {
                write!(
                    formatter,
                    "translation key `{key}` is duplicated for {locale}"
                )
            }
        }
    }
}

impl std::error::Error for TranslationError {}

/// Replace every `{name}` token in `template` with its matching argument.
///
/// Arguments that do not occur in the template are ignored, and tokens without
/// a matching argument are left untouched. The pass is a single scan, so a large
/// argument list costs no extra allocations.
#[must_use]
pub fn format_template(template: &str, args: &[(&str, &str)]) -> String {
    let mut output = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        output.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        if let Some(close) = after_open.find('}') {
            let name = &after_open[..close];
            if let Some((_, value)) = args.iter().find(|(key, _)| *key == name) {
                output.push_str(value);
                rest = &after_open[close + 1..];
                continue;
            }
        }
        output.push('{');
        rest = after_open;
    }
    output.push_str(rest);
    output
}

/// The CLDR plural category of the integer `n` in language `lang`: `one`,
/// `few`, `many` or `other`.
///
/// Covers English-like languages (`one` / `other`) and the Slavic three-form
/// rules (Polish, Russian, Ukrainian). Unknown
/// languages use the English rule.
#[must_use]
pub fn plural_category(lang: &str, n: u64) -> &'static str {
    let rem10 = n % 10;
    let rem100 = n % 100;
    let few = (2..=4).contains(&rem10) && !(12..=14).contains(&rem100);
    match lang {
        "pl" if n == 1 => "one",
        "ru" | "uk" if rem10 == 1 && rem100 != 11 => "one",
        "pl" | "ru" | "uk" if few => "few",
        "pl" | "ru" | "uk" => "many",
        _ if n == 1 => "one",
        _ => "other",
    }
}

/// Columnar translation catalog.
///
/// Every key is stored once in [`Catalog::keys`], and each locale is a dense
/// column of values indexed by the key's position. A `None` slot means the
/// locale has no translation for that key. This keeps the memory footprint
/// proportional to the number of values rather than to
/// `locales × keys`, and makes [`Catalog::merge`] a cheap column append.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog {
    keys: Vec<String>,
    key_index: HashMap<String, usize>,
    locales: HashMap<LocaleCode, Vec<Option<String>>>,
    locale_order: Vec<LocaleCode>,
}

impl Catalog {
    /// Create an empty catalog.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Iterate over every key in definition order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.keys.iter().map(String::as_str)
    }

    /// Iterate over every locale in first-seen order.
    pub fn locales(&self) -> impl Iterator<Item = &LocaleCode> {
        self.locale_order.iter()
    }

    /// The number of locales in the catalog.
    #[must_use]
    pub fn locale_count(&self) -> usize {
        self.locale_order.len()
    }

    /// The first locale inserted into the catalog, if any.
    #[must_use]
    pub fn first_locale(&self) -> Option<&LocaleCode> {
        self.locale_order.first()
    }

    /// Return `true` when `key` exists in the catalog.
    #[must_use]
    pub fn contains_key(&self, key: &str) -> bool {
        self.key_index.contains_key(key)
    }

    /// Resolve the value column for `locale`, falling back from `lang-REGION`
    /// to `lang` when only the language was loaded.
    fn resolve_values(&self, locale: &LocaleCode) -> Option<&[Option<String>]> {
        if let Some(values) = self.locales.get(locale) {
            return Some(values.as_slice());
        }
        if locale.region().is_some() {
            let base = locale.without_region();
            return self.locales.get(&base).map(Vec::as_slice);
        }
        None
    }

    /// Borrow a serializable view of `locale`, or `None` when the locale (and
    /// its region-less parent) is unknown.
    #[must_use]
    pub fn locale_view(&self, locale: &LocaleCode) -> Option<LocaleCatalog<'_>> {
        let values = self.resolve_values(locale)?;
        Some(LocaleCatalog {
            catalog: self,
            values,
        })
    }

    /// Look up a single translation without allocating.
    #[must_use]
    pub fn tr(&self, key: &str, locale: &LocaleCode) -> Option<&str> {
        let index = *self.key_index.get(key)?;
        self.resolve_values(locale)?.get(index)?.as_deref()
    }

    /// Look up a translation, falling back to the key itself when missing.
    #[must_use]
    pub fn tr_or_key<'a>(&'a self, key: &'a str, locale: &LocaleCode) -> &'a str {
        self.tr(key, locale).unwrap_or(key)
    }

    /// Look up a translation and substitute `{name}` placeholders.
    #[must_use]
    pub fn tr_format(
        &self,
        key: &str,
        locale: &LocaleCode,
        args: &[(&str, &str)],
    ) -> Option<String> {
        self.tr(key, locale)
            .map(|template| format_template(template, args))
    }

    /// Look up the plural form of `key` for `n` and substitute `{n}` plus `args`.
    ///
    /// The form is stored under `{key}_{category}` (see [`plural_category`]);
    /// a missing category falls back to `{key}_other`, then `{key}_many`.
    /// Like [`Self::tr_or_default_locale`], an unknown `locale` uses the
    /// catalog's first locale (and its plural rule).
    #[must_use]
    pub fn tr_plural(
        &self,
        key: &str,
        locale: &LocaleCode,
        n: u64,
        args: &[(&str, &str)],
    ) -> Option<String> {
        let lang = if self.resolve_values(locale).is_some() {
            locale.lang()
        } else {
            self.locale_order.first()?.lang()
        };
        let category = plural_category(lang, n);
        let template = [category, "other", "many"]
            .iter()
            .find_map(|category| self.tr_or_default_locale(&format!("{key}_{category}"), locale))?;
        let n = n.to_string();
        let mut all = Vec::with_capacity(args.len() + 1);
        all.push(("n", n.as_str()));
        all.extend_from_slice(args);
        Some(format_template(template, &all))
    }

    /// Look up a translation for `locale`, falling back to the catalog's first
    /// locale only when `locale` itself is unknown.
    ///
    /// A missing *key* inside a known locale still returns `None`.
    #[must_use]
    pub fn tr_or_default_locale<'a>(&'a self, key: &str, locale: &LocaleCode) -> Option<&'a str> {
        let values = self.resolve_values(locale).or_else(|| {
            self.locale_order
                .first()
                .and_then(|locale| self.locales.get(locale))
                .map(Vec::as_slice)
        })?;
        let index = *self.key_index.get(key)?;
        values.get(index)?.as_deref()
    }

    /// Merge `other` on top of this catalog.
    ///
    /// Keys and locales that only exist in `other` are added; values that
    /// already exist are overridden. An empty value cell follows `on_empty`.
    /// Merging is deterministic: later calls override earlier ones.
    pub fn merge(&mut self, other: &Catalog, on_empty: EmptyValue) {
        let mut remap = Vec::with_capacity(other.keys.len());
        for key in &other.keys {
            if let Some(index) = self.key_index.get(key) {
                remap.push(*index);
            } else {
                let index = self.keys.len();
                self.keys.push(key.clone());
                self.key_index.insert(key.clone(), index);
                remap.push(index);
            }
        }

        let total = self.keys.len();
        for values in self.locales.values_mut() {
            values.resize(total, None);
        }

        for locale in &other.locale_order {
            let Some(other_values) = other.locales.get(locale) else {
                continue;
            };
            if !self.locales.contains_key(locale) {
                self.locale_order.push(locale.clone());
                self.locales.insert(locale.clone(), vec![None; total]);
            }
            let Some(target) = self.locales.get_mut(locale) else {
                continue;
            };
            for (other_index, value) in other_values.iter().enumerate() {
                let Some(target_index) = remap.get(other_index).copied() else {
                    continue;
                };
                match value {
                    Some(value) => target[target_index] = Some(value.clone()),
                    None if on_empty == EmptyValue::Clear => target[target_index] = None,
                    None => {}
                }
            }
        }
    }

    /// Append `key` if missing and return its column index.
    fn push_key(&mut self, key: &str) -> usize {
        if let Some(index) = self.key_index.get(key) {
            return *index;
        }
        let index = self.keys.len();
        self.keys.push(key.to_owned());
        self.key_index.insert(key.to_owned(), index);
        for values in self.locales.values_mut() {
            values.push(None);
        }
        index
    }

    /// Create an empty column for `locale` when it is not present yet.
    fn ensure_locale(&mut self, locale: LocaleCode) {
        if self.locales.contains_key(&locale) {
            return;
        }
        let len = self.keys.len();
        self.locale_order.push(locale.clone());
        self.locales.insert(locale, vec![None; len]);
    }

    /// Store `value` for `locale` at `index`, ignoring unknown positions.
    fn set_value(&mut self, locale: &LocaleCode, index: usize, value: Option<String>) {
        if let Some(values) = self.locales.get_mut(locale)
            && let Some(slot) = values.get_mut(index)
        {
            *slot = value;
        }
    }
}

/// A borrowed, [`Serialize`] view of a single resolved locale.
///
/// Serializing the view produces a JSON object containing only the keys that
/// actually have a translation, which is exactly what template engines such as
/// Tera expect.
pub struct LocaleCatalog<'a> {
    catalog: &'a Catalog,
    values: &'a [Option<String>],
}

impl<'a> LocaleCatalog<'a> {
    /// Number of translated keys in this locale.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.iter().filter(|value| value.is_some()).count()
    }

    /// Return `true` when this locale has no translations.
    pub fn is_empty(&self) -> bool {
        self.values.iter().all(Option::is_none)
    }

    /// Look up a single translation, borrowing from the catalog.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&'a str> {
        let index = *self.catalog.key_index.get(key)?;
        self.values.get(index)?.as_deref()
    }

    /// Iterate over `(key, value)` pairs that are translated in this locale.
    pub fn iter(&self) -> impl Iterator<Item = (&'a str, &'a str)> + 'a {
        let catalog = self.catalog;
        catalog
            .keys
            .iter()
            .map(String::as_str)
            .zip(self.values.iter())
            .filter_map(|(key, value)| value.as_deref().map(|value| (key, value)))
    }
}

impl Serialize for LocaleCatalog<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.len()))?;
        for (key, value) in self.iter() {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

/// A parsed translation table.
///
/// The table can be merged on top of (or under) other tables through
/// [`merge_from`](Self::merge_from), [`merge_csv_str`](Self::merge_csv_str) and
/// [`merge_csv_raw`](Self::merge_csv_raw). For convenience,
/// [`from_csv_str_layered`](Self::from_csv_str_layered) parses and merges a
/// whole sequence at once.
#[derive(Clone, PartialEq, Debug)]
pub struct TranslationsAsset {
    catalog: Catalog,
}

impl std::ops::Deref for TranslationsAsset {
    type Target = Catalog;
    fn deref(&self) -> &Self::Target {
        &self.catalog
    }
}

impl TranslationsAsset {
    fn from_catalog(catalog: Catalog) -> Self {
        Self { catalog }
    }

    /// Parse a raw CSV table in the format described in the crate docs.
    ///
    /// # Errors
    ///
    /// Returns a [`TranslationError`] when the CSV is malformed, has a header that
    /// is neither a locale nor a documentation column, has no locale columns, or
    /// defines the same key twice.
    pub fn from_csv_raw(csv_data: &[u8]) -> Result<Self, TranslationError> {
        let mut reader = csv::ReaderBuilder::new()
            .flexible(true)
            .has_headers(true)
            .trim(csv::Trim::All)
            .from_reader(csv_data);

        let head = reader
            .headers()
            .map_err(|error| TranslationError::InvalidCsv(error.to_string()))?
            .clone();
        let mut locales: Vec<(LocaleCode, usize)> = Vec::new();
        for (index, column) in head.iter().enumerate().skip(1) {
            if column.is_empty() || is_documentation_column(column) {
                continue;
            }
            let locale = LocaleCode::try_from(column)
                .map_err(|_| TranslationError::InvalidHeader(column.to_owned()))?;
            locales.push((locale, index));
        }
        if locales.is_empty() {
            return Err(TranslationError::NoLocales);
        }

        let mut catalog = Catalog::new();
        for (locale, _) in &locales {
            catalog.ensure_locale(locale.clone());
        }

        let mut seen = HashSet::new();
        for record in reader.records() {
            let record = record.map_err(|error| TranslationError::InvalidCsv(error.to_string()))?;
            let key = record.get(0).unwrap_or_default().trim();
            if key.is_empty() {
                continue;
            }
            if !seen.insert(key.to_owned()) {
                let locale = locales
                    .first()
                    .map_or_else(String::new, |(locale, _)| locale.code());
                return Err(TranslationError::DuplicateKey {
                    locale,
                    key: key.to_owned(),
                });
            }
            let index = catalog.push_key(key);
            for (locale, column) in &locales {
                let raw = record.get(*column).unwrap_or_default();
                let value = if raw.is_empty() {
                    None
                } else {
                    Some(raw.to_owned())
                };
                catalog.set_value(locale, index, value);
            }
        }
        Ok(Self::from_catalog(catalog))
    }

    /// Parse a UTF-8 CSV table. See [`Self::from_csv_raw`].
    ///
    /// # Errors
    ///
    /// Returns a [`TranslationError`] under the same conditions as
    /// [`Self::from_csv_raw`].
    pub fn from_csv_str(csv_data: &str) -> Result<Self, TranslationError> {
        Self::from_csv_raw(csv_data.as_bytes())
    }

    /// Parse a sequence of tables and merge them in order into one asset.
    ///
    /// Later tables add keys and locales and override earlier values according
    /// to `on_empty`. An empty sequence yields an empty catalog.
    ///
    /// # Errors
    ///
    /// Returns the first [`TranslationError`] encountered while parsing a table.
    pub fn from_csv_str_layered<'a, I>(
        sources: I,
        on_empty: EmptyValue,
    ) -> Result<Self, TranslationError>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let mut asset = Self::from_catalog(Catalog::new());
        for source in sources {
            asset.merge_csv_str(source, on_empty)?;
        }
        Ok(asset)
    }

    /// Merge `other` on top of this asset. See [`Catalog::merge`].
    pub fn merge_from(&mut self, other: &TranslationsAsset, on_empty: EmptyValue) {
        self.catalog.merge(&other.catalog, on_empty);
    }

    /// Parse a UTF-8 table and merge it on top of this asset.
    ///
    /// # Errors
    ///
    /// Returns a [`TranslationError`] when the table is malformed.
    pub fn merge_csv_str(
        &mut self,
        csv_data: &str,
        on_empty: EmptyValue,
    ) -> Result<(), TranslationError> {
        self.merge_csv_raw(csv_data.as_bytes(), on_empty)
    }

    /// Parse a raw table and merge it on top of this asset.
    ///
    /// # Errors
    ///
    /// Returns a [`TranslationError`] when the table is malformed.
    pub fn merge_csv_raw(
        &mut self,
        csv_data: &[u8],
        on_empty: EmptyValue,
    ) -> Result<(), TranslationError> {
        let other = Self::from_csv_raw(csv_data)?;
        self.catalog.merge(&other.catalog, on_empty);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Catalog, EmptyValue, LocaleCode, TranslationError, TranslationsAsset, format_template,
        plural_category,
    };

    #[test]
    fn plural_categories_follow_cldr() {
        for (n, category) in [(0, "other"), (1, "one"), (2, "other"), (11, "other")] {
            assert_eq!(plural_category("en", n), category, "en {n}");
        }
        let pl = [
            (0, "many"),
            (1, "one"),
            (2, "few"),
            (4, "few"),
            (5, "many"),
            (11, "many"),
            (12, "many"),
            (14, "many"),
            (21, "many"),
            (22, "few"),
            (24, "few"),
            (25, "many"),
            (102, "few"),
            (112, "many"),
        ];
        for (n, category) in pl {
            assert_eq!(plural_category("pl", n), category, "pl {n}");
        }
    }

    #[test]
    fn tr_plural_picks_the_form_and_fills_n() {
        let csv = "key,en,pl\nwins_one,{n} win,{n} wygrana\nwins_other,{n} wins,\nwins_few,,{n} wygrane\nwins_many,,{n} wygranych\n";
        let asset = TranslationsAsset::from_csv_str(csv).expect("parses");
        let en = LocaleCode::try_from("en").expect("en");
        let pl = LocaleCode::try_from("pl").expect("pl");
        assert_eq!(
            asset.tr_plural("wins", &en, 1, &[]).as_deref(),
            Some("1 win")
        );
        assert_eq!(
            asset.tr_plural("wins", &en, 3, &[]).as_deref(),
            Some("3 wins")
        );
        assert_eq!(
            asset.tr_plural("wins", &pl, 1, &[]).as_deref(),
            Some("1 wygrana")
        );
        assert_eq!(
            asset.tr_plural("wins", &pl, 3, &[]).as_deref(),
            Some("3 wygrane")
        );
        assert_eq!(
            asset.tr_plural("wins", &pl, 5, &[]).as_deref(),
            Some("5 wygranych")
        );
        // An unknown locale uses the first locale and its rule.
        let de = LocaleCode::try_from("de").expect("de");
        assert_eq!(
            asset.tr_plural("wins", &de, 5, &[]).as_deref(),
            Some("5 wins")
        );
    }

    const CSV: &str = "key,en,pl\nnav_gallery,Gallery,Galeria\nfilesystem_available,Server filesystem available: {available}.,Wolne miejsce: {available}.\n";

    #[test]
    fn parses_plain_region_and_mixed_case_codes() {
        assert_eq!(LocaleCode::try_from("en").expect("en parses").code(), "en");
        assert_eq!(
            LocaleCode::try_from(" PL ").expect("pl parses").code(),
            "pl"
        );
        assert_eq!(
            LocaleCode::try_from("en-US").expect("region parses").code(),
            "en-us"
        );
        assert_eq!(
            LocaleCode::try_from("pt_BR")
                .expect("underscore parses")
                .code(),
            "pt-br"
        );
        assert_eq!(
            LocaleCode::try_from("en-US").expect("region parses").lang(),
            "en"
        );
        assert_eq!(
            LocaleCode::try_from("en-US")
                .expect("region parses")
                .region(),
            Some("us")
        );
    }

    #[test]
    fn rejects_malformed_codes() {
        assert!(LocaleCode::try_from("").is_err());
        assert!(LocaleCode::try_from("e").is_err());
        assert!(LocaleCode::try_from("english").is_err());
        assert!(LocaleCode::try_from("en-USA").is_err());
        assert!(LocaleCode::try_from("e1").is_err());
    }

    #[test]
    fn parses_locales_and_looks_up_translations() {
        let asset = TranslationsAsset::from_csv_str(CSV).expect("fixture parses");
        let en = LocaleCode::try_from("en").expect("en parses");
        let pl = LocaleCode::try_from("pl").expect("pl parses");
        assert_eq!(asset.locales().count(), 2);
        assert_eq!(asset.tr("nav_gallery", &en), Some("Gallery"));
        assert_eq!(asset.tr("nav_gallery", &pl), Some("Galeria"));
        assert_eq!(asset.tr("missing", &en), None);
    }

    #[test]
    fn formats_placeholders() {
        let asset = TranslationsAsset::from_csv_str(CSV).expect("fixture parses");
        let en = LocaleCode::try_from("en").expect("en parses");
        assert_eq!(
            asset.tr_format("filesystem_available", &en, &[("available", "2 GB")]),
            Some("Server filesystem available: 2 GB.".to_owned())
        );
        assert_eq!(format_template("{a} and {b}", &[("a", "1")]), "1 and {b}");
        assert_eq!(format_template("plain", &[("a", "1")]), "plain");
        assert_eq!(format_template("{} {a}", &[("a", "1")]), "{} 1");
        assert_eq!(format_template("{a}{a}", &[("a", "1")]), "11",);
    }

    #[test]
    fn unknown_headers_are_errors() {
        assert_eq!(
            TranslationsAsset::from_csv_str("key,en,polish\na,b,c\n"),
            Err(TranslationError::InvalidHeader("polish".into()))
        );
    }

    #[test]
    fn trims_values_and_ignores_non_locale_columns() {
        let csv = "key,Context, en, pl\nplay,menu button, Play, Graj\n";
        let asset = TranslationsAsset::from_csv_str(csv).expect("csv with context column parses");
        let en = LocaleCode::try_from("en").expect("en parses");
        let pl = LocaleCode::try_from("pl").expect("pl parses");
        assert_eq!(asset.locales().count(), 2);
        assert_eq!(asset.tr("play", &en), Some("Play"));
        assert_eq!(asset.tr("play", &pl), Some("Graj"));
    }

    #[test]
    fn rejects_duplicate_keys() {
        let csv = "key,en\nnav,Gallery\nnav,Duplicate\n";
        assert_eq!(
            TranslationsAsset::from_csv_str(csv),
            Err(TranslationError::DuplicateKey {
                locale: "en".to_owned(),
                key: "nav".to_owned(),
            })
        );
    }

    #[test]
    fn rejects_empty_locale_list() {
        assert_eq!(
            TranslationsAsset::from_csv_str("key\nnav\n"),
            Err(TranslationError::NoLocales)
        );
    }

    #[test]
    fn merge_adds_keys_locales_and_overrides_values() {
        let base = "key,en,pl\nnav_gallery,Gallery,Galeria\n";
        let overrides = "key,en,de\nnav_gallery,Photos,Fotos\nnav_admin,Admin,Verwaltung\n";

        let mut asset = TranslationsAsset::from_csv_str(base).expect("base parses");
        asset
            .merge_csv_str(overrides, EmptyValue::Skip)
            .expect("overrides parse");

        let en = LocaleCode::try_from("en").expect("en parses");
        let pl = LocaleCode::try_from("pl").expect("pl parses");
        let de = LocaleCode::try_from("de").expect("de parses");

        assert_eq!(asset.tr("nav_gallery", &en), Some("Photos"));
        assert_eq!(asset.tr("nav_gallery", &pl), Some("Galeria"));
        assert_eq!(asset.tr("nav_gallery", &de), Some("Fotos"));
        assert_eq!(asset.tr("nav_admin", &en), Some("Admin"));
        assert_eq!(asset.tr("nav_admin", &pl), None);
        assert_eq!(asset.locales().count(), 3);
        assert_eq!(asset.keys().count(), 2);
    }

    #[test]
    fn merge_empty_value_skips_or_clears() {
        let base = "key,en\nnav_gallery,Gallery\nnav_admin,Admin\n";
        let sparse = "key,en\nnav_gallery,\n";

        let mut skipped = TranslationsAsset::from_csv_str(base).expect("base parses");
        skipped
            .merge_csv_str(sparse, EmptyValue::Skip)
            .expect("sparse parses");
        let en = LocaleCode::try_from("en").expect("en parses");
        assert_eq!(skipped.tr("nav_gallery", &en), Some("Gallery"));

        let mut cleared = TranslationsAsset::from_csv_str(base).expect("base parses");
        cleared
            .merge_csv_str(sparse, EmptyValue::Clear)
            .expect("sparse parses");
        assert_eq!(cleared.tr("nav_gallery", &en), None);
    }

    #[test]
    fn layered_constructor_preserves_order() {
        let asset = TranslationsAsset::from_csv_str_layered(
            ["key,en\nnav,Gallery\n", "key,en\nnav,Photos\n"],
            EmptyValue::Skip,
        )
        .expect("layers parse");
        let en = LocaleCode::try_from("en").expect("en parses");
        assert_eq!(asset.tr("nav", &en), Some("Photos"));
    }

    #[test]
    fn region_falls_back_to_language() {
        let asset = TranslationsAsset::from_csv_str(CSV).expect("fixture parses");
        let en_gb = LocaleCode::try_from("en-GB").expect("en-GB parses");
        assert_eq!(asset.tr("nav_gallery", &en_gb), Some("Gallery"));
    }

    #[test]
    fn locale_view_serializes_only_translated_keys() {
        let asset = TranslationsAsset::from_csv_str("key,en,pl\nnav_gallery,Gallery,\n")
            .expect("fixture parses");
        let catalog: &Catalog = &asset;
        let en = LocaleCode::try_from("en").expect("en parses");
        let pl = LocaleCode::try_from("pl").expect("pl parses");

        let en_view = catalog.locale_view(&en).expect("en view");
        assert_eq!(en_view.len(), 1);
        assert_eq!(en_view.get("nav_gallery"), Some("Gallery"));

        let pl_view = catalog.locale_view(&pl).expect("pl view");
        assert!(pl_view.is_empty());
        assert_eq!(pl_view.get("nav_gallery"), None);
    }
}
