//! What the core tells the UI. One ordered stream of these.

use std::path::PathBuf;

use crate::domain::{LaneLabel, LaneRole, Lang, RecordingMs, UtteranceId};

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// One lane's step. `utterance` is assigned when an utterance's first text
    /// appears, so later features (translating partials, live speaker labels)
    /// have an id to address.
    Transcript {
        lane: LaneLabel,
        utterance: UtteranceId,
        committed_delta: String,
        volatile: String,
        finished: Option<UtteranceView>,
    },
    /// Exactly one per utterance whose `translating` was true.
    Translation {
        id: UtteranceId,
        lane: LaneLabel,
        outcome: TranslationOutcome,
    },
    Status(StatusView),
    /// A capture source needs a decision or reports one (output-device
    /// switching). Answer with `Kikimimic::decide_source`.
    Source(crate::capture::SourceEvent),
    /// Speaker labelling of a finished recording (`Kikimimic::diarize`).
    PostProcess {
        recording: PathBuf,
        state: PostProcessState,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PostProcessState {
    Running,
    /// `postprocess/transcript.jsonl` is written; reading the recording shows it.
    Done,
    Failed {
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct UtteranceView {
    pub id: UtteranceId,
    pub text: String,
    pub start: RecordingMs,
    pub end: RecordingMs,
    pub lang: Option<Lang>,
    /// A `Translation` event for this id will follow.
    pub translating: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TranslationOutcome {
    Text {
        text: String,
        lang: Lang,
    },
    /// Evicted from a full queue or abandoned at stop: nothing is coming.
    Dropped,
    Failed {
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct StatusView {
    pub phase: Phase,
    pub notices: Vec<Notice>,
    pub recording_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Listening,
    Stopping,
    Error { message: String },
}

/// Typed, so the UI words and localizes them; the core formats no sentence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// A lane could not start; the session continues without it.
    SourceUnavailable {
        role: LaneRole,
        reason: String,
    },
    /// The system lane delivers only digital silence: macOS withholds the
    /// permission that way instead of failing.
    SystemAudioSilent {
        lane: LaneLabel,
    },
    /// Recording stopped; the transcript continues.
    RecordingOff {
        reason: String,
    },
    Overrun {
        lane: LaneLabel,
        dropped_ms: u64,
    },
    DecodeFailed {
        lane: LaneLabel,
        reason: String,
    },
    TranslationUnavailable {
        reason: String,
    },
}
