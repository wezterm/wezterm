use crate::overlay::selector::{matcher_pattern, matcher_score};
use crate::scripting::guiwin::GuiWin;
use config::configuration;
use config::keyassignment::{
    EditCommand, EditCommandArgument, EditCommandOption, EditCommandSection, EditCommandSwitch,
    KeyAssignment,
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
use wezterm_dynamic::{FromDynamic, ToDynamic};
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

enum EditingCommandEntity {
    EditingCommandOption(Rc<RefCell<EditingCommandOption>>),
    EditingCommandSwitch(Rc<RefCell<EditingCommandSwitch>>),
    EditingCommandArgument(Rc<RefCell<EditingCommandArgument>>),
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
    entity: Option<EditingCommandEntity>,
}

impl<'a> TrieNode<'a> {
    fn new() -> Self {
        Self {
            children: HashMap::new(),
            is_end_of_word: true,
            entity: None,
        }
    }

    fn add_word(&mut self, word: &str, entity: EditingCommandEntity) {
        match word.chars().next() {
            Some(c) => {
                match self.children.get(&c) {
                    Some(child_node) => {
                        child_node.borrow_mut().add_word(&word[1..], entity);
                    }
                    None => {
                        let mut new_node = TrieNode::new();
                        new_node.add_word(&word[1..], entity);
                        self.children.insert(c, Rc::new(RefCell::new(new_node)));
                    }
                }
                self.is_end_of_word = false;
            }
            None => self.entity = Some(entity),
        }
    }

    fn find_char(&self, c: char) -> Option<Rc<RefCell<TrieNode<'a>>>> {
        self.children.get(&c).map(|child| Rc::clone(child))
    }
}

struct EditingCommandColors {
    description_fg: ColorAttribute,
    section_header_fg: ColorAttribute,
    key_fg: ColorAttribute,
    flag_fg: ColorAttribute,
}

impl EditingCommandColors {
    fn new() -> Self {
        let config = configuration();
        let colors = &config.resolved_palette;

        Self {
            description_fg: colors
                .edit_command_description_fg
                .unwrap_or(AnsiColor::Teal.into())
                .into(),
            section_header_fg: colors
                .edit_command_section_header_fg
                .unwrap_or(AnsiColor::Navy.into())
                .into(),
            key_fg: colors
                .edit_command_key_fg
                .unwrap_or(AnsiColor::Purple.into())
                .into(),
            flag_fg: colors
                .edit_command_flag_fg
                .unwrap_or(AnsiColor::Red.into())
                .into(),
        }
    }
}

struct EditingCommandSwitch {
    key: String,
    value: bool,
    description: String,
    flag: String,
}

impl EditingCommandSwitch {
    fn new(switch: &EditCommandSwitch) -> Self {
        Self {
            key: switch.key.clone(),
            value: switch.default,
            description: switch.description.clone(),
            flag: switch.flag.clone(),
        }
    }
}

struct EditingCommandOption {
    key: String,
    value: Option<String>,
    default: Option<String>,
    description: String,
    flag: String,
    allow_nil: bool,
    choices: Option<Vec<String>>,
}

impl EditingCommandOption {
    fn new(option: &EditCommandOption) -> Self {
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
}

struct EditingCommandArgument {
    key: String,
    description: String,
    action: Box<KeyAssignment>,
}

impl EditingCommandArgument {
    fn new(argument: &EditCommandArgument) -> Self {
        Self {
            key: argument.key.clone(),
            description: argument.description.clone(),
            action: argument.action.clone(),
        }
    }
}

struct EditingCommandSection<'a> {
    header: &'a str,
    switches: Vec<Rc<RefCell<EditingCommandSwitch>>>,
    options: Vec<Rc<RefCell<EditingCommandOption>>>,
    arguments: Vec<Rc<RefCell<EditingCommandArgument>>>,
}

impl<'a> EditingCommandSection<'a> {
    fn new(section: &'a EditCommandSection) -> Self {
        let switches = section.switches.as_ref().map_or_else(
            || vec![],
            |switches| {
                switches
                    .iter()
                    .map(|switch| Rc::new(RefCell::new(EditingCommandSwitch::new(switch))))
                    .collect()
            },
        );
        let options = section.options.as_ref().map_or_else(
            || vec![],
            |options| {
                options
                    .iter()
                    .map(|option| Rc::new(RefCell::new(EditingCommandOption::new(option))))
                    .collect()
            },
        );
        let arguments = section.arguments.as_ref().map_or_else(
            || vec![],
            |arguments| {
                arguments
                    .iter()
                    .map(|argument| Rc::new(RefCell::new(EditingCommandArgument::new(argument))))
                    .collect()
            },
        );

        Self {
            header: &section.header,
            switches,
            options,
            arguments,
        }
    }
}

struct EditingCommandState<'a> {
    window: GuiWin,
    pane: MuxPane,
    description: &'a str,
    sections: Vec<EditingCommandSection<'a>>,
    colors: EditingCommandColors,
    root_node: Rc<RefCell<TrieNode<'a>>>,
    cur_node: Rc<RefCell<TrieNode<'a>>>,
    selector_state: Option<SelectorState>,
    changes: Vec<Change>,
}

impl<'a> EditingCommandState<'a> {
    fn new(args: &'a EditCommand, window: GuiWin, pane: MuxPane) -> Self {
        let root_node = Rc::new(RefCell::new(TrieNode::new()));
        let cur_node = Rc::clone(&root_node);
        Self {
            window,
            pane,
            description: &args.description,
            sections: args
                .sections
                .iter()
                .map(|section| EditingCommandSection::new(section))
                .collect(),
            colors: EditingCommandColors::new(),
            root_node,
            cur_node,
            selector_state: None,
            changes: vec![],
        }
    }

    fn render(&mut self, term: &mut TermWizTerminal) -> termwiz::Result<()> {
        let changes = &mut self.changes;
        changes.append(&mut vec![
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
            changes.push(Change::Text("\r\n\r\n".to_string()));
            changes.push(Change::Attribute(AttributeChange::Intensity(
                Intensity::Bold,
            )));
            changes.push(Change::Attribute(AttributeChange::Foreground(
                self.colors.section_header_fg,
            )));
            changes.push(Change::Text(section.header.to_string()));
            changes.push(Change::AllAttributes(CellAttributes::default()));

            for switch in section.switches.iter().map(|switch| switch.borrow()) {
                changes.push(Change::Text("\r\n\t".to_string()));
                changes.push(Change::Attribute(AttributeChange::Foreground(
                    self.colors.key_fg,
                )));
                changes.push(Change::Text(format!("{}", switch.key)));
                changes.push(Change::Attribute(AttributeChange::Foreground(
                    ColorAttribute::Default,
                )));

                changes.push(Change::Text(format!(" {} (", switch.description)));
                if switch.value {
                    changes.push(Change::Attribute(AttributeChange::Intensity(
                        Intensity::Bold,
                    )));
                    changes.push(Change::Attribute(AttributeChange::Foreground(
                        self.colors.flag_fg,
                    )));
                    changes.push(Change::Text(switch.flag.to_string()));
                    changes.push(Change::AllAttributes(CellAttributes::default()));
                } else {
                    changes.push(Change::Text(switch.flag.to_string()));
                }
                changes.push(Change::Text(")".to_string()));
            }

            for option in section.options.iter().map(|option| option.borrow()) {
                changes.push(Change::Text("\r\n\t".to_string()));
                changes.push(Change::Attribute(AttributeChange::Foreground(
                    self.colors.key_fg,
                )));
                changes.push(Change::Text(format!("{}", option.key)));
                changes.push(Change::Attribute(AttributeChange::Foreground(
                    ColorAttribute::Default,
                )));

                changes.push(Change::Text(format!(" {} (", option.description)));

                if let Some(val) = &option.value {
                    changes.push(Change::Attribute(AttributeChange::Intensity(
                        Intensity::Bold,
                    )));
                    changes.push(Change::Attribute(AttributeChange::Foreground(
                        self.colors.flag_fg,
                    )));
                    changes.push(Change::Text(format!("{}{}", option.flag, val)));
                    changes.push(Change::AllAttributes(CellAttributes::default()));
                } else {
                    changes.push(Change::Text(format!("{}", option.flag)));
                }

                changes.push(Change::Text(")".to_string()));
            }

            for positional_arg in section
                .arguments
                .iter()
                .map(|positional_arg| positional_arg.borrow())
            {
                changes.push(Change::Text("\r\n\t".to_string()));
                changes.push(Change::Attribute(AttributeChange::Foreground(
                    self.colors.key_fg,
                )));
                changes.push(Change::Text(positional_arg.key.clone()));
                changes.push(Change::Attribute(AttributeChange::Foreground(
                    ColorAttribute::Default,
                )));
                changes.push(Change::Text(format!(" {}", positional_arg.description)));
            }
        }

        changes.push(Change::Text("\r\n\r\n\r\n".to_string()));

        term.render(changes)?;
        changes.clear();

        Ok(())
    }

    fn line_prompt(
        &mut self,
        term: &mut TermWizTerminal,
        option: &mut EditingCommandOption,
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
        option: &mut EditingCommandOption,
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
        let edit_command = EditedCommand::new(&self);
        promise::spawn::spawn_into_main_thread(async move {
            trampoline(name, window, pane, edit_command);
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
                                match cur_node_borrowed.entity.as_ref().unwrap() {
                                    EditingCommandEntity::EditingCommandSwitch(switch) => {
                                        {
                                            let mut switch = switch.borrow_mut();
                                            let switch = switch.deref_mut();
                                            switch.value = !switch.value;
                                        }
                                        self.cur_node = Rc::clone(&self.root_node);
                                        self.render(term)?;
                                    }
                                    EditingCommandEntity::EditingCommandOption(option) => {
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
                                    EditingCommandEntity::EditingCommandArgument(
                                        positional_arg,
                                    ) => {
                                        let positional_arg = positional_arg.borrow();
                                        let name = match *positional_arg.action {
                                            KeyAssignment::EmitEvent(ref id) => id,
                                            _ => anyhow::bail!("EditCommand requires action to be defined by wezterm.action_callback")
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
                    match cur_node.entity.as_ref() {
                        Some(EditingCommandEntity::EditingCommandOption(option)) => {
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
                    match cur_node.entity.as_ref() {
                        Some(EditingCommandEntity::EditingCommandOption(option)) => {
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
                        match cur_node.entity.as_ref().unwrap() {
                            EditingCommandEntity::EditingCommandOption(option) => {
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
                    match cur_node.entity.as_ref() {
                        Some(EditingCommandEntity::EditingCommandOption(option)) => {
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
                    match cur_node.entity.as_ref() {
                        Some(EditingCommandEntity::EditingCommandOption(option)) => {
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
struct EditedCommandSwitch {
    flag: String,
    value: bool,
}

#[derive(FromDynamic, ToDynamic)]
struct EditedCommandOption {
    flag: String,
    value: Option<String>,
}

#[derive(FromDynamic, ToDynamic)]
struct EditedCommand {
    switches: Vec<EditedCommandSwitch>,
    options: Vec<EditedCommandOption>,
}

impl EditedCommand {
    fn new(state: &EditingCommandState) -> Self {
        let mut switches: Vec<EditedCommandSwitch> = vec![];
        let mut options: Vec<EditedCommandOption> = vec![];

        for section in &state.sections {
            for switch in section.switches.iter().map(|switch| switch.borrow()) {
                let switch = switch.deref();
                switches.push(EditedCommandSwitch {
                    flag: switch.flag.clone(),
                    value: switch.value,
                });
            }
            for option in section.options.iter().map(|option| option.borrow()) {
                let option = option.deref();
                options.push(EditedCommandOption {
                    flag: option.flag.clone(),
                    value: option.value.clone(),
                });
            }
        }

        Self { switches, options }
    }
}

impl_lua_conversion_dynamic!(EditedCommand);

fn trampoline(name: String, window: GuiWin, pane: MuxPane, edited_command: EditedCommand) {
    promise::spawn::spawn(async move {
        config::with_lua_config_on_main_thread(move |lua| {
            do_event(lua, name, window, pane, edited_command)
        })
        .await
    })
    .detach();
}

async fn do_event(
    lua: Option<Rc<mlua::Lua>>,
    name: String,
    window: GuiWin,
    pane: MuxPane,
    edited_command: EditedCommand,
) -> anyhow::Result<()> {
    if let Some(lua) = lua {
        let args = lua.pack_multi((window, pane, edited_command))?;

        if let Err(err) = config::lua::emit_event(&lua, (name.clone(), args)).await {
            log::error!("while processing {} event: {:#}", name, err);
        }
    }

    Ok(())
}

pub fn show_edit_command_overlay(
    mut term: TermWizTerminal,
    args: EditCommand,
    window: GuiWin,
    pane: MuxPane,
) -> anyhow::Result<()> {
    term.no_grab_mouse_in_raw_mode();
    let mut state = EditingCommandState::new(&args, window, pane);
    state
        .changes
        .push(Change::CursorVisibility(CursorVisibility::Hidden));

    for section in &state.sections {
        for switch in &section.switches {
            state.root_node.borrow_mut().add_word(
                &switch.borrow().key,
                EditingCommandEntity::EditingCommandSwitch(Rc::clone(switch)),
            )
        }
        for option in &section.options {
            state.root_node.borrow_mut().add_word(
                &option.borrow().key,
                EditingCommandEntity::EditingCommandOption(Rc::clone(option)),
            )
        }
        for positional_arg in &section.arguments {
            state.root_node.borrow_mut().add_word(
                &positional_arg.borrow().key,
                EditingCommandEntity::EditingCommandArgument(Rc::clone(positional_arg)),
            )
        }
    }

    state.render(&mut term)?;
    state.run_loop(&mut term)
}
