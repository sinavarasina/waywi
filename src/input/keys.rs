use std::hash::Hash;

use bitflags::bitflags;

use crate::text::SharedStr;

/// Stable key used by the reconciler to match retained elements across frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ElementKey(pub u64);

impl ElementKey {
    #[must_use]
    pub fn from_hashable<T: Hash + 'static>(value: &T) -> Self {
        Self(crate::arena::hash_typed_key::<T, Self>(value, |k| k.0))
    }
}

/// Conversion trait allowing strings, integers, tuples, and domain IDs to serve as `.key(...)`.
pub trait IntoElementKey {
    fn into_element_key(self) -> ElementKey;
}

impl<T: Hash + 'static> IntoElementKey for T {
    fn into_element_key(self) -> ElementKey {
        ElementKey::from_hashable(&self)
    }
}

/// Internal generational node identifier protecting hit-testing and queued events from stale nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId {
    pub index: u32,
    pub generation: u32,
}

bitflags! {
    /// Keyboard modifier flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct Modifiers: u8 {
        const CTRL  = 1 << 0;
        const ALT   = 1 << 1;
        const SHIFT = 1 << 2;
        const SUPER = 1 << 3;
    }
}

impl Modifiers {
    pub const CONTROL: Self = Self::CTRL;
    pub const LOGO: Self = Self::SUPER;
}

/// Logical key identifier derived from XKB keysyms.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    A,
    B,
    C,
    D,
    E,
    FKey,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Digit(u8),
    Function(u8),
    Enter,
    Escape,
    Space,
    Tab,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Plus,
    Minus,
    Other(u32),
}

impl Key {
    /// Converts a raw XKB keysym (`u32`) into a WayWi [`Key`].
    #[must_use]
    pub fn from_keysym(keysym: u32) -> Self {
        match keysym {
            0x0061 | 0x0041 => Self::A,
            0x0062 | 0x0042 => Self::B,
            0x0063 | 0x0043 => Self::C,
            0x0064 | 0x0044 => Self::D,
            0x0065 | 0x0045 => Self::E,
            0x0066 | 0x0046 => Self::FKey,
            0x0067 | 0x0047 => Self::G,
            0x0068 | 0x0048 => Self::H,
            0x0069 | 0x0049 => Self::I,
            0x006a | 0x004a => Self::J,
            0x006b | 0x004b => Self::K,
            0x006c | 0x004c => Self::L,
            0x006d | 0x004d => Self::M,
            0x006e | 0x004e => Self::N,
            0x006f | 0x004f => Self::O,
            0x0070 | 0x0050 => Self::P,
            0x0071 | 0x0051 => Self::Q,
            0x0072 | 0x0052 => Self::R,
            0x0073 | 0x0053 => Self::S,
            0x0074 | 0x0054 => Self::T,
            0x0075 | 0x0055 => Self::U,
            0x0076 | 0x0056 => Self::V,
            0x0077 | 0x0057 => Self::W,
            0x0078 | 0x0058 => Self::X,
            0x0079 | 0x0059 => Self::Y,
            0x007a | 0x005a => Self::Z,
            0x0030..=0x0039 => Self::Digit((keysym - 0x0030) as u8),
            0xffbe..=0xffc9 => Self::Function((keysym - 0xffbe + 1) as u8),
            0xff0d | 0xff8d => Self::Enter,
            0xff1b => Self::Escape,
            0x0020 => Self::Space,
            0xff09 => Self::Tab,
            0xff08 => Self::Backspace,
            0xffff => Self::Delete,
            0xff52 => Self::Up,
            0xff54 => Self::Down,
            0xff51 => Self::Left,
            0xff53 => Self::Right,
            0xff50 => Self::Home,
            0xff57 => Self::End,
            0xff55 => Self::PageUp,
            0xff56 => Self::PageDown,
            0x002b | 0xffab => Self::Plus,
            0x002d | 0xffad => Self::Minus,
            other => Self::Other(other),
        }
    }
}

/// Combination of modifier state and a primary key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub modifiers: Modifiers,
    pub key: Key,
}

impl KeyChord {
    #[inline]
    #[must_use]
    pub const fn new(modifiers: Modifiers, key: Key) -> Self {
        Self { modifiers, key }
    }
}

/// Local keyboard event delivered to a WayWi surface with active keyboard focus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    pub modifiers: Modifiers,
    pub repeat: bool,
    pub utf8: Option<SharedStr>,
}

impl KeyEvent {
    #[inline]
    #[must_use]
    pub fn matches(&self, chord: KeyChord) -> bool {
        self.key == chord.key && self.modifiers == chord.modifiers
    }
}
