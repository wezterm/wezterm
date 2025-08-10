use crate::overlay::selector::{matcher_pattern, matcher_score};
use crate::scripting::guiwin::GuiWin;
use config::keyassignment::{
    KeyAssignment, TransientArgument as KTransientArgument, TransientContext as KTransientContext,
    TransientContextEntry as KTransientContextEntry,
    TransientCyclicSwitch as KTransientCyclicSwitch, TransientEntry as KTransientEntry,
    TransientMenu as KTransientMenu, TransientOption as KTransientOption,
    TransientSection as KTransientSection, TransientSwitch as KTransientSwitch,
};
use config::{configuration, AnsiColor, ColorAttribute};
use luahelper::impl_lua_conversion_dynamic;
use mux::termwiztermtab::TermWizTerminal;
use mux_lua::MuxPane;
use rayon::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use termwiz::input::{InputEvent, KeyCode, KeyEvent};
use termwiz::lineedit::{Action, BasicHistory, History, LineEditor, LineEditorHost};
use termwiz::surface::{Change, CursorVisibility, Position};
use termwiz::terminal::Terminal;
use termwiz_funcs::truncate_right;
use wezterm_dynamic::{FromDynamic, ToDynamic, Value};
use wezterm_term::{AttributeChange, CellAttributes, Intensity};
use window::Modifiers;

const ROW_OVERHEAD: usize = 6;

struct PromptHost {
    history: BasicHistory,
}

impl PromptHost {
    fn new() -> Self {
        Self {
            history: BasicHistory::default(),
        }
    }
}

impl LineEditorHost for PromptHost {
    fn history(&mut self) -> &mut dyn History {
        &mut self.history
    }

    fn resolve_action(
        &mut self,
        event: &InputEvent,
        editor: &mut LineEditor<'_>,
    ) -> Option<Action> {
        let (line, _cursor) = editor.get_line_and_cursor();
        if line.is_empty()
            && matches!(
                event,
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Escape,
                    ..
                })
            )
        {
            Some(Action::Cancel)
        } else {
            None
        }
    }
}

trait Renderable {
    fn render(
        &self,
        colors: &TransientColors,
        changes: &mut Vec<Change>,
        term: &mut TermWizTerminal,
        render_now: bool,
    ) -> termwiz::Result<()>;
}

struct SelectorState<'a> {
    active_idx: usize,
    max_items: usize,
    top_row: usize,
    filter_term: String,
    filtered_entries: Vec<String>,
    choices: Vec<String>,
    cols: usize,
    selector_size: usize,
    changes: &'a mut Vec<Change>,
    colors: &'a TransientColors,
    option: &'a TransientOption<'a>,
    row_entities: &'a Vec<Option<&'a RenderableEntity<'a>>>,
}

impl SelectorState<'_> {
    fn clear_selector(&mut self, term: &mut TermWizTerminal) -> anyhow::Result<()> {
        let rows = self.max_items.saturating_add(ROW_OVERHEAD);
        let start_row = self.selector_size + 2;
        let skip_rows = rows - start_row - 1;

        self.changes.append(&mut vec![
            Change::CursorVisibility(CursorVisibility::Hidden),
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::EndRelative(start_row),
            },
            Change::ClearToEndOfScreen(ColorAttribute::Default),
        ]);

        for renderable_entity in self.row_entities.iter().skip(skip_rows) {
            self.changes.push(Change::Text("\r\n".to_string()));
            if let Some(renderable_entity) = renderable_entity {
                renderable_entity.render(&self.colors, &mut self.changes, term)?;
            }
        }

        Ok(())
    }

    fn draw_separator_and_show_cursor(&mut self) {
        let cols = self.cols;
        self.changes.append(&mut vec![
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::EndRelative(2 + self.selector_size),
            },
            Change::ClearToEndOfScreen(ColorAttribute::Default),
            Change::Text("─".repeat(cols)),
            Change::Text("\r\n".to_string()),
            Change::CursorVisibility(CursorVisibility::Visible),
        ]);
    }

    fn render(&mut self, term: &mut TermWizTerminal) -> anyhow::Result<()> {
        let cols = self.cols;
        let max_width = cols.saturating_sub(6);

        let changes = &mut self.changes;

        let input_selector_size = self.selector_size;

        changes.append(&mut vec![
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::EndRelative(1 + input_selector_size),
            },
            Change::ClearToEndOfScreen(ColorAttribute::Default),
            Change::Text(truncate_right(
                &format!("{}: {}", self.option.delegate.description, self.filter_term),
                max_width,
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

            changes.push(Change::Text("\r\n".to_string()));

            let mut attr = CellAttributes::blank();

            if entry_idx == self.active_idx {
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
            if entry_idx == self.active_idx {
                changes.push(AttributeChange::Reverse(false).into());
            }
            changes.push(Change::AllAttributes(CellAttributes::default()));
        }
        changes.push(Change::CursorPosition {
            x: Position::Absolute(
                2 + self.option.delegate.description.len() + self.filter_term.len(),
            ),
            y: Position::EndRelative(1 + input_selector_size),
        });

        term.render(changes)?;
        changes.clear();

        Ok(())
    }

    fn run_loop(&mut self, term: &mut TermWizTerminal) -> anyhow::Result<()> {
        while let Ok(Some(event)) = term.poll_input(None) {
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
                    key: KeyCode::Backspace,
                    ..
                }) => {
                    if self.filter_term.pop().is_some() {
                        self.update_filter();
                    }
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('G' | 'C'),
                    modifiers: Modifiers::CTRL,
                })
                | InputEvent::Key(KeyEvent {
                    key: KeyCode::Escape,
                    ..
                }) => {
                    self.clear_selector(term)?;
                    break;
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char(c),
                    ..
                }) => {
                    self.filter_term.push(c);
                    self.update_filter();
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Enter,
                    ..
                }) => {
                    if let Some(entry) = self.filtered_entries.get(self.active_idx).cloned() {
                        self.option.value.replace(Some(entry));
                        self.clear_selector(term)?;
                        break;
                    }
                }
                _ => {}
            }
            self.render(term)?;
        }

        Ok(())
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
                let score = matcher_score(&pattern, &entry)?;
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
}

struct TrieNode<'a> {
    children: HashMap<char, Box<TrieNode<'a>>>,
    entry: Option<&'a RenderableEntity<'a>>,
}

impl<'a> TrieNode<'a> {
    fn new() -> Self {
        Self {
            children: HashMap::new(),
            entry: None,
        }
    }

    fn add_word(&mut self, word: &str, entry: &'a RenderableEntity<'a>) {
        let mut current = self;
        for ch in word.chars() {
            current = current
                .children
                .entry(ch)
                .or_insert_with(|| Box::new(TrieNode::new()));
        }
        current.entry = Some(entry);
    }

    fn find_char(&self, c: char) -> Option<&TrieNode> {
        self.children.get(&c).map(|child| child.as_ref())
    }
}

struct TransientColors {
    key_fg: ColorAttribute,
    flag_fg: ColorAttribute,
}

impl TransientColors {
    fn new() -> Self {
        let config = configuration();
        let colors = &config.resolved_palette;

        Self {
            key_fg: colors
                .transient_entry_key_fg
                .unwrap_or(AnsiColor::Purple.into())
                .into(),
            flag_fg: colors
                .transient_entry_flag_fg
                .unwrap_or(AnsiColor::Red.into())
                .into(),
        }
    }
}

struct TransientSwitch<'a> {
    delegate: &'a KTransientSwitch,
    value: RefCell<bool>,
    row: usize,
}

impl<'a> Renderable for TransientSwitch<'a> {
    fn render(
        &self,
        colors: &TransientColors,
        changes: &mut Vec<Change>,
        term: &mut TermWizTerminal,
        render_now: bool,
    ) -> termwiz::Result<()> {
        let delegate = self.delegate;
        changes.append(&mut vec![
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::Absolute(self.row),
            },
            Change::ClearToEndOfLine(ColorAttribute::Default),
            Change::Text("  ".to_string()),
            Change::Attribute(AttributeChange::Foreground(colors.key_fg)),
            Change::Text(format!("{}", delegate.key)),
            Change::Attribute(AttributeChange::Foreground(ColorAttribute::Default)),
            Change::Text(format!(" {} (", delegate.description)),
        ]);

        if *self.value.borrow() {
            changes.append(&mut vec![
                Change::Attribute(AttributeChange::Intensity(Intensity::Bold)),
                Change::Attribute(AttributeChange::Foreground(colors.flag_fg)),
                Change::Text(delegate.flag.to_string()),
                Change::AllAttributes(CellAttributes::default()),
            ]);
        } else {
            changes.push(Change::Text(delegate.flag.to_string()));
        }
        changes.push(Change::Text(")".to_string()));

        if render_now {
            term.render(changes)?;
            changes.clear();
        }

        Ok(())
    }
}

struct TransientOption<'a> {
    delegate: &'a KTransientOption,
    value: RefCell<Option<String>>,
    row: usize,
}

impl<'a> Renderable for TransientOption<'a> {
    fn render(
        &self,
        colors: &TransientColors,
        changes: &mut Vec<Change>,
        term: &mut TermWizTerminal,
        render_now: bool,
    ) -> termwiz::Result<()> {
        let delegate = self.delegate;
        changes.append(&mut vec![
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::Absolute(self.row),
            },
            Change::ClearToEndOfLine(ColorAttribute::Default),
            Change::Text("  ".to_string()),
            Change::Attribute(AttributeChange::Foreground(colors.key_fg)),
            Change::Text(format!("{}", delegate.key)),
            Change::Attribute(AttributeChange::Foreground(ColorAttribute::Default)),
            Change::Text(format!(" {} (", delegate.description)),
        ]);

        if let Some(val) = self.value.borrow().as_ref() {
            changes.append(&mut vec![
                Change::Attribute(AttributeChange::Intensity(Intensity::Bold)),
                Change::Attribute(AttributeChange::Foreground(colors.flag_fg)),
                Change::Text(format!("{}{}", delegate.flag, val)),
                Change::AllAttributes(CellAttributes::default()),
            ]);
        } else {
            changes.push(Change::Text(format!("{}", delegate.flag)));
        }

        changes.push(Change::Text(")".to_string()));

        if render_now {
            term.render(changes)?;
            changes.clear();
        }

        Ok(())
    }
}

struct TransientCyclicSwitch<'a> {
    delegate: &'a KTransientCyclicSwitch,
    active_idx: RefCell<Option<usize>>,
    row: usize,
}

impl<'a> Renderable for TransientCyclicSwitch<'a> {
    fn render(
        &self,
        colors: &TransientColors,
        changes: &mut Vec<Change>,
        term: &mut TermWizTerminal,
        render_now: bool,
    ) -> termwiz::Result<()> {
        let delegate = self.delegate;
        changes.append(&mut vec![
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::Absolute(self.row),
            },
            Change::ClearToEndOfLine(ColorAttribute::Default),
            Change::Text("  ".to_string()),
            Change::Attribute(AttributeChange::Foreground(colors.key_fg)),
            Change::Text(format!("{}", delegate.key)),
            Change::Attribute(AttributeChange::Foreground(ColorAttribute::Default)),
            Change::Text(format!(" {} (", delegate.description)),
        ]);

        if let Some(idx) = *self.active_idx.borrow() {
            changes.append(&mut vec![
                Change::Attribute(AttributeChange::Intensity(Intensity::Bold)),
                Change::Attribute(AttributeChange::Foreground(colors.flag_fg)),
                Change::Text(delegate.flag.to_string()),
                Change::AllAttributes(CellAttributes::default()),
            ]);
            if delegate.choices.first().is_some() {
                let mut prefix = " [";
                for (cur_idx, choice) in delegate.choices.iter().enumerate() {
                    if cur_idx == idx {
                        changes.append(&mut vec![
                            Change::Text(prefix.to_string()),
                            Change::Attribute(AttributeChange::Intensity(Intensity::Bold)),
                            Change::Attribute(AttributeChange::Foreground(colors.flag_fg)),
                            Change::Text(choice.to_string()),
                            Change::AllAttributes(CellAttributes::default()),
                        ]);
                    } else {
                        changes.push(Change::Text(format!("{}{}", prefix, choice)));
                    }
                    if cur_idx == 0 {
                        prefix = "|";
                    }
                }
                changes.push(Change::Text("]".to_string()));
            }
        } else {
            changes.push(Change::Text(delegate.flag.to_string()));
            if delegate.choices.first().is_some() {
                let mut prefix = " [";
                for (cur_idx, choice) in delegate.choices.iter().enumerate() {
                    changes.push(Change::Text(format!("{}{}", prefix, choice)));
                    if cur_idx == 0 {
                        prefix = "|";
                    }
                }
                changes.push(Change::Text("]".to_string()));
            }
        }
        changes.push(Change::Text(")".to_string()));

        if render_now {
            term.render(changes)?;
            changes.clear();
        }

        Ok(())
    }
}

struct TransientArgument<'a> {
    delegate: &'a KTransientArgument,
    row: usize,
}

impl<'a> Renderable for TransientArgument<'a> {
    fn render(
        &self,
        colors: &TransientColors,
        changes: &mut Vec<Change>,
        _term: &mut TermWizTerminal,
        _render_now: bool,
    ) -> termwiz::Result<()> {
        changes.append(&mut vec![
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::Absolute(self.row),
            },
            Change::ClearToEndOfLine(ColorAttribute::Default),
            Change::Text("  ".to_string()),
            Change::Attribute(AttributeChange::Foreground(colors.key_fg)),
            Change::Text(self.delegate.key.clone()),
            Change::Attribute(AttributeChange::Foreground(ColorAttribute::Default)),
            Change::Text(format!(" {}", self.delegate.description)),
        ]);

        Ok(())
    }
}

struct TransientSection<'a> {
    delegate: &'a KTransientSection,
    row: usize,
}

impl<'a> Renderable for TransientSection<'a> {
    fn render(
        &self,
        _colors: &TransientColors,
        changes: &mut Vec<Change>,
        _term: &mut TermWizTerminal,
        _render_now: bool,
    ) -> termwiz::Result<()> {
        changes.append(&mut vec![
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::Absolute(self.row),
            },
            Change::Text(self.delegate.header.to_string()),
            Change::AllAttributes(CellAttributes::default()),
        ]);

        Ok(())
    }
}

enum RenderableEntity<'a> {
    TransientContext(TransientContext<'a>),
    TransientContextEntry(TransientContextEntry<'a>),
    TransientSection(TransientSection<'a>),
    TransientOption(TransientOption<'a>),
    TransientSwitch(TransientSwitch<'a>),
    TransientArgument(TransientArgument<'a>),
    TransientCyclicSwitch(TransientCyclicSwitch<'a>),
}

impl<'a> RenderableEntity<'a> {
    fn render(
        &self,
        colors: &TransientColors,
        changes: &mut Vec<Change>,
        term: &mut TermWizTerminal,
    ) -> termwiz::Result<()> {
        match self {
            Self::TransientOption(option) => option.render(colors, changes, term, false),
            Self::TransientSwitch(switch) => switch.render(colors, changes, term, false),
            Self::TransientCyclicSwitch(cyclic_switch) => {
                cyclic_switch.render(colors, changes, term, false)
            }
            Self::TransientArgument(positional_arg) => {
                positional_arg.render(colors, changes, term, false)
            }
            Self::TransientSection(section) => section.render(colors, changes, term, false),
            Self::TransientContext(context) => context.render(colors, changes, term, false),
            Self::TransientContextEntry(entry) => entry.render(colors, changes, term, false),
        }
    }
}

#[derive(Clone)]
struct TransientContextEntry<'a> {
    delegate: &'a KTransientContextEntry,
    row: usize,
}

impl<'a> Renderable for TransientContextEntry<'a> {
    fn render(
        &self,
        _colors: &TransientColors,
        changes: &mut Vec<Change>,
        _term: &mut TermWizTerminal,
        _render_now: bool,
    ) -> termwiz::Result<()> {
        changes.append(&mut vec![
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::Absolute(self.row),
            },
            Change::ClearToEndOfLine(ColorAttribute::Default),
            Change::Text(self.delegate.label.to_string()),
            Change::Text(": ".to_string()),
            Change::Text(self.delegate.id.to_string()),
        ]);

        Ok(())
    }
}

#[derive(Clone)]
struct TransientContext<'a> {
    delegate: &'a KTransientContext,
    row: usize,
}

impl<'a> Renderable for TransientContext<'a> {
    fn render(
        &self,
        _colors: &TransientColors,
        changes: &mut Vec<Change>,
        _term: &mut TermWizTerminal,
        _render_entries: bool,
    ) -> termwiz::Result<()> {
        changes.append(&mut vec![
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::Absolute(self.row),
            },
            Change::Text(self.delegate.header.clone()),
        ]);

        Ok(())
    }
}

struct TransientState<'a> {
    window: GuiWin,
    pane: MuxPane,
    description: String,
    colors: TransientColors,
    root_node: &'a TrieNode<'a>,
    traversed_nodes: Vec<&'a TrieNode<'a>>,
    changes: Vec<Change>,
    row_entities: Vec<Option<&'a RenderableEntity<'a>>>,
    cancel: Option<Box<KeyAssignment>>,
}

impl<'a> TransientState<'a> {
    fn new(
        args: &KTransientMenu,
        window: GuiWin,
        pane: MuxPane,
        row_entities: Vec<Option<&'a RenderableEntity<'a>>>,
        trie_node: &'a TrieNode<'a>,
    ) -> Self {
        let root_node = trie_node;
        let traversed_nodes = vec![trie_node];

        Self {
            window,
            pane,
            description: args.description.clone(),
            colors: TransientColors::new(),
            root_node,
            traversed_nodes,
            changes: vec![Change::CursorVisibility(CursorVisibility::Hidden)],
            row_entities,
            cancel: args.cancel.clone(),
        }
    }

    fn render(&mut self, term: &mut TermWizTerminal) -> termwiz::Result<()> {
        let description_len =
            crate::tabbar::parse_status_text(&self.description, CellAttributes::blank()).len();

        self.changes.append(&mut vec![
            Change::ClearScreen(ColorAttribute::Default),
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::Absolute(0),
            },
            Change::Text(self.description.to_string()),
            Change::AllAttributes(CellAttributes::default()),
            Change::Text("\r\n".to_string()),
            Change::Text("─".repeat(description_len)),
        ]);

        for entity in self.row_entities.iter().skip(3) {
            self.changes.push(Change::Text("\r\n".to_string()));
            if let Some(entity) = entity {
                entity.render(&self.colors, &mut self.changes, term)?;
            }
        }
        term.render(&self.changes)?;

        Ok(())
    }

    fn line_prompt(
        &mut self,
        term: &mut TermWizTerminal,
        option: &'a TransientOption<'a>,
    ) -> anyhow::Result<()> {
        let size = term.get_screen_size()?;
        self.changes.append(&mut vec![
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::EndRelative(2),
            },
            Change::Text("─".repeat(size.cols)),
            Change::Text("\r\n".to_string()),
            Change::CursorVisibility(CursorVisibility::Visible),
        ]);
        term.render(&self.changes)?;
        self.changes.clear();

        let mut host = PromptHost::new();
        let mut editor = LineEditor::new(term);
        let mut prompt = option.delegate.description.clone();
        if let Some(default) = option.delegate.default.clone() {
            prompt.push_str(&format!(" (default {})", default));
        }
        prompt.push_str(": ");
        editor.set_prompt(&prompt);

        if let Some(line) = editor.read_line(&mut host)? {
            let new_val = if line.is_empty() {
                option.delegate.default.clone()
            } else {
                Some(line)
            };
            option.value.replace(new_val);
        }
        self.changes.append(&mut vec![
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::EndRelative(2),
            },
            Change::ClearToEndOfScreen(ColorAttribute::Default),
            Change::CursorVisibility(CursorVisibility::Hidden),
        ]);

        Ok(())
    }

    fn trigger_event(&self, name: &str, result: Option<TransientResult>) {
        let name = name.to_string();
        let window = self.window.clone();
        let pane = self.pane;

        promise::spawn::spawn_into_main_thread(async move {
            trampoline(name, window, pane, result);
            anyhow::Result::<()>::Ok(())
        })
        .detach();
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
                    if let Some(key_assignment) = self.cancel.as_ref() {
                        if let KeyAssignment::EmitEvent(ref id) = **key_assignment {
                            self.trigger_event(id, None);
                        }
                    }
                    break;
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char(c),
                    ..
                }) => {
                    let cur_node = self.traversed_nodes.last().unwrap();

                    let cur_node = match cur_node.find_char(c) {
                        Some(cur_node) => cur_node,
                        None => {
                            self.traversed_nodes = vec![self.root_node];
                            continue;
                        }
                    };

                    let transient_entry = match cur_node.entry.as_ref() {
                        Some(entry) => entry,
                        None => {
                            self.traversed_nodes.push(cur_node);
                            continue;
                        }
                    };

                    match transient_entry {
                        RenderableEntity::TransientSwitch(switch) => {
                            switch.value.replace_with(|&mut val| !val);

                            switch.render(&self.colors, &mut self.changes, term, true)?;
                        }
                        RenderableEntity::TransientOption(option) => {
                            if option.value.borrow().is_none() || !option.delegate.allow_nil {
                                if let Some(choices) = option.delegate.choices.clone() {
                                    let size = term.get_screen_size()?;

                                    let max_items = size.rows.saturating_sub(ROW_OVERHEAD);
                                    let selector_size = choices.len().min(max_items);

                                    let mut selector_state = SelectorState {
                                        active_idx: 0,
                                        max_items,
                                        top_row: 0,
                                        filter_term: String::new(),
                                        filtered_entries: choices.clone(),
                                        choices,
                                        cols: size.cols,
                                        selector_size,
                                        changes: &mut self.changes,
                                        colors: &self.colors,
                                        option,
                                        row_entities: &self.row_entities,
                                    };

                                    selector_state.draw_separator_and_show_cursor();
                                    selector_state.render(term)?;
                                    selector_state.run_loop(term)?;
                                } else {
                                    self.line_prompt(term, option)?;
                                }
                            } else {
                                option.value.replace(None);
                            }
                            option.render(&self.colors, &mut self.changes, term, true)?;
                        }
                        RenderableEntity::TransientCyclicSwitch(cyclic_switch) => {
                            if !cyclic_switch.delegate.choices.is_empty() {
                                cyclic_switch.active_idx.replace_with(|idx| {
                                    if let Some(idx) = idx {
                                        if *idx == cyclic_switch.delegate.choices.len() - 1 {
                                            if cyclic_switch.delegate.allow_nil {
                                                None
                                            } else {
                                                Some(0)
                                            }
                                        } else {
                                            Some(*idx + 1)
                                        }
                                    } else {
                                        Some(0)
                                    }
                                });
                                cyclic_switch.render(
                                    &self.colors,
                                    &mut self.changes,
                                    term,
                                    true,
                                )?;
                            }
                        }
                        RenderableEntity::TransientArgument(positional_arg) => {
                            let name = match *positional_arg.delegate.action {
                                KeyAssignment::EmitEvent(ref id) => id,
                                _ => anyhow::bail!("TransientMenu requires action to be defined by wezterm.action_callback")
                            };

                            let result = TransientResult::from(&self.row_entities);
                            self.trigger_event(name, Some(result));
                            break;
                        }
                        _ => {}
                    }
                    self.traversed_nodes = vec![self.root_node];
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Backspace,
                    ..
                }) => {
                    if self.traversed_nodes.len() >= 2 {
                        self.traversed_nodes.pop();
                    }
                }
                _ => {}
            }
        }

        Ok(())
    }
}

#[derive(FromDynamic, ToDynamic)]
struct TransientResultEntry {
    flag: String,
    value: Value,
}

#[derive(FromDynamic, ToDynamic)]
struct TransientResult {
    entries: Vec<TransientResultEntry>,
}
impl_lua_conversion_dynamic!(TransientResult);

impl<'a> From<&'a Vec<Option<&'a RenderableEntity<'a>>>> for TransientResult {
    fn from(value: &'a Vec<Option<&'a RenderableEntity<'a>>>) -> Self {
        let mut entries: Vec<TransientResultEntry> = vec![];

        for entry in value.iter().skip(3).filter_map(|k| *k) {
            match entry {
                RenderableEntity::TransientOption(option) => {
                    entries.push(TransientResultEntry {
                        flag: option.delegate.flag.clone(),
                        value: option.value.borrow().to_dynamic(),
                    });
                }
                RenderableEntity::TransientSwitch(switch) => {
                    entries.push(TransientResultEntry {
                        flag: switch.delegate.flag.clone(),
                        value: switch.value.borrow().to_dynamic(),
                    });
                }
                RenderableEntity::TransientCyclicSwitch(cyclic_switch) => {
                    entries.push(TransientResultEntry {
                        flag: cyclic_switch.delegate.flag.clone(),
                        value: cyclic_switch
                            .active_idx
                            .borrow()
                            .map_or_else(
                                || None,
                                |idx| cyclic_switch.delegate.choices.get(idx).cloned(),
                            )
                            .to_dynamic(),
                    });
                }
                _ => {}
            }
        }

        Self { entries }
    }
}

fn trampoline(name: String, window: GuiWin, pane: MuxPane, result: Option<TransientResult>) {
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
    result: Option<TransientResult>,
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

pub fn show_transient_menu_overlay(
    mut term: TermWizTerminal,
    args: KTransientMenu,
    window: GuiWin,
    pane: MuxPane,
) -> anyhow::Result<()> {
    term.no_grab_mouse_in_raw_mode();
    let mut row_entities: Vec<Option<RenderableEntity>> = vec![None, None];

    let mut row = 2;
    if let Some(k_context) = args.context.as_ref() {
        row_entities.push(None);
        row += 1;

        let transient_context = TransientContext {
            delegate: k_context,
            row,
        };
        row_entities.push(Some(RenderableEntity::TransientContext(transient_context)));
        row += 1;

        for k_context_entry in &k_context.entries {
            let entry = TransientContextEntry {
                delegate: k_context_entry,
                row,
            };
            row_entities.push(Some(RenderableEntity::TransientContextEntry(entry)));
            row += 1;
        }
    }

    for k_section in &args.sections {
        row_entities.push(None);
        row += 1;

        let transient_section = TransientSection {
            delegate: k_section,
            row,
        };
        row_entities.push(Some(RenderableEntity::TransientSection(transient_section)));
        row += 1;

        for k_transient_entry in &k_section.entries {
            let transient_entry = match k_transient_entry {
                KTransientEntry::TransientSwitch(switch) => {
                    RenderableEntity::TransientSwitch(TransientSwitch {
                        delegate: &switch,
                        value: RefCell::new(switch.default),
                        row,
                    })
                }
                KTransientEntry::TransientOption(option) => {
                    RenderableEntity::TransientOption(TransientOption {
                        delegate: &option,
                        value: RefCell::new(option.default.clone()),
                        row,
                    })
                }
                KTransientEntry::TransientCyclicSwitch(cyclic_switch) => {
                    let active_idx = cyclic_switch.default.as_ref().map_or_else(
                        || None,
                        |default| {
                            cyclic_switch
                                .choices
                                .iter()
                                .position(|choice| choice == default)
                        },
                    );
                    RenderableEntity::TransientCyclicSwitch(TransientCyclicSwitch {
                        delegate: &cyclic_switch,
                        active_idx: RefCell::new(active_idx),
                        row,
                    })
                }
                KTransientEntry::TransientArgument(positional_arg) => {
                    RenderableEntity::TransientArgument(TransientArgument {
                        delegate: &positional_arg,
                        row,
                    })
                }
            };
            row_entities.push(Some(transient_entry));
            row += 1;
        }
    }

    let mut trie_node = TrieNode::new();

    for entity in row_entities.iter().filter_map(|k| k.as_ref()) {
        match entity {
            RenderableEntity::TransientSwitch(switch) => {
                trie_node.add_word(&switch.delegate.key, entity);
            }
            RenderableEntity::TransientOption(option) => {
                trie_node.add_word(&option.delegate.key, entity);
            }
            RenderableEntity::TransientCyclicSwitch(cyclic_switch) => {
                trie_node.add_word(&cyclic_switch.delegate.key, entity);
            }
            RenderableEntity::TransientArgument(positional_arg) => {
                trie_node.add_word(&positional_arg.delegate.key, entity);
            }
            _ => {}
        }
    }

    let mut state = TransientState::new(
        &args,
        window,
        pane,
        row_entities.iter().map(|k| k.as_ref()).collect(),
        &trie_node,
    );

    state.render(&mut term)?;
    state.run_loop(&mut term)
}
