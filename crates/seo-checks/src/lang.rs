//! Validation of `lang` values: BCP 47 syntax from `oxilangtag`, the primary
//! language subtag looked up in the ISO 639 table from `isolang`. Regions and
//! scripts are checked by shape only (that is what `oxilangtag` does).

use isolang::Language;
use oxilangtag::LanguageTag;

/// Language codes that ISO 639 retired or reserves but that are still common
/// in real markup (`iw` is the old Hebrew, `in` the old Indonesian).
const LEGACY_PRIMARY: [&str; 9] = ["iw", "in", "ji", "jw", "mo", "sh", "und", "mul", "zxx"];

/// Whether `value` is a usable language tag: well-formed BCP 47 whose primary
/// subtag is a known language. Private-use tags (`x-...`) are accepted as is.
pub(crate) fn is_valid_language_tag(value: &str) -> bool {
    let value = value.trim();
    let Ok(tag) = LanguageTag::parse(value) else {
        return false;
    };
    // `x-foo` private use and `i-klingon` style grandfathered tags have no
    // ordinary primary language to look up.
    let lowered = value.to_ascii_lowercase();
    if lowered.starts_with("x-") || lowered.starts_with("i-") {
        return true;
    }
    known_primary_language(&tag.primary_language().to_ascii_lowercase())
}

fn known_primary_language(code: &str) -> bool {
    if LEGACY_PRIMARY.contains(&code) {
        return true;
    }
    match code.len() {
        2 => Language::from_639_1(code).is_some(),
        3 => Language::from_639_3(code).is_some(),
        _ => false,
    }
}

/// A `lang` attribute or `Content-Language` header may list several
/// comma-separated tags; all of them must be valid.
pub(crate) fn is_valid_language_list(value: &str) -> bool {
    value.split(',').all(is_valid_language_tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_common_tags() {
        // `x-private` is a private-use tag, the rest are ordinary ones.
        for tag in ["en", "ru", "uk-UA", "en-US", "zh-Hans-CN", "es-419", "pt-br", "de", "x-private"] {
            assert!(is_valid_language_tag(tag), "{tag} should be valid");
        }
    }

    #[test]
    fn rejects_malformed_and_unknown() {
        for tag in ["InvalidLangCode", "en_US", "xx", "", "e", "en-", "ru--RU", "русский"] {
            assert!(!is_valid_language_tag(tag), "{tag:?} should be invalid");
        }
    }

    #[test]
    fn keeps_legacy_codes() {
        assert!(is_valid_language_tag("iw"));
        assert!(is_valid_language_tag("und"));
    }

    #[test]
    fn list_needs_every_part_valid() {
        assert!(is_valid_language_list("en, fr"));
        assert!(!is_valid_language_list("en, nonsense"));
    }
}
