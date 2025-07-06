use crate::scripting::guiwin::GuiWin;
use config::keyassignment::{EditCommand, KeyAssignment};
use config::{AnsiColor, ColorAttribute};
use luahelper::impl_lua_conversion_dynamic;
use mux::termwiztermtab::TermWizTerminal;
use mux_lua::MuxPane;
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

#[derive(Debug, Clone, PartialEq)]
struct EditingCommandSwitch {
    key: String,
    value: bool,
    description: String,
    flag: String,
}

#[derive(Debug, Clone, PartialEq)]
struct EditingCommandOption {
    key: String,
    value: Option<String>,
    default: Option<String>,
    description: String,
    flag: String,
}

#[derive(Debug, Clone, PartialEq)]
struct EditingCommandArgument {
    key: String,
    description: String,
}

#[derive(Debug, Clone, PartialEq, FromDynamic, ToDynamic)]
struct EditedCommandSwitch {
    key: String,
    value: bool,
}

#[derive(Debug, Clone, PartialEq, FromDynamic, ToDynamic)]
struct EditedCommandOption {
    key: String,
    value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, FromDynamic, ToDynamic)]
struct EditedCommand {
    switches: Vec<EditedCommandSwitch>,
    options: Vec<EditedCommandOption>,
    argument: String,
}

impl_lua_conversion_dynamic!(EditedCommand);

pub fn show_edit_command_overlay(
    mut term: TermWizTerminal,
    args: EditCommand,
    window: GuiWin,
    pane: MuxPane,
) -> anyhow::Result<()> {
    let name = match *args.action {
        KeyAssignment::EmitEvent(ref id) => id,
        _ => anyhow::bail!("EditCommand requires action to be defined by wezterm.action_callback"),
    };

    term.no_grab_mouse_in_raw_mode();

    term.render(&[Change::CursorVisibility(CursorVisibility::Hidden)])?;

    let description = &args.description;
    let mut editing_switches: Vec<EditingCommandSwitch> = args
        .switches
        .iter()
        .map(|switch| EditingCommandSwitch {
            key: switch.key.clone(),
            value: switch.default,
            description: switch.description.clone(),
            flag: switch.flag.clone(),
        })
        .collect();
    let mut editing_options: Vec<EditingCommandOption> = args
        .options
        .iter()
        .map(|option| EditingCommandOption {
            key: option.key.clone(),
            value: option.default.clone(),
            default: option.default.clone(),
            description: option.description.clone(),
            flag: option.flag.clone(),
        })
        .collect();
    let mut editing_arguments: Vec<EditingCommandArgument> = args
        .arguments
        .iter()
        .map(|argument| EditingCommandArgument {
            key: argument.key.clone(),
            description: argument.description.clone(),
        })
        .collect();

    render(
        &mut term,
        description,
        &mut editing_switches,
        &mut editing_options,
        &mut editing_arguments,
    )?;

    let mut flag_mode = false;

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
            }) if flag_mode => {
                if let Some(switch) = editing_switches
                    .iter_mut()
                    .find(|switch| switch.key.chars().next() == Some(c))
                {
                    switch.value = !switch.value;
                    render(
                        &mut term,
                        description,
                        &mut editing_switches,
                        &mut editing_options,
                        &mut editing_arguments,
                    )?;
                } else if let Some(option) = editing_options
                    .iter_mut()
                    .find(|option| option.key.chars().next() == Some(c))
                {
                    let val = option.value.take();
                    if val.is_none() {
                        term.render(&[Change::CursorVisibility(CursorVisibility::Visible)])?;

                        let mut host = PromptHost::new();
                        let mut editor = LineEditor::new(&mut term);
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
                        term.render(&[Change::CursorVisibility(CursorVisibility::Hidden)])?;
                    }
                    render(
                        &mut term,
                        description,
                        &mut editing_switches,
                        &mut editing_options,
                        &mut editing_arguments,
                    )?;
                }
                flag_mode = false;
            }
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char('-'),
                modifiers: Modifiers::NONE,
            }) => {
                flag_mode = true;
            }
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char(c),
                ..
            }) => {
                if let Some(positional_arg) = editing_arguments
                    .iter()
                    .find(|positional_arg| positional_arg.key.chars().next() == Some(c))
                {
                    let switches: Vec<EditedCommandSwitch> = editing_switches
                        .iter()
                        .map(|switch| EditedCommandSwitch {
                            key: switch.key.clone(),
                            value: switch.value,
                        })
                        .collect();
                    let options: Vec<EditedCommandOption> = editing_options
                        .iter()
                        .map(|option| EditedCommandOption {
                            key: option.key.clone(),
                            value: option.value.clone(),
                        })
                        .collect();
                    let edit_command = EditedCommand {
                        switches,
                        options,
                        argument: positional_arg.key.to_string(),
                    };
                    let name = name.to_string();
                    promise::spawn::spawn_into_main_thread(async move {
                        trampoline(name, window, pane, edit_command);
                        anyhow::Result::<()>::Ok(())
                    })
                    .detach();
                    break;
                }
            }
            _ => {}
        }
    }

    Ok(())
}

fn render(
    term: &mut TermWizTerminal,
    description: &str,
    switches: &mut Vec<EditingCommandSwitch>,
    options: &mut Vec<EditingCommandOption>,
    arguments: &mut Vec<EditingCommandArgument>,
) -> anyhow::Result<()> {
    let mut changes = vec![
        Change::ClearScreen(ColorAttribute::Default),
        Change::CursorPosition {
            x: Position::Absolute(0),
            y: Position::Absolute(0),
        },
        Change::Attribute(AttributeChange::Intensity(Intensity::Bold)),
        Change::Attribute(AttributeChange::Foreground(AnsiColor::Teal.into())),
        Change::Text("Command".to_string()),
        Change::AllAttributes(CellAttributes::default()),
        Change::Text(format!(": {}\r\n", description)),
        Change::Text("-".repeat(9 + description.len())),
        Change::Text("\r\n\r\n".to_string()),
    ];

    changes.push(Change::Attribute(AttributeChange::Intensity(
        Intensity::Bold,
    )));
    changes.push(Change::Attribute(AttributeChange::Foreground(
        AnsiColor::Blue.into(),
    )));
    changes.push(Change::Text("Switches".to_string()));
    changes.push(Change::AllAttributes(CellAttributes::default()));

    for switch in switches {
        changes.push(Change::Text("\r\n\t".to_string()));
        changes.push(Change::Attribute(AttributeChange::Foreground(
            AnsiColor::Purple.into(),
        )));
        changes.push(Change::Text(format!("-{}", switch.key)));
        changes.push(Change::Attribute(AttributeChange::Foreground(
            ColorAttribute::Default,
        )));

        changes.push(Change::Text(format!(" {} (", switch.description)));
        if switch.value {
            changes.push(Change::Attribute(AttributeChange::Intensity(
                Intensity::Bold,
            )));
            changes.push(Change::Attribute(AttributeChange::Foreground(
                AnsiColor::Red.into(),
            )));
            changes.push(Change::Text(switch.flag.to_string()));
            changes.push(Change::AllAttributes(CellAttributes::default()));
        } else {
            changes.push(Change::Text(switch.flag.to_string()));
        }
        changes.push(Change::Text(")".to_string()));
    }

    changes.push(Change::Text("\r\n\r\n".to_string()));

    changes.push(Change::Attribute(AttributeChange::Intensity(
        Intensity::Bold,
    )));
    changes.push(Change::Attribute(AttributeChange::Foreground(
        AnsiColor::Blue.into(),
    )));
    changes.push(Change::Text("Options".to_string()));
    changes.push(Change::AllAttributes(CellAttributes::default()));

    for option in options {
        changes.push(Change::Text("\r\n\t".to_string()));
        changes.push(Change::Attribute(AttributeChange::Foreground(
            AnsiColor::Purple.into(),
        )));
        changes.push(Change::Text(format!("-{}", option.key)));
        changes.push(Change::Attribute(AttributeChange::Foreground(
            ColorAttribute::Default,
        )));

        changes.push(Change::Text(format!(" {} (", option.description)));

        if let Some(val) = &option.value {
            changes.push(Change::Attribute(AttributeChange::Intensity(
                Intensity::Bold,
            )));
            changes.push(Change::Attribute(AttributeChange::Foreground(
                AnsiColor::Red.into(),
            )));
            changes.push(Change::Text(format!("{}={}", option.flag, val)));
            changes.push(Change::AllAttributes(CellAttributes::default()));
        } else {
            changes.push(Change::Text(format!("{}=", option.flag)));
        }

        changes.push(Change::Text(")".to_string()));
    }

    changes.push(Change::Text("\r\n\r\n".to_string()));
    changes.push(Change::Attribute(AttributeChange::Intensity(
        Intensity::Bold,
    )));
    changes.push(Change::Attribute(AttributeChange::Foreground(
        AnsiColor::Blue.into(),
    )));
    changes.push(Change::Text("Arguments".to_string()));
    changes.push(Change::AllAttributes(CellAttributes::default()));

    for positional_arg in arguments {
        changes.push(Change::Text("\r\n\t".to_string()));
        changes.push(Change::Attribute(AttributeChange::Foreground(
            AnsiColor::Purple.into(),
        )));
        changes.push(Change::Text(positional_arg.key.clone()));
        changes.push(Change::Attribute(AttributeChange::Foreground(
            ColorAttribute::Default,
        )));
        changes.push(Change::Text(format!(" {}", positional_arg.description)));
    }

    changes.push(Change::Text("\r\n\r\n\r\n".to_string()));
    term.render(&changes)?;

    Ok(())
}

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
