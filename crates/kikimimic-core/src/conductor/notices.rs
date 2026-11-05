//! The notices a session currently carries. Each source owns one slot, so
//! posting or expiring one never erases another's.

use crate::domain::{LaneId, LaneRole};
use crate::event::Notice;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NoticeKey {
    Source(LaneRole),
    Silent(LaneId),
    Recording,
    Overrun(LaneId),
    Decode(LaneId),
}

#[derive(Default)]
pub(crate) struct Notices {
    entries: Vec<(NoticeKey, Notice)>,
    changed: bool,
}

impl Notices {
    /// Post (`Some`) or take down (`None`) one source's notice. Returns
    /// whether anything changed.
    pub fn set(&mut self, key: NoticeKey, notice: Option<Notice>) -> bool {
        let at = self.entries.iter().position(|(k, _)| *k == key);
        let changed = match (at, notice) {
            (Some(i), None) => {
                self.entries.remove(i);
                true
            }
            (Some(i), Some(n)) => {
                let changed = self.entries[i].1 != n;
                self.entries[i].1 = n;
                changed
            }
            (None, Some(n)) => {
                self.entries.push((key, n));
                true
            }
            (None, None) => false,
        };
        self.changed |= changed;
        changed
    }

    /// Whether anything changed since the last call: the cue to re-emit status.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    pub fn list(&self) -> Vec<Notice> {
        self.entries.iter().map(|(_, n)| n.clone()).collect()
    }

    pub fn clear(&mut self) {
        self.changed |= !self.entries.is_empty();
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{LaneLabel, LaneRole};

    fn overrun() -> Notice {
        Notice::Overrun {
            lane: LaneLabel::new(LaneRole::Mic, 1),
            dropped_ms: 10,
        }
    }

    #[test]
    fn one_sources_notice_does_not_erase_anothers() {
        let mut n = Notices::default();
        assert!(n.set(
            NoticeKey::Recording,
            Some(Notice::RecordingOff {
                reason: "full".into()
            })
        ));
        assert!(n.set(NoticeKey::Overrun(LaneId(0)), Some(overrun())));
        assert!(n.set(NoticeKey::Overrun(LaneId(0)), None));
        assert_eq!(
            n.list(),
            [Notice::RecordingOff {
                reason: "full".into()
            }]
        );
    }

    #[test]
    fn an_unchanged_notice_is_not_worth_a_re_emit() {
        let mut n = Notices::default();
        assert!(n.set(NoticeKey::Overrun(LaneId(0)), Some(overrun())));
        assert!(n.take_changed());
        assert!(!n.set(NoticeKey::Overrun(LaneId(0)), Some(overrun())));
        assert!(!n.set(NoticeKey::Decode(LaneId(0)), None));
        assert!(!n.take_changed());
    }
}
