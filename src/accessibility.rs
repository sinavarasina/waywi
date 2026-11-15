//! Accessibility metadata and AT-SPI2 / AccessKit screen-reader integration.

#[cfg(feature = "accessibility")]
pub(crate) mod bridge;
pub(crate) mod props;
#[cfg(feature = "accessibility")]
pub(crate) mod tree;

#[cfg(feature = "accessibility")]
pub use accesskit::{
    Action as AccessibleAction, ActionData as AccessibleActionData,
    ActionRequest as AccessibleActionRequest, Node as AccessibleNode, NodeId as AccessibleNodeId,
    Role as AccessKitRole, TreeId as AccessibleTreeId, TreeInfo as AccessibleTreeInfo,
    TreeUpdate as AccessibleTreeUpdate,
};
#[cfg(feature = "accessibility")]
pub(crate) use bridge::{AccessibilityBridge, atspi_runtime_available};
pub(crate) use props::AccessibleProps;
pub use props::AccessibleRole;
#[cfg(feature = "accessibility")]
pub(crate) use tree::{AccessibilityTreeState, apply_accessibility_action, build_tree_update};
