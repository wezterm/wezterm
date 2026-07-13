//! Test support for the mux crate: a Pane implementation that records
//! the action batches performed against it, and a harness that feeds
//! bytes to parse_buffered_data through a socketpair.
use crate::pane::{CachePolicy, ForEachPaneLogicalLine, LogicalLine, Pane, PaneId, WithPaneLines};
use crate::renderable::{RenderableDimensions, StableCursorPosition};
use crate::{parse_buffered_data, DomainId};
use filedescriptor::{socketpair, FileDescriptor};
use parking_lot::{MappedMutexGuard, Mutex};
use rangeset::RangeSet;
use std::ops::Range;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Once, Weak};
use std::thread;
use std::time::{Duration, Instant};
use termwiz::escape::Action;
use termwiz::surface::{Line, SequenceNo};
use url::Url;
use wezterm_term::color::ColorPalette;
use wezterm_term::{KeyCode, KeyModifiers, MouseEvent, StableRowIndex, TerminalSize};

mod sync_update;

pub(crate) struct RecordingPane {
    batches: Mutex<Vec<Vec<Action>>>,
}

impl RecordingPane {
    fn new() -> Self {
        Self {
            batches: Mutex::new(vec![]),
        }
    }

    fn flattened_actions(&self) -> Vec<Action> {
        self.batches.lock().iter().flatten().cloned().collect()
    }

    fn recorded_batches(&self) -> Vec<Vec<Action>> {
        self.batches.lock().clone()
    }
}

impl Pane for RecordingPane {
    fn perform_actions(&self, actions: Vec<Action>) {
        self.batches.lock().push(actions);
    }

    fn pane_id(&self) -> PaneId {
        0
    }
    fn is_dead(&self) -> bool {
        false
    }
    fn get_cursor_position(&self) -> StableCursorPosition {
        unreachable!()
    }
    fn get_current_seqno(&self) -> SequenceNo {
        unreachable!()
    }
    fn get_changed_since(
        &self,
        _lines: Range<StableRowIndex>,
        _seqno: SequenceNo,
    ) -> RangeSet<StableRowIndex> {
        unreachable!()
    }
    fn get_lines(&self, _lines: Range<StableRowIndex>) -> (StableRowIndex, Vec<Line>) {
        unreachable!()
    }
    fn with_lines_mut(&self, _lines: Range<StableRowIndex>, _with_lines: &mut dyn WithPaneLines) {
        unreachable!()
    }
    fn for_each_logical_line_in_stable_range_mut(
        &self,
        _lines: Range<StableRowIndex>,
        _for_line: &mut dyn ForEachPaneLogicalLine,
    ) {
        unreachable!()
    }
    fn get_logical_lines(&self, _lines: Range<StableRowIndex>) -> Vec<LogicalLine> {
        unreachable!()
    }
    fn get_dimensions(&self) -> RenderableDimensions {
        unreachable!()
    }
    fn get_title(&self) -> String {
        unreachable!()
    }
    fn send_paste(&self, _text: &str) -> anyhow::Result<()> {
        unreachable!()
    }
    fn reader(&self) -> anyhow::Result<Option<Box<dyn std::io::Read + Send>>> {
        unreachable!()
    }
    fn writer(&self) -> MappedMutexGuard<'_, dyn std::io::Write> {
        unreachable!()
    }
    fn resize(&self, _size: TerminalSize) -> anyhow::Result<()> {
        unreachable!()
    }
    fn key_down(&self, _key: KeyCode, _mods: KeyModifiers) -> anyhow::Result<()> {
        unreachable!()
    }
    fn key_up(&self, _key: KeyCode, _mods: KeyModifiers) -> anyhow::Result<()> {
        unreachable!()
    }
    fn mouse_event(&self, _event: MouseEvent) -> anyhow::Result<()> {
        unreachable!()
    }
    fn palette(&self) -> ColorPalette {
        unreachable!()
    }
    fn domain_id(&self) -> DomainId {
        unreachable!()
    }
    fn get_current_working_dir(&self, _policy: CachePolicy) -> Option<Url> {
        unreachable!()
    }
    fn is_mouse_grabbed(&self) -> bool {
        false
    }
    fn is_alt_screen_active(&self) -> bool {
        false
    }
}

// The tests mutate the process-global configuration, so they must not
// run concurrently with each other.
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn start_parser(
    synchronized_output_timeout_ms: u64,
) -> (FileDescriptor, Arc<RecordingPane>, thread::JoinHandle<()>) {
    static SCHEDULER: Once = Once::new();
    SCHEDULER.call_once(|| {
        // send_actions_to_mux notifies the mux via
        // spawn_into_main_thread, which panics unless a scheduler is
        // configured. There is no mux in these tests, so running the
        // notification inline is a no-op.
        promise::spawn::set_schedulers(
            Box::new(|runnable| {
                runnable.run();
            }),
            Box::new(|runnable| {
                runnable.run();
            }),
        );
    });

    let mut config = config::Config::default_config();
    config.synchronized_output_timeout_ms = synchronized_output_timeout_ms;
    config::use_this_configuration(config);

    let (tx, rx) = socketpair().unwrap();
    let pane = Arc::new(RecordingPane::new());
    let pane_for_parser: Weak<dyn Pane> = Arc::downgrade(&(pane.clone() as Arc<dyn Pane>));
    let parser = thread::spawn(move || {
        let dead = Arc::new(AtomicBool::new(false));
        parse_buffered_data(pane_for_parser, &dead, rx);
    });
    (tx, pane, parser)
}

// Dropping the write end closes the socketpair, which the parser
// thread observes as EOF and exits
fn stop_parser(tx: FileDescriptor, parser: thread::JoinHandle<()>) {
    drop(tx);
    parser.join().unwrap();
}

fn wait_for(mut condition: impl FnMut() -> bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if condition() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn contains_print(actions: &[Action], text: &str) -> bool {
    actions.iter().any(|action| match action {
        Action::PrintString(s) => s.contains(text),
        Action::Print(c) => text.contains(*c),
        _ => false,
    })
}

const DA1_QUERY: &[u8] = b"\x1b[c";
const KITTY_QUERY: &[u8] = b"\x1b[?u";
const BSU: &[u8] = b"\x1b[?2026h";
const ESU: &[u8] = b"\x1b[?2026l";
