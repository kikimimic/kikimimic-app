//! Platform-independent policy for a future meeting detector and menu-bar shell.
//! Signals are hints, not proof of a meeting. Automatic recording defaults off.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Manual,
    Automatic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
}

#[derive(Debug, Default)]
pub struct MeetingPolicy {
    pub enabled: bool,
    origin: Option<Origin>,
    suppressed: bool,
    absent_since: Option<u64>,
}

impl MeetingPolicy {
    /// Call only after the shared session owner has accepted a manual start.
    pub fn manual_started(&mut self) {
        self.origin = Some(Origin::Manual);
    }

    /// Also call on recording errors so the policy does not retain a stale session.
    pub fn stopped(&mut self, by_user: bool) {
        self.origin = None;
        self.suppressed |= by_user;
    }

    /// Poll with monotonic seconds. Require 60 seconds of absence before stopping
    /// an automatic session or rearming after a manual stop. Input-only activity
    /// and this application's capture must be excluded by the platform adapter.
    /// A returned Start reserves the session; failed starts must call stopped(false).
    pub fn poll(&mut self, present: bool, now: u64) -> Option<Action> {
        if present {
            self.absent_since = None;
            if self.enabled && !self.suppressed && self.origin.is_none() {
                self.origin = Some(Origin::Automatic);
                return Some(Action::Start);
            }
        } else {
            let since = *self.absent_since.get_or_insert(now);
            if now.saturating_sub(since) >= 60 {
                self.suppressed = false;
                if self.origin == Some(Origin::Automatic) {
                    self.origin = None;
                    return Some(Action::Stop);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opt_in_and_single_session() {
        let mut p = MeetingPolicy::default();
        assert_eq!(p.poll(true, 0), None);
        p.enabled = true;
        assert_eq!(p.poll(true, 1), Some(Action::Start));
        assert_eq!(p.poll(true, 2), None);
        p.manual_started();
        assert_eq!(p.poll(false, 3), None);
        assert_eq!(p.poll(false, 100), None);
    }

    #[test]
    fn grace_and_manual_stop_suppression() {
        let mut p = MeetingPolicy {
            enabled: true,
            ..Default::default()
        };
        assert_eq!(p.poll(true, 0), Some(Action::Start));
        assert_eq!(p.poll(false, 1), None);
        assert_eq!(p.poll(true, 50), None);
        assert_eq!(p.poll(false, 51), None);
        assert_eq!(p.poll(false, 111), Some(Action::Stop));
        assert_eq!(p.poll(true, 112), Some(Action::Start));
        p.stopped(true);
        assert_eq!(p.poll(true, 113), None);
        assert_eq!(p.poll(false, 114), None);
        assert_eq!(p.poll(false, 174), None);
        assert_eq!(p.poll(true, 175), Some(Action::Start));
    }
}
