use crate::overlay::selector::{matcher_pattern, matcher_score};
use crate::scripting::guiwin::GuiWin;
use config::configuration;
use config::keyassignment::{
    KeyAssignment, TransientArgument as KTransientArgument, TransientContext as KTransientContext,
    TransientContextEntry as KTransientContextEntry,
    TransientCyclicSwitch as KTransientCyclicSwitch, TransientEntry as KTransientEntry,
    TransientMenu as KTransientMenu, TransientOption as KTransientOption,
    TransientSection as KTransientSection, TransientSwitch as KTransientSwitch,
};
use config::{AnsiColor, ColorAttribute};
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

enum TransientEntry {
    TransientOption(Rc<RefCell<TransientOption>>),
    TransientSwitch(Rc<RefCell<TransientSwitch>>),
    TransientArgument(TransientArgument),
    TransientCyclicSwitch(Rc<RefCell<TransientCyclicSwitch>>),
}

impl Renderable for TransientEntry {
    fn render(
        &self,
        colors: &TransientColors,
        changes: &mut Vec<Change>,
        term: &mut TermWizTerminal,
        _render_now: bool,
    ) -> termwiz::Result<()> {
        match self {
            Self::TransientOption(option) => option.borrow().render(colors, changes, term, false),
            Self::TransientSwitch(switch) => switch.borrow().render(colors, changes, term, false),
            Self::TransientArgument(positional_arg) => {
                positional_arg.render(colors, changes, term, false)
            }
            Self::TransientCyclicSwitch(cyclic_switch) => {
                cyclic_switch.borrow().render(colors, changes, term, false)
            }
        }
    }
}

impl TransientEntry {
    fn new(
        entry: &KTransientEntry,
        root: &mut TrieNode,
        row: &mut usize,
        row_entities: &mut Vec<Option<RenderableEntity>>,
    ) -> Self {
        *row += 1;
        match entry {
            KTransientEntry::TransientSwitch(switch) => {
                let new_switch = Rc::new(RefCell::new(TransientSwitch::new(switch, *row)));
                let cloned_switch = Rc::clone(&new_switch);
                let entry = Self::TransientSwitch(cloned_switch);
                let cloned_switch = Rc::clone(&new_switch);
                row_entities.push(Some(RenderableEntity::TransientSwitch(cloned_switch)));
                root.add_word(&switch.key, entry);
                Self::TransientSwitch(new_switch)
            }
            KTransientEntry::TransientOption(option) => {
                let new_option = Rc::new(RefCell::new(TransientOption::new(option, *row)));
                let cloned_option = Rc::clone(&new_option);
                let entry = Self::TransientOption(cloned_option);
                let cloned_option = Rc::clone(&new_option);
                row_entities.push(Some(RenderableEntity::TransientOption(cloned_option)));
                root.add_word(&option.key, entry);
                Self::TransientOption(new_option)
            }
            KTransientEntry::TransientCyclicSwitch(cyclic_switch) => {
                let new_cyclic_switch = Rc::new(RefCell::new(TransientCyclicSwitch::new(
                    cyclic_switch,
                    *row,
                )));
                let cloned_cyclic_switch = Rc::clone(&new_cyclic_switch);
                let entry = Self::TransientCyclicSwitch(cloned_cyclic_switch);
                let cloned_cyclic_switch = Rc::clone(&new_cyclic_switch);
                row_entities.push(Some(RenderableEntity::TransientCyclicSwitch(
                    cloned_cyclic_switch,
                )));
                root.add_word(&cyclic_switch.key, entry);
                Self::TransientCyclicSwitch(new_cyclic_switch)
            }
            KTransientEntry::TransientArgument(positional_arg) => {
                let new_argument = TransientArgument::new(positional_arg, *row);
                let entry = Self::TransientArgument(new_argument.clone());
                row_entities.push(Some(RenderableEntity::TransientArgument(
                    new_argument.clone(),
                )));
                root.add_word(&positional_arg.key, entry);
                Self::TransientArgument(new_argument)
            }
        }
    }
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
    option: &'a mut TransientOption,
    row_entities: &'a Vec<Option<RenderableEntity<'a>>>,
}

impl<'a> SelectorState<'a> {
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
                &format!("{}: {}", self.option.description, self.filter_term),
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
            x: Position::Absolute(2 + self.option.description.len() + self.filter_term.len()),
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
                    key: KeyCode::Enter,
                    ..
                }) => {
                    if let Some(entry) = self.filtered_entries.get(self.active_idx).cloned() {
                        self.option.value = Some(entry);
                        self.clear_selector(term)?;
                        break;
                    }
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
                    key: KeyCode::Char(c),
                    ..
                }) => {
                    self.filter_term.push(c);
                    self.update_filter();
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

struct TrieNode {
    children: HashMap<char, Rc<RefCell<TrieNode>>>,
    is_end_of_word: bool,
    entry: Option<TransientEntry>,
}

impl TrieNode {
    fn new() -> Self {
        Self {
            children: HashMap::new(),
            is_end_of_word: true,
            entry: None,
        }
    }

    fn add_word(&mut self, word: &str, entry: TransientEntry) {
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

struct TransientSwitch {
    key: String,
    value: bool,
    description: String,
    flag: String,
    row: usize,
}

impl Renderable for TransientSwitch {
    fn render(
        &self,
        colors: &TransientColors,
        changes: &mut Vec<Change>,
        term: &mut TermWizTerminal,
        render_now: bool,
    ) -> termwiz::Result<()> {
        changes.append(&mut vec![
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::Absolute(self.row),
            },
            Change::ClearToEndOfLine(ColorAttribute::Default),
            Change::Text("  ".to_string()),
            Change::Attribute(AttributeChange::Foreground(colors.key_fg)),
            Change::Text(format!("{}", self.key)),
            Change::Attribute(AttributeChange::Foreground(ColorAttribute::Default)),
            Change::Text(format!(" {} (", self.description)),
        ]);

        if self.value {
            changes.append(&mut vec![
                Change::Attribute(AttributeChange::Intensity(Intensity::Bold)),
                Change::Attribute(AttributeChange::Foreground(colors.flag_fg)),
                Change::Text(self.flag.to_string()),
                Change::AllAttributes(CellAttributes::default()),
            ]);
        } else {
            changes.push(Change::Text(self.flag.to_string()));
        }
        changes.push(Change::Text(")".to_string()));

        if render_now {
            term.render(changes)?;
            changes.clear();
        }

        Ok(())
    }
}

impl TransientSwitch {
    fn new(value: &KTransientSwitch, row: usize) -> Self {
        Self {
            key: value.key.clone(),
            value: value.default,
            description: value.description.clone(),
            flag: value.flag.clone(),
            row,
        }
    }
}

struct TransientOption {
    key: String,
    value: Option<String>,
    default: Option<String>,
    description: String,
    flag: String,
    allow_nil: bool,
    choices: Option<Vec<String>>,
    row: usize,
}

impl Renderable for TransientOption {
    fn render(
        &self,
        colors: &TransientColors,
        changes: &mut Vec<Change>,
        term: &mut TermWizTerminal,
        render_now: bool,
    ) -> termwiz::Result<()> {
        changes.append(&mut vec![
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::Absolute(self.row),
            },
            Change::ClearToEndOfLine(ColorAttribute::Default),
            Change::Text("  ".to_string()),
            Change::Attribute(AttributeChange::Foreground(colors.key_fg)),
            Change::Text(format!("{}", self.key)),
            Change::Attribute(AttributeChange::Foreground(ColorAttribute::Default)),
            Change::Text(format!(" {} (", self.description)),
        ]);

        if let Some(val) = &self.value {
            changes.append(&mut vec![
                Change::Attribute(AttributeChange::Intensity(Intensity::Bold)),
                Change::Attribute(AttributeChange::Foreground(colors.flag_fg)),
                Change::Text(format!("{}{}", self.flag, val)),
                Change::AllAttributes(CellAttributes::default()),
            ]);
        } else {
            changes.push(Change::Text(format!("{}", self.flag)));
        }

        changes.push(Change::Text(")".to_string()));

        if render_now {
            term.render(changes)?;
            changes.clear();
        }

        Ok(())
    }
}

impl TransientOption {
    fn new(option: &KTransientOption, row: usize) -> Self {
        Self {
            key: option.key.clone(),
            value: option.default.clone(),
            default: option.default.clone(),
            description: option.description.clone(),
            flag: option.flag.clone(),
            allow_nil: option.allow_nil,
            choices: option.choices.clone(),
            row,
        }
    }
}

struct TransientCyclicSwitch {
    key: String,
    active_idx: Option<usize>,
    description: String,
    flag: String,
    choices: Vec<String>,
    allow_nil: bool,
    row: usize,
}

impl Renderable for TransientCyclicSwitch {
    fn render(
        &self,
        colors: &TransientColors,
        changes: &mut Vec<Change>,
        term: &mut TermWizTerminal,
        render_now: bool,
    ) -> termwiz::Result<()> {
        changes.append(&mut vec![
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::Absolute(self.row),
            },
            Change::ClearToEndOfLine(ColorAttribute::Default),
            Change::Text("  ".to_string()),
            Change::Attribute(AttributeChange::Foreground(colors.key_fg)),
            Change::Text(format!("{}", self.key)),
            Change::Attribute(AttributeChange::Foreground(ColorAttribute::Default)),
            Change::Text(format!(" {} (", self.description)),
        ]);

        if let Some(idx) = self.active_idx {
            changes.append(&mut vec![
                Change::Attribute(AttributeChange::Intensity(Intensity::Bold)),
                Change::Attribute(AttributeChange::Foreground(colors.flag_fg)),
                Change::Text(self.flag.to_string()),
                Change::AllAttributes(CellAttributes::default()),
            ]);
            if self.choices.first().is_some() {
                let mut prefix = " [";
                for (cur_idx, choice) in self.choices.iter().enumerate() {
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
            changes.push(Change::Text(self.flag.to_string()));
            if self.choices.first().is_some() {
                let mut prefix = " [";
                for (cur_idx, choice) in self.choices.iter().enumerate() {
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

impl TransientCyclicSwitch {
    fn new(cyclic_switch: &KTransientCyclicSwitch, row: usize) -> Self {
        let active_idx = cyclic_switch.default.as_ref().map_or_else(
            || None,
            |default| {
                cyclic_switch
                    .choices
                    .iter()
                    .position(|choice| choice == default)
            },
        );
        Self {
            key: cyclic_switch.key.clone(),
            active_idx,
            description: cyclic_switch.description.clone(),
            flag: cyclic_switch.flag.clone(),
            choices: cyclic_switch.choices.clone(),
            allow_nil: cyclic_switch.allow_nil,
            row,
        }
    }
}

#[derive(Clone)]
struct TransientArgument {
    key: String,
    description: String,
    action: Box<KeyAssignment>,
    row: usize,
}

impl Renderable for TransientArgument {
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
            Change::Text(self.key.clone()),
            Change::Attribute(AttributeChange::Foreground(ColorAttribute::Default)),
            Change::Text(format!(" {}", self.description)),
        ]);

        Ok(())
    }
}

impl TransientArgument {
    fn new(positional_arg: &KTransientArgument, row: usize) -> Self {
        Self {
            key: positional_arg.key.clone(),
            description: positional_arg.description.clone(),
            action: positional_arg.action.clone(),
            row,
        }
    }
}

struct TransientSection<'a> {
    header: &'a str,
    entries: Vec<TransientEntry>,
    row: usize,
}

impl<'a> TransientSection<'a> {
    fn new(
        section: &'a KTransientSection,
        root: &mut TrieNode,
        row: &mut usize,
        row_entities: &mut Vec<Option<RenderableEntity>>,
    ) -> Self {
        let mut entries = vec![];
        row_entities.push(None);
        let section_row = *row;
        for entry in &section.entries {
            entries.push(TransientEntry::new(entry, root, row, row_entities));
        }

        row_entities.append(&mut vec![None, None]);
        *row += 2;

        Self {
            header: &section.header,
            entries,
            row: section_row,
        }
    }

    fn render(
        &self,
        colors: &TransientColors,
        changes: &mut Vec<Change>,
        term: &mut TermWizTerminal,
        render_entries: bool,
    ) -> termwiz::Result<()> {
        changes.append(&mut vec![
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::Absolute(self.row),
            },
            Change::Text(self.header.to_string()),
            Change::AllAttributes(CellAttributes::default()),
        ]);

        if render_entries {
            for entry in &self.entries {
                entry.render(colors, changes, term, false)?;
            }
        }

        Ok(())
    }
}

enum RenderableEntity<'a> {
    TransientContext(TransientContext),
    TransientContextEntry(TransientContextEntry),
    TransientSection(Rc<TransientSection<'a>>),
    TransientOption(Rc<RefCell<TransientOption>>),
    TransientSwitch(Rc<RefCell<TransientSwitch>>),
    TransientArgument(TransientArgument),
    TransientCyclicSwitch(Rc<RefCell<TransientCyclicSwitch>>),
}

impl RenderableEntity<'_> {
    fn render(
        &self,
        colors: &TransientColors,
        changes: &mut Vec<Change>,
        term: &mut TermWizTerminal,
    ) -> termwiz::Result<()> {
        match self {
            RenderableEntity::TransientContext(context) => {
                context.render(colors, changes, term, false)
            }
            RenderableEntity::TransientContextEntry(entry) => {
                entry.render(colors, changes, term, false)
            }
            RenderableEntity::TransientSection(section) => {
                section.render(colors, changes, term, false)
            }
            RenderableEntity::TransientSwitch(switch) => {
                switch.borrow().render(colors, changes, term, false)
            }
            RenderableEntity::TransientCyclicSwitch(cyclic_switch) => {
                cyclic_switch.borrow().render(colors, changes, term, false)
            }
            RenderableEntity::TransientOption(option) => {
                option.borrow().render(colors, changes, term, false)
            }
            RenderableEntity::TransientArgument(positional_arg) => {
                positional_arg.render(colors, changes, term, false)
            }
        }
    }
}

#[derive(Clone)]
struct TransientContextEntry {
    label: String,
    id: String,
    row: usize,
}

impl TransientContextEntry {
    fn new(entry: &KTransientContextEntry, row: &mut usize) -> Self {
        *row += 1;

        Self {
            label: entry.label.clone(),
            id: entry.id.clone(),
            row: *row,
        }
    }
}

impl Renderable for TransientContextEntry {
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
            Change::Text(self.label.to_string()),
            Change::Text(": ".to_string()),
            Change::Text(self.id.to_string()),
        ]);

        Ok(())
    }
}

#[derive(Clone)]
struct TransientContext {
    header: String,
    entries: Vec<TransientContextEntry>,
    row: usize,
}

impl TransientContext {
    fn new(
        context: &KTransientContext,
        row: &mut usize,
        row_entities: &mut Vec<Option<RenderableEntity>>,
    ) -> Self {
        let context_row = *row;

        row_entities.push(None);
        let mut entries: Vec<TransientContextEntry> = vec![];

        for context_entry in &context.entries {
            let entry = TransientContextEntry::new(context_entry, row);
            row_entities.push(Some(RenderableEntity::TransientContextEntry(entry.clone())));
            entries.push(entry);
        }
        *row += 1;

        Self {
            header: context.header.clone(),
            entries,
            row: context_row,
        }
    }
}

impl Renderable for TransientContext {
    fn render(
        &self,
        colors: &TransientColors,
        changes: &mut Vec<Change>,
        term: &mut TermWizTerminal,
        render_entries: bool,
    ) -> termwiz::Result<()> {
        changes.append(&mut vec![
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::Absolute(self.row),
            },
            Change::Text(self.header.clone()),
        ]);

        if render_entries {
            for entry in &self.entries {
                entry.render(colors, changes, term, false)?;
            }
        }

        Ok(())
    }
}

struct TransientState<'a> {
    window: GuiWin,
    pane: MuxPane,
    description: &'a str,
    sections: Vec<Rc<TransientSection<'a>>>,
    colors: TransientColors,
    root_node: Rc<RefCell<TrieNode>>,
    cur_node: Rc<RefCell<TrieNode>>,
    changes: Vec<Change>,
    row_entities: Vec<Option<RenderableEntity<'a>>>,
    context: Option<TransientContext>,
    cancel: Option<Box<KeyAssignment>>,
}

impl<'a> TransientState<'a> {
    fn new(args: &'a KTransientMenu, window: GuiWin, pane: MuxPane) -> Self {
        let mut trie_node = TrieNode::new();
        let mut sections = vec![];
        let mut row = 3;
        let mut row_entities: Vec<Option<RenderableEntity>> = vec![None, None, None];
        let mut context: Option<TransientContext> = None;

        if let Some(k_context) = args.context.as_ref() {
            let transient_context = TransientContext::new(&k_context, &mut row, &mut row_entities);
            row_entities[transient_context.row] = Some(RenderableEntity::TransientContext(
                transient_context.clone(),
            ));

            context = Some(transient_context);

            row_entities.push(None);
            row += 1;
        }

        for section in &args.sections {
            let transient_section =
                TransientSection::new(section, &mut trie_node, &mut row, &mut row_entities);
            let transient_section_row = transient_section.row;
            let new_transient_section = Rc::new(transient_section);
            row_entities[transient_section_row] = Some(RenderableEntity::TransientSection(
                Rc::clone(&new_transient_section),
            ));
            sections.push(new_transient_section);
        }
        let root_node = Rc::new(RefCell::new(trie_node));
        let cur_node = Rc::clone(&root_node);

        Self {
            window,
            pane,
            description: &args.description,
            sections,
            colors: TransientColors::new(),
            root_node,
            cur_node,
            changes: vec![Change::CursorVisibility(CursorVisibility::Hidden)],
            row_entities,
            context,
            cancel: args.cancel.clone(),
        }
    }

    fn render(&mut self, term: &mut TermWizTerminal) -> termwiz::Result<()> {
        self.changes.append(&mut vec![
            Change::ClearScreen(ColorAttribute::Default),
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::Absolute(0),
            },
            Change::Text(self.description.to_string()),
            Change::AllAttributes(CellAttributes::default()),
            Change::Text("\r\n".to_string()),
            Change::Text("─".repeat(self.description.len())),
        ]);

        if let Some(context) = self.context.as_ref() {
            context.render(&self.colors, &mut self.changes, term, true)?;
        }

        for section in &self.sections {
            section.render(&self.colors, &mut self.changes, term, true)?;
        }

        self.changes.push(Change::Text("\r\n\r\n\r\n".to_string()));

        term.render(&self.changes)?;
        self.changes.clear();

        Ok(())
    }

    fn line_prompt(
        &mut self,
        term: &mut TermWizTerminal,
        option: &mut TransientOption,
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
        let mut prompt = option.description.clone();
        if let Some(default) = option.default.clone() {
            prompt.push_str(&format!(" (default {})", default));
        }
        prompt.push_str(": ");
        editor.set_prompt(&prompt);

        if let Some(line) = editor.read_line(&mut host)? {
            option.value = if line.is_empty() {
                option.default.clone()
            } else {
                Some(line)
            };
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
                    let cur_node = Rc::clone(&self.cur_node);
                    let cur_node = cur_node.borrow();
                    self.cur_node = match cur_node.find_char(c) {
                        Some(cur_node) => {
                            let cur_node_borrowed = cur_node.borrow();
                            if cur_node_borrowed.is_end_of_word {
                                match cur_node_borrowed.entry.as_ref().unwrap() {
                                    TransientEntry::TransientSwitch(switch) => {
                                        let mut switch = switch.borrow_mut();
                                        switch.value = !switch.value;

                                        switch.render(
                                            &self.colors,
                                            &mut self.changes,
                                            term,
                                            true,
                                        )?;
                                    }
                                    TransientEntry::TransientOption(option) => {
                                        let mut option = option.borrow_mut();
                                        if option.value.is_none() || !option.allow_nil {
                                            if let Some(choices) = option.choices.clone() {
                                                let size = term.get_screen_size()?;

                                                let max_items =
                                                    size.rows.saturating_sub(ROW_OVERHEAD);
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
                                                    option: &mut *option,
                                                    row_entities: &self.row_entities,
                                                };

                                                selector_state.draw_separator_and_show_cursor();
                                                selector_state.render(term)?;
                                                selector_state.run_loop(term)?;
                                            } else {
                                                self.line_prompt(term, &mut *option)?;
                                            }
                                        } else {
                                            option.value = None;
                                        }
                                        option.render(
                                            &self.colors,
                                            &mut self.changes,
                                            term,
                                            true,
                                        )?;
                                    }
                                    TransientEntry::TransientCyclicSwitch(cyclic_switch) => {
                                        let mut cyclic_switch = cyclic_switch.borrow_mut();

                                        if !cyclic_switch.choices.is_empty() {
                                            cyclic_switch.active_idx =
                                                if let Some(idx) = cyclic_switch.active_idx {
                                                    if idx == cyclic_switch.choices.len() - 1 {
                                                        if cyclic_switch.allow_nil {
                                                            None
                                                        } else {
                                                            Some(0)
                                                        }
                                                    } else {
                                                        Some(idx + 1)
                                                    }
                                                } else {
                                                    Some(0)
                                                };

                                            cyclic_switch.render(
                                                &self.colors,
                                                &mut self.changes,
                                                term,
                                                true,
                                            )?;
                                        }
                                    }
                                    TransientEntry::TransientArgument(positional_arg) => {
                                        let name = match *positional_arg.action {
                                            KeyAssignment::EmitEvent(ref id) => id,
                                            _ => anyhow::bail!("TransientMenu requires action to be defined by wezterm.action_callback")
                                        };

                                        let result = TransientResult::from(&self.sections);
                                        self.trigger_event(name, Some(result));
                                        break;
                                    }
                                }
                                Rc::clone(&self.root_node)
                            } else {
                                Rc::clone(&cur_node)
                            }
                        }
                        None => Rc::clone(&self.root_node),
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

impl<'a> From<&'a Vec<Rc<TransientSection<'a>>>> for TransientResult {
    fn from(value: &'a Vec<Rc<TransientSection>>) -> Self {
        let mut entries: Vec<TransientResultEntry> = vec![];

        for section in value {
            for entry in &section.entries {
                match entry {
                    TransientEntry::TransientOption(option) => {
                        let option = option.borrow();
                        entries.push(TransientResultEntry {
                            flag: option.flag.clone(),
                            value: option.value.to_dynamic(),
                        });
                    }
                    TransientEntry::TransientSwitch(switch) => {
                        let switch = switch.borrow();
                        entries.push(TransientResultEntry {
                            flag: switch.flag.clone(),
                            value: switch.value.to_dynamic(),
                        });
                    }
                    TransientEntry::TransientCyclicSwitch(cyclic_switch) => {
                        let cyclic_switch = cyclic_switch.borrow();
                        entries.push(TransientResultEntry {
                            flag: cyclic_switch.flag.clone(),
                            value: cyclic_switch
                                .active_idx
                                .map_or_else(|| None, |idx| cyclic_switch.choices.get(idx).cloned())
                                .to_dynamic(),
                        });
                    }
                    _ => {}
                }
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
    let mut state = TransientState::new(&args, window, pane);

    state.render(&mut term)?;
    state.run_loop(&mut term)
}
