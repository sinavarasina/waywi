use bitflags::bitflags;

use crate::geometry::Rect as GeoRect;

use super::text_input::{TextInputState, snap_char_boundary, snap_char_boundary_up};

/// Maximum UTF-8 byte length permitted by `zwp_text_input_v3.set_surrounding_text`.
pub const MAX_SURROUNDING_BYTES: usize = 4000;

bitflags! {
    /// Content behavior hints corresponding to `zwp_text_input_v3::content_hint`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct ImeContentHint: u32 {
        /// Suggest word completions.
        const COMPLETION = 0x1;
        /// Suggest word corrections.
        const SPELLCHECK = 0x2;
        /// Switch to uppercase letters at the start of a sentence.
        const AUTO_CAPITALIZATION = 0x4;
        /// Prefer lowercase letters.
        const LOWERCASE = 0x8;
        /// Prefer uppercase letters.
        const UPPERCASE = 0x10;
        /// Prefer titlecase letters.
        const TITLECASE = 0x20;
        /// Characters should be hidden (e.g. passwords).
        const HIDDEN_TEXT = 0x40;
        /// Typed text should not be stored in dictionaries/history.
        const SENSITIVE_DATA = 0x80;
        /// Prefer Latin characters.
        const LATIN = 0x100;
        /// Multiline input context.
        const MULTILINE = 0x200;
        /// Client renders its own on-screen input widget.
        const ON_SCREEN_INPUT_PROVIDED = 0x400;
        /// Prefer omitting emoji input support.
        const NO_EMOJI = 0x800;
        /// Client displays preedit composition text inline.
        const PREEDIT_SHOWN = 0x1000;
    }
}

/// Primary semantic purpose of an active text field (`zwp_text_input_v3::content_purpose`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ImeContentPurpose {
    #[default]
    Normal,
    Alpha,
    Digits,
    Number,
    Phone,
    Url,
    Email,
    Name,
    Password,
    Pin,
    Date,
    Time,
    Datetime,
    Terminal,
}

impl ImeContentPurpose {
    /// Converts to the Wayland protocol wire discriminant (`0..=13`).
    #[must_use]
    pub const fn to_wire_u32(self) -> u32 {
        match self {
            Self::Normal => 0,
            Self::Alpha => 1,
            Self::Digits => 2,
            Self::Number => 3,
            Self::Phone => 4,
            Self::Url => 5,
            Self::Email => 6,
            Self::Name => 7,
            Self::Password => 8,
            Self::Pin => 9,
            Self::Date => 10,
            Self::Time => 11,
            Self::Datetime => 12,
            Self::Terminal => 13,
        }
    }
}

/// Surrounding plain text window capped cleanly to [`MAX_SURROUNDING_BYTES`] on UTF-8 boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImeSurroundingText {
    /// UTF-8 plain text around the cursor (excluding active preedit, `<= 4000` bytes).
    pub text: String,
    /// Cursor position as a UTF-8 byte offset inside `text`.
    pub cursor: usize,
    /// Selection anchor as a UTF-8 byte offset inside `text` (equals `cursor` when no selection).
    pub anchor: usize,
}

impl ImeSurroundingText {
    /// Extracts a protocol-safe surrounding-text slice of at most [`MAX_SURROUNDING_BYTES`] centered
    /// around the cursor and selection anchor, preserving UTF-8 code-point boundaries.
    #[must_use]
    pub fn from_slice(full_text: &str, cursor_byte: usize, anchor_byte: usize) -> Self {
        let cursor = snap_char_boundary(full_text, cursor_byte);
        let anchor = snap_char_boundary(full_text, anchor_byte);

        if full_text.len() <= MAX_SURROUNDING_BYTES {
            return Self {
                text: full_text.to_owned(),
                cursor,
                anchor,
            };
        }

        let sel_start = cursor.min(anchor);
        let sel_end = cursor.max(anchor);
        let sel_len = sel_end - sel_start;

        let (raw_start, raw_end) = if sel_len >= MAX_SURROUNDING_BYTES {
            let start = cursor.saturating_sub(MAX_SURROUNDING_BYTES / 2);
            let end = (start + MAX_SURROUNDING_BYTES).min(full_text.len());
            (start, end)
        } else {
            let remaining = MAX_SURROUNDING_BYTES - sel_len;
            let before_budget = remaining / 2;
            let mut start = sel_start.saturating_sub(before_budget);
            let mut end = (start + MAX_SURROUNDING_BYTES).min(full_text.len());
            if end == full_text.len() {
                start = end.saturating_sub(MAX_SURROUNDING_BYTES);
            }
            if end < sel_end {
                end = sel_end;
                start = end.saturating_sub(MAX_SURROUNDING_BYTES);
            }
            (start, end)
        };

        let start = snap_char_boundary_up(full_text, raw_start);
        let end = snap_char_boundary(full_text, raw_end).max(start);
        let slice = &full_text[start..end];
        let clamped_cursor =
            snap_char_boundary(slice, cursor.saturating_sub(start).min(slice.len()));
        let clamped_anchor =
            snap_char_boundary(slice, anchor.saturating_sub(start).min(slice.len()));

        Self {
            text: slice.to_owned(),
            cursor: clamped_cursor,
            anchor: clamped_anchor,
        }
    }
}

/// Declarative per-surface Wayland IME (`zwp_text_input_v3`) configuration.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ImeConfig {
    /// Whether text input should be enabled when this surface holds compositor text-input focus.
    pub enabled: bool,
    /// Plain text context around the cursor (excluding preedit).
    pub surrounding_text: Option<ImeSurroundingText>,
    /// Surface-local cursor bounding rectangle for positioning IME candidate popups.
    pub cursor_rect: Option<GeoRect>,
    /// Behavior hints sent via `set_content_type`.
    pub content_hint: ImeContentHint,
    /// Primary content purpose sent via `set_content_type`.
    pub content_purpose: ImeContentPurpose,
    /// Monotonic counter incremented when surrounding text/cursor changes externally (e.g. key input).
    pub external_change_seq: u64,
}

impl ImeConfig {
    /// Creates a disabled IME configuration (`zwp_text_input_v3.disable`).
    #[must_use]
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Creates an enabled IME configuration with default hints (`PREEDIT_SHOWN`).
    #[must_use]
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            content_hint: ImeContentHint::PREEDIT_SHOWN,
            ..Self::default()
        }
    }

    /// Populates `surrounding_text` and `external_change_seq` from a [`TextInputState`].
    #[must_use]
    pub fn from_text_input(mut self, input: &TextInputState) -> Self {
        self.surrounding_text = Some(ImeSurroundingText::from_slice(
            input.text(),
            input.cursor_bytes(),
            input.anchor_bytes(),
        ));
        self.external_change_seq = input.external_change_seq();
        self
    }

    /// Sets surrounding text explicitly.
    #[must_use]
    pub fn surrounding_text(mut self, surrounding: ImeSurroundingText) -> Self {
        self.surrounding_text = Some(surrounding);
        self
    }

    /// Sets the surface-local cursor rectangle used by compositors to place candidate popups.
    #[must_use]
    pub fn cursor_rect(mut self, rect: GeoRect) -> Self {
        self.cursor_rect = Some(rect);
        self
    }

    /// Sets `zwp_text_input_v3.set_content_type` hint bitflags.
    #[must_use]
    pub fn content_hint(mut self, hint: ImeContentHint) -> Self {
        self.content_hint = hint;
        self
    }

    /// Sets `zwp_text_input_v3.set_content_type` primary purpose.
    #[must_use]
    pub fn content_purpose(mut self, purpose: ImeContentPurpose) -> Self {
        self.content_purpose = purpose;
        self
    }

    /// Sets the external edit sequence counter used to report `change_cause = other`.
    #[must_use]
    pub fn external_change_seq(mut self, seq: u64) -> Self {
        self.external_change_seq = seq;
        self
    }
}

/// Active un-committed composition string (preedit) sent by the Wayland IME.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImePreedit {
    /// Preedit UTF-8 text displayed inline at the cursor.
    pub text: String,
    /// Selection/cursor start inside `text` in UTF-8 bytes (`None` when hidden by IME).
    pub cursor_begin: Option<usize>,
    /// Selection/cursor end inside `text` in UTF-8 bytes (`None` when hidden by IME).
    pub cursor_end: Option<usize>,
}

/// Normalized Wayland `zwp_text_input_v3` events delivered to [`Surface::on_ime`](crate::surface::Surface::on_ime).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImeEvent {
    /// Compositor granted text-input focus to this surface (`zwp_text_input_v3.enter`).
    Enter,
    /// Compositor revoked text-input focus (`zwp_text_input_v3.leave`); active preedit must clear.
    Leave,
    /// Atomic `zwp_text_input_v3.done` update evaluated in specification order:
    /// 1. remove any previous preedit,
    /// 2. delete `delete_before` / `delete_after` bytes around the cursor,
    /// 3. insert `commit` text at the cursor,
    /// 4. apply the new optional `preedit` at the updated cursor position.
    Update {
        delete_before: u32,
        delete_after: u32,
        commit: Option<String>,
        preedit: Option<ImePreedit>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surrounding_text_window_respects_max_bytes_and_utf8_boundaries() {
        let unit = "あいうえお"; // 15 bytes per unit
        let long_text = unit.repeat(400); // 6000 bytes > 4000
        let cursor = 3000;
        let anchor = 2985;

        let sur = ImeSurroundingText::from_slice(&long_text, cursor, anchor);
        assert!(sur.text.len() <= MAX_SURROUNDING_BYTES);
        assert!(sur.text.is_char_boundary(sur.cursor));
        assert!(sur.text.is_char_boundary(sur.anchor));
        assert!(sur.cursor <= sur.text.len());
        assert!(sur.anchor <= sur.text.len());
        assert_eq!(sur.cursor - sur.anchor, 15);
    }

    #[test]
    fn purpose_wire_discriminants_match_protocol() {
        assert_eq!(ImeContentPurpose::Normal.to_wire_u32(), 0);
        assert_eq!(ImeContentPurpose::Password.to_wire_u32(), 8);
        assert_eq!(ImeContentPurpose::Terminal.to_wire_u32(), 13);
    }
}
