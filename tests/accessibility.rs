#![cfg(feature = "accessibility")]

use waywi::accessibility::{
    AccessKitRole, AccessibleAction, AccessibleActionRequest, AccessibleRole, AccessibleTreeId,
};
use waywi::prelude::*;

struct A11yTestApp;

#[derive(Default)]
struct A11yState {
    count: u32,
}

#[derive(Clone)]
enum A11yMsg {
    Increment,
}

impl Application for A11yTestApp {
    type State = A11yState;
    type Message = A11yMsg;

    fn init(_ctx: &mut EffectCtx<'_>) -> Result<Self::State> {
        Ok(A11yState::default())
    }

    fn update(state: &mut Self::State, msg: Self::Message, _ctx: &mut EffectCtx<'_>) -> Result<()> {
        match msg {
            A11yMsg::Increment => state.count += 1,
        }
        Ok(())
    }

    fn view(state: &Self::State) -> Surfaces<Self::Message> {
        let mut s = Surfaces::new();
        let surf = Surface::panel("a11y-panel", Output::Primary)
            .size(Size::px(300.logical_px(), 48.logical_px()))
            .title("Accessibility Test Panel")
            .accessible_description("Top status bar")
            .child(
                Row::new()
                    .child(
                        Rect::new()
                            .size(Size::px(120.logical_px(), 32.logical_px()))
                            .fill(Color::rgb(40, 80, 120))
                            .accessible_role(AccessibleRole::Button)
                            .accessible_label(format!("Increment ({})", state.count))
                            .accessible_description("Increase counter by one")
                            .on_click(A11yMsg::Increment),
                    )
                    .child(Text::new(format!("Count: {}", state.count))),
            );
        s.push(surf);
        s
    }
}

#[test]
fn headless_accessibility_tree_exposes_roles_and_dispatches_click_action() {
    let mut rt = HeadlessRuntime::<A11yTestApp>::new(
        Capabilities::from_compile_features(),
        vec![OutputInfo {
            id: OutputId(1),
            logical_size: LogicalSize::new(1920.logical_px(), 1080.logical_px()),
            ..OutputInfo::default()
        }],
    )
    .unwrap();

    let tree = rt.accessibility_tree();
    assert_eq!(
        tree.tree.as_ref().and_then(|t| t.toolkit_name.as_deref()),
        Some("waywi")
    );

    let (btn_id, btn_node) = tree
        .nodes
        .iter()
        .find(|(_, n)| n.label() == Some("Increment (0)"))
        .expect("accessible button present in tree");
    assert_eq!(btn_node.role(), AccessKitRole::Button);
    assert_eq!(btn_node.description(), Some("Increase counter by one"));
    assert!(btn_node.supports_action(AccessibleAction::Click));
    assert!(btn_node.supports_action(AccessibleAction::Focus));

    assert!(
        tree.nodes
            .iter()
            .any(|(_, n)| { n.role() == AccessKitRole::Label && n.label() == Some("Count: 0") })
    );

    assert!(rt.dispatch_accessibility_action(AccessibleActionRequest {
        action: AccessibleAction::Focus,
        target_tree: AccessibleTreeId::ROOT,
        target_node: *btn_id,
        data: None,
    }));
    assert!(rt.dispatch_accessibility_action(AccessibleActionRequest {
        action: AccessibleAction::Click,
        target_tree: AccessibleTreeId::ROOT,
        target_node: *btn_id,
        data: None,
    }));

    rt.step().unwrap();
    assert_eq!(rt.state().count, 1);

    let updated = rt.accessibility_tree();
    assert_eq!(updated.focus, *btn_id);
    assert!(
        updated
            .nodes
            .iter()
            .any(|(_, n)| n.label() == Some("Increment (1)"))
    );
}
