use std::ops::Deref;

use crate::arena::{FrameSharedFn, FrameVec};

use super::builder::Surface;

/// Collection of declared surfaces returned by `view(state)`.
#[must_use]
#[derive(Clone)]
pub struct Surfaces<M = ()> {
    pub(crate) items: FrameVec<Surface<M>>,
}

impl<M: 'static> Default for Surfaces<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: 'static> Surfaces<M> {
    pub fn new() -> Self {
        Self {
            items: FrameVec::new(),
        }
    }

    pub fn push(&mut self, surface: Surface<M>) {
        self.items.push(surface);
    }

    pub fn extend<I>(&mut self, surfaces: I)
    where
        I: IntoIterator<Item = Surface<M>>,
    {
        for s in surfaces {
            self.items.push(s);
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Maps all contained surfaces into parent message type `N`.
    pub fn map<N: 'static, F>(self, f: F) -> Surfaces<N>
    where
        F: Fn(M) -> N + 'static,
    {
        let mapper = FrameSharedFn::new(f);
        let mut out = FrameVec::with_capacity(self.items.len());
        for s in self.items {
            let m = mapper.clone();
            out.push(s.map(move |msg| m.call(msg)));
        }
        Surfaces { items: out }
    }
}

impl<M: 'static> Deref for Surfaces<M> {
    type Target = [Surface<M>];

    fn deref(&self) -> &Self::Target {
        &self.items
    }
}

impl<M: 'static> FromIterator<Surface<M>> for Surfaces<M> {
    fn from_iter<T: IntoIterator<Item = Surface<M>>>(iter: T) -> Self {
        let mut s = Self::new();
        s.extend(iter);
        s
    }
}

impl<M: 'static> IntoIterator for Surfaces<M> {
    type Item = Surface<M>;
    type IntoIter = crate::arena::FrameVecIntoIter<Surface<M>>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}
