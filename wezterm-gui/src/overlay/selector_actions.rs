use crate::overlay::selector::{matcher_pattern, matcher_score};
use crate::scripting::guiwin::GuiWin;
use config::keyassignment::{
    InputSelectorEntry, KeyAssignment, SelectorActions, TransientArgument, TransientContext,
};
use config::{configuration, AnsiColor, ColorAttribute};
use luahelper::impl_lua_conversion_dynamic;
use mux::termwiztermtab::TermWizTerminal;
use mux_lua::MuxPane;
use rayon::prelude::*;
use std::collections::HashMap;
use std::rc::Rc;
use termwiz::input::{InputEvent, KeyCode, KeyEvent};
use termwiz::surface::{Change, CursorVisibility, Position};
use termwiz::terminal::{ScreenSize, Terminal};
use termwiz_funcs::truncate_right;
use wezterm_dynamic::{FromDynamic, ToDynamic};
use wezterm_term::{AttributeChange, CellAttributes};
use window::Modifiers;

struct TrieNode<'a> {
    children: HashMap<char, Box<TrieNode<'a>>>,
    entry: Option<&'a TransientArgument>,
}

impl<'a> TrieNode<'a> {
    fn new() -> Self {
        Self {
            children: HashMap::new(),
            entry: None,
        }
    }

    fn add_word(&mut self, word: &str, entry: &'a TransientArgument) {
        let mut current = self;
        for ch in word.chars() {
            current = current
                .children
                .entry(ch)
                .or_insert_with(|| Box::new(TrieNode::new()));
        }
        current.entry = Some(entry);
    }

    fn find_char(&self, c: char) -> Option<&TrieNode<'_>> {
        self.children.get(&c).map(|child| child.as_ref())
    }
}

struct SelectorActionsColors {
    action_key_fg: ColorAttribute,
    multiple_marker_bg: ColorAttribute,
}

impl SelectorActionsColors {
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
struct SelectorEntry<'a> {
    delegate: &'a InputSelectorEntry,
    idx: usize,
}

struct ArgumentSection<'a> {
    header: String,
    arguments: Vec<&'a TransientArgument>,
}

struct SelectorState<'a> {
    active_idx: usize,
    max_items: usize,
    top_row: usize,
    choices: &'a Vec<SelectorEntry<'a>>,
    cols: usize,
    selector_size: usize,
    multiple_idx: Option<Vec<bool>>,
    filtered_entries: Vec<&'a SelectorEntry<'a>>,
    filtering: bool,
    filter_term: String,
    description: String,
    fuzzy_description: String,
    window: GuiWin,
    pane: MuxPane,
    root_node: &'a TrieNode<'a>,
    traversed_nodes: Vec<&'a TrieNode<'a>>,
    context: Option<&'a TransientContext>,
    changes: Vec<Change>,
    colors: SelectorActionsColors,
    section: ArgumentSection<'a>,
    cancel: Option<Box<KeyAssignment>>,
    repeat: [u8; 2],
}

impl<'a> SelectorState<'a> {
    fn new(
        args: &'a SelectorActions,
        window: GuiWin,
        pane: MuxPane,
        size: &ScreenSize,
        trie_node: &'a TrieNode<'_>,
        choices: &'a Vec<SelectorEntry<'_>>,
    ) -> Self {
        let context_size = args
            .context
            .as_ref()
            .map_or_else(|| 0, |v| v.entries.len() + 2);

        let positional_args_size = args.section.arguments.len() + 1;

        let overhead = context_size + positional_args_size + 3;

        let max_items = size.rows.saturating_sub(overhead);
        let selector_size = choices.len().min(max_items);

        let multiple_idx = args.multiple.then(|| vec![false; choices.len()]);
        let filtered_entries = choices.iter().collect();

        let arguments: Vec<&TransientArgument> = args.section.arguments.iter().collect();
        let section = ArgumentSection {
            header: args
                .section
                .header
                .clone()
                .unwrap_or_else(|| "Default".to_string()),
            arguments,
        };

        let fuzzy_description = args
            .fuzzy_description
            .clone()
            .unwrap_or_else(|| args.description.clone());

        let changes = if args.fuzzy {
            vec![]
        } else {
            vec![Change::CursorVisibility(CursorVisibility::Hidden)]
        };

        SelectorState {
            active_idx: 0,
            max_items,
            top_row: 0,
            choices,
            cols: size.cols,
            selector_size,
            multiple_idx,
            filtered_entries,
            filtering: args.fuzzy,
            filter_term: String::new(),
            description: args.description.clone(),
            fuzzy_description,
            window,
            pane,
            root_node: trie_node,
            traversed_nodes: vec![trie_node],
            context: args.context.as_ref(),
            changes,
            colors: SelectorActionsColors::new(),
            section,
            cancel: args.cancel.clone(),
            repeat: [1, 1],
        }
    }

    fn render_constants(&mut self) -> termwiz::Result<()> {
        if let Some(context) = self.context.as_ref() {
            self.changes.append(&mut vec![
                Change::Text(context.header.clone()),
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

        self.changes.push(Change::Text(self.section.header.clone()));
        self.changes
            .push(Change::AllAttributes(CellAttributes::default()));
        for positional_arg in &self.section.arguments {
            self.changes.append(&mut vec![
                Change::Text("\r\n".to_string()),
                Change::Attribute(AttributeChange::Foreground(self.colors.action_key_fg)),
                Change::Text(positional_arg.key.clone()),
                Change::Attribute(AttributeChange::Foreground(ColorAttribute::Default)),
                Change::Text(format!(" {}", positional_arg.description)),
                Change::AllAttributes(CellAttributes::default()),
            ]);
        }

        self.changes.push(Change::CursorPosition {
            x: Position::Absolute(0),
            y: Position::EndRelative(self.selector_size + 2),
        });
        self.changes.push(Change::Text("─".repeat(self.cols)));

        Ok(())
    }

    fn move_up(&mut self) {
        self.active_idx = self.active_idx.saturating_sub(self.repeat[0] as usize);
        if self.active_idx < self.top_row {
            self.top_row = self.active_idx;
        }
    }

    fn move_down(&mut self) {
        self.active_idx =
            (self.active_idx + self.repeat[0] as usize).min(self.filtered_entries.len() - 1);
        if self.active_idx > self.top_row + self.max_items {
            self.top_row = self.active_idx.saturating_sub(self.max_items);
        }
    }

    fn toggle_multiple_idx_and_move(&mut self, down: bool) {
        // start_idx and end_idx are guaranteed to be within bounds of filtered_entries if
        // filtered_entries is not empty
        if !self.filtered_entries.is_empty() && self.multiple_idx.as_ref().is_some() {
            let init_active_idx = self.active_idx;
            let (start_idx, end_idx) = if down {
                self.move_down();
                let end_idx = if self.active_idx != init_active_idx {
                    self.active_idx.saturating_sub(1)
                } else {
                    init_active_idx
                };
                (init_active_idx, end_idx)
            } else {
                self.move_up();
                let start_idx = if self.active_idx != init_active_idx {
                    self.active_idx + 1
                } else {
                    init_active_idx
                };
                (start_idx, init_active_idx)
            };

            let multiple_idx = self.multiple_idx.as_mut().unwrap();
            for entry in &self.filtered_entries[start_idx..=end_idx] {
                multiple_idx[entry.idx] ^= true;
            }
        }
    }

    fn set_search(&mut self, val: bool) {
        self.filtering = val;
        let cursor_visibility = if val {
            CursorVisibility::Visible
        } else {
            CursorVisibility::Hidden
        };
        self.changes
            .push(Change::CursorVisibility(cursor_visibility));
    }

    fn toggle_search(&mut self) {
        self.set_search(!self.filtering);
    }

    fn set_filtered_entries_multiple_marker(&mut self, mark: bool) {
        if let Some(multiple_idx) = self.multiple_idx.as_mut() {
            for entry in &self.filtered_entries {
                multiple_idx[entry.idx] = mark;
            }
        }
    }

    fn toggle_filtered_entries_multiple_marker(&mut self) {
        if let Some(multiple_idx) = self.multiple_idx.as_mut() {
            for entry in &self.filtered_entries {
                multiple_idx[entry.idx] ^= true;
            }
        }
    }

    fn update_filter(&mut self) {
        if self.filter_term.is_empty() {
            self.filtered_entries = self.choices.iter().collect();
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
                let score = matcher_score(&pattern, &entry.delegate.label)?;
                Some(MatchResult { row_idx, score })
            })
            .collect();

        scores.sort_by(|a, b| a.score.cmp(&b.score).reverse());

        for result in scores {
            self.filtered_entries.push(&self.choices[result.row_idx]);
        }

        self.active_idx = 0;
        self.top_row = 0;
    }

    fn render(&mut self, term: &mut TermWizTerminal) -> anyhow::Result<()> {
        let changes = &mut self.changes;
        let max_width = self.cols.saturating_sub(6);

        changes.append(&mut vec![
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::EndRelative(self.selector_size + 1),
            },
            Change::ClearToEndOfScreen(ColorAttribute::Default),
            Change::Text(format!(
                "{}\r\n",
                truncate_right(&self.description, max_width)
            )),
        ]);

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
            let mut line = crate::tabbar::parse_status_text(&entry.delegate.label, attr.clone());
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

        if self.filtering {
            changes.append(&mut vec![
                Change::CursorPosition {
                    x: Position::Absolute(0),
                    y: Position::EndRelative(self.selector_size + 1),
                },
                Change::ClearToEndOfLine(ColorAttribute::Default),
                Change::Text(truncate_right(
                    &format!("{}{}", self.fuzzy_description, self.filter_term),
                    max_width,
                )),
            ]);
        }

        term.render(changes)?;
        changes.clear();

        Ok(())
    }

    fn run_loop(&mut self, term: &mut TermWizTerminal) -> anyhow::Result<()> {
        while let Ok(Some(event)) = term.poll_input(None) {
            self.repeat[0] = self.repeat[1];
            if self.repeat[1] != 1 {
                self.repeat[1] = 1;
            }
            match event {
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('P' | 'K'),
                    modifiers: Modifiers::CTRL,
                }) => {
                    self.move_up();
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('N' | 'J'),
                    modifiers: Modifiers::CTRL,
                }) => {
                    self.move_down();
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('/'),
                    modifiers: Modifiers::CTRL,
                }) => {
                    self.toggle_search();
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Backspace,
                    modifiers: _,
                }) if self.filtering => {
                    if self.filter_term.pop().is_some() {
                        self.update_filter();
                    } else {
                        continue;
                    }
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Backspace,
                    modifiers: _,
                }) => {
                    if self.traversed_nodes.len() >= 2 {
                        self.traversed_nodes.pop();
                    }
                    continue;
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('G' | 'C'),
                    modifiers: Modifiers::CTRL,
                })
                | InputEvent::Key(KeyEvent {
                    key: KeyCode::Escape,
                    ..
                }) => {
                    if let Some(key_assignment) = self.cancel.as_ref() {
                        if let KeyAssignment::EmitEvent(ref id) = **key_assignment {
                            self.trigger_event(id, None);
                        }
                    }
                    break;
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('A'),
                    modifiers: Modifiers::CTRL,
                }) => {
                    self.set_filtered_entries_multiple_marker(true);
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('D'),
                    modifiers: Modifiers::CTRL,
                }) => {
                    self.set_filtered_entries_multiple_marker(false);
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('T'),
                    modifiers: Modifiers::CTRL,
                }) => {
                    self.toggle_filtered_entries_multiple_marker();
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char(c),
                    modifiers: _,
                }) if self.filtering => {
                    self.filter_term.push(c);
                    self.update_filter();
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('j'),
                    modifiers: Modifiers::NONE,
                }) if !self
                    .traversed_nodes
                    .last()
                    .as_ref()
                    .unwrap()
                    .children
                    .contains_key(&'j') =>
                {
                    self.move_down();
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('k'),
                    modifiers: Modifiers::NONE,
                }) if !self
                    .traversed_nodes
                    .last()
                    .as_ref()
                    .unwrap()
                    .children
                    .contains_key(&'k') =>
                {
                    self.move_up();
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('/'),
                    modifiers: Modifiers::NONE,
                }) if !self
                    .traversed_nodes
                    .last()
                    .as_ref()
                    .unwrap()
                    .children
                    .contains_key(&'/') =>
                {
                    self.set_search(true);
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char(c),
                    modifiers: Modifiers::NONE,
                }) if c.is_ascii_digit()
                    && !self
                        .traversed_nodes
                        .last()
                        .as_ref()
                        .unwrap()
                        .children
                        .contains_key(&c) =>
                {
                    self.repeat[1] = c as u8 - '0' as u8;
                    continue;
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char(c),
                    ..
                }) => {
                    let cur_node = self
                        .traversed_nodes
                        .last()
                        .expect("Root node is always traversed");

                    let cur_node = match cur_node.find_char(c) {
                        Some(cur_node) => cur_node,
                        None => {
                            self.traversed_nodes = vec![self.root_node];
                            continue;
                        }
                    };

                    let positional_arg = match cur_node.entry.as_ref() {
                        Some(positional_arg) => positional_arg,
                        None => {
                            self.traversed_nodes.push(cur_node);
                            continue;
                        }
                    };

                    let name = match *positional_arg.action {
                        KeyAssignment::EmitEvent(ref id) => id,
                        _ => anyhow::bail!("SelectorActions requires action to be defined by wezterm.action_callback")
                    };

                    let mut choices: Vec<InputSelectorEntry> = vec![];

                    if let Some(multiple_idx) = self.multiple_idx.as_ref() {
                        choices = multiple_idx
                            .iter()
                            .enumerate()
                            .filter(|(_, val)| **val)
                            .map(|(idx, _)| InputSelectorEntry {
                                label: self.choices[idx].delegate.label.clone(),
                                id: self.choices[idx].delegate.id.clone(),
                            })
                            .collect();
                    }

                    if choices.is_empty() && self.filtered_entries.is_empty() {
                        self.traversed_nodes = vec![self.root_node];
                        continue;
                    }

                    if choices.is_empty() {
                        let entry = &self.choices[self.filtered_entries[self.active_idx].idx];
                        choices = vec![InputSelectorEntry {
                            label: entry.delegate.label.clone(),
                            id: entry.delegate.id.clone(),
                        }];
                    }

                    let result = SelectorActionsResult { choices };
                    self.trigger_event(name, Some(result));
                    break;
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Tab,
                    modifiers: Modifiers::NONE,
                }) => {
                    self.toggle_multiple_idx_and_move(true);
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Tab,
                    modifiers: Modifiers::SHIFT,
                }) => {
                    self.toggle_multiple_idx_and_move(false);
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Enter,
                    ..
                }) => {
                    if self.filtering {
                        self.set_search(false);
                    } else {
                        continue;
                    }
                }
                _ => continue,
            }
            self.render(term)?;
        }

        Ok(())
    }

    fn trigger_event(&self, name: &str, result: Option<SelectorActionsResult>) {
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
struct SelectorActionsResult {
    choices: Vec<InputSelectorEntry>,
}
impl_lua_conversion_dynamic!(SelectorActionsResult);

fn create_trie<'a>(args: &'a SelectorActions, trie_node: &mut TrieNode<'a>) {
    for positional_arg in &args.section.arguments {
        trie_node.add_word(&positional_arg.key, positional_arg);
    }
}

fn trampoline(name: String, window: GuiWin, pane: MuxPane, result: Option<SelectorActionsResult>) {
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
    result: Option<SelectorActionsResult>,
) -> anyhow::Result<()> {
    if let Some(lua) = lua {
        let args = if let Some(result) = result {
            lua.pack_multi((window, pane, result))?
        } else {
            lua.pack_multi((window, pane))?
        };

        if let Err(err) = config::lua::emit_event(&lua, (name.clone(), args)).await {
            log::error!("while processing {} event: {:#}", name, err);
        }
    }

    Ok(())
}

pub fn show_selector_actions_overlay(
    mut term: TermWizTerminal,
    args: SelectorActions,
    window: GuiWin,
    pane: MuxPane,
) -> anyhow::Result<()> {
    term.no_grab_mouse_in_raw_mode();
    let size = term.get_screen_size()?;

    let choices: Vec<SelectorEntry<'_>> = args
        .choices
        .iter()
        .enumerate()
        .map(|(idx, delegate)| SelectorEntry { delegate, idx })
        .collect();

    let mut trie_node = TrieNode::new();
    create_trie(&args, &mut trie_node);

    let mut state = SelectorState::new(&args, window, pane, &size, &trie_node, &choices);

    state.render_constants()?;
    state.render(&mut term)?;
    state.run_loop(&mut term)?;
    Ok(())
}
