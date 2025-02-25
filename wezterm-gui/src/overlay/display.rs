use config::keyassignment::DisplayText;
use mux::termwiztermtab::TermWizTerminal;
use termwiz::input::{InputEvent, KeyCode, KeyEvent};
use termwiz::surface::Change;
use termwiz::terminal::Terminal;
use window::Modifiers;

pub fn show_display_text_overlay(
    mut term: TermWizTerminal,
    args: DisplayText,
) -> anyhow::Result<()> {
    term.no_grab_mouse_in_raw_mode();
    let mut text = args.text.replace("\r\n", "\n").replace("\n", "\r\n");
    text.push_str("\r\n");
    term.render(&[Change::Text(text)])?;
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
            _ => {}
        }
    }
    Ok(())
}
