use std::borrow::Borrow;
use std::fmt::{self, Write as _};
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::Arc;

const INLINE_CAP: usize = 22;

/// Zero-allocation-friendly string representation for `Text` and short labels.
///
/// Layout is compact (24 bytes on 64-bit platforms):
/// - `&'static str`: zero allocations;
/// - inline stack buffer (up to 22 UTF-8 bytes): zero heap allocations for dynamic short labels
///   like `"72%"` or `"12:45"`;
/// - `Arc<str>`: shared reference counting for longer dynamic strings.
#[derive(Clone)]
pub struct SharedStr(SharedStrRepr);

#[derive(Clone)]
enum SharedStrRepr {
    Static(&'static str),
    Arc(Arc<str>),
    Inline { len: u8, buf: [u8; INLINE_CAP] },
}

impl SharedStr {
    /// Empty string without allocation.
    pub const EMPTY: Self = Self(SharedStrRepr::Static(""));

    /// Constructs a `SharedStr` from a `'static` string slice without allocation.
    #[inline]
    #[must_use]
    pub const fn from_static(s: &'static str) -> Self {
        Self(SharedStrRepr::Static(s))
    }

    /// Constructs a `SharedStr` from a borrowed slice, storing inline when `<= 22` bytes and
    /// falling back to `Arc<str>` otherwise.
    #[must_use]
    pub fn from_str_inline_or_arc(s: &str) -> Self {
        if s.is_empty() {
            return Self::EMPTY;
        }
        if s.len() <= INLINE_CAP {
            let mut buf = [0u8; INLINE_CAP];
            buf[..s.len()].copy_from_slice(s.as_bytes());
            Self(SharedStrRepr::Inline {
                len: s.len() as u8,
                buf,
            })
        } else {
            Self(SharedStrRepr::Arc(Arc::from(s)))
        }
    }

    /// Formats `args` into the 22-byte inline buffer with zero heap allocation when possible,
    /// spilling to an `Arc<str>` only if the formatted output exceeds 22 bytes.
    #[must_use]
    pub fn format_inline(args: fmt::Arguments<'_>) -> Self {
        struct InlineWriter {
            buf: [u8; INLINE_CAP],
            len: usize,
            overflow: Option<String>,
        }

        impl fmt::Write for InlineWriter {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                if let Some(ref mut ov) = self.overflow {
                    ov.push_str(s);
                    return Ok(());
                }
                let bytes = s.as_bytes();
                if self.len + bytes.len() <= INLINE_CAP {
                    self.buf[self.len..self.len + bytes.len()].copy_from_slice(bytes);
                    self.len += bytes.len();
                } else {
                    let mut out = String::with_capacity(self.len + bytes.len());
                    // SAFETY: Only valid UTF-8 chunks were written into `self.buf[..self.len]`.
                    let prefix = unsafe { std::str::from_utf8_unchecked(&self.buf[..self.len]) };
                    out.push_str(prefix);
                    out.push_str(s);
                    self.overflow = Some(out);
                }
                Ok(())
            }
        }

        if let Some(static_str) = args.as_str() {
            return Self::from_static(static_str);
        }

        let mut writer = InlineWriter {
            buf: [0u8; INLINE_CAP],
            len: 0,
            overflow: None,
        };
        let _ = writer.write_fmt(args);
        match writer.overflow {
            Some(s) => Self(SharedStrRepr::Arc(Arc::from(s))),
            None => Self(SharedStrRepr::Inline {
                len: writer.len as u8,
                buf: writer.buf,
            }),
        }
    }

    /// Returns the string slice.
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        match &self.0 {
            SharedStrRepr::Static(s) => s,
            SharedStrRepr::Arc(s) => s.as_ref(),
            SharedStrRepr::Inline { len, buf } => {
                // SAFETY: Only valid UTF-8 bytes up to `len <= INLINE_CAP` are stored.
                unsafe { std::str::from_utf8_unchecked(&buf[..usize::from(*len)]) }
            }
        }
    }

    /// Returns `true` if this string is stored inline on the stack (no heap pointer).
    #[inline]
    #[must_use]
    pub const fn is_inline(&self) -> bool {
        matches!(self.0, SharedStrRepr::Inline { .. })
    }

    /// Returns `true` if this string is backed by `&'static str`.
    #[inline]
    #[must_use]
    pub const fn is_static(&self) -> bool {
        matches!(self.0, SharedStrRepr::Static(_))
    }
}

impl Default for SharedStr {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl Deref for SharedStr {
    type Target = str;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl AsRef<str> for SharedStr {
    #[inline]
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for SharedStr {
    #[inline]
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq for SharedStr {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for SharedStr {}

impl Hash for SharedStr {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl fmt::Debug for SharedStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl fmt::Display for SharedStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_str(), f)
    }
}

impl From<&'static str> for SharedStr {
    #[inline]
    fn from(value: &'static str) -> Self {
        Self::from_static(value)
    }
}

impl From<String> for SharedStr {
    #[inline]
    fn from(value: String) -> Self {
        if value.len() <= INLINE_CAP {
            Self::from_str_inline_or_arc(&value)
        } else {
            Self(SharedStrRepr::Arc(Arc::from(value)))
        }
    }
}

impl From<Arc<str>> for SharedStr {
    #[inline]
    fn from(value: Arc<str>) -> Self {
        Self(SharedStrRepr::Arc(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_str_is_24_bytes_and_inline_zero_alloc() {
        assert_eq!(std::mem::size_of::<SharedStr>(), 24);
        let static_s = SharedStr::from("CPU");
        assert!(static_s.is_static());

        let val = std::hint::black_box(72);
        let formatted = SharedStr::format_inline(format_args!("{val}% ok"));
        assert!(formatted.is_inline());
        assert_eq!(formatted.as_str(), "72% ok");

        let long_num = std::hint::black_box(123);
        let long = SharedStr::format_inline(format_args!(
            "this is definitely longer than twenty two bytes {long_num}"
        ));
        assert!(!long.is_inline());
        assert!(!long.is_static());
        assert!(long.starts_with("this is definitely"));
    }
}
