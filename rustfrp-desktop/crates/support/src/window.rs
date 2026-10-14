//! Window release and notifications are state machines independent of any native toolkit.
use serde::Deserialize;
use std::time::Duration;

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiState {
    pub busy: bool,
    pub dirty: bool,
    pub sensitive: bool,
}

#[derive(Default)]
pub struct WindowPolicy {
    hidden_since: Option<Duration>,
    ui: Option<UiState>,
}
impl WindowPolicy {
    pub fn hidden(&mut self, now: Duration) {
        if self.hidden_since.is_none() {
            self.hidden_since = Some(now);
        }
    }
    pub fn shown(&mut self) {
        self.hidden_since = None;
    }
    pub fn report(&mut self, state: UiState) {
        self.ui = Some(state);
    }
    pub fn destroyed(&mut self) {
        self.hidden_since = None;
        self.ui = None;
    }
    pub fn can_release(&self, now: Duration, lightweight: bool, host_busy: bool) -> bool {
        lightweight
            && !host_busy
            && self
                .hidden_since
                .is_some_and(|hidden| now.saturating_sub(hidden) >= Duration::from_secs(45))
            && self
                .ui
                .is_some_and(|state| !state.busy && !state.dirty && !state.sensitive)
    }
}
#[derive(Default)]
pub struct ConnectionNotifications {
    last: Option<bool>,
    candidate: Option<bool>,
}
impl ConnectionNotifications {
    pub fn observe(&mut self, connected: Option<bool>, enabled: bool) -> Option<bool> {
        let Some(connected) = connected else {
            self.candidate = None;
            return None;
        };
        if !enabled || self.last.is_none() {
            self.last = Some(connected);
            self.candidate = None;
            return None;
        }
        if self.last == Some(connected) {
            self.candidate = None;
            return None;
        }
        if self.candidate == Some(connected) {
            self.last = Some(connected);
            self.candidate = None;
            return Some(connected);
        }
        self.candidate = Some(connected);
        None
    }
}
#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    #[test]
    fn only_known_idle_hidden_windows_release_after_45_seconds() {
        let mut policy = WindowPolicy::default();
        let now = Duration::from_secs(60);
        assert!(!policy.can_release(now, true, false));
        policy.hidden(Duration::ZERO);
        assert!(!policy.can_release(now, true, false));
        for state in [
            UiState {
                busy: true,
                ..Default::default()
            },
            UiState {
                dirty: true,
                ..Default::default()
            },
            UiState {
                sensitive: true,
                ..Default::default()
            },
        ] {
            policy.report(state);
            assert!(!policy.can_release(now, true, false));
        }
        policy.report(UiState::default());
        assert!(policy.can_release(now, true, false));
        assert!(!policy.can_release(Duration::from_secs(44), true, false));
        assert!(!policy.can_release(now, false, false));
        assert!(!policy.can_release(now, true, true));
        policy.hidden(Duration::from_secs(59));
        assert!(policy.can_release(now, true, false));
        policy.shown();
        assert!(!policy.can_release(now, true, false));
        policy.hidden(Duration::ZERO);
        policy.destroyed();
        assert!(!policy.can_release(now, true, false));
    }
    #[test]
    fn connection_notifications_debounce_and_never_invent_disconnects_from_request_errors() {
        let mut tracker = ConnectionNotifications::default();
        assert_eq!(tracker.observe(Some(true), true), None);
        assert_eq!(tracker.observe(Some(false), true), None);
        assert_eq!(tracker.observe(None, true), None);
        assert_eq!(tracker.observe(Some(false), true), None);
        assert_eq!(tracker.observe(Some(false), true), Some(false));
        assert_eq!(tracker.observe(Some(false), true), None);
        assert_eq!(tracker.observe(Some(true), true), None);
        assert_eq!(tracker.observe(Some(true), true), Some(true));
        assert_eq!(tracker.observe(Some(false), false), None);
        assert_eq!(tracker.observe(Some(false), true), None);
    }
}
