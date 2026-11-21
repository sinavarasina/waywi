use std::time::Instant;

use crate::text::SharedStr;

use super::sheet::{PlaybackMode, SpriteSheet};

/// Retained runtime playback timeline for a keyed animated [`Sprite`](super::Sprite).
#[derive(Debug, Clone)]
pub(crate) struct SpriteTimeline {
    pub(crate) animation: SharedStr,
    pub(crate) started_at: Instant,
    pub(crate) current_frame: usize,
    pub(crate) finished: bool,
    pub(crate) end_notified: bool,
    pub(crate) next_deadline: Option<Instant>,
}

/// Outcome returned by [`SpriteTimeline::advance`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SpriteAdvance {
    pub(crate) frame: usize,
    pub(crate) frame_changed: bool,
    pub(crate) next_deadline: Option<Instant>,
    pub(crate) just_finished: bool,
}

impl SpriteTimeline {
    pub(crate) fn new(animation: SharedStr, now: Instant, start_frame: usize) -> Self {
        Self {
            animation,
            started_at: now,
            current_frame: start_frame,
            finished: false,
            end_notified: false,
            next_deadline: None,
        }
    }

    /// Advances timeline to `now` and returns frame, dirty flag, exact next deadline, and
    /// one-shot completion notification flag.
    ///
    /// When `notify_end` is `true` (inside `tick_animations`), transitioning to `finished` marks
    /// `end_notified = true` and returns `just_finished = true`. When `notify_end` is `false`
    /// (during `reconcile`), if `has_end_handler` is `true` and the clip is already finished but
    /// unnotified, `next_deadline` is scheduled for `now` so `tick_animations` delivers the
    /// callback immediately on the next tick.
    pub(crate) fn advance(
        &mut self,
        sheet: &SpriteSheet,
        anim_name: &str,
        playback_override: Option<PlaybackMode>,
        now: Instant,
        notify_end: bool,
        has_end_handler: bool,
    ) -> SpriteAdvance {
        let Some(def) = sheet.get_animation(anim_name) else {
            self.next_deadline = None;
            return SpriteAdvance {
                frame: 0,
                frame_changed: false,
                next_deadline: None,
                just_finished: false,
            };
        };
        let mode = playback_override.unwrap_or(def.mode);
        let elapsed = now.saturating_duration_since(self.started_at);
        let (next_frame, finished, next_offset) =
            def.frames.resolve_at_elapsed(elapsed, mode, def.fps);

        let frame_changed = next_frame != self.current_frame;
        self.current_frame = next_frame;
        self.finished = finished;

        let just_finished = if finished && !self.end_notified {
            if notify_end {
                self.end_notified = true;
                self.next_deadline = None;
                has_end_handler
            } else if has_end_handler {
                self.next_deadline = Some(now);
                false
            } else {
                self.end_notified = true;
                self.next_deadline = None;
                false
            }
        } else {
            self.next_deadline = next_offset.map(|off| self.started_at + off);
            false
        };

        SpriteAdvance {
            frame: next_frame,
            frame_changed,
            next_deadline: self.next_deadline,
            just_finished,
        }
    }
}
