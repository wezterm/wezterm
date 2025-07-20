use crate::scripting::guiwin::GuiWin;
use config::{
    keyassignment::{KeyAssignment, TabulatedList, TransientArgument, TransientContext},
    AnsiColor, ColorAttribute,
};
use luahelper::impl_lua_conversion_dynamic;
use mux::termwiztermtab::TermWizTerminal;
use mux_lua::MuxPane;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use termwiz::{
    input::{InputEvent, KeyCode, KeyEvent},
    surface::{Change, CursorVisibility, Position},
    terminal::{ScreenSize, Terminal},
};
use wezterm_dynamic::{FromDynamic, ToDynamic};
use wezterm_term::{AttributeChange, CellAttributes};
use window::Modifiers;

struct TrieNode {
    children: HashMap<char, Rc<RefCell<TrieNode>>>,
    is_end_of_word: bool,
    entry: Option<TransientArgument>,
}

impl TrieNode {
    fn new() -> Self {
        Self {
            children: HashMap::new(),
            is_end_of_word: true,
            entry: None,
        }
    }

    fn add_word(&mut self, word: &str, entry: TransientArgument) {
        match word.chars().next() {
            Some(c) => {
                match self.children.get(&c) {
                    Some(child_node) => {
                        child_node.borrow_mut().add_word(&word[1..], entry);
                    }
                    None => {
                        let mut new_node = TrieNode::new();
                        new_node.add_word(&word[1..], entry);
                        self.children.insert(c, Rc::new(RefCell::new(new_node)));
                    }
                }
                self.is_end_of_word = false;
            }
            None => self.entry = Some(entry),
        }
    }

    fn find_char(&self, c: char) -> Option<Rc<RefCell<TrieNode>>> {
        self.children.get(&c).map(|child| Rc::clone(child))
    }
}

struct SelectorState {
    active_idx: usize,
    max_items: usize,
    top_row: usize,
    choices: Vec<String>,
    cols: usize,
    selector_size: usize,
    multiple_idx: Option<Vec<bool>>,
}

impl SelectorState {
    fn new(choices: Vec<String>, size: &ScreenSize, overhead: usize, multiple: bool) -> Self {
        let max_items = size.rows.saturating_sub(overhead);
        let selector_size = choices.len().min(max_items);
        let multiple_idx = multiple.then(|| choices.iter().map(|_| false).collect());

        Self {
            active_idx: 0,
            max_items,
            top_row: 0,
            choices,
            cols: size.cols,
            selector_size,
            multiple_idx,
        }
    }

    fn move_up(&mut self) {
        self.active_idx = self.active_idx.saturating_sub(1);
        if self.active_idx < self.top_row {
            self.top_row = self.active_idx;
        }
    }

    fn move_down(&mut self) {
        self.active_idx = (self.active_idx + 1).min(self.choices.len() - 1);
        if self.active_idx > self.top_row + self.max_items {
            self.top_row = self.active_idx.saturating_sub(self.max_items);
        }
    }

    fn toggle_multiple_idx(&mut self) {
        if let Some(multiple_idx) = self.multiple_idx.as_mut() {
            multiple_idx[self.active_idx] ^= true;
        }
    }
}

struct TabulatedListState {
    window: GuiWin,
    pane: MuxPane,
    selector_state: SelectorState,
    root_node: Rc<RefCell<TrieNode>>,
    cur_node: Rc<RefCell<TrieNode>>,
    context: Option<TransientContext>,
    arguments: Vec<TransientArgument>,
    changes: Vec<Change>,
}

impl TabulatedListState {
    fn new(args: &TabulatedList, window: GuiWin, pane: MuxPane, size: &ScreenSize) -> Self {
        let mut trie_node = TrieNode::new();

        let context_size = args
            .context
            .as_ref()
            .map_or_else(|| 0, |v| v.entries.len() + 2);

        let positional_args_size = args.actions.len() + 1;

        let overhead = context_size + positional_args_size + 2;

        let selector_state =
            SelectorState::new(args.choices.clone(), size, overhead, args.multiple);

        let mut arguments = vec![];

        for positional_arg in &args.actions {
            trie_node.add_word(&positional_arg.key, positional_arg.clone());
            arguments.push(positional_arg.clone());
        }

        let root_node = Rc::new(RefCell::new(trie_node));
        let cur_node = Rc::clone(&root_node);

        Self {
            window,
            pane,
            selector_state,
            root_node,
            cur_node,
            context: args.context.clone(),
            arguments,
            changes: vec![Change::CursorVisibility(CursorVisibility::Hidden)],
        }
    }

    fn render(&mut self, term: &mut TermWizTerminal) -> termwiz::Result<()> {
        self.changes.append(&mut vec![
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::Absolute(self.selector_state.selector_size + 1),
            },
            Change::Text("─".repeat(self.selector_state.cols)),
            Change::Text("\r\n".to_string()),
        ]);

        if let Some(context) = self.context.as_ref() {
            self.changes
                .push(Change::Text(format!("{}\r\n", context.header)));
            for entry in &context.entries {
                self.changes
                    .push(Change::Text(format!("{}: {}\r\n", entry.label, entry.id)));
            }
            self.changes.push(Change::Text("\r\n".to_string()));
        }

        self.changes.push(Change::Text(format!("Arguments")));
        for positional_arg in &self.arguments {
            self.changes.push(Change::Text(format!(
                "\r\n{} {}",
                positional_arg.key, positional_arg.description
            )));
        }

        self.selector(term)?;

        Ok(())
    }

    fn selector(&mut self, term: &mut TermWizTerminal) -> anyhow::Result<()> {
        let cols = self.selector_state.cols;
        let max_width = cols.saturating_sub(6);
        let changes = &mut self.changes;
        let selector_state = &self.selector_state;
        changes.push(Change::CursorPosition {
            x: Position::Absolute(0),
            y: Position::Absolute(0),
        });

        let multiple_idx = &selector_state.multiple_idx;

        let max_items = self.selector_state.max_items;

        for (row_num, (entry_idx, entry)) in selector_state
            .choices
            .iter()
            .enumerate()
            .skip(self.selector_state.top_row)
            .enumerate()
        {
            if row_num > max_items {
                break;
            }

            if row_num != 0 {
                changes.push(Change::Text("\r\n".to_string()));
            }

            let mut attr = CellAttributes::blank();

            if let Some(multiple_idx) = multiple_idx {
                if multiple_idx[entry_idx] {
                    changes.append(&mut vec![
                        Change::Attribute(AttributeChange::Background(AnsiColor::Purple.into())),
                        Change::Text(" ".to_string()),
                        Change::Attribute(AttributeChange::Background(ColorAttribute::Default)),
                    ]);
                } else {
                    changes.push(Change::Text(" ".to_string()));
                }
            }

            if entry_idx == selector_state.active_idx {
                changes.push(AttributeChange::Reverse(true).into());
                attr.set_reverse(true);
            }

            changes.push(Change::Text("    ".to_string()));
            let mut line = crate::tabbar::parse_status_text(entry, attr.clone());
            if line.len() > max_width {
                line.resize(max_width, termwiz::surface::SEQ_ZERO);
            }
            changes.append(&mut line.changes(&attr));
            changes.push(Change::Text(" ".to_string()));
            if entry_idx == selector_state.active_idx {
                changes.push(AttributeChange::Reverse(false).into());
            }
            changes.push(Change::AllAttributes(CellAttributes::default()));
        }
        term.render(changes)?;
        changes.clear();

        Ok(())
    }

    fn run_loop(&mut self, term: &mut TermWizTerminal) -> anyhow::Result<()> {
        while let Ok(Some(event)) = term.poll_input(None) {
            match event {
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('G' | 'C' | 'D' | '['),
                    modifiers: Modifiers::CTRL,
                })
                | InputEvent::Key(KeyEvent {
                    key: KeyCode::Escape,
                    ..
                }) => {
                    break;
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('P' | 'K'),
                    modifiers: Modifiers::CTRL,
                }) => {
                    self.selector_state.move_up();
                    self.selector(term)?;
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('N' | 'J'),
                    modifiers: Modifiers::CTRL,
                }) => {
                    self.selector_state.move_down();
                    self.selector(term)?;
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Tab,
                    modifiers: _,
                }) => {
                    self.selector_state.toggle_multiple_idx();
                    self.selector(term)?;
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char(c),
                    ..
                }) => {
                    let cur_node = Rc::clone(&self.cur_node);
                    let cur_node = cur_node.borrow();
                    match cur_node.find_char(c) {
                        Some(cur_node) => {
                            let cur_node_borrowed = cur_node.borrow();
                            if cur_node_borrowed.is_end_of_word {
                                if let Some(positional_arg) = cur_node_borrowed.entry.as_ref() {
                                    let name = match *positional_arg.action {
                                            KeyAssignment::EmitEvent(ref id) => id,
                                            _ => anyhow::bail!("TabulatedList requires action to be defined by wezterm.action_callback")
                                        };
                                    self.trigger_event(name);
                                    break;
                                }
                            } else {
                                self.cur_node = Rc::clone(&cur_node);
                            }
                        }
                        None => {
                            self.cur_node = Rc::clone(&self.root_node);
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(())
    }

    fn trigger_event(&self, name: &str) {
        let name = name.to_string();
        let window = self.window.clone();
        let pane = self.pane;
        let selector_state = &self.selector_state;

        let choices = if let Some(multiple_idx) = selector_state.multiple_idx.as_ref() {
            multiple_idx
                .iter()
                .enumerate()
                .filter(|(_, val)| **val)
                .map(|(idx, _)| selector_state.choices[idx].clone())
                .collect()
        } else {
            vec![selector_state.choices[selector_state.active_idx].clone()]
        };
        let result = TabulatedListResult { choices };
        promise::spawn::spawn_into_main_thread(async move {
            trampoline(name, window, pane, result);
            anyhow::Result::<()>::Ok(())
        })
        .detach();
    }
}

#[derive(FromDynamic, ToDynamic)]
struct TabulatedListResult {
    choices: Vec<String>,
}
impl_lua_conversion_dynamic!(TabulatedListResult);

fn trampoline(name: String, window: GuiWin, pane: MuxPane, result: TabulatedListResult) {
    promise::spawn::spawn(async move {
        config::with_lua_config_on_main_thread(move |lua| do_event(lua, name, window, pane, result))
            .await
    })
    .detach();
}

async fn do_event(
    lua: Option<Rc<mlua::Lua>>,
    name: String,
    window: GuiWin,
    pane: MuxPane,
    result: TabulatedListResult,
) -> anyhow::Result<()> {
    if let Some(lua) = lua {
        let args = lua.pack_multi((window, pane, result))?;

        if let Err(err) = config::lua::emit_event(&lua, (name.clone(), args)).await {
            log::error!("while processing {} event: {:#}", name, err);
        }
    }

    Ok(())
}

pub fn show_tabulated_list_overlay(
    mut term: TermWizTerminal,
    args: TabulatedList,
    window: GuiWin,
    pane: MuxPane,
) -> anyhow::Result<()> {
    term.no_grab_mouse_in_raw_mode();
    let size = term.get_screen_size()?;
    let mut state = TabulatedListState::new(&args, window, pane, &size);

    state.render(&mut term)?;
    state.run_loop(&mut term)?;
    Ok(())
}
