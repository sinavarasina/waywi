use crate::geometry::{LogicalPx, LogicalSize, Point, ScaleFactor};

/// Opaque WayWi identifier for a logical Wayland output (`wl_output`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OutputId(pub u64);

impl OutputId {
    #[inline]
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[inline]
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Output transform reported by the Wayland compositor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OutputTransform {
    #[default]
    Normal,
    Rotate90,
    Rotate180,
    Rotate270,
    Flipped,
    Flipped90,
    Flipped180,
    Flipped270,
}

/// Output selection policy for a [`Surface`](crate::surface::Surface).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum Output {
    /// Selects WayWi's primary output policy (lowest `(x, y)` origin, then lowest `OutputId`).
    ///
    /// Note: Wayland itself does not define a universal "primary monitor" protocol; applications
    /// requiring deterministic placement should prefer [`Output::named`] or [`Output::id`].
    #[default]
    Primary,
    /// Matches an output by its compositor-advertised connector name (e.g., `"DP-1"`, `"eDP-1"`).
    Named(String),
    /// Matches a specific output discovered at runtime.
    Id(OutputId),
    /// Targets every currently connected output (expanding one retained surface instance per output).
    All,
}

impl Output {
    #[inline]
    #[must_use]
    pub const fn primary() -> Self {
        Self::Primary
    }

    #[inline]
    #[must_use]
    pub fn named(name: impl Into<String>) -> Self {
        Self::Named(name.into())
    }

    #[inline]
    #[must_use]
    pub const fn id(id: OutputId) -> Self {
        Self::Id(id)
    }

    #[inline]
    #[must_use]
    pub const fn all() -> Self {
        Self::All
    }
}

/// Metadata snapshot for a connected Wayland output.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputInfo {
    pub id: OutputId,
    pub name: Option<String>,
    pub description: Option<String>,
    pub make: String,
    pub model: String,
    pub logical_position: Point,
    pub logical_size: LogicalSize,
    pub physical_size_mm: (i32, i32),
    pub scale_factor: ScaleFactor,
    pub transform: OutputTransform,
    pub refresh_mhz: i32,
}

impl Default for OutputInfo {
    fn default() -> Self {
        Self {
            id: OutputId(1),
            name: Some("HEADLESS-1".to_owned()),
            description: Some("WayWi Headless Output".to_owned()),
            make: "WayWi".to_owned(),
            model: "Virtual".to_owned(),
            logical_position: Point::ZERO,
            logical_size: LogicalSize::new(LogicalPx(1920.0), LogicalPx(1080.0)),
            physical_size_mm: (0, 0),
            scale_factor: ScaleFactor::ONE,
            transform: OutputTransform::Normal,
            refresh_mhz: 60_000,
        }
    }
}

/// Resolves which connected outputs match an [`Output`] selector.
///
/// Primary output policy: if an output has `logical_position == (0, 0)`, prefer it; otherwise
/// choose the output with the smallest `(y, x, id)` tuple.
#[must_use]
pub fn resolve_outputs<'a>(selector: &Output, outputs: &'a [OutputInfo]) -> Vec<&'a OutputInfo> {
    match selector {
        Output::Primary => select_primary_output(outputs).into_iter().collect(),
        Output::Named(target) => outputs
            .iter()
            .filter(|o| o.name.as_deref() == Some(target.as_str()))
            .collect(),
        Output::Id(id) => outputs.iter().filter(|o| o.id == *id).collect(),
        Output::All => outputs.iter().collect(),
    }
}

/// WayWi's documented primary output selection policy.
#[must_use]
pub fn select_primary_output(outputs: &[OutputInfo]) -> Option<&OutputInfo> {
    outputs.iter().min_by(|a, b| {
        let key_a = (
            a.logical_position.y.0 as i64,
            a.logical_position.x.0 as i64,
            a.id.0,
        );
        let key_b = (
            b.logical_position.y.0 as i64,
            b.logical_position.x.0 as i64,
            b.id.0,
        );
        key_a.cmp(&key_b)
    })
}
