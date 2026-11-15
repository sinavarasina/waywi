use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionKey, SubscriptionSpec};

/// Timer constructors for use inside `subscriptions(state)`.
pub struct Timer;

impl Timer {
    /// Schedules a one-shot timer keyed by `key`.
    ///
    /// Changing `key` (for example `("osd-hide", state.osd_seq)`) automatically cancels the
    /// previous timer and starts a fresh timeout; removing it from `subscriptions(state)` cancels
    /// it immediately.
    pub fn once<M: 'static>(
        key: impl IntoSubscriptionKey,
        duration: Duration,
        msg: M,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::TimerOnce {
                key: key.into_subscription_key(),
                duration,
                msg,
            },
        }
    }

    /// Schedules a repeating interval timer keyed by `key`.
    pub fn every<M: 'static>(
        key: impl IntoSubscriptionKey,
        interval: Duration,
        msg: M,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::TimerEvery {
                key: key.into_subscription_key(),
                interval: interval.max(Duration::from_millis(1)),
                msg,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ActiveTimerId {
    key: SubscriptionKey,
    repeating: bool,
}

struct ActiveTimer<M> {
    interval: Option<Duration>,
    next_deadline: Instant,
    msg: M,
    fired_once: bool,
}

/// Shared timer scheduler reconciling subscribed timer streams and imperative one-shot
/// `EffectCtx::after(...)` timers on the single `App` event loop.
pub(crate) struct TimerScheduler<M> {
    subscribed: HashMap<ActiveTimerId, ActiveTimer<M>>,
    imperative: Vec<(Instant, M)>,
}

impl<M: Clone + 'static> Default for TimerScheduler<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: Clone + 'static> TimerScheduler<M> {
    pub(crate) fn new() -> Self {
        Self {
            subscribed: HashMap::new(),
            imperative: Vec::new(),
        }
    }

    /// Reconciles active timer subscriptions from flattened subscription specs.
    pub(crate) fn reconcile<'a>(
        &mut self,
        specs: impl Iterator<Item = &'a SubscriptionSpec<M>>,
        now: Instant,
    ) {
        let mut seen = HashSet::new();
        for spec in specs {
            match spec {
                SubscriptionSpec::TimerOnce { key, duration, msg } => {
                    let id = ActiveTimerId {
                        key: *key,
                        repeating: false,
                    };
                    seen.insert(id);
                    if let Some(existing) = self.subscribed.get_mut(&id) {
                        existing.msg = msg.clone();
                    } else {
                        self.subscribed.insert(
                            id,
                            ActiveTimer {
                                interval: None,
                                next_deadline: now + *duration,
                                msg: msg.clone(),
                                fired_once: false,
                            },
                        );
                    }
                }
                SubscriptionSpec::TimerEvery { key, interval, msg } => {
                    let id = ActiveTimerId {
                        key: *key,
                        repeating: true,
                    };
                    seen.insert(id);
                    let iv = (*interval).max(Duration::from_millis(1));
                    if let Some(existing) = self.subscribed.get_mut(&id) {
                        existing.msg = msg.clone();
                        if existing.interval != Some(iv) {
                            existing.interval = Some(iv);
                            existing.next_deadline = now + iv;
                        }
                    } else {
                        self.subscribed.insert(
                            id,
                            ActiveTimer {
                                interval: Some(iv),
                                next_deadline: now + iv,
                                msg: msg.clone(),
                                fired_once: false,
                            },
                        );
                    }
                }
                _ => {}
            }
        }
        self.subscribed.retain(|id, _| seen.contains(id));
    }

    pub(crate) fn push_imperative(&mut self, deadline: Instant, msg: M) {
        self.imperative.push((deadline, msg));
    }

    /// Collects all timers due at or before `now`.
    pub(crate) fn collect_due(&mut self, now: Instant) -> Vec<M> {
        let mut due = Vec::new();
        for timer in self.subscribed.values_mut() {
            if let Some(iv) = timer.interval {
                if now >= timer.next_deadline {
                    due.push(timer.msg.clone());
                    while timer.next_deadline <= now {
                        timer.next_deadline += iv;
                    }
                }
            } else if !timer.fired_once && now >= timer.next_deadline {
                timer.fired_once = true;
                due.push(timer.msg.clone());
            }
        }

        let mut i = 0;
        while i < self.imperative.len() {
            if now >= self.imperative[i].0 {
                let (_, msg) = self.imperative.remove(i);
                due.push(msg);
            } else {
                i += 1;
            }
        }
        due
    }

    #[must_use]
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        let mut earliest: Option<Instant> = None;
        for timer in self.subscribed.values() {
            if timer.interval.is_none() && timer.fired_once {
                continue;
            }
            earliest = Some(earliest.map_or(timer.next_deadline, |e| e.min(timer.next_deadline)));
        }
        for (dl, _) in &self.imperative {
            earliest = Some(earliest.map_or(*dl, |e| e.min(*dl)));
        }
        earliest
    }
}
