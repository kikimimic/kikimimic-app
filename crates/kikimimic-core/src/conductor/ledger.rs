//! Utterance ids and the translation lifecycle. The only owner of both.
//!
//! The invariant it protects: every utterance shown as `translating` gets
//! exactly one terminal `Translation` event. An id leaves `owed` exactly once,
//! and that removal is what produces the event; a late result for an id that
//! is no longer owed is ignored.

use std::collections::{BTreeMap, VecDeque};

use crate::domain::{LaneLabel, Lang, UtteranceId};
use crate::event::{Event, TranslationOutcome};

/// Sentences waiting for the translator. Past this, the oldest goes: the
/// newest line on screen is the one worth translating.
const QUEUE_CAP: usize = 8;
/// Recent utterances handed to the translator as context.
const CONTEXT_LINES: usize = 3;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MtJob {
    pub id: UtteranceId,
    pub lane: LaneLabel,
    pub text: String,
    pub source: Option<Lang>,
    pub target: Lang,
    pub context: Vec<String>,
    pub terms: Vec<String>,
}

#[derive(Default)]
pub(crate) struct Ledger {
    next_id: u64,
    owed: BTreeMap<UtteranceId, (LaneLabel, Lang)>,
    queue: VecDeque<MtJob>,
    in_flight: Option<UtteranceId>,
    history: VecDeque<String>,
}

impl Ledger {
    pub fn allocate(&mut self) -> UtteranceId {
        self.next_id += 1;
        UtteranceId(self.next_id)
    }

    /// A finished utterance. With a `target` it is queued for translation;
    /// returns the events for any job the queue had to evict.
    pub fn finalize(
        &mut self,
        id: UtteranceId,
        lane: &LaneLabel,
        text: &str,
        source: Option<Lang>,
        target: Option<Lang>,
        terms: Vec<String>,
    ) -> Vec<Event> {
        let context: Vec<String> = self.history.iter().cloned().collect();
        self.history.push_back(text.to_string());
        if self.history.len() > CONTEXT_LINES {
            self.history.pop_front();
        }
        let Some(target) = target else {
            return Vec::new();
        };
        self.owed.insert(id, (lane.clone(), target.clone()));
        self.queue.push_back(MtJob {
            id,
            lane: lane.clone(),
            text: text.to_string(),
            source,
            target,
            context,
            terms,
        });
        let mut events = Vec::new();
        while self.queue.len() > QUEUE_CAP {
            let evicted = self.queue.pop_front().expect("over capacity");
            events.extend(self.settle_with(evicted.id, TranslationOutcome::Dropped));
        }
        events
    }

    /// The next job for the translator, if it is free.
    pub fn next_job(&mut self) -> Option<MtJob> {
        if self.in_flight.is_some() {
            return None;
        }
        let job = self.queue.pop_front()?;
        self.in_flight = Some(job.id);
        Some(job)
    }

    /// The translator answered. `None` if the id was already settled.
    pub fn settle(&mut self, id: UtteranceId, result: Result<String, String>) -> Option<Event> {
        if self.in_flight == Some(id) {
            self.in_flight = None;
        }
        let (_, target) = self.owed.get(&id)?.clone();
        let outcome = match result {
            Ok(text) => TranslationOutcome::Text { text, lang: target },
            Err(message) => TranslationOutcome::Failed { message },
        };
        self.settle_with(id, outcome)
    }

    fn settle_with(&mut self, id: UtteranceId, outcome: TranslationOutcome) -> Option<Event> {
        let (lane, _) = self.owed.remove(&id)?;
        Some(Event::Translation { id, lane, outcome })
    }

    pub fn is_settled(&self) -> bool {
        self.owed.is_empty()
    }

    /// Give up on everything still owed (stop's deadline passed).
    pub fn abandon_all(&mut self) -> Vec<Event> {
        self.queue.clear();
        self.in_flight = None;
        let ids: Vec<_> = self.owed.keys().copied().collect();
        ids.into_iter()
            .filter_map(|id| self.settle_with(id, TranslationOutcome::Dropped))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::LaneRole;

    fn lane() -> LaneLabel {
        LaneLabel::new(LaneRole::Mic, 1)
    }

    fn ja() -> Option<Lang> {
        Some(Lang::parse("ja").unwrap())
    }

    fn queued(l: &mut Ledger, text: &str) -> (UtteranceId, Vec<Event>) {
        let id = l.allocate();
        let ev = l.finalize(id, &lane(), text, None, ja(), vec![]);
        (id, ev)
    }

    #[test]
    fn ids_never_repeat() {
        let mut l = Ledger::default();
        assert_ne!(l.allocate(), l.allocate());
    }

    #[test]
    fn one_job_is_out_at_a_time() {
        let mut l = Ledger::default();
        queued(&mut l, "a");
        queued(&mut l, "b");
        let first = l.next_job().unwrap();
        assert!(l.next_job().is_none());
        l.settle(first.id, Ok("A".into())).unwrap();
        assert_eq!(l.next_job().unwrap().text, "b");
    }

    #[test]
    fn a_full_queue_drops_the_oldest_and_says_so() {
        let mut l = Ledger::default();
        let (oldest, _) = queued(&mut l, "0");
        for i in 1..QUEUE_CAP {
            assert!(queued(&mut l, &i.to_string()).1.is_empty());
        }
        let (_, evicted) = queued(&mut l, "overflow");
        assert_eq!(
            evicted,
            [Event::Translation {
                id: oldest,
                lane: lane(),
                outcome: TranslationOutcome::Dropped
            }]
        );
    }

    #[test]
    fn a_late_result_after_abandoning_is_ignored() {
        let mut l = Ledger::default();
        let (id, _) = queued(&mut l, "a");
        let job = l.next_job().unwrap();
        assert_eq!(l.abandon_all().len(), 1);
        assert_eq!(l.settle(job.id, Ok("late".into())), None);
        assert!(l.is_settled());
        let _ = id;
    }

    #[test]
    fn no_target_means_nothing_is_owed() {
        let mut l = Ledger::default();
        let id = l.allocate();
        l.finalize(id, &lane(), "a", None, None, vec![]);
        assert!(l.is_settled());
        assert!(l.next_job().is_none());
    }

    #[test]
    fn the_translator_sees_the_previous_lines_as_context() {
        let mut l = Ledger::default();
        queued(&mut l, "one");
        queued(&mut l, "two");
        l.next_job();
        let first = l.in_flight.unwrap();
        l.settle(first, Ok("1".into()));
        assert_eq!(l.next_job().unwrap().context, ["one"]);
    }
}
