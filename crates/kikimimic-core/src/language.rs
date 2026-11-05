//! Language names. The one list both the translation prompts and the app's
//! picker read.

/// (code, English name, native name), in the order the picker offers them:
/// the languages a meeting is realistically held in. Whisper knows many
/// more, and [`language_name`] passes those through untouched.
pub const NAMES: &[(&str, &str, &str)] = &[
    ("ja", "Japanese", "日本語"),
    ("en", "English", "English"),
    ("zh", "Chinese", "中文"),
    ("ko", "Korean", "한국어"),
    ("es", "Spanish", "Español"),
    ("fr", "French", "Français"),
    ("de", "German", "Deutsch"),
    ("pt", "Portuguese", "Português"),
    ("ru", "Russian", "Русский"),
    ("it", "Italian", "Italiano"),
    ("id", "Indonesian", "Bahasa Indonesia"),
    ("vi", "Vietnamese", "Tiếng Việt"),
    ("th", "Thai", "ไทย"),
    ("hi", "Hindi", "हिन्दी"),
    ("ar", "Arabic", "العربية"),
];

/// The language's English name, for prompting the translation model
/// ("Translate from Japanese to English" beats "from ja to en"). Unknown
/// codes are passed through: a model given "sv" still does the right thing
/// far more often than one given nothing.
pub fn language_name(code: &str) -> &str {
    NAMES
        .iter()
        .find(|(c, ..)| *c == code)
        .map_or(code, |(_, name, _)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_codes_get_their_english_name() {
        assert_eq!(language_name("ja"), "Japanese");
    }

    #[test]
    fn unknown_codes_pass_through() {
        assert_eq!(language_name("sv"), "sv");
    }

    #[test]
    fn every_code_is_a_valid_language() {
        for (code, ..) in NAMES {
            assert!(crate::domain::Lang::parse(code).is_ok(), "{code}");
        }
    }
}
