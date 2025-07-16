use crate::overlay::selector::{matcher_pattern, matcher_score};
use crate::scripting::guiwin::GuiWin;
use config::configuration;
use config::keyassignment::{
    KeyAssignment, TransientArgument as KTransientArgument,
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
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use termwiz::input::{InputEvent, KeyCode, KeyEvent};
use termwiz::lineedit::{Action, BasicHistory, History, LineEditor, LineEditorHost};
use termwiz::surface::{Change, CursorVisibility, Position};
use termwiz::terminal::Terminal;
use termwiz_funcs::truncate_right;
use wezterm_dynamic::{FromDynamic, ToDynamic, Value};
use wezterm_term::{AttributeChange, CellAttributes, Intensity};
use window::Modifiers;

const ROW_OVERHEAD: usize = 3;

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

enum TransientEntry {
    TransientOption(Rc<RefCell<TransientOption>>),
    TransientSwitch(Rc<RefCell<TransientSwitch>>),
    TransientArgument(Rc<RefCell<TransientArgument>>),
    TransientCyclicSwitch(Rc<RefCell<TransientCyclicSwitch>>),
}

impl TransientEntry {
    fn render(&self, colors: &TransientColors, changes: &mut Vec<Change>) {
        match self {
            Self::TransientOption(option) => option.borrow().render(colors, changes),
            Self::TransientSwitch(switch) => switch.borrow().render(colors, changes),
            Self::TransientArgument(positional_arg) => {
                positional_arg.borrow().render(colors, changes)
            }
            Self::TransientCyclicSwitch(cyclic_switch) => {
                cyclic_switch.borrow().render(colors, changes)
            }
        }
    }
}

struct SelectorState {
    active_idx: usize,
    max_items: usize,
    top_row: usize,
    filter_term: String,
    filtered_entries: Vec<String>,
    choices: Vec<String>,
}

impl SelectorState {
    fn new(choices: Vec<String>) -> Self {
        Self {
            active_idx: 0,
            max_items: 0,
            top_row: 0,
            filter_term: String::new(),
            filtered_entries: choices.clone(),
            choices,
        }
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
    children: HashMap<char, Rc<RefCell<TrieNode<'a>>>>,
    is_end_of_word: bool,
    entry: Option<TransientEntry>,
}

impl<'a> TrieNode<'a> {
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

    fn find_char(&self, c: char) -> Option<Rc<RefCell<TrieNode<'a>>>> {
        self.children.get(&c).map(|child| Rc::clone(child))
    }
}

struct TransientColors {
    description_fg: ColorAttribute,
    section_header_fg: ColorAttribute,
    key_fg: ColorAttribute,
    flag_fg: ColorAttribute,
}

impl TransientColors {
    fn new() -> Self {
        let config = configuration();
        let colors = &config.resolved_palette;

        Self {
            description_fg: colors
                .transient_description_fg
                .unwrap_or(AnsiColor::Teal.into())
                .into(),
            section_header_fg: colors
                .transient_section_header_fg
                .unwrap_or(AnsiColor::Navy.into())
                .into(),
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
}

impl TransientSwitch {
    fn new(switch: &KTransientSwitch) -> Self {
        Self {
            key: switch.key.clone(),
            value: switch.default,
            description: switch.description.clone(),
            flag: switch.flag.clone(),
        }
    }

    fn render(&self, colors: &TransientColors, changes: &mut Vec<Change>) {
        changes.push(Change::Text("\r\n\t".to_string()));
        changes.push(Change::Attribute(AttributeChange::Foreground(
            colors.key_fg,
        )));
        changes.push(Change::Text(format!("{}", self.key)));
        changes.push(Change::Attribute(AttributeChange::Foreground(
            ColorAttribute::Default,
        )));

        changes.push(Change::Text(format!(" {} (", self.description)));
        if self.value {
            changes.push(Change::Attribute(AttributeChange::Intensity(
                Intensity::Bold,
            )));
            changes.push(Change::Attribute(AttributeChange::Foreground(
                colors.flag_fg,
            )));
            changes.push(Change::Text(self.flag.to_string()));
            changes.push(Change::AllAttributes(CellAttributes::default()));
        } else {
            changes.push(Change::Text(self.flag.to_string()));
        }
        changes.push(Change::Text(")".to_string()));
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
}

impl TransientOption {
    fn new(option: &KTransientOption) -> Self {
        Self {
            key: option.key.clone(),
            value: option.default.clone(),
            default: option.default.clone(),
            description: option.description.clone(),
            flag: option.flag.clone(),
            allow_nil: option.allow_nil,
            choices: option.choices.clone(),
        }
    }

    fn render(&self, colors: &TransientColors, changes: &mut Vec<Change>) {
        changes.push(Change::Text("\r\n\t".to_string()));
        changes.push(Change::Attribute(AttributeChange::Foreground(
            colors.key_fg,
        )));
        changes.push(Change::Text(format!("{}", self.key)));
        changes.push(Change::Attribute(AttributeChange::Foreground(
            ColorAttribute::Default,
        )));

        changes.push(Change::Text(format!(" {} (", self.description)));

        if let Some(val) = &self.value {
            changes.push(Change::Attribute(AttributeChange::Intensity(
                Intensity::Bold,
            )));
            changes.push(Change::Attribute(AttributeChange::Foreground(
                colors.flag_fg,
            )));
            changes.push(Change::Text(format!("{}{}", self.flag, val)));
            changes.push(Change::AllAttributes(CellAttributes::default()));
        } else {
            changes.push(Change::Text(format!("{}", self.flag)));
        }

        changes.push(Change::Text(")".to_string()));
    }
}

struct TransientCyclicSwitch {
    pub key: String,
    pub active_idx: Option<usize>,
    pub description: String,
    pub flag: String,
    pub choices: Vec<String>,
    pub allow_nil: bool,
}

impl TransientCyclicSwitch {
    fn new(cyclic_switch: &KTransientCyclicSwitch) -> Self {
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
        }
    }

    fn render(&self, colors: &TransientColors, changes: &mut Vec<Change>) {
        changes.push(Change::Text("\r\n\t".to_string()));
        changes.push(Change::Attribute(AttributeChange::Foreground(
            colors.key_fg,
        )));
        changes.push(Change::Text(format!("{}", self.key)));
        changes.push(Change::Attribute(AttributeChange::Foreground(
            ColorAttribute::Default,
        )));

        changes.push(Change::Text(format!(" {} (", self.description)));
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
    }
}

struct TransientArgument {
    key: String,
    description: String,
    action: Box<KeyAssignment>,
}

impl TransientArgument {
    fn new(argument: &KTransientArgument) -> Self {
        Self {
            key: argument.key.clone(),
            description: argument.description.clone(),
            action: argument.action.clone(),
        }
    }

    fn render(&self, colors: &TransientColors, changes: &mut Vec<Change>) {
        changes.push(Change::Text("\r\n\t".to_string()));
        changes.push(Change::Attribute(AttributeChange::Foreground(
            colors.key_fg,
        )));
        changes.push(Change::Text(self.key.clone()));
        changes.push(Change::Attribute(AttributeChange::Foreground(
            ColorAttribute::Default,
        )));
        changes.push(Change::Text(format!(" {}", self.description)));
    }
}

struct TransientSection<'a> {
    header: &'a str,
    entries: Vec<TransientEntry>,
}

impl<'a> TransientSection<'a> {
    fn new(section: &'a KTransientSection) -> Self {
        let entries = section
            .entries
            .iter()
            .map(|entry| match entry {
                KTransientEntry::TransientSwitch(switch) => TransientEntry::TransientSwitch(
                    Rc::new(RefCell::new(TransientSwitch::new(switch))),
                ),
                KTransientEntry::TransientOption(option) => TransientEntry::TransientOption(
                    Rc::new(RefCell::new(TransientOption::new(option))),
                ),
                KTransientEntry::TransientCyclicSwitch(cyclic_switch) => {
                    TransientEntry::TransientCyclicSwitch(Rc::new(RefCell::new(
                        TransientCyclicSwitch::new(cyclic_switch),
                    )))
                }
                KTransientEntry::TransientArgument(positional_arg) => {
                    TransientEntry::TransientArgument(Rc::new(RefCell::new(
                        TransientArgument::new(positional_arg),
                    )))
                }
            })
            .collect();

        Self {
            header: &section.header,
            entries,
        }
    }

    fn render(&self, colors: &TransientColors, changes: &mut Vec<Change>) {
        changes.push(Change::Text("\r\n\r\n".to_string()));
        changes.push(Change::Attribute(AttributeChange::Intensity(
            Intensity::Bold,
        )));
        changes.push(Change::Attribute(AttributeChange::Foreground(
            colors.section_header_fg,
        )));
        changes.push(Change::Text(self.header.to_string()));
        changes.push(Change::AllAttributes(CellAttributes::default()));

        for entry in &self.entries {
            entry.render(colors, changes);
        }
    }
}

struct TransientState<'a> {
    window: GuiWin,
    pane: MuxPane,
    description: &'a str,
    sections: Vec<TransientSection<'a>>,
    colors: TransientColors,
    root_node: Rc<RefCell<TrieNode<'a>>>,
    cur_node: Rc<RefCell<TrieNode<'a>>>,
    selector_state: Option<SelectorState>,
    changes: Vec<Change>,
}

impl<'a> TransientState<'a> {
    fn new(args: &'a KTransientMenu, window: GuiWin, pane: MuxPane) -> Self {
        let root_node = Rc::new(RefCell::new(TrieNode::new()));
        let cur_node = Rc::clone(&root_node);
        Self {
            window,
            pane,
            description: &args.description,
            sections: args
                .sections
                .iter()
                .map(|section| TransientSection::new(section))
                .collect(),
            colors: TransientColors::new(),
            root_node,
            cur_node,
            selector_state: None,
            changes: vec![],
        }
    }

    fn render(&mut self, term: &mut TermWizTerminal) -> termwiz::Result<()> {
        self.changes.append(&mut vec![
            Change::ClearScreen(ColorAttribute::Default),
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::Absolute(0),
            },
            Change::Attribute(AttributeChange::Intensity(Intensity::Bold)),
            Change::Attribute(AttributeChange::Foreground(self.colors.description_fg)),
            Change::Text(self.description.to_string()),
            Change::AllAttributes(CellAttributes::default()),
            Change::Text("\r\n".to_string()),
            Change::Text("─".repeat(self.description.len())),
        ]);

        for section in &self.sections {
            section.render(&self.colors, &mut self.changes);
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
                y: Position::EndRelative(1),
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

        let line = editor.read_line_with_optional_initial_value(&mut host, None)?;
        if let Some(line) = line {
            option.value = if line.len() == 0 {
                option.default.clone()
            } else {
                Some(line)
            };
        }
        self.changes
            .push(Change::CursorVisibility(CursorVisibility::Hidden));

        Ok(())
    }

    fn selector(
        &mut self,
        term: &mut TermWizTerminal,
        option: &mut TransientOption,
    ) -> anyhow::Result<()> {
        let size = term.get_screen_size()?;
        let cols = size.cols;
        let max_width = cols.saturating_sub(6);
        let max_items = size.rows.saturating_sub(ROW_OVERHEAD);
        let selector_state = self.selector_state.as_mut().unwrap();
        if max_items != selector_state.max_items {
            selector_state.max_items = max_items;
        }
        let input_selector_size = selector_state.choices.len().min(max_items);
        let changes = &mut self.changes;
        changes.append(&mut vec![
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::EndRelative(1 + input_selector_size),
            },
            Change::ClearToEndOfScreen(ColorAttribute::Default),
            Change::Text("─".repeat(cols)),
            Change::Text("\r\n".to_string()),
            Change::Text(truncate_right(
                &format!("{}: {}", option.description, selector_state.filter_term),
                max_width,
            )),
        ]);

        for (row_num, (entry_idx, entry)) in selector_state
            .filtered_entries
            .iter()
            .enumerate()
            .skip(selector_state.top_row)
            .enumerate()
        {
            if row_num > max_items {
                break;
            }

            changes.push(Change::Text("\r\n".to_string()));

            let mut attr = CellAttributes::blank();

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
        changes.append(&mut vec![
            Change::CursorPosition {
                x: Position::Absolute(
                    2 + option.description.len() + selector_state.filter_term.len(),
                ),
                y: Position::EndRelative(input_selector_size),
            },
            Change::CursorVisibility(CursorVisibility::Visible),
        ]);

        term.render(changes)?;
        changes.clear();

        Ok(())
    }

    fn trigger_event(&self, name: &str) {
        let name = name.to_string();
        let window = self.window.clone();
        let pane = self.pane;
        let result = TransientResult::new(&self.sections);
        promise::spawn::spawn_into_main_thread(async move {
            trampoline(name, window, pane, result);
            anyhow::Result::<()>::Ok(())
        })
        .detach();
    }

    fn run_loop(&mut self, term: &mut TermWizTerminal) -> anyhow::Result<()> {
        while let Ok(Some(event)) = term.poll_input(None) {
            let selector_state = self.selector_state.is_some();
            match event {
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('G' | 'C' | 'D' | '['),
                    modifiers: Modifiers::CTRL,
                }) => {
                    break;
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Escape,
                    ..
                }) if !selector_state => {
                    break;
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char(c),
                    ..
                }) if !selector_state => {
                    let cur_node = Rc::clone(&self.cur_node);
                    let cur_node = cur_node.borrow();
                    match cur_node.find_char(c) {
                        Some(cur_node) => {
                            let cur_node_borrowed = cur_node.borrow();
                            if cur_node_borrowed.is_end_of_word {
                                match cur_node_borrowed.entry.as_ref().unwrap() {
                                    TransientEntry::TransientSwitch(switch) => {
                                        {
                                            let mut switch = switch.borrow_mut();
                                            let switch = switch.deref_mut();
                                            switch.value = !switch.value;
                                        }
                                        self.cur_node = Rc::clone(&self.root_node);
                                        self.render(term)?;
                                    }
                                    TransientEntry::TransientOption(option) => {
                                        {
                                            let mut option = option.borrow_mut();
                                            let option = option.deref_mut();
                                            if option.value.is_none() || !option.allow_nil {
                                                if let Some(choices) = option.choices.clone() {
                                                    self.cur_node = Rc::clone(&cur_node);
                                                    self.selector_state =
                                                        Some(SelectorState::new(choices));
                                                    self.selector(term, option)?;
                                                    continue;
                                                } else {
                                                    self.line_prompt(term, option)?;
                                                }
                                            } else {
                                                option.value = None;
                                            }
                                        }
                                        self.cur_node = Rc::clone(&self.root_node);
                                        self.render(term)?;
                                    }
                                    TransientEntry::TransientCyclicSwitch(cyclic_switch) => {
                                        {
                                            let mut cyclic_switch = cyclic_switch.borrow_mut();
                                            let cyclic_switch = cyclic_switch.deref_mut();

                                            if cyclic_switch.choices.first().is_some() {
                                                if let Some(idx) = cyclic_switch.active_idx {
                                                    if idx == cyclic_switch.choices.len() - 1 {
                                                        cyclic_switch.active_idx =
                                                            if cyclic_switch.allow_nil {
                                                                None
                                                            } else {
                                                                Some(0)
                                                            };
                                                    } else {
                                                        cyclic_switch.active_idx.replace(idx + 1);
                                                    }
                                                } else {
                                                    cyclic_switch.active_idx = Some(0);
                                                }
                                            }
                                        }
                                        self.cur_node = Rc::clone(&self.root_node);
                                        self.render(term)?;
                                    }
                                    TransientEntry::TransientArgument(positional_arg) => {
                                        let positional_arg = positional_arg.borrow();
                                        let name = match *positional_arg.action {
                                            KeyAssignment::EmitEvent(ref id) => id,
                                            _ => anyhow::bail!("TransientMenu requires action to be defined by wezterm.action_callback")
                                        };
                                        self.trigger_event(name);
                                        break;
                                    }
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
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('P' | 'K'),
                    modifiers: Modifiers::CTRL,
                }) => {
                    let cur_node = Rc::clone(&self.cur_node);
                    let cur_node = cur_node.borrow();
                    match cur_node.entry.as_ref() {
                        Some(TransientEntry::TransientOption(option)) => {
                            self.selector_state.as_mut().unwrap().move_up();
                            self.render(term)?;
                            let mut option = option.borrow_mut();
                            let option = option.deref_mut();
                            self.selector(term, option)?;
                        }
                        _ => {}
                    }
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char('N' | 'J'),
                    modifiers: Modifiers::CTRL,
                }) => {
                    let cur_node = Rc::clone(&self.cur_node);
                    let cur_node = cur_node.borrow();
                    match cur_node.entry.as_ref() {
                        Some(TransientEntry::TransientOption(option)) => {
                            self.selector_state.as_mut().unwrap().move_down();
                            self.render(term)?;
                            let mut option = option.borrow_mut();
                            let option = option.deref_mut();
                            self.selector(term, option)?;
                        }
                        _ => {}
                    }
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Escape,
                    ..
                }) => {
                    self.selector_state = None;
                    self.cur_node = Rc::clone(&self.root_node);
                    self.changes
                        .push(Change::CursorVisibility(CursorVisibility::Hidden));
                    self.render(term)?;
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Enter,
                    ..
                }) => {
                    let selector_state = self.selector_state.as_ref().unwrap();
                    let active_idx = selector_state.active_idx;
                    if let Some(entry) = selector_state.filtered_entries.get(active_idx).cloned() {
                        let cur_node = Rc::clone(&self.cur_node);
                        let cur_node = cur_node.borrow();
                        match cur_node.entry.as_ref().unwrap() {
                            TransientEntry::TransientOption(option) => {
                                option.borrow_mut().value = Some(entry);
                                self.selector_state = None;
                                self.cur_node = Rc::clone(&self.root_node);
                                self.changes
                                    .push(Change::CursorVisibility(CursorVisibility::Hidden));
                                self.render(term)?;
                            }
                            _ => {}
                        }
                    }
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Backspace,
                    ..
                }) => {
                    let cur_node = Rc::clone(&self.cur_node);
                    let cur_node = cur_node.borrow();
                    match cur_node.entry.as_ref() {
                        Some(TransientEntry::TransientOption(option)) => {
                            let selector_state = self.selector_state.as_mut().unwrap();
                            if selector_state.filter_term.pop().is_some() {
                                selector_state.update_filter();
                                self.render(term)?;
                                let mut option = option.borrow_mut();
                                let option = option.deref_mut();
                                self.selector(term, option)?;
                            }
                        }
                        _ => {}
                    }
                }
                InputEvent::Key(KeyEvent {
                    key: KeyCode::Char(c),
                    ..
                }) => {
                    let cur_node = Rc::clone(&self.cur_node);
                    let cur_node = cur_node.borrow();
                    match cur_node.entry.as_ref() {
                        Some(TransientEntry::TransientOption(option)) => {
                            let selector_state = self.selector_state.as_mut().unwrap();
                            selector_state.filter_term.push(c);
                            selector_state.update_filter();
                            self.render(term)?;
                            let mut option = option.borrow_mut();
                            let option = option.deref_mut();
                            self.selector(term, option)?;
                        }
                        _ => {}
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

impl TransientResult {
    fn new(sections: &Vec<TransientSection>) -> Self {
        let mut entries: Vec<TransientResultEntry> = vec![];

        for section in sections {
            for entry in &section.entries {
                match entry {
                    TransientEntry::TransientOption(option) => {
                        let option = option.borrow();
                        let option = option.deref();
                        entries.push(TransientResultEntry {
                            flag: option.flag.clone(),
                            value: option.value.to_dynamic(),
                        });
                    }
                    TransientEntry::TransientSwitch(switch) => {
                        let switch = switch.borrow();
                        let switch = switch.deref();
                        entries.push(TransientResultEntry {
                            flag: switch.flag.clone(),
                            value: switch.value.to_dynamic(),
                        });
                    }
                    TransientEntry::TransientCyclicSwitch(cyclic_switch) => {
                        let cyclic_switch = cyclic_switch.borrow();
                        let cyclic_switch = cyclic_switch.deref();
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

fn trampoline(name: String, window: GuiWin, pane: MuxPane, result: TransientResult) {
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
    result: TransientResult,
) -> anyhow::Result<()> {
    if let Some(lua) = lua {
        let args = lua.pack_multi((window, pane, result))?;

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
    state
        .changes
        .push(Change::CursorVisibility(CursorVisibility::Hidden));

    for section in &state.sections {
        for entry in &section.entries {
            match entry {
                TransientEntry::TransientSwitch(switch) => {
                    state.root_node.borrow_mut().add_word(
                        &switch.borrow().key,
                        TransientEntry::TransientSwitch(Rc::clone(&switch)),
                    );
                }
                TransientEntry::TransientOption(option) => {
                    state.root_node.borrow_mut().add_word(
                        &option.borrow().key,
                        TransientEntry::TransientOption(Rc::clone(&option)),
                    );
                }
                TransientEntry::TransientCyclicSwitch(cyclic_switch) => {
                    state.root_node.borrow_mut().add_word(
                        &cyclic_switch.borrow().key,
                        TransientEntry::TransientCyclicSwitch(Rc::clone(&cyclic_switch)),
                    );
                }
                TransientEntry::TransientArgument(positional_arg) => {
                    state.root_node.borrow_mut().add_word(
                        &positional_arg.borrow().key,
                        TransientEntry::TransientArgument(Rc::clone(&positional_arg)),
                    );
                }
            }
        }
    }

    state.render(&mut term)?;
    state.run_loop(&mut term)
}
