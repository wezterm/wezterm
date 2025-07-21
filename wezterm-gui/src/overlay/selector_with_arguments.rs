use crate::overlay::selector::{matcher_pattern, matcher_score};
use crate::scripting::guiwin::GuiWin;
use config::configuration;
use config::{
    keyassignment::{KeyAssignment, SelectorWithArguments, TransientArgument, TransientContext},
    AnsiColor, ColorAttribute,
};
use luahelper::impl_lua_conversion_dynamic;
use mux::termwiztermtab::TermWizTerminal;
use mux_lua::MuxPane;
use rayon::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use termwiz::{
    input::{InputEvent, KeyCode, KeyEvent},
    surface::{Change, CursorVisibility, Position},
    terminal::{ScreenSize, Terminal},
};
use termwiz_funcs::truncate_right;
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

struct SelectorWithArgumentsColors {
    action_key_fg: ColorAttribute,
    multiple_marker_bg: ColorAttribute,
}

impl SelectorWithArgumentsColors {
    fn new() -> Self {
        let config = configuration();
        let colors = &config.resolved_palette;

        Self {
            action_key_fg: colors
                .transient_entry_key_fg
                .unwrap_or(AnsiColor::Purple.into())
                .into(),
            multiple_marker_bg: colors
                .selector_multiple_marker_bg
                .unwrap_or(AnsiColor::Purple.into())
                .into(),
        }
    }
}

#[derive(Clone)]
struct SelectorEntry {
    idx: usize,
    label: String,
}

struct SelectorState {
    active_idx: usize,
    max_items: usize,
    top_row: usize,
    choices: Vec<SelectorEntry>,
    cols: usize,
    selector_size: usize,
    multiple_idx: Option<Vec<bool>>,
    filtered_entries: Vec<SelectorEntry>,
    filtering: bool,
    filter_term: String,
    description: String,
    fuzzy_description: Option<String>,
    window: GuiWin,
    pane: MuxPane,
    root_node: Rc<RefCell<TrieNode>>,
    cur_node: Rc<RefCell<TrieNode>>,
    context: Option<TransientContext>,
    arguments: Vec<TransientArgument>,
    changes: Vec<Change>,
    colors: SelectorWithArgumentsColors,
    argument_header: String,
}

impl SelectorState {
    fn new(args: &SelectorWithArguments, window: GuiWin, pane: MuxPane, size: &ScreenSize) -> Self {
        let context_size = args
            .context
            .as_ref()
            .map_or_else(|| 0, |v| v.entries.len() + 2);

        let positional_args_size = args.actions.len() + 1;

        let overhead = context_size + positional_args_size + 3;

        let choices: Vec<SelectorEntry> = args
            .choices
            .iter()
            .enumerate()
            .map(|(idx, label)| SelectorEntry {
                idx,
                label: label.clone(),
            })
            .collect();

        let max_items = size.rows.saturating_sub(overhead);
        let selector_size = choices.len().min(max_items);

        let multiple_idx: Option<Vec<bool>> = args
            .multiple
            .then(|| choices.iter().map(|_| false).collect());

        let filtered_entries = choices.clone();

        let mut arguments = vec![];
        let mut trie_node = TrieNode::new();

        for positional_arg in &args.actions {
            trie_node.add_word(&positional_arg.key, positional_arg.clone());
            arguments.push(positional_arg.clone());
        }

        let root_node = Rc::new(RefCell::new(trie_node));
        let cur_node = Rc::clone(&root_node);

        SelectorState {
            active_idx: 0,
            max_items,
            top_row: 0,
            choices,
            cols: size.cols,
            selector_size,
            multiple_idx,
            filtered_entries,
            filtering: false,
            filter_term: String::new(),
            description: args.description.clone(),
            fuzzy_description: args.fuzzy_description.clone(),
            window,
            pane,
            root_node,
            cur_node,
            context: args.context.clone(),
            arguments,
            changes: vec![Change::CursorVisibility(CursorVisibility::Hidden)],
            colors: SelectorWithArgumentsColors::new(),
            argument_header: args
                .action_header
                .clone()
                .unwrap_or("Arguments".to_string()),
        }
    }

    fn render_constants(&mut self) -> termwiz::Result<()> {
        if let Some(context) = self.context.as_ref() {
            self.changes.append(&mut vec![
                Change::Text(context.header.to_string()),
                Change::AllAttributes(CellAttributes::default()),
            ]);
            for entry in &context.entries {
                self.changes.append(&mut vec![
                    Change::Text(format!("\r\n{}", entry.label)),
                    Change::AllAttributes(CellAttributes::default()),
                    Change::Text(format!(": {}", entry.id)),
                    Change::AllAttributes(CellAttributes::default()),
                ]);
            }
            self.changes.push(Change::Text("\r\n\r\n".to_string()));
        }

        self.changes
            .push(Change::Text(self.argument_header.clone()));
        self.changes
            .push(Change::AllAttributes(CellAttributes::default()));
        for positional_arg in &self.arguments {
            self.changes.append(&mut vec![
                Change::Text("\r\n".to_string()),
                Change::Attribute(AttributeChange::Foreground(self.colors.action_key_fg)),
                Change::Text(positional_arg.key.to_string()),
                Change::Attribute(AttributeChange::Foreground(ColorAttribute::Default)),
                Change::Text(format!(" {}", positional_arg.description)),
                Change::AllAttributes(CellAttributes::default()),
            ]);
        }

        self.changes
            .push(Change::Text(format!("\r\n{}\r\n", "─".repeat(self.cols))));

        Ok(())
    }

    fn move_up(&mut self) {
        self.active_idx = self.active_idx.saturating_sub(1);
        if self.active_idx < self.top_row {
            self.top_row = self.active_idx;
        }
    }

    fn move_down(&mut self) {
        self.active_idx = (self.active_idx + 1).min(self.filtered_entries.len() - 1);
        if self.active_idx > self.top_row + self.max_items {
            self.top_row = self.active_idx.saturating_sub(self.max_items);
        }
    }

    fn toggle_multiple_idx(&mut self) {
        if let Some(multiple_idx) = self.multiple_idx.as_mut() {
            if let Some(entry) = self.filtered_entries.get(self.active_idx) {
                multiple_idx[entry.idx] ^= true;
            }
        }
    }

    fn toggle_search(&mut self) {
        self.filtering ^= true;
    }

    fn update_filter(&mut self) {
        if self.filter_term.is_empty() {
            self.filtered_entries = self.choices.clone();
            return;
        }

        self.filtered_entries.clear();

        struct MatchResult {
            row_idx: usize,
            score: u32,
        }

        let pattern = matcher_pattern(&self.filter_term);

        let mut scores: Vec<MatchResult> = self
            .choices
            .par_iter()
            .enumerate()
            .filter_map(|(row_idx, entry)| {
                let score = matcher_score(&pattern, &entry.label)?;
                Some(MatchResult { row_idx, score })
            })
            .collect();

        scores.sort_by(|a, b| a.score.cmp(&b.score).reverse());

        for result in scores {
            self.filtered_entries
                .push(self.choices[result.row_idx].clone());
        }

        self.active_idx = 0;
        self.top_row = 0;
    }

    fn render(&mut self, term: &mut TermWizTerminal) -> anyhow::Result<()> {
        let changes = &mut self.changes;

        let cols = self.cols;
        let max_width = cols.saturating_sub(6);
        changes.append(&mut vec![
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::EndRelative(self.selector_size + 1),
            },
            Change::ClearToEndOfScreen(ColorAttribute::Default),
        ]);

        if !self.filtering {
            changes.push(Change::Text(format!(
                "{}\r\n",
                truncate_right(&self.description, max_width)
            )));
        } else {
            let description = self.fuzzy_description.as_ref().unwrap_or(&self.description);
            changes.push(Change::Text(truncate_right(
                &format!("{}{}\r\n", description, self.filter_term),
                max_width,
            )));
        }

        let max_items = self.max_items;

        for (row_num, (entry_idx, entry)) in self
            .filtered_entries
            .iter()
            .enumerate()
            .skip(self.top_row)
            .enumerate()
        {
            if row_num > max_items {
                break;
            }

            if row_num != 0 {
                changes.push(Change::Text("\r\n".to_string()));
            }

            let mut attr = CellAttributes::blank();

            if let Some(multiple_idx) = self.multiple_idx.as_ref() {
                if multiple_idx[self.filtered_entries[entry_idx].idx] {
                    changes.append(&mut vec![
                        Change::Attribute(AttributeChange::Background(
                            self.colors.multiple_marker_bg,
                        )),
                        Change::Text(" ".to_string()),
                        Change::Attribute(AttributeChange::Background(ColorAttribute::Default)),
                    ]);
                } else {
                    changes.push(Change::Text(" ".to_string()));
                }
            }

            if entry_idx == self.active_idx {
                changes.push(AttributeChange::Reverse(true).into());
                attr.set_reverse(true);
            }

            changes.push(Change::Text("    ".to_string()));
            let mut line = crate::tabbar::parse_status_text(&entry.label, attr.clone());
            if line.len() > max_width {
                line.resize(max_width, termwiz::surface::SEQ_ZERO);
            }
            changes.append(&mut line.changes(&attr));
            changes.push(Change::Text(" ".to_string()));
            if entry_idx == self.active_idx {
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
                    self.move_up();
                    self.render(term)?;
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('N' | 'J'),
                    modifiers: Modifiers::CTRL,
                }) => {
                    self.move_down();
                    self.render(term)?;
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('/'),
                    modifiers: Modifiers::CTRL,
                }) => {
                    self.toggle_search();
                    self.render(term)?;
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Tab,
                    modifiers: Modifiers::NONE,
                }) => {
                    self.toggle_multiple_idx();
                    self.move_down();
                    self.render(term)?;
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Tab,
                    modifiers: Modifiers::SHIFT,
                }) => {
                    self.toggle_multiple_idx();
                    self.move_up();
                    self.render(term)?;
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Backspace,
                    modifiers: _,
                }) if self.filtering => {
                    if self.filter_term.pop().is_some() {
                        self.update_filter();
                        self.render(term)?;
                    }
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char(c),
                    modifiers: _,
                }) if self.filtering => {
                    self.filter_term.push(c);
                    self.update_filter();
                    self.render(term)?;
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

                            if !cur_node_borrowed.is_end_of_word {
                                self.cur_node = Rc::clone(&cur_node);
                                continue;
                            }

                            let positional_arg = match cur_node_borrowed.entry.as_ref() {
                                Some(positional_arg) => positional_arg,
                                None => continue,
                            };

                            let name = match *positional_arg.action {
                                KeyAssignment::EmitEvent(ref id) => id,
                                _ => anyhow::bail!("SelectorWithArguments requires action to be defined by wezterm.action_callback")
                            };

                            let choices: Vec<String> = if let Some(multiple_idx) =
                                self.multiple_idx.as_ref()
                            {
                                multiple_idx
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, val)| **val)
                                    .map(|(idx, _)| self.choices[idx].label.clone())
                                    .collect()
                            } else {
                                if !self.filtered_entries.is_empty() {
                                    vec![self.choices[self.filtered_entries[self.active_idx].idx]
                                        .label
                                        .clone()]
                                } else {
                                    vec![]
                                }
                            };

                            if !choices.is_empty() {
                                let result = SelectorWithArgumentsResult { choices };
                                self.trigger_event(name, result);
                                break;
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

    fn trigger_event(&self, name: &str, result: SelectorWithArgumentsResult) {
        let name = name.to_string();
        let window = self.window.clone();
        let pane = self.pane;

        promise::spawn::spawn_into_main_thread(async move {
            trampoline(name, window, pane, result);
            anyhow::Result::<()>::Ok(())
        })
        .detach();
    }
}

#[derive(FromDynamic, ToDynamic)]
struct SelectorWithArgumentsResult {
    choices: Vec<String>,
}
impl_lua_conversion_dynamic!(SelectorWithArgumentsResult);

fn trampoline(name: String, window: GuiWin, pane: MuxPane, result: SelectorWithArgumentsResult) {
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
    result: SelectorWithArgumentsResult,
) -> anyhow::Result<()> {
    if let Some(lua) = lua {
        let args = lua.pack_multi((window, pane, result))?;

        if let Err(err) = config::lua::emit_event(&lua, (name.clone(), args)).await {
            log::error!("while processing {} event: {:#}", name, err);
        }
    }

    Ok(())
}

pub fn show_selector_with_arguments_overlay(
    mut term: TermWizTerminal,
    args: SelectorWithArguments,
    window: GuiWin,
    pane: MuxPane,
) -> anyhow::Result<()> {
    term.no_grab_mouse_in_raw_mode();
    let size = term.get_screen_size()?;
    let mut state = SelectorState::new(&args, window, pane, &size);

    state.render_constants()?;
    state.render(&mut term)?;
    state.run_loop(&mut term)?;
    Ok(())
}
