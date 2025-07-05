use crate::scripting::guiwin::GuiWin;
use config::keyassignment::{EditCommand, KeyAssignment};
use mux::termwiztermtab::TermWizTerminal;
use mux_lua::MuxPane;
use serde::Serialize;
use std::rc::Rc;
use termwiz::input::{InputEvent, KeyCode, KeyEvent};
use termwiz::lineedit::{Action, BasicHistory, History, LineEditor, LineEditorHost};
use termwiz::surface::{Change, CursorVisibility};
use termwiz::terminal::Terminal;
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

#[derive(Debug, Clone, PartialEq, Serialize)]
struct EditedCommandSwitch {
    key: String,
    value: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct EditedCommandOption {
    key: String,
    value: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct EditingCommandOption {
    key: String,
    value: Option<String>,
    default: Option<String>,
    description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct EditedCommand {
    switches: Vec<EditedCommandSwitch>,
    options: Vec<EditedCommandOption>,
    argument: String,
}

pub fn show_edit_command_overlay(
    mut term: TermWizTerminal,
    args: EditCommand,
    window: GuiWin,
    pane: MuxPane,
) -> anyhow::Result<()> {
    let name = match *args.action {
        KeyAssignment::EmitEvent(id) => id,
        _ => anyhow::bail!("EditCommand requires action to be defined by wezterm.action_callback"),
    };

    term.no_grab_mouse_in_raw_mode();

    let mut changes = vec![
        Change::CursorVisibility(CursorVisibility::Hidden),
        Change::Text(format!("{}\r\n\r\nSwitches", args.description)),
    ];

    for switch in &args.switches {
        changes.push(Change::Text(format!(
            "\r\n\t-{} {} ({})",
            switch.key, switch.description, switch.flag
        )));
    }

    changes.push(Change::Text("\r\n\r\nOptions".to_string()));
    for option in &args.options {
        changes.push(Change::Text(format!(
            "\r\n\t-{} {} ({}=",
            option.key, option.description, option.flag,
        )));
        if let Some(default) = &option.default {
            changes.push(Change::Text(format!("{})", default)))
        } else {
            changes.push(Change::Text(")".to_string()))
        }
    }

    changes.push(Change::Text("\r\n\r\nArguments".to_string()));
    for positional_arg in &args.arguments {
        changes.push(Change::Text(format!(
            "\r\n\t{} {}",
            positional_arg.key, positional_arg.description
        )));
    }

    changes.push(Change::Text("\r\n".to_string()));

    let (mut switches, mut editing_options, mut arguments) = (vec![], vec![], vec![]);

    for switch in &args.switches {
        switches.push({
            EditedCommandSwitch {
                key: switch.key.clone(),
                value: switch.default,
            }
        });
    }

    for option in &args.options {
        editing_options.push({
            EditingCommandOption {
                key: option.key.clone(),
                value: option.default.clone(),
                default: option.default.clone(),
                description: option.description.clone(),
            }
        });
    }

    for argument in &args.arguments {
        arguments.push(argument.key.clone());
    }

    term.render(&changes)?;

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
                key: KeyCode::Char('-'),
                modifiers: Modifiers::NONE,
            }) => {
                if let Ok(Some(event)) = term.poll_input(None) {
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
                            if let Some(switch) = switches
                                .iter_mut()
                                .find(|switch| switch.key.chars().next() == Some(c))
                            {
                                switch.value = !switch.value;
                            } else if let Some(option) = editing_options
                                .iter_mut()
                                .find(|option| option.key.chars().next() == Some(c))
                            {
                                let val = option.value.take();
                                if val.is_none() {
                                    term.render(&[Change::CursorVisibility(
                                        CursorVisibility::Visible,
                                    )])?;
                                    let mut host = PromptHost::new();
                                    let mut editor = LineEditor::new(&mut term);
                                    let mut prompt = option.description.clone();
                                    if let Some(default) = option.default.clone() {
                                        prompt.push_str(&format!(" (default {})", default));
                                    }
                                    prompt.push_str(": ");
                                    editor.set_prompt(&prompt);
                                    let line = editor
                                        .read_line_with_optional_initial_value(&mut host, None)?;
                                    term.render(&[Change::CursorVisibility(
                                        CursorVisibility::Hidden,
                                    )])?;
                                    if let Some(line) = line {
                                        option.value = if line.len() == 0 {
                                            option.default.clone()
                                        } else {
                                            Some(line)
                                        };
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char(c),
                ..
            }) => {
                if let Some(positional_arg) = arguments
                    .iter()
                    .find(|positional_arg| positional_arg.chars().next() == Some(c))
                {
                    let mut options = vec![];
                    for option in editing_options {
                        options.push(EditedCommandOption {
                            key: option.key,
                            value: option.value,
                        });
                    }
                    let edit_command = EditedCommand {
                        switches,
                        options,
                        argument: positional_arg.to_string(),
                    };
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
        let serialized = serde_json::to_string(&edited_command).unwrap();
        let args = lua.pack_multi((window, pane, serialized))?;

        if let Err(err) = config::lua::emit_event(&lua, (name.clone(), args)).await {
            log::error!("while processing {} event: {:#}", name, err);
        }
    }

    Ok(())
}
