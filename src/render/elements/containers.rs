use crate::arena::{FrameSharedFn, FrameVec};
use crate::geometry::{Edges, LogicalPx};
use crate::input::CommonProps;
use crate::render::elements::Element;
use crate::render::style::{Align, CornerRadius, Justify};

/// Neutral grouping container for parent-relative positioning, transforms, and z-order.
#[must_use]
#[derive(Clone)]
pub struct Group<M = ()> {
    pub(crate) common: CommonProps<M>,
    pub(crate) children: FrameVec<Element<M>>,
}

impl<M: 'static> Default for Group<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: 'static> Group<M> {
    pub fn new() -> Self {
        Self {
            common: CommonProps::default(),
            children: FrameVec::new(),
        }
    }

    pub fn child(mut self, child: impl Into<Element<M>>) -> Self {
        self.children.push(child.into());
        self
    }

    pub fn children<I, E>(mut self, children: I) -> Self
    where
        I: IntoIterator<Item = E>,
        E: Into<Element<M>>,
    {
        for child in children {
            self.children.push(child.into());
        }
        self
    }

    crate::input::impl_common_builders!();

    pub fn map<N: 'static, F>(self, f: F) -> Group<N>
    where
        F: Fn(M) -> N + 'static,
    {
        let mapper = FrameSharedFn::new(f);
        let mut mapped_children = FrameVec::with_capacity(self.children.len());
        for child in self.children {
            mapped_children.push(child.map_with(&mapper));
        }
        Group {
            common: self.common.map_with(&mapper),
            children: mapped_children,
        }
    }
}

/// Neutral horizontal layout container.
#[must_use]
#[derive(Clone)]
pub struct Row<M = ()> {
    pub(crate) common: CommonProps<M>,
    pub(crate) gap: LogicalPx,
    pub(crate) align: Align,
    pub(crate) justify: Justify,
    pub(crate) padding: Edges,
    pub(crate) children: FrameVec<Element<M>>,
}

impl<M: 'static> Default for Row<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: 'static> Row<M> {
    pub fn new() -> Self {
        Self {
            common: CommonProps::default(),
            gap: LogicalPx::ZERO,
            align: Align::Start,
            justify: Justify::Start,
            padding: Edges::ZERO,
            children: FrameVec::new(),
        }
    }

    pub fn gap(mut self, gap: LogicalPx) -> Self {
        self.gap = gap;
        self
    }

    pub fn align(mut self, align: Align) -> Self {
        self.align = align;
        self
    }

    /// Sets main-axis distribution along the horizontal row.
    pub fn justify(mut self, justify: Justify) -> Self {
        self.justify = justify;
        self
    }

    pub fn padding(mut self, padding: Edges) -> Self {
        self.padding = padding;
        self
    }

    pub fn child(mut self, child: impl Into<Element<M>>) -> Self {
        self.children.push(child.into());
        self
    }

    pub fn children<I, E>(mut self, children: I) -> Self
    where
        I: IntoIterator<Item = E>,
        E: Into<Element<M>>,
    {
        for child in children {
            self.children.push(child.into());
        }
        self
    }

    crate::input::impl_common_builders!();

    pub fn map<N: 'static, F>(self, f: F) -> Row<N>
    where
        F: Fn(M) -> N + 'static,
    {
        let mapper = FrameSharedFn::new(f);
        let mut mapped_children = FrameVec::with_capacity(self.children.len());
        for child in self.children {
            mapped_children.push(child.map_with(&mapper));
        }
        Row {
            common: self.common.map_with(&mapper),
            gap: self.gap,
            align: self.align,
            justify: self.justify,
            padding: self.padding,
            children: mapped_children,
        }
    }
}

/// Neutral vertical layout container.
#[must_use]
#[derive(Clone)]
pub struct Column<M = ()> {
    pub(crate) common: CommonProps<M>,
    pub(crate) gap: LogicalPx,
    pub(crate) align: Align,
    pub(crate) justify: Justify,
    pub(crate) padding: Edges,
    pub(crate) children: FrameVec<Element<M>>,
}

impl<M: 'static> Default for Column<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: 'static> Column<M> {
    pub fn new() -> Self {
        Self {
            common: CommonProps::default(),
            gap: LogicalPx::ZERO,
            align: Align::Start,
            justify: Justify::Start,
            padding: Edges::ZERO,
            children: FrameVec::new(),
        }
    }

    pub fn gap(mut self, gap: LogicalPx) -> Self {
        self.gap = gap;
        self
    }

    pub fn align(mut self, align: Align) -> Self {
        self.align = align;
        self
    }

    /// Sets main-axis distribution along the vertical column.
    pub fn justify(mut self, justify: Justify) -> Self {
        self.justify = justify;
        self
    }

    pub fn padding(mut self, padding: Edges) -> Self {
        self.padding = padding;
        self
    }

    pub fn child(mut self, child: impl Into<Element<M>>) -> Self {
        self.children.push(child.into());
        self
    }

    pub fn children<I, E>(mut self, children: I) -> Self
    where
        I: IntoIterator<Item = E>,
        E: Into<Element<M>>,
    {
        for child in children {
            self.children.push(child.into());
        }
        self
    }

    crate::input::impl_common_builders!();

    pub fn map<N: 'static, F>(self, f: F) -> Column<N>
    where
        F: Fn(M) -> N + 'static,
    {
        let mapper = FrameSharedFn::new(f);
        let mut mapped_children = FrameVec::with_capacity(self.children.len());
        for child in self.children {
            mapped_children.push(child.map_with(&mapper));
        }
        Column {
            common: self.common.map_with(&mapper),
            gap: self.gap,
            align: self.align,
            justify: self.justify,
            padding: self.padding,
            children: mapped_children,
        }
    }
}

/// Rectangular or rounded clipping boundary around a child subtree.
#[must_use]
#[derive(Clone)]
pub struct Clip<M = ()> {
    pub(crate) common: CommonProps<M>,
    pub(crate) corner_radius: CornerRadius,
    pub(crate) child: FrameVec<Element<M>>,
}

impl<M: 'static> Clip<M> {
    pub fn new(child: impl Into<Element<M>>) -> Self {
        let mut children = FrameVec::with_capacity(1);
        children.push(child.into());
        Self {
            common: CommonProps::default(),
            corner_radius: CornerRadius::ZERO,
            child: children,
        }
    }

    pub fn corner_radius(mut self, radius: impl Into<CornerRadius>) -> Self {
        self.corner_radius = radius.into();
        self
    }

    crate::input::impl_common_builders!();

    pub fn map<N: 'static, F>(self, f: F) -> Clip<N>
    where
        F: Fn(M) -> N + 'static,
    {
        let mapper = FrameSharedFn::new(f);
        let mut mapped = FrameVec::with_capacity(self.child.len());
        for c in self.child {
            mapped.push(c.map_with(&mapper));
        }
        Clip {
            common: self.common.map_with(&mapper),
            corner_radius: self.corner_radius,
            child: mapped,
        }
    }
}
