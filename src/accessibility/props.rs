use crate::text::SharedStr;

/// Semantic role advertised to AT-SPI2 screen readers and accessibility tools.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessibleRole {
    /// Generic grouping or container with no stronger semantic role.
    Generic,
    /// Top-level application window (`Surface::window`).
    Window,
    /// Layer-shell dock, bar, HUD, or overlay panel.
    Panel,
    /// Visual group of related controls.
    Group,
    /// Clickable button control.
    Button,
    /// Static or dynamic text label.
    Label,
    /// Raster or vector image / icon.
    Image,
    /// Custom-drawn vector canvas surface.
    Canvas,
    /// Horizontal layout row container.
    Row,
    /// Vertical layout column container.
    Column,
    /// List container.
    List,
    /// Individual item inside a list.
    ListItem,
    /// Two-state toggle switch.
    Switch,
    /// Checkable boolean box.
    CheckBox,
    /// Adjustable range slider.
    Slider,
    /// Continuous or determinate progress indicator.
    ProgressIndicator,
    /// Live status readout (e.g. battery, volume, clock).
    Status,
}

/// Internal accessibility metadata stored on elements and surfaces.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AccessibleProps {
    pub(crate) role: Option<AccessibleRole>,
    pub(crate) label: Option<SharedStr>,
    pub(crate) description: Option<SharedStr>,
    pub(crate) value: Option<SharedStr>,
    pub(crate) hidden: bool,
}
