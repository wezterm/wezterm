use accesskit::{
    Action, ActionData, ActionHandler, ActionRequest, ActivationHandler, Node, NodeId, Role,
    TextPosition, TextSelection, TreeId, TreeInfo, TreeUpdate,
};
use accesskit_windows::{Adapter, QueuedEvents, HWND};
use std::sync::{Arc, Weak};

use super::window::HWindow;
use super::Connection;
use crate::Rect;

const ROOT: NodeId = NodeId(0);

#[derive(Default)]
pub(super) struct InputState {
    target: Option<(usize, Rect)>,
}

impl InputState {
    fn input_id(pane: usize) -> NodeId {
        NodeId(1 + 2 * pane as u64)
    }

    pub fn accepts(&self, request: &ActionRequest) -> Option<usize> {
        self.target.and_then(|(pane, _)| {
            (request.target_tree == TreeId::ROOT && request.target_node == Self::input_id(pane))
                .then_some(pane)
        })
    }

    fn tree(&self) -> TreeUpdate {
        let mut root = Node::new(Role::Window);
        let mut nodes = vec![];
        let mut focus = ROOT;
        if let Some((pane, cursor)) = self.target {
            let input_id = Self::input_id(pane);
            let text_id = NodeId(input_id.0 + 1);
            let bounds = accesskit::Rect {
                x0: cursor.origin.x as f64,
                y0: cursor.origin.y as f64,
                x1: (cursor.origin.x + cursor.size.width) as f64,
                y1: (cursor.origin.y + cursor.size.height) as f64,
            };
            root.set_children([input_id]);
            let mut input = Node::new(Role::MultilineTextInput);
            input.set_label("Terminal input");
            input.set_description("Text is sent immediately to the active terminal pane.");
            input.set_bounds(bounds);
            input.set_value("");
            input.set_children([text_id]);
            for action in [
                Action::Focus,
                Action::SetValue,
                Action::ReplaceSelectedText,
                Action::SetTextSelection,
            ] {
                input.add_action(action);
            }
            let caret = TextPosition {
                node: text_id,
                character_index: 0,
            };
            input.set_text_selection(TextSelection {
                anchor: caret,
                focus: caret,
            });

            // 输入立即交给终端；这里仅表示待提交的空缓冲区，不冒充屏幕文本。
            let mut text = Node::new(Role::TextRun);
            text.set_value("");
            text.set_character_lengths(Vec::<u8>::new());
            text.set_bounds(bounds);
            nodes.extend([(input_id, input), (text_id, text)]);
            focus = input_id;
        }
        nodes.push((ROOT, root));
        TreeUpdate {
            nodes,
            tree: Some(TreeInfo::new(ROOT)),
            tree_id: TreeId::ROOT,
            focus,
        }
    }
}

impl ActivationHandler for InputState {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        Some(self.tree())
    }
}

struct InputActionHandler {
    hwnd: HWindow,
    lifetime: Weak<()>,
}

impl ActionHandler for InputActionHandler {
    fn do_action(&mut self, request: ActionRequest) {
        let lifetime = self.lifetime.clone();
        Connection::with_window_inner(self.hwnd, move |inner| {
            inner.accessibility_action(request, lifetime);
            Ok(())
        });
    }
}

pub(super) struct InputBridge {
    pub adapter: Adapter,
    pub state: InputState,
    pub lifetime: Arc<()>,
}

impl InputBridge {
    pub fn new(hwnd: HWindow, focused: bool) -> Self {
        let lifetime = Arc::new(());
        let handler = InputActionHandler {
            hwnd,
            lifetime: Arc::downgrade(&lifetime),
        };
        Self {
            adapter: Adapter::new(HWND(hwnd.0 as _), focused, handler),
            state: InputState::default(),
            lifetime,
        }
    }

    pub fn update_target(&mut self, pane: Option<usize>, cursor: Rect) {
        let target = pane.map(|pane| (pane, cursor));
        if self.state.target == target {
            return;
        }
        self.state.target = target;
        let state = &self.state;
        let events = self.adapter.update_if_active(|| state.tree());
        self.queue_events(events);
    }

    pub fn update_focus(&mut self, focused: bool) {
        let events = self.adapter.update_window_focus_state(focused);
        self.queue_events(events);
    }

    fn queue_events(&self, events: Option<QueuedEvents>) {
        if let Some(events) = events {
            let lifetime = Arc::downgrade(&self.lifetime);
            // 通知可能重入窗口过程，必须等窗口状态借用结束后再发送。
            promise::spawn::spawn(async move {
                if lifetime.upgrade().is_some() {
                    events.raise();
                }
            })
            .detach();
        }
    }
}

pub(super) fn input_text(request: ActionRequest) -> Option<String> {
    match (request.action, request.data) {
        (Action::SetValue | Action::ReplaceSelectedText, Some(ActionData::Value(text)))
            if !text.is_empty() =>
        {
            Some(text.into())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use accesskit_consumer::Tree;

    fn state(pane: usize) -> InputState {
        InputState {
            target: Some((pane, Rect::zero())),
        }
    }

    fn request(pane: usize, text: &str) -> ActionRequest {
        ActionRequest {
            action: Action::SetValue,
            target_tree: TreeId::ROOT,
            target_node: InputState::input_id(pane),
            data: Some(ActionData::Value(text.into())),
        }
    }

    #[test]
    fn pending_input_is_editable_with_an_empty_text_range() {
        let tree = Tree::new(state(3).tree(), true);
        let input = tree
            .state()
            .node_by_tree_local_id(InputState::input_id(3), TreeId::ROOT)
            .unwrap();
        assert!(!input.is_read_only());
        assert!(input.supports_text_ranges());
        assert_eq!(input.value().as_deref(), Some(""));
        assert!(input.text_selection().unwrap().is_degenerate());
        assert_eq!(input.document_range().text(), "");
    }

    #[test]
    fn a_background_window_has_no_focused_input() {
        let tree = Tree::new(state(3).tree(), false);
        assert!(tree.state().focus().is_none());
        let without_pane = Tree::new(InputState::default().tree(), true);
        assert!(without_pane
            .state()
            .node_by_tree_local_id(InputState::input_id(3), TreeId::ROOT,)
            .is_none());
    }

    #[test]
    fn inactive_pane_and_empty_updates_cannot_insert_text() {
        assert_eq!(state(4).accepts(&request(3, "wrong pane")), None);
        assert_eq!(InputState::default().accepts(&request(3, "no pane")), None);
        assert_eq!(state(3).accepts(&request(3, "中文\nsecond line")), Some(3));
        assert_eq!(input_text(request(3, "")), None);
        assert_eq!(
            input_text(request(3, "中文\nsecond line")),
            Some("中文\nsecond line".into())
        );
    }
}
