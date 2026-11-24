use crate::arena::FrameSharedFn;
use crate::subscription::{Subscription, SubscriptionSpec};

/// Snapshot of XKB keyboard layouts and the currently active layout on the Wayland seat.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyboardLayoutInfo {
    /// Human-readable XKB layout names declared by the active keymap.
    pub layouts: Vec<String>,
    /// Zero-based index of the currently active XKB layout group.
    pub active_index: u32,
    /// Name of the currently active layout (or empty string if unknown).
    pub active_name: String,
}

impl KeyboardLayoutInfo {
    /// Constructs a layout snapshot from a layout list and active group index.
    #[must_use]
    pub fn from_layouts(layouts: Vec<String>, active_index: u32) -> Self {
        let active_name = layouts
            .get(active_index as usize)
            .cloned()
            .unwrap_or_default();
        Self {
            layouts,
            active_index,
            active_name,
        }
    }

    /// Returns a compact short label suitable for bars (e.g. `"English (US)"` -> `"US"`).
    #[must_use]
    pub fn short_label(&self) -> String {
        short_label_from_name(&self.active_name)
    }
}

/// Extracts a concise uppercase badge label from an XKB layout name.
#[must_use]
pub fn short_label_from_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Some(start) = trimmed.rfind('(')
        && let Some(end_rel) = trimmed[start + 1..].find(')')
    {
        let inner = trimmed[start + 1..start + 1 + end_rel].trim();
        if (2..=3).contains(&inner.len()) && inner.chars().all(|c| c.is_ascii_uppercase()) {
            return inner.to_owned();
        }
    }
    trimmed
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .take(2)
        .collect::<String>()
        .to_ascii_uppercase()
}

/// Subscription builder for Wayland seat XKB keyboard layout state changes.
pub struct KeyboardLayout;

impl KeyboardLayout {
    /// Subscribes to XKB keymap layout names and active group index updates on the Wayland seat.
    pub fn subscribe<M: 'static, F>(map: F) -> Subscription<M>
    where
        F: Fn(KeyboardLayoutInfo) -> M + 'static,
    {
        Subscription {
            spec: SubscriptionSpec::KeyboardLayout {
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Parses XKB layout group names from a Wayland `wl_keyboard::keymap` text string.
pub(crate) fn parse_xkb_layouts(keymap_str: &str) -> Vec<String> {
    let sanitized: String = keymap_str
        .trim_end_matches('\0')
        .chars()
        .filter(|&c| c != '\0')
        .collect();
    if sanitized.is_empty() {
        return Vec::new();
    }

    let ctx = xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS);
    let Some(keymap) = xkbcommon::xkb::Keymap::new_from_string(
        &ctx,
        sanitized,
        xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1,
        xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
    ) else {
        return Vec::new();
    };

    let count = keymap.num_layouts();
    (0..count)
        .map(|idx| keymap.layout_get_name(idx).to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_label_extracts_country_or_prefix() {
        assert_eq!(short_label_from_name("English (US)"), "US");
        assert_eq!(short_label_from_name("German"), "GE");
        assert_eq!(short_label_from_name("Japanese (OADG 109A)"), "JA");
        assert_eq!(short_label_from_name("English (UK)"), "UK");
        assert_eq!(short_label_from_name(""), "");
    }

    #[test]
    fn parse_xkb_layouts_handles_invalid_and_empty_safely() {
        assert!(parse_xkb_layouts("").is_empty());
        assert!(parse_xkb_layouts("\0\0\0").is_empty());
        assert!(parse_xkb_layouts("not valid\0keymap").is_empty());
    }

    #[test]
    fn keyboard_layout_info_tracks_active_name() {
        let info = KeyboardLayoutInfo::from_layouts(
            vec!["English (US)".to_owned(), "German".to_owned()],
            1,
        );
        assert_eq!(info.active_name, "German");
        assert_eq!(info.short_label(), "GE");

        let out_of_range = KeyboardLayoutInfo::from_layouts(vec!["English (US)".to_owned()], 3);
        assert_eq!(out_of_range.active_name, "");
    }
}
