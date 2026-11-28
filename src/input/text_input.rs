use super::ime::{ImeEvent, ImePreedit};
use super::keys::{Key, KeyEvent, Modifiers};

/// Result returned by [`TextInputState::handle_key`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextInputOutcome {
    /// Text content changed (`insert`, `backspace`, `delete`, etc.).
    Edited,
    /// Cursor or selection changed without modifying text.
    CursorMoved,
    /// User pressed `Enter`.
    Submitted,
    /// User pressed `Escape`.
    Cancelled,
    /// Key event was not handled by the text buffer.
    Unchanged,
}

impl TextInputOutcome {
    /// Returns `true` when the text content was modified (for triggering search/filtering).
    #[inline]
    #[must_use]
    pub const fn text_changed(self) -> bool {
        matches!(self, Self::Edited)
    }

    /// Returns `true` when text, cursor, or selection changed and the UI should redraw.
    #[inline]
    #[must_use]
    pub const fn needs_redraw(self) -> bool {
        matches!(self, Self::Edited | Self::CursorMoved)
    }
}

/// Headless UTF-8 single-line text input state controller.
///
/// Manages text storage, char-boundary cursor movement, selection, word-level editing, and
/// masking without imposing any visual styling or element layout. Intended to live inside
/// `Application::State` and be driven by [`KeyEvent`] callbacks.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextInputState {
    text: String,
    cursor: usize,
    anchor: Option<usize>,
    max_len_chars: Option<usize>,
    preedit: Option<ImePreedit>,
    external_change_seq: u64,
}

impl TextInputState {
    /// Creates an empty text input buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a text input initialized with `text` and cursor placed at the end.
    #[must_use]
    pub fn with_text(text: impl Into<String>) -> Self {
        let mut s = Self::new();
        s.set_text(text);
        s
    }

    /// Sets an optional maximum scalar character limit (`None` = unlimited).
    #[must_use]
    pub fn with_max_chars(mut self, max_chars: usize) -> Self {
        self.max_len_chars = Some(max_chars);
        self.truncate_to_max_chars();
        self
    }

    /// Full text contents.
    #[inline]
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns `true` when the buffer is empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Length in Unicode scalar values.
    #[must_use]
    pub fn len_chars(&self) -> usize {
        self.text.chars().count()
    }

    /// Current cursor position in UTF-8 byte offset (`0..=self.text().len()`).
    #[inline]
    #[must_use]
    pub const fn cursor_bytes(&self) -> usize {
        self.cursor
    }

    /// Current selection anchor position in UTF-8 byte offset (equals [`Self::cursor_bytes`] when
    /// no selection is active).
    #[inline]
    #[must_use]
    pub const fn anchor_bytes(&self) -> usize {
        match self.anchor {
            Some(a) => a,
            None => self.cursor,
        }
    }

    /// Monotonic counter incremented when buffer text, cursor, or selection changes externally
    /// (outside Wayland IME updates).
    #[inline]
    #[must_use]
    pub const fn external_change_seq(&self) -> u64 {
        self.external_change_seq
    }

    /// Active un-committed Wayland IME composition string (`preedit`), if any.
    #[inline]
    #[must_use]
    pub const fn preedit(&self) -> Option<&ImePreedit> {
        self.preedit.as_ref()
    }

    /// Returns `true` when an active IME preedit string is currently displayed.
    #[inline]
    #[must_use]
    pub const fn has_preedit(&self) -> bool {
        self.preedit.is_some()
    }

    /// Returns `(before_cursor, active_preedit, after_cursor)` slices so custom UI can render inline
    /// composition text with an underline or cursor highlight without allocating.
    #[must_use]
    pub fn display_segments(&self) -> (&str, &str, &str) {
        let pre = self.preedit.as_ref().map_or("", |p| p.text.as_str());
        (self.before_cursor(), pre, self.after_cursor())
    }

    /// Returns the combined text with any active IME preedit inserted at the cursor.
    #[must_use]
    pub fn display_text(&self) -> String {
        let (before, pre, after) = self.display_segments();
        if pre.is_empty() {
            return self.text.clone();
        }
        let mut out = String::with_capacity(before.len() + pre.len() + after.len());
        out.push_str(before);
        out.push_str(pre);
        out.push_str(after);
        out
    }

    /// Returns the cursor byte offset inside [`Self::display_text`], placing it inside/after active
    /// IME preedit text when present.
    #[must_use]
    pub fn display_cursor_bytes(&self) -> usize {
        match self.preedit {
            Some(ref p) => {
                let in_preedit = p.cursor_end.or(p.cursor_begin).unwrap_or(p.text.len());
                self.cursor + in_preedit.min(p.text.len())
            }
            None => self.cursor,
        }
    }

    /// Current cursor position measured in Unicode scalar values.
    #[must_use]
    pub fn cursor_chars(&self) -> usize {
        self.text[..self.cursor].chars().count()
    }

    /// Text slice before the cursor (useful for cursor rendering in custom UI).
    #[inline]
    #[must_use]
    pub fn before_cursor(&self) -> &str {
        &self.text[..self.cursor]
    }

    /// Text slice after the cursor.
    #[inline]
    #[must_use]
    pub fn after_cursor(&self) -> &str {
        &self.text[self.cursor..]
    }

    /// Active normalized selection byte range `(start, end)`, or `None` if collapsed.
    #[must_use]
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        if anchor == self.cursor {
            None
        } else if anchor < self.cursor {
            Some((anchor, self.cursor))
        } else {
            Some((self.cursor, anchor))
        }
    }

    /// Currently selected text slice, if any.
    #[must_use]
    pub fn selected_text(&self) -> Option<&str> {
        let (start, end) = self.selection_range()?;
        Some(&self.text[start..end])
    }

    /// Returns a masked representation (e.g. `'*'` or `'•'`) matching the scalar character count.
    #[must_use]
    pub fn masked(&self, mask_char: char) -> String {
        let count = self.len_chars();
        let mut out = String::with_capacity(count * mask_char.len_utf8());
        for _ in 0..count {
            out.push(mask_char);
        }
        out
    }

    /// Replaces the entire buffer contents, resets selection, clears preedit, and moves the cursor
    /// to the end.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = sanitize_single_line(&text.into());
        self.truncate_to_max_chars();
        self.cursor = self.text.len();
        self.anchor = None;
        self.preedit = None;
        self.bump_external_change_seq();
    }

    /// Clears the buffer, preedit, and cursor/selection. Returns `true` if any state cleared.
    pub fn clear(&mut self) -> bool {
        let changed = !self.text.is_empty() || self.preedit.take().is_some();
        self.text.clear();
        self.cursor = 0;
        self.anchor = None;
        self.finish_external_edit(changed)
    }

    /// Selects the entire buffer and returns `true` if cursor/selection changed.
    pub fn select_all(&mut self) -> bool {
        if self.text.is_empty() {
            return false;
        }
        let changed = self.selection_range() != Some((0, self.text.len()));
        self.anchor = Some(0);
        self.cursor = self.text.len();
        self.finish_external_edit(changed)
    }

    /// Collapses any active selection at the current cursor position without modifying text.
    /// Returns `true` if a selection was cleared.
    pub fn clear_selection(&mut self) -> bool {
        let had = self.anchor.take().is_some_and(|a| a != self.cursor);
        self.finish_external_edit(had)
    }

    /// Deletes the active selection range if one exists. Returns `true` if text was removed.
    pub fn delete_selection(&mut self) -> bool {
        let deleted = self.delete_selection_internal();
        self.finish_external_edit(deleted)
    }

    /// Moves the cursor to `byte_idx` (snapped down to a valid UTF-8 char boundary).
    pub fn set_cursor_bytes(&mut self, byte_idx: usize, keep_selection: bool) -> bool {
        let clamped = snap_char_boundary(&self.text, byte_idx);
        let prev_cursor = self.cursor;
        let prev_anchor = self.anchor;
        self.update_cursor_target(clamped, keep_selection);
        let changed = self.cursor != prev_cursor || self.anchor != prev_anchor;
        self.finish_external_edit(changed)
    }

    /// Inserts `s` (stripping control chars/newlines) at the cursor, replacing any active selection.
    pub fn insert_str(&mut self, s: &str) -> bool {
        let changed = self.insert_str_internal(s);
        self.finish_external_edit(changed)
    }

    fn insert_str_internal(&mut self, s: &str) -> bool {
        let clean = sanitize_single_line(s);
        let had_selection = self.delete_selection_internal();
        if clean.is_empty() {
            return had_selection;
        }

        let to_insert = if let Some(max_chars) = self.max_len_chars {
            let remaining = max_chars.saturating_sub(self.len_chars());
            if remaining == 0 {
                return had_selection;
            }
            let end_byte = clean
                .char_indices()
                .nth(remaining)
                .map_or(clean.len(), |(idx, _)| idx);
            &clean[..end_byte]
        } else {
            clean.as_str()
        };

        if to_insert.is_empty() {
            return had_selection;
        }
        self.text.insert_str(self.cursor, to_insert);
        self.cursor += to_insert.len();
        self.anchor = None;
        true
    }

    /// Deletes the active selection or the character immediately before the cursor.
    pub fn backspace(&mut self) -> bool {
        if self.delete_selection_internal() {
            return self.finish_external_edit(true);
        }
        if self.cursor == 0 {
            return false;
        }
        let prev = prev_char_boundary(&self.text, self.cursor);
        self.text.drain(prev..self.cursor);
        self.cursor = prev;
        self.finish_external_edit(true)
    }

    /// Deletes the active selection or the character immediately after the cursor.
    pub fn delete_forward(&mut self) -> bool {
        if self.delete_selection_internal() {
            return self.finish_external_edit(true);
        }
        if self.cursor >= self.text.len() {
            return false;
        }
        let next = next_char_boundary(&self.text, self.cursor);
        self.text.drain(self.cursor..next);
        self.finish_external_edit(true)
    }

    /// Deletes from the start of the previous word to the cursor (or deletes the active selection).
    pub fn delete_word_backward(&mut self) -> bool {
        if self.delete_selection_internal() {
            return self.finish_external_edit(true);
        }
        if self.cursor == 0 {
            return false;
        }
        let start = prev_word_boundary(&self.text, self.cursor);
        self.text.drain(start..self.cursor);
        self.cursor = start;
        self.finish_external_edit(true)
    }

    /// Deletes from the cursor to the end of the next word (or deletes the active selection).
    pub fn delete_word_forward(&mut self) -> bool {
        if self.delete_selection_internal() {
            return self.finish_external_edit(true);
        }
        if self.cursor >= self.text.len() {
            return false;
        }
        let end = next_word_boundary(&self.text, self.cursor);
        self.text.drain(self.cursor..end);
        self.finish_external_edit(true)
    }

    /// Moves the cursor one character left.
    pub fn move_left(&mut self, extend_selection: bool) -> bool {
        if !extend_selection && let Some((start, _)) = self.selection_range() {
            self.anchor = None;
            self.cursor = start;
            return true;
        }
        let target = prev_char_boundary(&self.text, self.cursor);
        self.apply_cursor_move(target, extend_selection)
    }

    /// Moves the cursor one character right.
    pub fn move_right(&mut self, extend_selection: bool) -> bool {
        if !extend_selection && let Some((_, end)) = self.selection_range() {
            self.anchor = None;
            self.cursor = end;
            return true;
        }
        let target = next_char_boundary(&self.text, self.cursor);
        self.apply_cursor_move(target, extend_selection)
    }

    /// Moves the cursor to the start of the previous word.
    pub fn move_word_left(&mut self, extend_selection: bool) -> bool {
        let target = prev_word_boundary(&self.text, self.cursor);
        self.apply_cursor_move(target, extend_selection)
    }

    /// Moves the cursor to the end of the next word.
    pub fn move_word_right(&mut self, extend_selection: bool) -> bool {
        let target = next_word_boundary(&self.text, self.cursor);
        self.apply_cursor_move(target, extend_selection)
    }

    /// Moves the cursor to the start of the buffer.
    pub fn move_home(&mut self, extend_selection: bool) -> bool {
        self.apply_cursor_move(0, extend_selection)
    }

    /// Moves the cursor to the end of the buffer.
    pub fn move_end(&mut self, extend_selection: bool) -> bool {
        self.apply_cursor_move(self.text.len(), extend_selection)
    }

    /// Applies a keyboard [`KeyEvent`] (`utf8` printable characters, editing, cursor motion,
    /// `Enter`, `Escape`, and `Ctrl+A` / `Ctrl+Backspace` / `Ctrl+Delete` / `Ctrl+Left` /
    /// `Ctrl+Right`).
    ///
    /// Note: Clipboard paste (`Ctrl+V`) / copy (`Ctrl+C`) requires querying the Wayland clipboard
    /// externally and passing the string to [`Self::insert_str`] or reading [`Self::selected_text`].
    pub fn handle_key(&mut self, ev: &KeyEvent) -> TextInputOutcome {
        let ctrl = ev.modifiers.contains(Modifiers::CTRL);
        let shift = ev.modifiers.contains(Modifiers::SHIFT);
        let alt_or_super = ev.modifiers.intersects(Modifiers::ALT | Modifiers::SUPER);

        if alt_or_super {
            return TextInputOutcome::Unchanged;
        }

        match ev.key {
            Key::Enter if !ctrl => TextInputOutcome::Submitted,
            Key::Escape if !ctrl => TextInputOutcome::Cancelled,
            Key::Backspace => {
                let changed = if ctrl {
                    self.delete_word_backward()
                } else {
                    self.backspace()
                };
                if changed {
                    TextInputOutcome::Edited
                } else {
                    TextInputOutcome::Unchanged
                }
            }
            Key::Delete => {
                let changed = if ctrl {
                    self.delete_word_forward()
                } else {
                    self.delete_forward()
                };
                if changed {
                    TextInputOutcome::Edited
                } else {
                    TextInputOutcome::Unchanged
                }
            }
            Key::Left => {
                let moved = if ctrl {
                    self.move_word_left(shift)
                } else {
                    self.move_left(shift)
                };
                if moved {
                    TextInputOutcome::CursorMoved
                } else {
                    TextInputOutcome::Unchanged
                }
            }
            Key::Right => {
                let moved = if ctrl {
                    self.move_word_right(shift)
                } else {
                    self.move_right(shift)
                };
                if moved {
                    TextInputOutcome::CursorMoved
                } else {
                    TextInputOutcome::Unchanged
                }
            }
            Key::Home => {
                if self.move_home(shift) {
                    TextInputOutcome::CursorMoved
                } else {
                    TextInputOutcome::Unchanged
                }
            }
            Key::End => {
                if self.move_end(shift) {
                    TextInputOutcome::CursorMoved
                } else {
                    TextInputOutcome::Unchanged
                }
            }
            Key::A if ctrl => {
                if self.select_all() {
                    TextInputOutcome::CursorMoved
                } else {
                    TextInputOutcome::Unchanged
                }
            }
            _ if !ctrl => {
                if let Some(ref utf8) = ev.utf8 {
                    if self.insert_str(utf8.as_str()) {
                        return TextInputOutcome::Edited;
                    }
                } else if ev.key == Key::Space && self.insert_str(" ") {
                    return TextInputOutcome::Edited;
                }
                TextInputOutcome::Unchanged
            }
            _ => TextInputOutcome::Unchanged,
        }
    }

    /// Applies a Wayland IME [`ImeEvent`] (`zwp_text_input_v3`) in protocol evaluation order
    /// without bumping [`Self::external_change_seq`].
    pub fn handle_ime(&mut self, ev: &ImeEvent) -> TextInputOutcome {
        match ev {
            ImeEvent::Enter => TextInputOutcome::Unchanged,
            ImeEvent::Leave => {
                if self.preedit.take().is_some() {
                    TextInputOutcome::Edited
                } else {
                    TextInputOutcome::Unchanged
                }
            }
            ImeEvent::Update {
                delete_before,
                delete_after,
                commit,
                preedit,
            } => {
                let prev_text_len = self.text.len();
                let prev_cursor = self.cursor;
                let prev_anchor = self.anchor;
                let prev_preedit = self.preedit.take();

                let mut text_changed = false;

                // Protocol order step 1: old preedit removed (taken above).
                // Step 2: delete surrounding text around cursor at valid UTF-8 boundaries.
                let del_b = *delete_before as usize;
                let del_a = *delete_after as usize;
                if del_b > 0 || del_a > 0 {
                    let start = snap_char_boundary(&self.text, self.cursor.saturating_sub(del_b));
                    let end = snap_char_boundary_up(
                        &self.text,
                        self.cursor.saturating_add(del_a).min(self.text.len()),
                    );
                    if start < end {
                        self.text.drain(start..end);
                        self.cursor = start;
                        self.anchor = None;
                        text_changed = true;
                    }
                }

                // Step 3: insert commit text with cursor at end (replacing any active selection).
                if let Some(commit_str) = commit
                    && !commit_str.is_empty()
                    && self.insert_str_internal(commit_str)
                {
                    text_changed = true;
                }

                // Step 4: apply new preedit if non-empty.
                self.preedit = preedit
                    .as_ref()
                    .filter(|p| !p.text.is_empty())
                    .map(|p| {
                        let clean = sanitize_single_line(&p.text);
                        ImePreedit {
                            cursor_begin: p.cursor_begin.map(|b| snap_char_boundary(&clean, b)),
                            cursor_end: p.cursor_end.map(|e| snap_char_boundary(&clean, e)),
                            text: clean,
                        }
                    })
                    .filter(|p| !p.text.is_empty());

                if text_changed
                    || self.text.len() != prev_text_len
                    || self.cursor != prev_cursor
                    || self.anchor != prev_anchor
                    || prev_preedit != self.preedit
                {
                    TextInputOutcome::Edited
                } else {
                    TextInputOutcome::Unchanged
                }
            }
        }
    }

    fn bump_external_change_seq(&mut self) {
        self.external_change_seq = self.external_change_seq.wrapping_add(1);
    }

    fn finish_external_edit(&mut self, changed: bool) -> bool {
        if changed {
            self.bump_external_change_seq();
        }
        changed
    }

    fn delete_selection_internal(&mut self) -> bool {
        let Some((start, end)) = self.selection_range() else {
            self.anchor = None;
            return false;
        };
        self.text.drain(start..end);
        self.cursor = start;
        self.anchor = None;
        true
    }

    fn apply_cursor_move(&mut self, target: usize, extend_selection: bool) -> bool {
        let prev_cursor = self.cursor;
        let prev_anchor = self.anchor;
        self.update_cursor_target(target, extend_selection);
        let changed = self.cursor != prev_cursor || self.anchor != prev_anchor;
        self.finish_external_edit(changed)
    }

    fn update_cursor_target(&mut self, target: usize, extend_selection: bool) {
        if extend_selection {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
            self.cursor = target;
            if self.anchor == Some(self.cursor) {
                self.anchor = None;
            }
        } else {
            self.anchor = None;
            self.cursor = target;
        }
    }

    fn truncate_to_max_chars(&mut self) {
        if let Some(max_chars) = self.max_len_chars
            && let Some((cut, _)) = self.text.char_indices().nth(max_chars)
        {
            self.text.truncate(cut);
            self.cursor = self.cursor.min(self.text.len());
            self.anchor = self.anchor.map(|a| a.min(self.text.len()));
        }
    }
}

fn sanitize_single_line(raw: &str) -> String {
    raw.chars().filter(|c| !c.is_control()).collect()
}

pub(crate) fn snap_char_boundary(s: &str, idx: usize) -> usize {
    let mut pos = idx.min(s.len());
    while pos > 0 && !s.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

pub(crate) fn snap_char_boundary_up(s: &str, idx: usize) -> usize {
    let mut pos = idx.min(s.len());
    while pos < s.len() && !s.is_char_boundary(pos) {
        pos += 1;
    }
    pos
}

fn prev_char_boundary(s: &str, idx: usize) -> usize {
    let clamped = snap_char_boundary(s, idx);
    s[..clamped]
        .char_indices()
        .next_back()
        .map_or(0, |(pos, _)| pos)
}

fn next_char_boundary(s: &str, idx: usize) -> usize {
    let clamped = snap_char_boundary(s, idx);
    match s[clamped..].chars().next() {
        Some(ch) => clamped + ch.len_utf8(),
        None => s.len(),
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn prev_word_boundary(s: &str, idx: usize) -> usize {
    let clamped = snap_char_boundary(s, idx);
    let mut iter = s[..clamped].char_indices().rev().peekable();
    while let Some(&(_, c)) = iter.peek() {
        if is_word_char(c) {
            break;
        }
        iter.next();
    }
    let mut boundary = iter.peek().map_or(0, |&(pos, _)| pos);
    for (pos, c) in iter {
        if is_word_char(c) {
            boundary = pos;
        } else {
            break;
        }
    }
    boundary
}

fn next_word_boundary(s: &str, idx: usize) -> usize {
    let clamped = snap_char_boundary(s, idx);
    let mut iter = s[clamped..].char_indices().peekable();
    while let Some(&(_, c)) = iter.peek() {
        if is_word_char(c) {
            break;
        }
        iter.next();
    }
    while let Some(&(rel, c)) = iter.peek() {
        if !is_word_char(c) {
            return clamped + rel;
        }
        iter.next();
    }
    s.len()
}

#[cfg(test)]
mod tests {
    use crate::text::SharedStr;

    use super::*;

    fn key(k: Key, modifiers: Modifiers, utf8: Option<&str>) -> KeyEvent {
        KeyEvent {
            key: k,
            modifiers,
            repeat: false,
            utf8: utf8.map(SharedStr::from_str_inline_or_arc),
        }
    }

    #[test]
    fn text_input_utf8_editing_cursor_word_ops_and_selection() {
        let mut input = TextInputState::new();
        assert_eq!(
            input.handle_key(&key(Key::H, Modifiers::empty(), Some("h"))),
            TextInputOutcome::Edited
        );
        assert_eq!(
            input.handle_key(&key(Key::I, Modifiers::empty(), Some("i "))),
            TextInputOutcome::Edited
        );
        // UTF-8 multi-byte chars are inserted and deleted cleanly at scalar boundaries.
        assert_eq!(
            input.handle_key(&key(Key::Other(0), Modifiers::empty(), Some("日本語"))),
            TextInputOutcome::Edited
        );
        assert_eq!(input.text(), "hi 日本語");
        assert_eq!(input.len_chars(), 6);
        assert_eq!(input.cursor_chars(), 6);

        // Control chars leaked in XKB utf8 for Backspace/Enter must not be inserted as text.
        assert_eq!(
            input.handle_key(&key(Key::Backspace, Modifiers::empty(), Some("\u{8}"))),
            TextInputOutcome::Edited
        );
        assert_eq!(input.text(), "hi 日本");
        assert_eq!(input.before_cursor(), "hi 日本");
        assert_eq!(input.after_cursor(), "");

        // Move left 2 chars ("日本") -> cursor between "hi " and "日本".
        assert_eq!(
            input.handle_key(&key(Key::Left, Modifiers::empty(), None)),
            TextInputOutcome::CursorMoved
        );
        assert_eq!(
            input.handle_key(&key(Key::Left, Modifiers::empty(), None)),
            TextInputOutcome::CursorMoved
        );
        assert_eq!(input.before_cursor(), "hi ");
        assert_eq!(input.after_cursor(), "日本");

        // Ctrl+Backspace deletes word before cursor ("hi ").
        assert_eq!(
            input.handle_key(&key(Key::Backspace, Modifiers::CTRL, None)),
            TextInputOutcome::Edited
        );
        assert_eq!(input.text(), "日本");
        assert_eq!(input.before_cursor(), "");

        // Ctrl+A selects all, typing replaces selection.
        assert_eq!(
            input.handle_key(&key(Key::A, Modifiers::CTRL, Some("\u{1}"))),
            TextInputOutcome::CursorMoved
        );
        assert_eq!(input.selected_text(), Some("日本"));
        assert_eq!(
            input.handle_key(&key(Key::W, Modifiers::empty(), Some("wifi"))),
            TextInputOutcome::Edited
        );
        assert_eq!(input.text(), "wifi");
        assert_eq!(input.masked('•'), "••••");
    }

    #[test]
    fn text_input_respects_max_chars_navigation_submit_and_cancel() {
        let mut input = TextInputState::new().with_max_chars(5);
        assert!(input.insert_str("abcdefg"));
        assert_eq!(input.text(), "abcde");

        assert_eq!(
            input.handle_key(&key(Key::Up, Modifiers::empty(), None)),
            TextInputOutcome::Unchanged
        );
        assert_eq!(
            input.handle_key(&key(Key::Enter, Modifiers::empty(), Some("\r"))),
            TextInputOutcome::Submitted
        );
        assert_eq!(
            input.handle_key(&key(Key::Escape, Modifiers::empty(), Some("\u{1b}"))),
            TextInputOutcome::Cancelled
        );
        assert_eq!(
            input.handle_key(&key(Key::Space, Modifiers::empty(), None)),
            TextInputOutcome::Unchanged
        );
    }

    #[test]
    fn text_input_ime_preedit_commit_delete_surrounding_and_leave() {
        let mut input = TextInputState::with_text("hello 世界!");
        let seq_init = input.external_change_seq();
        // Move cursor left 1 char ('!') so cursor sits after "世界".
        assert_eq!(
            input.handle_key(&key(Key::Left, Modifiers::empty(), None)),
            TextInputOutcome::CursorMoved
        );
        assert!(input.external_change_seq() > seq_init);
        let seq_before_ime = input.external_change_seq();

        // Preedit at cursor does not bump external_change_seq and preserves buffer text.
        assert_eq!(
            input.handle_ime(&ImeEvent::Update {
                delete_before: 0,
                delete_after: 0,
                commit: None,
                preedit: Some(ImePreedit {
                    text: "にほん".into(),
                    cursor_begin: Some(0),
                    cursor_end: Some("にほん".len()),
                }),
            }),
            TextInputOutcome::Edited
        );
        assert_eq!(input.text(), "hello 世界!");
        assert_eq!(input.display_segments(), ("hello 世界", "にほん", "!"));
        assert_eq!(input.display_text(), "hello 世界にほん!");
        assert_eq!(
            input.display_cursor_bytes(),
            input.cursor_bytes() + "にほん".len()
        );
        assert_eq!(input.external_change_seq(), seq_before_ime);

        // Delete 4 surrounding bytes before cursor (snaps safely through multi-byte '界' (3 bytes) -> removes "世界" 6 bytes, or wait: cursor_before="hello 世界": "界" starts at offset len-3, len-4 lands inside '世' -> snaps down to '世' start!).
        assert_eq!(
            input.handle_ime(&ImeEvent::Update {
                delete_before: 6, // exact UTF-8 byte length of "世界"
                delete_after: 0,
                commit: Some("日本".into()),
                preedit: None,
            }),
            TextInputOutcome::Edited
        );
        assert_eq!(input.text(), "hello 日本!");
        assert!(!input.has_preedit());
        assert_eq!(input.external_change_seq(), seq_before_ime);

        // Leave clears active preedit and triggers redraw cleanly.
        assert_eq!(
            input.handle_ime(&ImeEvent::Update {
                delete_before: 0,
                delete_after: 0,
                commit: None,
                preedit: Some(ImePreedit {
                    text: "かな".into(),
                    cursor_begin: None,
                    cursor_end: None,
                }),
            }),
            TextInputOutcome::Edited
        );
        assert!(input.has_preedit());
        assert_eq!(input.handle_ime(&ImeEvent::Leave), TextInputOutcome::Edited);
        assert!(!input.has_preedit());
        assert_eq!(
            input.handle_ime(&ImeEvent::Leave),
            TextInputOutcome::Unchanged
        );
    }
}
