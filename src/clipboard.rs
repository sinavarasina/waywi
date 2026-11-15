use crate::arena::FrameSharedFn;
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

/// Target Wayland selection buffer monitored or updated via data-control protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClipboardSelection {
    /// Standard copy/paste clipboard selection.
    Clipboard,
    /// Middle-click primary selection buffer.
    Primary,
}

/// Snapshot of a Wayland clipboard or primary-selection update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardSnapshot {
    /// Selection buffer that produced this snapshot.
    pub selection: ClipboardSelection,
    /// All MIME types advertised by the selection offer.
    pub mime_types: Vec<String>,
    /// Decoded UTF-8 text if `primary_mime` carries UTF-8 text.
    pub text: Option<String>,
    /// Raw payload bytes read for `primary_mime` (capped at 4 MiB).
    pub bytes: Vec<u8>,
    /// Preferred MIME type selected from `mime_types`.
    pub primary_mime: Option<String>,
    /// `true` when the selection was cleared (`NULL` offer).
    pub cleared: bool,
}

/// Clipboard change or error event delivered to [`Clipboard`] subscriptions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardEvent {
    /// A selection changed or was cleared.
    Changed(ClipboardSnapshot),
    /// Non-fatal pipe transfer or protocol error.
    Error(String),
}

/// Wayland clipboard and primary-selection subscription builder.
pub struct Clipboard;

impl Clipboard {
    /// Subscribes to both standard clipboard and primary-selection changes.
    pub fn subscribe<M: 'static>(map: impl Fn(ClipboardEvent) -> M + 'static) -> Subscription<M> {
        Self::build(None, map)
    }

    /// Subscribes only to changes for a specific [`ClipboardSelection`] buffer.
    pub fn subscribe_selection<M: 'static>(
        selection: ClipboardSelection,
        map: impl Fn(ClipboardEvent) -> M + 'static,
    ) -> Subscription<M> {
        Self::build(Some(selection), map)
    }

    fn build<M: 'static>(
        selection: Option<ClipboardSelection>,
        map: impl Fn(ClipboardEvent) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::Clipboard {
                key: ("wayland:clipboard", selection).into_subscription_key(),
                selection,
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Selects the most useful MIME type to transfer eagerly from a clipboard offer.
#[must_use]
pub fn select_preferred_mime(mimes: &[String]) -> Option<&str> {
    const PRIORITY: &[&str] = &[
        "text/plain;charset=utf-8",
        "UTF8_STRING",
        "text/plain",
        "TEXT",
        "image/png",
        "image/webp",
        "image/jpeg",
    ];
    for candidate in PRIORITY {
        if let Some(found) = mimes.iter().find(|m| m.eq_ignore_ascii_case(candidate)) {
            return Some(found.as_str());
        }
    }
    if let Some(text_any) = mimes.iter().find(|m| m.starts_with("text/")) {
        return Some(text_any.as_str());
    }
    if let Some(img_any) = mimes.iter().find(|m| m.starts_with("image/")) {
        return Some(img_any.as_str());
    }
    mimes.first().map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferred_mime_prioritizes_utf8_text_then_images() {
        let mimes = vec![
            "text/html".to_owned(),
            "text/plain".to_owned(),
            "text/plain;charset=utf-8".to_owned(),
        ];
        assert_eq!(
            select_preferred_mime(&mimes),
            Some("text/plain;charset=utf-8")
        );

        let img_mimes = vec![
            "application/octet-stream".to_owned(),
            "image/png".to_owned(),
        ];
        assert_eq!(select_preferred_mime(&img_mimes), Some("image/png"));
        assert_eq!(select_preferred_mime(&[]), None);
    }
}
