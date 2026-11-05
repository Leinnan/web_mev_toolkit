//! Lookups that never blank a label. [`Localize::t`] is a plain string,
//! [`Localize::t_args`] fills `{name}` slots and [`Localize::t_n`] picks the plural
//! form of a count (`{key}_one`, `{key}_few`, `{key}_many`, `{key}_other`).

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use crate::{Catalog, LocaleCode, format_template};

/// Text lookups. Missing keys fall back to the default language, then to the
/// key itself (logged once).
pub trait Localize {
    /// The table to read.
    fn catalog(&self) -> &Catalog;
    /// The active language.
    fn locale(&self) -> &LocaleCode;

    /// The string for `key`.
    fn t(&self, key: &str) -> String {
        lookup(self.catalog(), self.locale(), key).to_owned()
    }

    /// The string for `key` with its `{name}` slots filled.
    fn t_args(&self, key: &str, args: &[(&str, &str)]) -> String {
        format_template(lookup(self.catalog(), self.locale(), key), args)
    }

    /// The plural form of `key` for `n`, with `{n}` and `args` filled.
    fn t_n(&self, key: &str, n: u64, args: &[(&str, &str)]) -> String {
        let catalog = self.catalog();
        catalog
            .tr_plural(key, self.locale(), n, args)
            .or_else(|| catalog.tr_plural(key, &LocaleCode::default(), n, args))
            .unwrap_or_else(|| {
                missing(key);
                format!("{key} {n}")
            })
    }
}

/// A catalog and the language of one request.
pub struct Lang<'a> {
    /// The table to read.
    pub catalog: &'a Catalog,
    /// The active language.
    pub locale: LocaleCode,
}

impl Localize for Lang<'_> {
    fn catalog(&self) -> &Catalog {
        self.catalog
    }
    fn locale(&self) -> &LocaleCode {
        &self.locale
    }
}

fn lookup<'a>(catalog: &'a Catalog, locale: &LocaleCode, key: &'a str) -> &'a str {
    catalog
        .tr_or_default_locale(key, locale)
        .or_else(|| catalog.tr(key, &LocaleCode::default()))
        .unwrap_or_else(|| {
            missing(key);
            key
        })
}

fn missing(key: &str) {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(Default::default);
    if let Ok(mut seen) = seen.lock()
        && seen.insert(key.to_owned())
    {
        tracing::warn!("missing translation key `{key}`");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TranslationsAsset;

    const CSV: &str = "key,Context,en,pl\n\
        hello,,Hello,Cześć\n\
        only_en,,English,\n\
        named,,Hi {name},Hej {name}\n\
        apples_one,,{n} apple,{n} jabłko\n\
        apples_other,,{n} apples,\n\
        apples_few,,,{n} jabłka\n\
        apples_many,,,{n} jabłek\n";

    fn table() -> TranslationsAsset {
        TranslationsAsset::from_csv_str(CSV).expect("the table parses")
    }

    fn lang<'a>(table: &'a TranslationsAsset, code: &str) -> Lang<'a> {
        Lang {
            catalog: table,
            locale: LocaleCode::try_from(code).expect("a locale code"),
        }
    }

    #[test]
    fn lookups_fall_back_to_the_default_language_then_the_key() {
        let table = table();
        let pl = lang(&table, "pl");
        assert_eq!(pl.t("hello"), "Cześć");
        assert_eq!(pl.t("only_en"), "English");
        assert_eq!(pl.t("no_such_key"), "no_such_key");
    }

    #[test]
    fn slots_and_plurals_are_filled() {
        let table = table();
        let pl = lang(&table, "pl");
        assert_eq!(pl.t_args("named", &[("name", "Ola")]), "Hej Ola");
        assert_eq!(pl.t_n("apples", 1, &[]), "1 jabłko");
        assert_eq!(pl.t_n("apples", 3, &[]), "3 jabłka");
        assert_eq!(pl.t_n("apples", 5, &[]), "5 jabłek");
        assert_eq!(lang(&table, "en").t_n("apples", 5, &[]), "5 apples");
        assert_eq!(pl.t_n("no_such_key", 2, &[]), "no_such_key 2");
    }
}
