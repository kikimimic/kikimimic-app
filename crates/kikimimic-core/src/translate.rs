//! The queue between recognition and translation.
//!
//! Recognition must never wait for translation: a slow sentence would stall
//! the decode loop and cost the *next* sentence its audio. So finished
//! utterances go into a bounded queue that a translation thread drains, and
//! the queue — not the pipeline — absorbs the overload.

use std::collections::VecDeque;

use crate::Lane;

/// One sentence waiting to be translated, carrying the id of the transcript
/// line the result belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// The utterance this translates; the UI attaches the result by it.
    pub id: u64,
    pub lane: Lane,
    pub text: String,
    /// The language it was spoken in, when detection was confident enough to
    /// say. `None` leaves it for the model to infer.
    pub source: Option<String>,
    pub target: String,
}

/// A bounded backlog of sentences awaiting translation.
///
/// When it overflows the **oldest** job is dropped. What is worth reading in a
/// live meeting is the sentence currently on screen; holding the queue for a
/// translation from a minute ago would delay every sentence behind it and
/// eventually all of them. The dropped id is handed back so the UI can take
/// down its "translating…" placeholder instead of leaving it forever.
pub struct TranslationQueue {
    capacity: usize,
    jobs: VecDeque<Job>,
}

impl TranslationQueue {
    /// `capacity` is in sentences. Sized for a burst — both lanes talking
    /// over each other — not for an unbounded backlog.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "a queue that can hold nothing is a bug");
        Self {
            capacity,
            jobs: VecDeque::with_capacity(capacity),
        }
    }

    /// Queue a sentence, returning whichever job had to be dropped to make
    /// room for it.
    pub fn push(&mut self, job: Job) -> Option<Job> {
        let evicted = (self.jobs.len() >= self.capacity)
            .then(|| self.jobs.pop_front())
            .flatten();
        self.jobs.push_back(job);
        evicted
    }

    pub fn pop(&mut self) -> Option<Job> {
        self.jobs.pop_front()
    }

    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// Abandon everything still waiting, e.g. when the session ends before
    /// the backlog cleared. The caller still owes the UI a notice per job.
    pub fn drain(&mut self) -> Vec<Job> {
        self.jobs.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: u64) -> Job {
        Job {
            id,
            lane: Lane::Mic,
            text: format!("sentence {id}"),
            source: Some("en".into()),
            target: "ja".into(),
        }
    }

    #[test]
    fn sentences_come_out_in_the_order_they_were_spoken() {
        let mut q = TranslationQueue::new(4);
        assert_eq!(q.push(job(1)), None);
        assert_eq!(q.push(job(2)), None);
        assert_eq!(q.pop(), Some(job(1)));
        assert_eq!(q.pop(), Some(job(2)));
        assert_eq!(q.pop(), None);
    }

    #[test]
    fn an_overflowing_queue_drops_its_oldest_sentence() {
        // Translation fell behind. The sentence on screen right now matters
        // more than the one from a minute ago, and the caller is told which
        // one it lost so the placeholder can come down.
        let mut q = TranslationQueue::new(2);
        q.push(job(1));
        q.push(job(2));
        assert_eq!(q.push(job(3)), Some(job(1)));
        assert_eq!(q.len(), 2);
        assert_eq!(q.pop(), Some(job(2)));
    }

    #[test]
    fn draining_hands_back_everything_still_waiting() {
        let mut q = TranslationQueue::new(4);
        q.push(job(1));
        q.push(job(2));
        assert_eq!(q.drain(), vec![job(1), job(2)]);
        assert!(q.is_empty());
    }
}
