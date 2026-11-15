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
}

impl SpriteTimeline {
    pub(crate) fn new(animation: SharedStr, now: Instant, start_frame: usize) -> Self {
        Self {
            animation,
            started_at: now,
            current_frame: start_frame,
            finished: false,
        }
    }

    /// Advances timeline to `now` and returns `(frame_index, changed, next_deadline)`.
    pub(crate) fn advance(
        &mut self,
        sheet: &SpriteSheet,
        anim_name: &str,
        playback_override: Option<PlaybackMode>,
        now: Instant,
    ) -> (usize, bool, Option<Instant>) {
        let Some(def) = sheet.get_animation(anim_name) else {
            return (0, false, None);
        };
        let mode = playback_override.unwrap_or(def.mode);
        let frame_dur = def.fps.frame_duration();
        let elapsed = now.saturating_duration_since(self.started_at);
        let step = (elapsed.as_secs_f64() / frame_dur.as_secs_f64()).floor() as usize;
        let count = def.frames.count();

        let next_frame = def.frames.frame_at(step, mode);
        let changed = next_frame != self.current_frame;
        self.current_frame = next_frame;

        if mode == PlaybackMode::Once && step + 1 >= count {
            self.finished = true;
            (next_frame, changed, None)
        } else {
            self.finished = false;
            let next_step_time = self.started_at + frame_dur.mul_f64((step + 1) as f64);
            (next_frame, changed, Some(next_step_time))
        }
    }
}
