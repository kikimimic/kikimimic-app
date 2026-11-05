//! Values the whole core speaks in. Each invariant lives in a type so that the
//! code handling it cannot forget it.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A whisper-style language code ("ja", "en", "yue"). Parsed once at the
/// boundary; inside the core a language is never a free `String`.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Lang(Box<str>);

impl Lang {
    pub fn parse(code: &str) -> Result<Self, String> {
        let ok = (2..=3).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_lowercase());
        if ok {
            Ok(Self(code.into()))
        } else {
            Err(format!("not a language code: {code:?}"))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Lang {
    type Error = String;
    fn try_from(code: String) -> Result<Self, String> {
        Self::parse(&code)
    }
}

impl From<Lang> for String {
    fn from(lang: Lang) -> Self {
        lang.0.into()
    }
}

impl fmt::Debug for Lang {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for Lang {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Languages that may be spoken: one or two, by construction. Beyond two,
/// detection on a one-second window is closer to a guess than a decision.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(try_from = "Vec<Lang>", into = "Vec<Lang>")]
pub enum Spoken {
    One(Lang),
    Two(Lang, Lang),
}

impl Spoken {
    /// The language to force on the recognizer when only one is possible.
    /// Pinning skips detection: faster, and immune to whisper silently
    /// *translating* when told the wrong language.
    pub fn pinned(&self) -> Option<&Lang> {
        match self {
            Self::One(l) => Some(l),
            Self::Two(..) => None,
        }
    }

    pub fn contains(&self, lang: &Lang) -> bool {
        self.iter().any(|l| l == lang)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Lang> {
        let (a, b) = match self {
            Self::One(a) => (a, None),
            Self::Two(a, b) => (a, Some(b)),
        };
        std::iter::once(a).chain(b)
    }
}

impl TryFrom<Vec<Lang>> for Spoken {
    type Error = String;
    fn try_from(langs: Vec<Lang>) -> Result<Self, String> {
        let mut it = langs.into_iter();
        match (it.next(), it.next(), it.next()) {
            (Some(a), None, _) => Ok(Self::One(a)),
            (Some(a), Some(b), None) if a != b => Ok(Self::Two(a, b)),
            (Some(_), Some(_), None) => Err("the two spoken languages must differ".into()),
            (None, ..) => Err("at least one spoken language is required".into()),
            _ => Err("at most two spoken languages".into()),
        }
    }
}

impl From<Spoken> for Vec<Lang> {
    fn from(s: Spoken) -> Self {
        s.iter().cloned().collect()
    }
}

/// What is spoken, and what it should be translated into. Kept apart on
/// purpose: a single-language meeting with no translation, and "this English
/// meeting, read in Japanese", are both common.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct LanguagePolicy {
    pub spoken: Spoken,
    /// `None`: don't translate.
    pub target: Option<Lang>,
    /// Also translate the target language into the other spoken language,
    /// for showing the screen to the other party.
    pub mutual: bool,
}

impl Default for LanguagePolicy {
    fn default() -> Self {
        let lang = |c| Lang::parse(c).expect("valid");
        Self {
            spoken: Spoken::Two(lang("ja"), lang("en")),
            target: Some(lang("ja")),
            mutual: false,
        }
    }
}

impl LanguagePolicy {
    /// Where an utterance spoken in `source` should be translated, `None` when
    /// it needs no translation. An unconfident `source` (`None`) is still
    /// translated: the alternative is silently dropping it.
    pub fn target_for(&self, source: Option<&Lang>) -> Option<&Lang> {
        let target = self.target.as_ref()?;
        if source != Some(target) {
            return Some(target);
        }
        if !self.mutual {
            return None;
        }
        self.spoken.iter().find(|l| *l != target)
    }
}

/// A lane in the current session, by position. Nothing assumes two.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct LaneId(pub u8);

/// What a lane listens to.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LaneRole {
    Mic,
    System,
}

/// What files, the transcript and the UI call a lane: "mic", "speaker", then
/// "mic-2", "speaker-2". Derived in one place so v2 recordings stay readable.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct LaneLabel(Box<str>);

impl LaneLabel {
    /// `ordinal` counts lanes of the same role, from 1.
    pub fn new(role: LaneRole, ordinal: usize) -> Self {
        let base = match role {
            LaneRole::Mic => "mic",
            LaneRole::System => "speaker",
        };
        if ordinal <= 1 {
            Self(base.into())
        } else {
            Self(format!("{base}-{ordinal}").into())
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Labels for a session's lanes, in order.
pub fn lane_labels(roles: &[LaneRole]) -> Vec<LaneLabel> {
    roles
        .iter()
        .enumerate()
        .map(|(i, role)| LaneLabel::new(*role, roles[..=i].iter().filter(|r| *r == role).count()))
        .collect()
}

/// Monotonic for the life of the app: the UI keeps the previous session's
/// lines on screen, and a reused id would attach a translation to the wrong
/// one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UtteranceId(pub u64);

/// When audio was captured, on the one monotonic clock every capture backend
/// of a platform shares (mach host time, QPC, CLOCK_MONOTONIC), in ns.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct HostNanos(pub u64);

/// Position in the audio pushed into one lane's recognizer.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct StreamMs(pub u64);

/// Position in the session's recording. The only time the UI and the
/// transcript file ever show.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RecordingMs(pub u64);

/// 16 kHz mono audio, the only format recognizers accept.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Pcm16k(Vec<f32>);

impl Pcm16k {
    pub const RATE: u32 = 16_000;

    /// For audio known to be 16 kHz mono already (fixtures, engine tests).
    /// Device audio goes through the core's resampler instead.
    pub fn assume_16k(samples: Vec<f32>) -> Self {
        Self(samples)
    }

    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }

    pub fn len_ms(&self) -> u64 {
        self.0.len() as u64 * 1000 / Self::RATE as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(c: &str) -> Lang {
        Lang::parse(c).unwrap()
    }

    #[test]
    fn a_language_code_is_validated_once() {
        assert!(Lang::parse("ja").is_ok());
        assert!(Lang::parse("yue").is_ok());
        assert!(Lang::parse("JA").is_err());
        assert!(Lang::parse("japanese").is_err());
    }

    #[test]
    fn spoken_languages_are_one_or_two_distinct() {
        assert_eq!(Spoken::try_from(vec![l("ja")]), Ok(Spoken::One(l("ja"))));
        assert!(Spoken::try_from(vec![]).is_err());
        assert!(Spoken::try_from(vec![l("ja"), l("ja")]).is_err());
        assert!(Spoken::try_from(vec![l("ja"), l("en"), l("ko")]).is_err());
    }

    #[test]
    fn only_a_single_spoken_language_is_pinned() {
        assert_eq!(Spoken::One(l("en")).pinned(), Some(&l("en")));
        assert_eq!(Spoken::Two(l("ja"), l("en")).pinned(), None);
    }

    #[test]
    fn speech_in_another_language_goes_to_the_target() {
        let p = LanguagePolicy::default();
        assert_eq!(p.target_for(Some(&l("en"))), Some(&l("ja")));
        assert_eq!(p.target_for(None), Some(&l("ja")));
    }

    #[test]
    fn speech_already_in_the_target_is_left_alone_unless_mutual() {
        let mut p = LanguagePolicy::default();
        assert_eq!(p.target_for(Some(&l("ja"))), None);
        p.mutual = true;
        assert_eq!(p.target_for(Some(&l("ja"))), Some(&l("en")));
    }

    #[test]
    fn no_target_means_no_translation() {
        let p = LanguagePolicy {
            target: None,
            ..LanguagePolicy::default()
        };
        assert_eq!(p.target_for(Some(&l("en"))), None);
    }

    #[test]
    fn lanes_keep_their_v2_names_and_number_the_rest() {
        use LaneRole::*;
        let names: Vec<_> = lane_labels(&[Mic, System, Mic, System])
            .into_iter()
            .map(|l| l.as_str().to_string())
            .collect();
        assert_eq!(names, ["mic", "speaker", "mic-2", "speaker-2"]);
    }

    #[test]
    fn settings_json_rejects_three_spoken_languages() {
        let err = serde_json::from_str::<Spoken>(r#"["ja","en","ko"]"#);
        assert!(err.is_err());
    }
}
