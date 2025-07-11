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
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use termwiz::input::{InputEvent, KeyCode, KeyEvent};
use termwiz::lineedit::{Action, BasicHistory, History, LineEditor, LineEditorHost};
use termwiz::surface::{Change, CursorVisibility, Position};
use termwiz::terminal::Terminal;
use wezterm_dynamic::{FromDynamic, ToDynamic};
use wezterm_term::{AttributeChange, CellAttributes, Intensity};
use window::Modifiers;

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

enum EditingCommandEntity<'a> {
    EditingCommandOption(&'a RefCell<EditingCommandOption>),
    EditingCommandSwitch(&'a RefCell<EditingCommandSwitch>),
    EditingCommandArgument(&'a RefCell<EditingCommandArgument>),
}

struct TrieNode<'a> {
    children: HashMap<char, Rc<RefCell<TrieNode<'a>>>>,
    is_end_of_word: bool,
    entity: Option<EditingCommandEntity<'a>>,
}

impl<'a> TrieNode<'a> {
    fn new() -> Self {
        Self {
            children: HashMap::new(),
            is_end_of_word: true,
            entity: None,
        }
    }

    fn add_word(&mut self, word: &str, entity: EditingCommandEntity<'a>) {
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

struct Trie<'a> {
    root: Rc<RefCell<TrieNode<'a>>>,
}

impl Trie<'_> {
    fn new() -> Self {
        Self {
            root: Rc::new(RefCell::new(TrieNode::new())),
        }
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
}

impl EditingCommandOption {
    fn new(option: &EditCommandOption) -> Self {
        Self {
            key: option.key.clone(),
            value: option.default.clone(),
            default: option.default.clone(),
            description: option.description.clone(),
            flag: option.flag.clone(),
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
    switches: Vec<RefCell<EditingCommandSwitch>>,
    options: Vec<RefCell<EditingCommandOption>>,
    arguments: Vec<RefCell<EditingCommandArgument>>,
}

impl<'a> EditingCommandSection<'a> {
    fn new(section: &'a EditCommandSection) -> Self {
        Self {
            header: &section.header,
            switches: section
                .switches
                .iter()
                .map(|switch| RefCell::new(EditingCommandSwitch::new(switch)))
                .collect(),
            options: section
                .options
                .iter()
                .map(|option| RefCell::new(EditingCommandOption::new(option)))
                .collect(),
            arguments: section
                .arguments
                .iter()
                .map(|argument| RefCell::new(EditingCommandArgument::new(argument)))
                .collect(),
        }
    }
}

struct EditingCommandState<'a> {
    window: GuiWin,
    pane: MuxPane,
    description: &'a str,
    sections: Vec<EditingCommandSection<'a>>,
    colors: EditingCommandColors,
    trie: Trie<'a>,
    cur_node: RefCell<Rc<RefCell<TrieNode<'a>>>>,
}

impl<'a> EditingCommandState<'a> {
    fn new(args: &'a EditCommand, window: GuiWin, pane: MuxPane) -> Self {
        let trie = Trie::new();
        let cur_node = RefCell::new(Rc::clone(&trie.root));
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
            trie,
            cur_node,
        }
    }

    fn render(&self, term: &mut TermWizTerminal) -> termwiz::Result<()> {
        let mut changes = vec![
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
            Change::Text("-".repeat(self.description.len())),
        ];

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
        term.render(&changes)?;

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

    fn run_loop(&self, term: &mut TermWizTerminal) -> anyhow::Result<()> {
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
                    key: KeyCode::Char(c),
                    ..
                }) => {
                    let cur_node = Rc::clone(&self.cur_node.borrow());
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
                                        self.cur_node.replace(Rc::clone(&self.trie.root));
                                        self.render(term)?;
                                    }
                                    EditingCommandEntity::EditingCommandOption(option) => {
                                        {
                                            let mut option = option.borrow_mut();
                                            let option = option.deref_mut();
                                            let val = option.value.take();
                                            if val.is_none() {
                                                let size = term.get_screen_size()?;
                                                term.render(&[
                                                    Change::CursorVisibility(
                                                        CursorVisibility::Visible,
                                                    ),
                                                    Change::Text("-".repeat(size.cols)),
                                                    Change::Text("\r\n".to_string()),
                                                ])?;

                                                let mut host = PromptHost::new();
                                                let mut editor = LineEditor::new(term);
                                                let mut prompt = option.description.clone();
                                                if let Some(default) = option.default.clone() {
                                                    prompt.push_str(&format!(
                                                        " (default {})",
                                                        default
                                                    ));
                                                }
                                                prompt.push_str(": ");
                                                editor.set_prompt(&prompt);
                                                let line = editor
                                                    .read_line_with_optional_initial_value(
                                                        &mut host, None,
                                                    )?;
                                                if let Some(line) = line {
                                                    option.value = if line.len() == 0 {
                                                        option.default.clone()
                                                    } else {
                                                        Some(line)
                                                    };
                                                }
                                                term.render(&[Change::CursorVisibility(
                                                    CursorVisibility::Hidden,
                                                )])?;
                                            }
                                        }
                                        self.cur_node.replace(Rc::clone(&self.trie.root));
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
                                self.cur_node.replace(Rc::clone(&cur_node));
                            }
                        }
                        None => {
                            self.cur_node.replace(Rc::clone(&self.trie.root));
                        }
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
    term.render(&[Change::CursorVisibility(CursorVisibility::Hidden)])?;

    let state = EditingCommandState::new(&args, window, pane);

    for section in &state.sections {
        for switch in &section.switches {
            state.trie.root.borrow_mut().add_word(
                &switch.borrow().key,
                EditingCommandEntity::EditingCommandSwitch(switch),
            )
        }
        for option in &section.options {
            state.trie.root.borrow_mut().add_word(
                &option.borrow().key,
                EditingCommandEntity::EditingCommandOption(option),
            )
        }
        for positional_arg in &section.arguments {
            state.trie.root.borrow_mut().add_word(
                &positional_arg.borrow().key,
                EditingCommandEntity::EditingCommandArgument(positional_arg),
            )
        }
    }

    state.render(&mut term)?;
    state.run_loop(&mut term)
}
