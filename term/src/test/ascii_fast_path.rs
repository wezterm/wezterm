//! The printable-ASCII fast path in `Performer::flush_print` must leave the
//! terminal in exactly the state that printing one grapheme at a time does.
use super::TestTermConfig;
use crate::terminalstate::test_hooks;
use crate::{Terminal, TerminalSize};
use std::sync::Arc;

const TOKENS: &[&str] = &[
    // text
    "hello",
    "world",
    " ",
    "   ",
    "#",
    "`",
    "abc def",
    "x",
    "{|}~",
    "0123456789",
    "e\u{301}",
    "a\u{20e3}",
    "\u{600}1",
    "é",
    "中文",
    "😀",
    "👨\u{200d}👩\u{200d}👧",
    "\u{200b}",
    "\u{fe0f}",
    "ok\u{fe0f}",
    "😀abc",
    "a\u{300}\u{301}b",
    // C0 controls
    "\r\n",
    "\n",
    "\r",
    "\t",
    "\x08",
    // SGR and hyperlinks
    "\x1b[31m",
    "\x1b[0m",
    "\x1b[1;4m",
    "\x1b[44m",
    "\x1b]8;;http://example.com\x1b\\",
    "\x1b]8;;\x1b\\",
    // cursor movement and erasing
    "\x1b[5G",
    "\x1b[1G",
    "\x1b[3;4H",
    "\x1b[H",
    "\x1b[2K",
    "\x1b[K",
    "\x1b[2J",
    "\x1b[3D",
    // modes: insert, autowrap, left/right margins, origin, scroll region
    "\x1b[4h",
    "\x1b[4l",
    "\x1b[?7l",
    "\x1b[?7h",
    "\x1b[?69h",
    "\x1b[2;6s",
    "\x1b[?69l",
    "\x1b[?6h",
    "\x1b[?6l",
    "\x1b[2;3r",
    "\x1b[r",
    // charsets and shifts
    "\x1b(0",
    "\x1b(B",
    "\x1b(A",
    "\x1b)0",
    "\x0e",
    "\x0f",
    // alternate screen
    "\x1b[?1049h",
    "\x1b[?1049l",
];

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

fn run(input: &[u8], chunks: &[usize], rows: usize, cols: usize, fast: bool) -> String {
    test_hooks::set_ascii_fast_path_disabled(!fast);
    let mut term = Terminal::new(
        TerminalSize {
            rows,
            cols,
            pixel_width: cols * 8,
            pixel_height: rows * 16,
            dpi: 0,
        },
        Arc::new(TestTermConfig { scrollback: 20 }),
        "WezTerm",
        "O_o",
        Box::new(Vec::new()),
    );
    let mut pos = 0;
    for &n in chunks {
        let end = (pos + n).min(input.len());
        term.advance_bytes(&input[pos..end]);
        pos = end;
    }
    term.advance_bytes(&input[pos..]);
    test_hooks::set_ascii_fast_path_disabled(false);

    let mut state = format!("{:?}\n", term.cursor_pos());
    for line in term.screen().all_lines() {
        state.push_str(&format!("{:?}\n", line));
    }
    state
}

#[test]
fn ascii_fast_path_matches_grapheme_path() {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    for case in 0..1500 {
        let rows = 2 + rng.below(6);
        let cols = 3 + rng.below(20);
        let mut input = String::new();
        for _ in 0..rng.below(60) {
            if rng.below(3) == 0 {
                let n = 1 + rng.below(40);
                for _ in 0..n {
                    input.push((0x20 + rng.below(0x5f)) as u8 as char);
                }
            } else {
                input.push_str(TOKENS[rng.below(TOKENS.len())]);
            }
        }
        let chunks: Vec<usize> = (0..rng.below(8)).map(|_| 1 + rng.below(32)).collect();

        let fast = run(input.as_bytes(), &chunks, rows, cols, true);
        let slow = run(input.as_bytes(), &chunks, rows, cols, false);
        if fast != slow {
            let (f, s) = fast
                .lines()
                .zip(slow.lines())
                .find(|(f, s)| f != s)
                .unwrap_or(("<length differs>", ""));
            panic!(
                "case {}: {}x{} chunks {:?}\ninput {:?}\nfast: {}\nslow: {}",
                case, rows, cols, chunks, input, f, s
            );
        }
    }
}
