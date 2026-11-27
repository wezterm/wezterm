use crate::client::Client;
use codec::*;
use mux::tab::TabId;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use wezterm_term::{MouseEvent, MouseEventKind};

pub struct MouseState {
    /// How many moves we've sent that the server hasn't replied to yet
    moves_in_flight: usize,
    queue: VecDeque<MouseEvent>,
    client: Client,
    remote_pane_id: TabId,
}

impl MouseState {
    pub fn new(remote_pane_id: TabId, client: Client) -> Self {
        Self {
            remote_pane_id,
            client,
            moves_in_flight: 0,
            queue: VecDeque::new(),
        }
    }

    pub fn append(&mut self, event: MouseEvent) {
        append(&mut self.queue, event);
        log::trace!("MouseEvent {}: queued", self.queue.len());
    }

    fn pop(&mut self) -> Option<MouseEvent> {
        pop(&mut self.queue, self.moves_in_flight > 0)
    }

    pub fn next(state: Arc<Mutex<Self>>) -> bool {
        let mut mouse = state.lock();
        let mut sent = false;
        // Send everything that is ready, stopping at a move that
        // we need to hold back
        while let Some(event) = mouse.pop() {
            sent = true;
            let client = mouse.client.clone();

            let state = Arc::clone(&state);
            let is_move = event.kind == MouseEventKind::Move;
            if is_move {
                mouse.moves_in_flight += 1;
            }
            let remote_pane_id = mouse.remote_pane_id;

            promise::spawn::spawn(async move {
                client
                    .mouse_event(SendMouseEvent {
                        pane_id: remote_pane_id,
                        event,
                    })
                    .await
                    .ok();

                if is_move {
                    state.lock().moves_in_flight -= 1;
                    // The server has replied to this move, so try to send
                    // a newer move that we may have held back
                    Self::next(Arc::clone(&state));
                }
                Ok::<(), anyhow::Error>(())
            })
            .detach();
        }
        sent
    }
}

fn append(queue: &mut VecDeque<MouseEvent>, event: MouseEvent) {
    if let Some(last) = queue.back_mut() {
        if last.modifiers == event.modifiers
            && last.kind == MouseEventKind::Move
            && event.kind == MouseEventKind::Move
            && last.button == event.button
        {
            // Collapse any interim moves and just buffer up
            // the last of them
            *last = event;
            return;
        }
    }
    // Anything other than a move is a distinct input, like a key
    // press, so we need to send each one
    queue.push_back(event);
}

/// Returns the next event to send, if we're able to send it now.
/// We hold back a move while the server hasn't yet replied to an
/// earlier move, so that a newer move can replace it. Once another
/// event is queued behind it that can no longer happen, so we send
/// it. We never hold back anything else.
fn pop(queue: &mut VecDeque<MouseEvent>, move_in_flight: bool) -> Option<MouseEvent> {
    let hold = move_in_flight && queue.len() == 1 && queue[0].kind == MouseEventKind::Move;
    if hold {
        None
    } else {
        queue.pop_front()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use wezterm_term::{KeyModifiers, MouseButton};

    fn event(kind: MouseEventKind, button: MouseButton, x: usize) -> MouseEvent {
        MouseEvent {
            kind,
            x,
            y: 0,
            x_pixel_offset: 0,
            y_pixel_offset: 0,
            button,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn wheel_up() -> MouseEvent {
        event(MouseEventKind::Press, MouseButton::WheelUp(1), 0)
    }

    fn press() -> MouseEvent {
        event(MouseEventKind::Press, MouseButton::Left, 0)
    }

    fn release() -> MouseEvent {
        event(MouseEventKind::Release, MouseButton::Left, 0)
    }

    fn move_to(x: usize) -> MouseEvent {
        event(MouseEventKind::Move, MouseButton::None, x)
    }

    /// Ensure that we send each wheel event on its own, without
    /// waiting for the server to reply to an earlier move
    #[test]
    fn wheel_events_are_neither_merged_nor_held() {
        let mut queue = VecDeque::new();
        for _ in 0..3 {
            append(&mut queue, wheel_up());
        }

        let move_in_flight = true;
        for _ in 0..3 {
            assert_eq!(pop(&mut queue, move_in_flight), Some(wheel_up()));
        }
        assert_eq!(pop(&mut queue, move_in_flight), None);
    }

    /// Ensure that we still collapse moves into the newest one, and
    /// that we hold it back until the server replies to the earlier move
    #[test]
    fn moves_are_collapsed_and_held() {
        let mut queue = VecDeque::new();
        append(&mut queue, move_to(1));
        append(&mut queue, move_to(2));

        assert_eq!(pop(&mut queue, true), None);
        assert_eq!(pop(&mut queue, false), Some(move_to(2)));
        assert_eq!(pop(&mut queue, false), None);
    }

    /// Ensure that queueing an event behind a held back move releases
    /// it, and that we then send everything in the order it happened.
    /// A move that is queued after those events still has to wait.
    #[test]
    fn held_move_is_released_by_the_event_behind_it() {
        let mut queue = VecDeque::new();
        append(&mut queue, move_to(1));

        let move_in_flight = true;
        assert_eq!(pop(&mut queue, move_in_flight), None);

        append(&mut queue, press());
        append(&mut queue, release());
        append(&mut queue, wheel_up());
        append(&mut queue, move_to(2));

        assert_eq!(pop(&mut queue, move_in_flight), Some(move_to(1)));
        assert_eq!(pop(&mut queue, move_in_flight), Some(press()));
        assert_eq!(pop(&mut queue, move_in_flight), Some(release()));
        assert_eq!(pop(&mut queue, move_in_flight), Some(wheel_up()));
        assert_eq!(pop(&mut queue, move_in_flight), None);
        assert_eq!(pop(&mut queue, false), Some(move_to(2)));
    }
}
