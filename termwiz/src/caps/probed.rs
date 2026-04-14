use crate::color::SrgbaTuple;
use crate::escape::csi::{Device, Window};
use crate::escape::osc::{ColorOrQuery, DynamicColorNumber};
use crate::escape::parser::Parser;
use crate::escape::{
    Action, ControlCode, DeviceControlMode, Esc, EscCode, OperatingSystemCommand, CSI,
};
use crate::terminal::ScreenSize;
use crate::{bail, Result};
use std::io::{Read, Write};

const TMUX_BEGIN: &str = "\u{1b}Ptmux;\u{1b}";
const TMUX_END: &str = "\u{1b}\\";
const NO_DYNAMIC_COLOR_RESPONSE: &str = "terminal did not respond to dynamic color query";

/// Represents a terminal name and version.
/// The name XtVersion is because this value is produced
/// by querying the terminal using the XTVERSION escape
/// sequence, which was defined by xterm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XtVersion(String);

impl XtVersion {
    /// Split the version string into a name component and a version
    /// component.  Currently it recognizes `Name(Version)` and
    /// `Name Version` forms. If a form is not recognized, returns None.
    pub fn name_and_version(&self) -> Option<(&str, &str)> {
        if self.0.ends_with(")") {
            let paren = self.0.find('(')?;
            Some((&self.0[0..paren], &self.0[paren + 1..self.0.len() - 1]))
        } else {
            let space = self.0.find(' ')?;
            Some((&self.0[0..space], &self.0[space + 1..]))
        }
    }

    /// Returns true if this represents tmux
    pub fn is_tmux(&self) -> bool {
        self.0.starts_with("tmux ")
    }

    /// Return the full underlying version string
    pub fn full_version(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_xtversion_name() {
        for (input, result) in [
            ("WezTerm something", Some(("WezTerm", "something"))),
            ("xterm(something)", Some(("xterm", "something"))),
            ("something-else", None),
        ] {
            let version = XtVersion(input.to_string());
            assert_eq!(version.name_and_version(), result, "{input}");
        }
    }
}

/// This struct is a helper that uses probing to determine specific capabilities
/// of the associated Terminal instance.
/// It will write and read data to and from the associated Terminal.
pub struct ProbeCapabilities<'a> {
    read: Box<&'a mut dyn Read>,
    write: Box<&'a mut dyn Write>,
}

impl<'a> ProbeCapabilities<'a> {
    pub fn new<R: Read, W: Write>(read: &'a mut R, write: &'a mut W) -> Self {
        Self {
            read: Box::new(read),
            write: Box::new(write),
        }
    }

    /// Probe for the XTVERSION response
    pub fn xt_version(&mut self) -> Result<XtVersion> {
        self.xt_version_impl(false)
    }

    /// Assuming that we are talking to tmux, probe for the XTVERSION response
    /// of its outer terminal.
    pub fn outer_xt_version(&mut self) -> Result<XtVersion> {
        self.xt_version_impl(true)
    }

    fn xt_version_impl(&mut self, tmux_escape: bool) -> Result<XtVersion> {
        let xt_version = CSI::Device(Box::new(Device::RequestTerminalNameAndVersion));
        let dev_attributes = CSI::Device(Box::new(Device::RequestPrimaryDeviceAttributes));

        if tmux_escape {
            write!(self.write, "{TMUX_BEGIN}{xt_version}{TMUX_END}")?;
            self.write.flush()?;
            std::thread::sleep(std::time::Duration::from_millis(100));
            write!(self.write, "{dev_attributes}")?;
        } else {
            write!(self.write, "{xt_version}{dev_attributes}")?;
        }
        self.write.flush()?;
        let mut term = vec![];
        let mut parser = Parser::new();
        let mut done = false;

        while !done {
            let mut byte = [0u8];
            self.read.read(&mut byte)?;

            parser.parse(&byte, |action| {
                // print!("{action:?}\r\n");
                match action {
                    Action::Esc(Esc::Code(EscCode::StringTerminator)) => {}
                    Action::DeviceControl(dev) => {
                        if let DeviceControlMode::Data(b) = dev {
                            term.push(b);
                        }
                    }
                    _ => {
                        done = true;
                    }
                }
            });
        }

        Ok(XtVersion(String::from_utf8_lossy(&term).into()))
    }

    /// Probe the terminal for the current value of a dynamic color.
    pub fn dynamic_color(&mut self, which: DynamicColorNumber) -> Result<SrgbaTuple> {
        let query = OperatingSystemCommand::ChangeDynamicColors(which, vec![ColorOrQuery::Query]);
        write!(self.write, "{query}")?;
        self.write.flush()?;

        let mut parser = Parser::new();
        let mut outcome = None;
        let mut waiting_st = false;

        while outcome.is_none() || waiting_st {
            let mut byte = [0u8];
            if self.read.read(&mut byte)? == 0 {
                return Err(no_dynamic_color_response_err());
            }

            parser.parse(&byte, |action| {
                if waiting_st { 
                    waiting_st = false;
                    if !matches!(action, Action::Esc(Esc::Code(EscCode::StringTerminator))) {
                        outcome = Some(Err(no_dynamic_color_response_err()));
                    }
                    return;
                }

                if let Some(color) = parse_dynamic_color_response(action, which) {
                    waiting_st = byte[0] == ControlCode::Escape as u8;
                    outcome = Some(Ok(color));
                } else {
                    outcome = Some(Err(no_dynamic_color_response_err()));
                }
            });
        }

        outcome.expect("outcome should be set at this point")
    }

    /// Probe the terminal and determine the ScreenSize.
    pub fn screen_size(&mut self) -> Result<ScreenSize> {
        let xt_version = self.xt_version()?;

        let is_tmux = xt_version.is_tmux();

        // some tmux versions have their rows/cols swapped in ReportTextAreaSizeCells
        let swapped_cols_rows = match xt_version.full_version() {
            "tmux 3.2" | "tmux 3.2a" | "tmux 3.3" | "tmux 3.3a" => true,
            _ => false,
        };

        let query_cells = CSI::Window(Box::new(Window::ReportTextAreaSizeCells));
        let query_pixels = CSI::Window(Box::new(Window::ReportCellSizePixels));
        let dev_attributes = CSI::Device(Box::new(Device::RequestPrimaryDeviceAttributes));

        write!(self.write, "{query_cells}{query_pixels}")?;

        // tmux refuses to directly support responding to 14t or 16t queries
        // for pixel dimensions, so we need to jump through to the outer
        // terminal and see what it says
        if is_tmux {
            write!(self.write, "{TMUX_BEGIN}{query_pixels}{TMUX_END}")?;
        }

        if is_tmux || cfg!(windows) {
            self.write.flush()?;
            // I really wanted to avoid a delay here, but tmux and conpty will
            // both re-order the response to dev_attributes before sending the
            // response for the passthru of query_pixels if we don't delay.
            // The delay is potentially imperfect for things like a laggy ssh
            // connection. The consequence of the timing being wrong is that
            // we won't be able to reason about the pixel dimensions, which is
            // "OK", but that was kinda the whole point of probing this way
            // vs. termios.

            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        write!(self.write, "{dev_attributes}")?;
        self.write.flush()?;

        let mut parser = Parser::new();
        let mut done = false;
        let mut size = ScreenSize {
            rows: 0,
            cols: 0,
            xpixel: 0,
            ypixel: 0,
        };

        while !done {
            let mut byte = [0u8];
            self.read.read(&mut byte)?;

            parser.parse(&byte, |action| {
                // print!("{action:?}\r\n");
                match action {
                    // ConPTY appears to trigger 1 or more xtversion queries
                    // to wezterm in response to this probe, so we need to
                    // prepared to accept and discard data of that shape
                    // here, so that we keep going until we get our reports
                    Action::DeviceControl(_) => {}
                    Action::Esc(Esc::Code(EscCode::StringTerminator)) => {}

                    // and now look for the actual responses we're expecting
                    Action::CSI(csi) => match csi {
                        CSI::Window(win) => match *win {
                            Window::ResizeWindowCells { width, height } => {
                                let width = width.unwrap_or(1);
                                let height = height.unwrap_or(1);
                                if width > 0 && height > 0 {
                                    let width = width as usize;
                                    let height = height as usize;
                                    if swapped_cols_rows {
                                        size.rows = width;
                                        size.cols = height;
                                    } else {
                                        size.rows = height;
                                        size.cols = width;
                                    }
                                }
                            }
                            Window::ReportCellSizePixelsResponse { width, height } => {
                                let width = width.unwrap_or(1);
                                let height = height.unwrap_or(1);
                                if width > 0 && height > 0 {
                                    let width = width as usize;
                                    let height = height as usize;
                                    size.xpixel = width;
                                    size.ypixel = height;
                                }
                            }
                            _ => {
                                done = true;
                            }
                        },
                        _ => {
                            done = true;
                        }
                    },
                    _ => {
                        done = true;
                    }
                }
            });
        }

        if size.rows == 0 && size.cols == 0 {
            bail!("no size information available");
        }

        Ok(size)
    }
}

fn parse_dynamic_color_response(
    action: Action,
    requested: DynamicColorNumber,
) -> Option<SrgbaTuple> {
    let Action::OperatingSystemCommand(osc) = action else {
        return None;
    };

    let OperatingSystemCommand::ChangeDynamicColors(response_which, response_colors) = &*osc else {
        return None;
    };

    if *response_which != requested {
        return None;
    }

    if let [ColorOrQuery::Color(color)] = response_colors.as_slice() {
        Some(*color)
    } else {
        None
    }
}

fn no_dynamic_color_response_err() -> crate::Error {
    crate::error::StringWrap(NO_DYNAMIC_COLOR_RESPONSE.into()).into()
}

#[cfg(test)]
mod test_probe_capabilities {
    use super::*;
    use crate::color::SrgbaTuple;
    use crate::escape::osc::{ColorOrQuery, DynamicColorNumber, OperatingSystemCommand};
    use std::io::Cursor;
    use std::str::FromStr;

    fn dynamic_color_query(which: DynamicColorNumber) -> OperatingSystemCommand {
        OperatingSystemCommand::ChangeDynamicColors(which, vec![ColorOrQuery::Query])
    }

    fn dynamic_color_writes(which: DynamicColorNumber) -> String {
        format!("{}", dynamic_color_query(which))
    }

    #[test]
    fn test_dynamic_color() {
        let expected = SrgbaTuple::from_str("rgb:1212/3434/5656").unwrap();
        let response = format!(
            "{}",
            OperatingSystemCommand::ChangeDynamicColors(
                DynamicColorNumber::TextForegroundColor,
                vec![ColorOrQuery::Color(expected)],
            ),
        );
        let mut read = Cursor::new(response.into_bytes());
        let mut write = vec![];
        let mut probe = ProbeCapabilities::new(&mut read, &mut write);

        let color = probe
            .dynamic_color(DynamicColorNumber::TextForegroundColor)
            .unwrap();

        assert_eq!(color, expected);
        assert_eq!(
            String::from_utf8(write).unwrap(),
            dynamic_color_writes(DynamicColorNumber::TextForegroundColor)
        );
    }

    #[test]
    fn test_dynamic_cursor_color() {
        let expected = SrgbaTuple::from_str("rgb:aaaa/bbbb/cccc").unwrap();
        let response = format!(
            "{}",
            OperatingSystemCommand::ChangeDynamicColors(
                DynamicColorNumber::TextCursorColor,
                vec![ColorOrQuery::Color(expected)],
            ),
        );
        let mut read = Cursor::new(response.into_bytes());
        let mut write = vec![];
        let mut probe = ProbeCapabilities::new(&mut read, &mut write);

        let color = probe
            .dynamic_color(DynamicColorNumber::TextCursorColor)
            .unwrap();

        assert_eq!(color, expected);
        assert_eq!(
            String::from_utf8(write).unwrap(),
            dynamic_color_writes(DynamicColorNumber::TextCursorColor)
        );
    }

    #[test]
    fn test_dynamic_color_rejects_incomplete_response() {
        let response = format!(
            "{}",
            OperatingSystemCommand::ChangeDynamicColors(
                DynamicColorNumber::TextCursorColor,
                vec![ColorOrQuery::Query],
            ),
        );
        let mut read = Cursor::new(response.into_bytes());
        let mut write = vec![];
        let mut probe = ProbeCapabilities::new(&mut read, &mut write);

        let err = probe
            .dynamic_color(DynamicColorNumber::TextCursorColor)
            .unwrap_err();

        assert_eq!(
            err.to_string(),
            "terminal did not respond to dynamic color query"
        );
        assert_eq!(
            String::from_utf8(write).unwrap(),
            dynamic_color_writes(DynamicColorNumber::TextCursorColor)
        );
    }

    #[test]
    fn test_dynamic_color_errors_when_terminal_does_not_respond() {
        let response = String::new();
        let mut read = Cursor::new(response.into_bytes());
        let mut write = vec![];
        let mut probe = ProbeCapabilities::new(&mut read, &mut write);

        let err = probe
            .dynamic_color(DynamicColorNumber::TextCursorColor)
            .unwrap_err();

        assert_eq!(
            err.to_string(),
            "terminal did not respond to dynamic color query"
        );
        assert_eq!(
            String::from_utf8(write).unwrap(),
            dynamic_color_writes(DynamicColorNumber::TextCursorColor)
        );
    }

    #[test]
    fn test_dynamic_color_supports_consecutive_st_terminated_probes() {
        let foreground = SrgbaTuple::from_str("rgb:1212/3434/5656").unwrap();
        let background = SrgbaTuple::from_str("rgb:abab/cdcd/efef").unwrap();
        let response = format!(
            "{}{}",
            OperatingSystemCommand::ChangeDynamicColors(
                DynamicColorNumber::TextForegroundColor,
                vec![ColorOrQuery::Color(foreground)],
            ),
            OperatingSystemCommand::ChangeDynamicColors(
                DynamicColorNumber::TextBackgroundColor,
                vec![ColorOrQuery::Color(background)],
            ),
        );
        let mut read = Cursor::new(response.into_bytes());
        let mut write = vec![];
        let mut probe = ProbeCapabilities::new(&mut read, &mut write);

        let parsed_foreground = probe
            .dynamic_color(DynamicColorNumber::TextForegroundColor)
            .unwrap();
        let parsed_background = probe
            .dynamic_color(DynamicColorNumber::TextBackgroundColor)
            .unwrap();

        assert_eq!(parsed_foreground, foreground);
        assert_eq!(parsed_background, background);
        assert_eq!(
            String::from_utf8(write).unwrap(),
            format!(
                "{}{}",
                dynamic_color_writes(DynamicColorNumber::TextForegroundColor),
                dynamic_color_writes(DynamicColorNumber::TextBackgroundColor),
            )
        );
    }

    #[test]
    fn test_dynamic_color_supports_consecutive_bel_terminated_probes() {
        let foreground = SrgbaTuple::from_str("rgb:1212/3434/5656").unwrap();
        let background = SrgbaTuple::from_str("rgb:abab/cdcd/efef").unwrap();
        let response = "\x1b]10;rgb:1212/3434/5656\x07\x1b]11;rgb:abab/cdcd/efef\x07";
        let mut read = Cursor::new(response.as_bytes());
        let mut write = vec![];
        let mut probe = ProbeCapabilities::new(&mut read, &mut write);

        let parsed_foreground = probe
            .dynamic_color(DynamicColorNumber::TextForegroundColor)
            .unwrap();
        let parsed_background = probe
            .dynamic_color(DynamicColorNumber::TextBackgroundColor)
            .unwrap();

        assert_eq!(parsed_foreground, foreground);
        assert_eq!(parsed_background, background);
        assert_eq!(
            String::from_utf8(write).unwrap(),
            format!(
                "{}{}",
                dynamic_color_writes(DynamicColorNumber::TextForegroundColor),
                dynamic_color_writes(DynamicColorNumber::TextBackgroundColor),
            )
        );
    }
}
