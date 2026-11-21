use crate::arena::{FrameSharedFn, FrameVec};
use crate::image::Image;
use crate::input::{CommonProps, ElementKey, IntoElementKey};
use crate::render::canvas::Canvas;
use crate::render::elements::containers::{Clip, Column, Group, Row};
use crate::render::elements::rect::Rect;
use crate::render::memo::check_memo;
use crate::sprite::Sprite;
use crate::text::Text;

/// Unified element tree node.
#[must_use]
#[derive(Clone)]
pub struct Element<M = ()> {
    pub(crate) kind: ElementKind<M>,
}

#[derive(Clone)]
pub(crate) enum ElementKind<M> {
    Text(Text<M>),
    Image(Image<M>),
    Sprite(Box<Sprite<M>>),
    Rect(Rect<M>),
    Canvas(Canvas<M>),
    Group(Group<M>),
    Row(Row<M>),
    Column(Column<M>),
    Clip(Clip<M>),
    /// $O(1)$ memoization hit emitted by `Element::memo`; skips subtree evaluation and diffing.
    MemoHit {
        key: ElementKey,
    },
}

impl<M: 'static> Element<M> {
    /// Keyed subtree memoization boundary.
    ///
    /// When `dep` equals the previous frame's value for `key`, `build` is not executed and the
    /// reconciler skips diffing this subtree in $O(1)$.
    pub fn memo<K, D, F>(key: K, dep: &D, build: F) -> Self
    where
        K: IntoElementKey,
        D: PartialEq + Clone + 'static,
        F: FnOnce(&D) -> Element<M>,
    {
        let el_key = key.into_element_key();
        if check_memo(el_key, dep) {
            Self {
                kind: ElementKind::MemoHit { key: el_key },
            }
        } else {
            let mut built = build(dep);
            if let Some(common) = built.common_mut() {
                common.key = Some(el_key);
            }
            built
        }
    }

    /// Maps emitted messages into a parent message type `N`.
    pub fn map<N: 'static, F>(self, f: F) -> Element<N>
    where
        F: Fn(M) -> N + 'static,
    {
        let mapper = FrameSharedFn::new(f);
        self.map_with(&mapper)
    }

    pub(crate) fn map_with<N: 'static>(self, mapper: &FrameSharedFn<M, N>) -> Element<N> {
        let kind = match self.kind {
            ElementKind::Text(t) => ElementKind::Text(Text {
                common: t.common.map_with(mapper),
                content: t.content,
                font_size: t.font_size,
                line_height_multiplier: t.line_height_multiplier,
                color: t.color,
                font: t.font,
                align: t.align,
            }),
            ElementKind::Image(img) => ElementKind::Image(Image {
                common: img.common.map_with(mapper),
                asset: img.asset,
                fit: img.fit,
                filter: img.filter,
                source_rect: img.source_rect,
                flip_x: img.flip_x,
                flip_y: img.flip_y,
            }),
            ElementKind::Sprite(sp) => ElementKind::Sprite(Box::new(Sprite {
                common: sp.common.map_with(mapper),
                sheet: sp.sheet,
                mode: sp.mode,
                filter: sp.filter,
                flip_x: sp.flip_x,
                flip_y: sp.flip_y,
                on_animation_end: sp.on_animation_end.map(|m| mapper.call(m)),
            })),
            ElementKind::Rect(r) => ElementKind::Rect(Rect {
                common: r.common.map_with(mapper),
                fill: r.fill,
                corner_radius: r.corner_radius,
                border: r.border,
            }),
            ElementKind::Canvas(c) => ElementKind::Canvas(Canvas {
                common: c.common.map_with(mapper),
                render_key: c.render_key,
                draw: c.draw,
            }),
            ElementKind::Group(g) => {
                let mut children = FrameVec::with_capacity(g.children.len());
                for ch in g.children {
                    children.push(ch.map_with(mapper));
                }
                ElementKind::Group(Group {
                    common: g.common.map_with(mapper),
                    children,
                })
            }
            ElementKind::Row(r) => {
                let mut children = FrameVec::with_capacity(r.children.len());
                for ch in r.children {
                    children.push(ch.map_with(mapper));
                }
                ElementKind::Row(Row {
                    common: r.common.map_with(mapper),
                    gap: r.gap,
                    align: r.align,
                    padding: r.padding,
                    children,
                })
            }
            ElementKind::Column(c) => {
                let mut children = FrameVec::with_capacity(c.children.len());
                for ch in c.children {
                    children.push(ch.map_with(mapper));
                }
                ElementKind::Column(Column {
                    common: c.common.map_with(mapper),
                    gap: c.gap,
                    align: c.align,
                    padding: c.padding,
                    children,
                })
            }
            ElementKind::Clip(c) => {
                let mut children = FrameVec::with_capacity(c.child.len());
                for ch in c.child {
                    children.push(ch.map_with(mapper));
                }
                ElementKind::Clip(Clip {
                    common: c.common.map_with(mapper),
                    corner_radius: c.corner_radius,
                    child: children,
                })
            }
            ElementKind::MemoHit { key } => ElementKind::MemoHit { key },
        };
        Element { kind }
    }

    pub(crate) fn common(&self) -> Option<&CommonProps<M>> {
        match &self.kind {
            ElementKind::Text(t) => Some(&t.common),
            ElementKind::Image(i) => Some(&i.common),
            ElementKind::Sprite(s) => Some(&s.common),
            ElementKind::Rect(r) => Some(&r.common),
            ElementKind::Canvas(c) => Some(&c.common),
            ElementKind::Group(g) => Some(&g.common),
            ElementKind::Row(r) => Some(&r.common),
            ElementKind::Column(c) => Some(&c.common),
            ElementKind::Clip(c) => Some(&c.common),
            ElementKind::MemoHit { .. } => None,
        }
    }

    pub(crate) fn common_mut(&mut self) -> Option<&mut CommonProps<M>> {
        match &mut self.kind {
            ElementKind::Text(t) => Some(&mut t.common),
            ElementKind::Image(i) => Some(&mut i.common),
            ElementKind::Sprite(s) => Some(&mut s.common),
            ElementKind::Rect(r) => Some(&mut r.common),
            ElementKind::Canvas(c) => Some(&mut c.common),
            ElementKind::Group(g) => Some(&mut g.common),
            ElementKind::Row(r) => Some(&mut r.common),
            ElementKind::Column(c) => Some(&mut c.common),
            ElementKind::Clip(c) => Some(&mut c.common),
            ElementKind::MemoHit { .. } => None,
        }
    }
}

impl<M: 'static> From<Text<M>> for Element<M> {
    fn from(value: Text<M>) -> Self {
        Self {
            kind: ElementKind::Text(value),
        }
    }
}

impl<M: 'static> From<Image<M>> for Element<M> {
    fn from(value: Image<M>) -> Self {
        Self {
            kind: ElementKind::Image(value),
        }
    }
}

impl<M: 'static> From<Sprite<M>> for Element<M> {
    fn from(value: Sprite<M>) -> Self {
        Self {
            kind: ElementKind::Sprite(Box::new(value)),
        }
    }
}

impl<M: 'static> From<Rect<M>> for Element<M> {
    fn from(value: Rect<M>) -> Self {
        Self {
            kind: ElementKind::Rect(value),
        }
    }
}

impl<M: 'static> From<Canvas<M>> for Element<M> {
    fn from(value: Canvas<M>) -> Self {
        Self {
            kind: ElementKind::Canvas(value),
        }
    }
}

impl<M: 'static> From<Group<M>> for Element<M> {
    fn from(value: Group<M>) -> Self {
        Self {
            kind: ElementKind::Group(value),
        }
    }
}

impl<M: 'static> From<Row<M>> for Element<M> {
    fn from(value: Row<M>) -> Self {
        Self {
            kind: ElementKind::Row(value),
        }
    }
}

impl<M: 'static> From<Column<M>> for Element<M> {
    fn from(value: Column<M>) -> Self {
        Self {
            kind: ElementKind::Column(value),
        }
    }
}

impl<M: 'static> From<Clip<M>> for Element<M> {
    fn from(value: Clip<M>) -> Self {
        Self {
            kind: ElementKind::Clip(value),
        }
    }
}
