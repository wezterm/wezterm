use crate::color::SrgbaTuple;
use crate::escape::csi::{Device, Window};
use crate::escape::osc::{ColorOrQuery, DynamicColorNumber};
use crate::escape::parser::Parser;
use crate::escape::{Action, DeviceControlMode, Esc, EscCode, OperatingSystemCommand, CSI};
use crate::terminal::ScreenSize;
use crate::{bail, Result};
use std::io::{Read, Write};

const TMUX_BEGIN: &str = "\u{1b}Ptmux;\u{1b}";
const TMUX_END: &str = "\u{1b}\\";
const INCOMPLETE_DYNAMIC_COLOR_RESPONSE: &str =
    "terminal returned an incomplete dynamic color response";
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
    use crate::color::SrgbaTuple;
    use crate::escape::csi::DeviceAttributes;
    use crate::escape::osc::{ColorOrQuery, DynamicColorNumber, OperatingSystemCommand};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::io::{Cursor, Read, Write};
    use std::rc::Rc;
    use std::str::FromStr;

    fn xt_version_response(version: &str) -> String {
        format!("\u{1b}P>|{version}\u{1b}\\")
    }

    fn dynamic_color_query(which: DynamicColorNumber) -> OperatingSystemCommand {
        OperatingSystemCommand::ChangeDynamicColors(which, vec![ColorOrQuery::Query])
    }

    fn dynamic_color_probe_writes() -> String {
        #[cfg(windows)]
        {
            String::new()
        }

        #[cfg(not(windows))]
        {
            format!(
                "{}{}",
                CSI::Device(Box::new(Device::RequestTerminalNameAndVersion)),
                CSI::Device(Box::new(Device::RequestPrimaryDeviceAttributes)),
            )
        }
    }

    fn dynamic_color_probe_response(version: &str) -> String {
        #[cfg(windows)]
        {
            let _ = version;
            String::new()
        }

        #[cfg(not(windows))]
        {
            format!(
                "{}{}",
                xt_version_response(version),
                CSI::Device(Box::new(Device::DeviceAttributes(DeviceAttributes::Vt102))),
            )
        }
    }

    fn dynamic_color_writes(which: DynamicColorNumber) -> String {
        format!(
            "{}{}{}",
            dynamic_color_probe_writes(),
            dynamic_color_query(which),
            CSI::Device(Box::new(Device::RequestPrimaryDeviceAttributes)),
        )
    }

    #[derive(Default)]
    struct DynamicColorIoState {
        pending_write: Vec<u8>,
        read_data: VecDeque<u8>,
    }

    struct DynamicColorReader {
        state: Rc<RefCell<DynamicColorIoState>>,
    }

    impl Read for DynamicColorReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let mut state = self.state.borrow_mut();
            let Some(byte) = state.read_data.pop_front() else {
                return Ok(0);
            };
            buf[0] = byte;
            Ok(1)
        }
    }

    struct DynamicColorWriter {
        state: Rc<RefCell<DynamicColorIoState>>,
        xt_version_query: String,
        xt_version: String,
        query: String,
        response: String,
        dev_attributes_query: String,
        dev_attributes: String,
    }

    impl Write for DynamicColorWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.state.borrow_mut().pending_write.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            let mut state = self.state.borrow_mut();
            let pending = String::from_utf8_lossy(&state.pending_write);

            if pending == self.xt_version_query {
                state.read_data.extend(self.xt_version.bytes());
                state.read_data.extend(self.dev_attributes.bytes());
            } else if pending == self.query {
                state.read_data.extend(self.response.bytes());
            } else if pending == self.dev_attributes_query {
                state.read_data.extend(self.dev_attributes.bytes());
            }

            state.pending_write.clear();
            Ok(())
        }
    }

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

    #[test]
    fn test_dynamic_color() {
        let expected = SrgbaTuple::from_str("rgb:1212/3434/5656").unwrap();
        let dev_attributes =
            CSI::Device(Box::new(Device::DeviceAttributes(DeviceAttributes::Vt102)));
        let response = format!(
            "{}{}{}",
            dynamic_color_probe_response("WezTerm test"),
            OperatingSystemCommand::ChangeDynamicColors(
                DynamicColorNumber::TextForegroundColor,
                vec![ColorOrQuery::Color(expected)],
            ),
            dev_attributes,
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
        let dev_attributes =
            CSI::Device(Box::new(Device::DeviceAttributes(DeviceAttributes::Vt102)));
        let response = format!(
            "{}{}{}",
            dynamic_color_probe_response("WezTerm test"),
            OperatingSystemCommand::ChangeDynamicColors(
                DynamicColorNumber::TextCursorColor,
                vec![ColorOrQuery::Color(expected)],
            ),
            dev_attributes,
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
        let dev_attributes =
            CSI::Device(Box::new(Device::DeviceAttributes(DeviceAttributes::Vt102)));
        let response = format!(
            "{}{}{}",
            dynamic_color_probe_response("WezTerm test"),
            OperatingSystemCommand::ChangeDynamicColors(
                DynamicColorNumber::TextCursorColor,
                vec![ColorOrQuery::Query],
            ),
            dev_attributes,
        );
        let mut read = Cursor::new(response.into_bytes());
        let mut write = vec![];
        let mut probe = ProbeCapabilities::new(&mut read, &mut write);

        let err = probe
            .dynamic_color(DynamicColorNumber::TextCursorColor)
            .unwrap_err();

        assert_eq!(
            err.to_string(),
            "terminal returned an incomplete dynamic color response"
        );
        assert_eq!(
            String::from_utf8(write).unwrap(),
            dynamic_color_writes(DynamicColorNumber::TextCursorColor)
        );
    }

    #[test]
    fn test_dynamic_color_stops_after_primary_device_attributes() {
        let dev_attributes =
            CSI::Device(Box::new(Device::DeviceAttributes(DeviceAttributes::Vt102)));
        let response = format!(
            "{}{dev_attributes}",
            dynamic_color_probe_response("WezTerm test")
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
    fn test_dynamic_color_consumes_primary_device_attributes_before_returning() {
        let first = SrgbaTuple::from_str("rgb:1111/2222/3333").unwrap();
        let second = SrgbaTuple::from_str("rgb:4444/5555/6666").unwrap();
        let dev_attributes =
            CSI::Device(Box::new(Device::DeviceAttributes(DeviceAttributes::Vt102)));
        let response = format!(
            "{}{}{}{}{}{}",
            dynamic_color_probe_response("WezTerm test"),
            OperatingSystemCommand::ChangeDynamicColors(
                DynamicColorNumber::TextForegroundColor,
                vec![ColorOrQuery::Color(first)],
            ),
            dev_attributes,
            dynamic_color_probe_response("WezTerm test"),
            OperatingSystemCommand::ChangeDynamicColors(
                DynamicColorNumber::TextCursorColor,
                vec![ColorOrQuery::Color(second)],
            ),
            dev_attributes,
        );
        let mut read = Cursor::new(response.into_bytes());
        let mut write = vec![];
        let mut probe = ProbeCapabilities::new(&mut read, &mut write);

        let foreground = probe
            .dynamic_color(DynamicColorNumber::TextForegroundColor)
            .unwrap();
        let cursor = probe
            .dynamic_color(DynamicColorNumber::TextCursorColor)
            .unwrap();

        assert_eq!(foreground, first);
        assert_eq!(cursor, second);
    }

    #[test]
    fn test_dynamic_color_avoids_primary_device_attributes_reordering() {
        let expected = SrgbaTuple::from_str("rgb:1212/3434/5656").unwrap();
        let which = DynamicColorNumber::TextForegroundColor;
        let xt_version_query = dynamic_color_probe_writes();
        let xt_version = if cfg!(windows) {
            String::new()
        } else {
            xt_version_response("tmux 3.4")
        };
        let query = format!("{}", dynamic_color_query(which));
        let response = format!(
            "{}",
            OperatingSystemCommand::ChangeDynamicColors(which, vec![ColorOrQuery::Color(expected)])
        );
        let dev_attributes = format!(
            "{}",
            CSI::Device(Box::new(Device::DeviceAttributes(DeviceAttributes::Vt102)))
        );
        let dev_attributes_query = format!(
            "{}",
            CSI::Device(Box::new(Device::RequestPrimaryDeviceAttributes))
        );

        let state = Rc::new(RefCell::new(DynamicColorIoState::default()));
        let mut read = DynamicColorReader {
            state: Rc::clone(&state),
        };
        let mut write = DynamicColorWriter {
            state,
            xt_version_query,
            xt_version,
            query,
            response,
            dev_attributes_query,
            dev_attributes,
        };
        let mut probe = ProbeCapabilities::new(&mut read, &mut write);

        let color = probe.dynamic_color(which).unwrap();

        assert_eq!(color, expected);
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
        let needs_delay = cfg!(windows) || self.xt_version()?.is_tmux();

        let query = OperatingSystemCommand::ChangeDynamicColors(which, vec![ColorOrQuery::Query]);
        let dev_attributes = CSI::Device(Box::new(Device::RequestPrimaryDeviceAttributes));
        write!(self.write, "{query}")?;
        self.write.flush()?;

        if needs_delay {
            // In tmux and ConPTY, the primary device attributes response can be
            // reordered ahead of the dynamic color reply if we send it immediately.
            // Delay a little before using it as a sentinel, matching the style used
            // by the other probes in this module.
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        write!(self.write, "{dev_attributes}")?;
        self.write.flush()?;

        let mut parser = Parser::new();
        let mut color = None;
        loop {
            let mut byte = [0u8];
            if self.read.read(&mut byte)? == 0 {
                bail!("terminal closed while waiting for dynamic color response");
            }

            let mut saw_dev_attributes = false;
            parser.parse(&byte, |action| {
                if is_primary_device_attributes_response(&action) {
                    saw_dev_attributes = true;
                    return;
                }

                if color.is_none() {
                    color = parse_dynamic_color_response(action, which);
                }
            });

            if saw_dev_attributes {
                break match color {
                    Some(color) => color,
                    None => Err(dynamic_color_probe_err(NO_DYNAMIC_COLOR_RESPONSE)),
                };
            }
        }
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
) -> Option<Result<SrgbaTuple>> {
    let Action::OperatingSystemCommand(osc) = action else {
        return None;
    };

    let OperatingSystemCommand::ChangeDynamicColors(response_which, response_colors) = &*osc else {
        return None;
    };

    if *response_which != requested {
        return None;
    }

    let [color] = response_colors.as_slice() else {
        return Some(Err(dynamic_color_probe_err(
            INCOMPLETE_DYNAMIC_COLOR_RESPONSE,
        )));
    };

    match color {
        ColorOrQuery::Color(color) => Some(Ok(*color)),
        ColorOrQuery::Query => Some(Err(dynamic_color_probe_err(
            INCOMPLETE_DYNAMIC_COLOR_RESPONSE,
        ))),
    }
}

fn is_primary_device_attributes_response(action: &Action) -> bool {
    matches!(
        action,
        Action::CSI(CSI::Device(device)) if matches!(**device, Device::DeviceAttributes(_))
    )
}

fn dynamic_color_probe_err(message: &str) -> crate::Error {
    crate::error::StringWrap(message.into()).into()
}
