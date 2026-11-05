//! What is spoken, and what it should be translated into.

/// The session's language settings: which languages are actually spoken, and
/// where the translation should land.
///
/// The two are separate on purpose. Collapsing them into one "main/sub pair"
/// cannot express the most common case — a single-language meeting with no
/// translation at all — and cannot express "this English meeting, read in
/// Japanese", where only one language is ever spoken but a translation is
/// still wanted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguagePolicy {
    /// Languages that may be spoken, as whisper language codes. At most two:
    /// beyond that, detection on a one-second window is closer to a guess
    /// than a decision.
    pub spoken: Vec<String>,
    /// Where translations go; `None` means "don't translate".
    pub target: Option<String>,
    /// Also translate the target language itself into the *other* spoken
    /// language — for showing the screen to the other party.
    pub mutual: bool,
}

impl Default for LanguagePolicy {
    fn default() -> Self {
        Self {
            spoken: vec!["ja".into(), "en".into()],
            target: Some("ja".into()),
            mutual: false,
        }
    }
}

impl LanguagePolicy {
    /// The language to force on the recogniser, if there is only one it could
    /// be. Pinning skips `lang_detect` entirely, which is both faster and
    /// immune to the failure where whisper, told the wrong language, silently
    /// *translates* rather than misrecognises.
    pub fn pinned_lang(&self) -> Option<&str> {
        match self.spoken.as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }

    /// Which language an utterance spoken in `source` should be translated
    /// into — `None` when it needs no translation.
    ///
    /// `source` is `None` when detection was unconfident; the utterance is
    /// still translated, because the alternative is silently dropping it.
    pub fn target_for(&self, source: Option<&str>) -> Option<&str> {
        let target = self.target.as_deref()?;
        if source != Some(target) {
            return Some(target);
        }
        // Already in the language the reader wants. Worth translating only
        // for the other party's benefit, and only when there is another
        // language to translate into.
        if !self.mutual {
            return None;
        }
        self.spoken
            .iter()
            .map(String::as_str)
            .find(|l| *l != target)
    }
}

/// The language's English name, for prompting the translation model
/// ("Translate from Japanese to English" beats "from ja to en"). Unknown
/// codes are passed through: a model given "sv" still does the right thing
/// far more often than one given nothing.
pub fn language_name(code: &str) -> &str {
    NAMES
        .iter()
        .find(|(c, _)| *c == code)
        .map_or(code, |(_, name)| *name)
}

/// The languages the picker offers, in the order it offers them: the ones a
/// meeting is realistically held in. Whisper knows many more, and
/// [`language_name`] passes those through untouched.
pub const NAMES: &[(&str, &str)] = &[
    ("ja", "Japanese"),
    ("en", "English"),
    ("zh", "Chinese"),
    ("ko", "Korean"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("pt", "Portuguese"),
    ("ru", "Russian"),
    ("it", "Italian"),
    ("id", "Indonesian"),
    ("vi", "Vietnamese"),
    ("th", "Thai"),
    ("hi", "Hindi"),
    ("ar", "Arabic"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(spoken: &[&str], target: Option<&str>, mutual: bool) -> LanguagePolicy {
        LanguagePolicy {
            spoken: spoken.iter().map(|s| s.to_string()).collect(),
            target: target.map(str::to_string),
            mutual,
        }
    }

    #[test]
    fn a_single_spoken_language_is_pinned_on_the_recogniser() {
        assert_eq!(policy(&["en"], None, false).pinned_lang(), Some("en"));
        // Two candidates: detection has to run.
        assert_eq!(policy(&["ja", "en"], None, false).pinned_lang(), None);
    }

    #[test]
    fn translation_off_means_nothing_is_translated() {
        let p = policy(&["ja", "en"], None, false);
        assert_eq!(p.target_for(Some("en")), None);
        assert_eq!(p.target_for(None), None);
    }

    #[test]
    fn an_english_meeting_read_in_japanese() {
        let p = policy(&["en"], Some("ja"), false);
        assert_eq!(p.target_for(Some("en")), Some("ja"));
    }

    #[test]
    fn what_is_already_in_the_readers_language_is_left_alone() {
        let p = policy(&["ja", "en"], Some("ja"), false);
        assert_eq!(p.target_for(Some("ja")), None);
        assert_eq!(p.target_for(Some("en")), Some("ja"));
    }

    #[test]
    fn mutual_translates_the_readers_own_language_for_the_other_party() {
        let p = policy(&["ja", "en"], Some("ja"), true);
        assert_eq!(p.target_for(Some("ja")), Some("en"));
        assert_eq!(p.target_for(Some("en")), Some("ja"));
    }

    #[test]
    fn mutual_with_nothing_to_translate_into_is_a_no_op() {
        // One spoken language and translating into it: there is no "other
        // party's language" for the flag to mean.
        let p = policy(&["ja"], Some("ja"), true);
        assert_eq!(p.target_for(Some("ja")), None);
    }

    #[test]
    fn an_undetected_language_is_still_translated() {
        // Detection withheld its guess (short window, close call). Skipping
        // the sentence would lose it for good; the model is given the
        // sentence without being told what it came from.
        let p = policy(&["ja", "en"], Some("ja"), false);
        assert_eq!(p.target_for(None), Some("ja"));
    }

    #[test]
    fn language_names_are_for_the_prompt_and_unknown_codes_pass_through() {
        assert_eq!(language_name("ja"), "Japanese");
        assert_eq!(language_name("sv"), "sv");
    }
}
