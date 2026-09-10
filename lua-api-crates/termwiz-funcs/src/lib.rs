use config::lua::get_or_create_module;
use config::lua::mlua::{self, IntoLua, Lua};
use finl_unicode::grapheme_clusters::Graphemes;
use luahelper::impl_lua_conversion_dynamic;
use std::fmt::Write;
use std::str::FromStr;
use termwiz::caps::{Capabilities, ColorLevel, ProbeHints};
use termwiz::cell::{grapheme_column_width, unicode_column_width, AttributeChange, CellAttributes};
use termwiz::color::{AnsiColor, ColorAttribute, ColorSpec, SrgbaTuple};
use termwiz::escape::csi::{Sgr, CSI};
use termwiz::escape::osc::OperatingSystemCommand;
use termwiz::render::terminfo::TerminfoRenderer;
use termwiz::surface::change::Change;
use termwiz::surface::Line;
use wezterm_dynamic::{FromDynamic, ToDynamic};

pub fn register(lua: &Lua) -> anyhow::Result<()> {
    let wezterm_mod = get_or_create_module(lua, "wezterm")?;
    wezterm_mod.set("nerdfonts", NerdFonts {})?;
    wezterm_mod.set("format", lua.create_function(format)?)?;
    wezterm_mod.set(
        "column_width",
        lua.create_function(|_, s: String| Ok(unicode_column_width(&s, None)))?,
    )?;

    wezterm_mod.set(
        "pad_right",
        lua.create_function(|_, (s, width): (String, usize)| Ok(pad_right(s, width)))?,
    )?;

    wezterm_mod.set(
        "pad_left",
        lua.create_function(|_, (s, width): (String, usize)| Ok(pad_left(s, width)))?,
    )?;

    wezterm_mod.set(
        "truncate_right",
        lua.create_function(|_, (s, max_width): (String, usize)| {
            Ok(truncate_right(&s, max_width))
        })?,
    )?;

    wezterm_mod.set(
        "truncate_left",
        lua.create_function(|_, (s, max_width): (String, usize)| Ok(truncate_left(&s, max_width)))?,
    )?;
    wezterm_mod.set("permute_any_mods", lua.create_function(permute_any_mods)?)?;
    wezterm_mod.set(
        "permute_any_or_no_mods",
        lua.create_function(permute_any_or_no_mods)?,
    )?;

    Ok(())
}

struct NerdFonts {}

impl mlua::UserData for NerdFonts {
    fn add_methods<'lua, M: mlua::UserDataMethods<'lua, Self>>(methods: &mut M) {
        methods.add_meta_method(
            mlua::MetaMethod::Index,
            |_, _, key: String| -> mlua::Result<Option<String>> {
                Ok(termwiz::nerdfonts::NERD_FONTS
                    .get(key.as_str())
                    .map(|c| c.to_string()))
            },
        );
    }
}

#[derive(Debug, FromDynamic, ToDynamic, Clone, PartialEq, Eq)]
pub enum FormatColor {
    AnsiColor(AnsiColor),
    Color(String),
    Default,
}

impl FormatColor {
    fn to_attr(self) -> ColorAttribute {
        let spec: ColorSpec = self.into();
        let attr: ColorAttribute = spec.into();
        attr
    }
}

impl From<FormatColor> for ColorSpec {
    fn from(val: FormatColor) -> Self {
        match val {
            FormatColor::AnsiColor(c) => c.into(),
            FormatColor::Color(s) => {
                let rgba = SrgbaTuple::from_str(&s).unwrap_or_else(|()| (0xff, 0xff, 0xff).into());
                rgba.into()
            }
            FormatColor::Default => ColorSpec::Default,
        }
    }
}

#[derive(Debug, FromDynamic, ToDynamic, Clone, PartialEq, Eq)]
pub enum FormatItem {
    Foreground(FormatColor),
    Background(FormatColor),
    Attribute(AttributeChange),
    ResetAttributes,
    Text(String),
}
impl_lua_conversion_dynamic!(FormatItem);

impl From<FormatItem> for Change {
    fn from(val: FormatItem) -> Self {
        match val {
            FormatItem::Attribute(change) => change.into(),
            FormatItem::Text(t) => t.into(),
            FormatItem::Foreground(c) => AttributeChange::Foreground(c.to_attr()).into(),
            FormatItem::Background(c) => AttributeChange::Background(c.to_attr()).into(),
            FormatItem::ResetAttributes => Change::AllAttributes(CellAttributes::default()),
        }
    }
}

fn format_color_spec(color: ColorAttribute) -> ColorSpec {
    match color {
        ColorAttribute::Default => ColorSpec::Default,
        ColorAttribute::PaletteIndex(idx) => ColorSpec::PaletteIndex(idx),
        ColorAttribute::TrueColorWithDefaultFallback(color)
        | ColorAttribute::TrueColorWithPaletteFallback(color, _) => ColorSpec::TrueColor(color),
    }
}

pub fn format_as_escapes(items: Vec<FormatItem>) -> anyhow::Result<String> {
    let mut result = String::new();
    let mut attrs = CellAttributes::default();
    let mut reset_attributes = false;

    // The receiving context may have nondefault attributes, such as italic tab
    // hover text. Preserve explicit changes instead of diffing against defaults.
    for item in items {
        let change = match item {
            FormatItem::Text(text) => {
                result.push_str(&text);
                continue;
            }
            FormatItem::ResetAttributes => {
                write!(result, "{}", CSI::Sgr(Sgr::Reset))?;
                if attrs.hyperlink().is_some() {
                    write!(result, "{}", OperatingSystemCommand::SetHyperlink(None))?;
                }
                attrs = CellAttributes::default();
                reset_attributes = true;
                continue;
            }
            FormatItem::Attribute(change) => change,
            FormatItem::Foreground(color) => AttributeChange::Foreground(color.to_attr()),
            FormatItem::Background(color) => AttributeChange::Background(color.to_attr()),
        };
        attrs.apply_change(&change);
        let sgr = match change {
            AttributeChange::Intensity(value) => Sgr::Intensity(value),
            AttributeChange::Underline(value) => Sgr::Underline(value),
            AttributeChange::Italic(value) => Sgr::Italic(value),
            AttributeChange::Blink(value) => Sgr::Blink(value),
            AttributeChange::Reverse(value) => Sgr::Inverse(value),
            AttributeChange::StrikeThrough(value) => Sgr::StrikeThrough(value),
            AttributeChange::Invisible(value) => Sgr::Invisible(value),
            AttributeChange::Foreground(color) => Sgr::Foreground(format_color_spec(color)),
            AttributeChange::Background(color) => Sgr::Background(format_color_spec(color)),
            AttributeChange::Hyperlink(link) => {
                write!(
                    result,
                    "{}",
                    OperatingSystemCommand::SetHyperlink(link.map(|link| (*link).clone()))
                )?;
                continue;
            }
        };
        reset_attributes |= !matches!(sgr, Sgr::Foreground(_) | Sgr::Background(_));
        write!(result, "{}", CSI::Sgr(sgr))?;
    }

    // Plain and color-only fragments can be nested inside styled text without
    // resetting the surrounding graphical attributes. Text remains opaque.
    if reset_attributes {
        write!(result, "{}", CSI::Sgr(Sgr::Reset))?;
    } else {
        if attrs.foreground() != ColorAttribute::Default {
            write!(result, "{}", CSI::Sgr(Sgr::Foreground(ColorSpec::Default)))?;
        }
        if attrs.background() != ColorAttribute::Default {
            write!(result, "{}", CSI::Sgr(Sgr::Background(ColorSpec::Default)))?;
        }
    }
    if attrs.hyperlink().is_some() {
        write!(result, "{}", OperatingSystemCommand::SetHyperlink(None))?;
    }
    Ok(result)
}

fn format<'lua>(_: &'lua Lua, items: Vec<FormatItem>) -> mlua::Result<String> {
    format_as_escapes(items).map_err(mlua::Error::external)
}

#[cfg(test)]
mod format_tests {
    use super::*;
    use std::sync::Arc;
    use termwiz::cell::{Blink, Intensity, Underline};
    use termwiz::escape::parser::Parser;
    use termwiz::escape::Action;
    use termwiz::hyperlink::Hyperlink;

    #[test]
    fn explicit_graphical_attributes_are_not_elided_or_reset_before_text() {
        for (attribute, escape) in [
            (AttributeChange::Italic(false), "\x1b[23m"),
            (AttributeChange::Italic(true), "\x1b[3m"),
            (AttributeChange::Intensity(Intensity::Normal), "\x1b[22m"),
            (AttributeChange::Intensity(Intensity::Bold), "\x1b[1m"),
            (AttributeChange::Intensity(Intensity::Half), "\x1b[2m"),
            (AttributeChange::Underline(Underline::None), "\x1b[24m"),
            (AttributeChange::Underline(Underline::Curly), "\x1b[4:3m"),
            (AttributeChange::Blink(Blink::None), "\x1b[25m"),
            (AttributeChange::Blink(Blink::Rapid), "\x1b[6m"),
            (AttributeChange::Reverse(false), "\x1b[27m"),
            (AttributeChange::StrikeThrough(false), "\x1b[29m"),
            (AttributeChange::Invisible(false), "\x1b[28m"),
        ] {
            // An explicit off value matters even when the formatter has not
            // seen a matching on value: the receiver can inherit that style.
            assert_eq!(
                format_as_escapes(vec![
                    FormatItem::Attribute(attribute),
                    FormatItem::Text("x".into())
                ])
                .unwrap(),
                format!("{escape}x\x1b[0m")
            );
        }
    }

    #[test]
    fn colors_and_raw_defaults_preserve_order() {
        let rgb = SrgbaTuple::from((0x12, 0x34, 0x56));
        for (color, expected) in [
            (ColorAttribute::Default, ColorSpec::Default),
            (
                ColorAttribute::PaletteIndex(200),
                ColorSpec::PaletteIndex(200),
            ),
            (
                ColorAttribute::TrueColorWithDefaultFallback(rgb),
                ColorSpec::TrueColor(rgb),
            ),
            (
                ColorAttribute::TrueColorWithPaletteFallback(rgb, 4),
                ColorSpec::TrueColor(rgb),
            ),
        ] {
            let text = format_as_escapes(vec![
                FormatItem::Text("\x1b[31m\x1b[44m".into()),
                FormatItem::Attribute(AttributeChange::Foreground(color)),
                FormatItem::Attribute(AttributeChange::Background(color)),
                FormatItem::Text("x".into()),
            ])
            .unwrap();
            let actions = Parser::new().parse_as_vec(text.as_bytes());
            assert_eq!(actions[2], Action::CSI(CSI::Sgr(Sgr::Foreground(expected))));
            assert_eq!(actions[3], Action::CSI(CSI::Sgr(Sgr::Background(expected))));
            assert_eq!(actions[4], Action::Print('x'));
            assert!(!actions.contains(&Action::CSI(CSI::Sgr(Sgr::Reset))));
        }
    }

    #[test]
    fn plain_and_empty_formats_are_unchanged() {
        assert_eq!(format_as_escapes(vec![]).unwrap(), "");
        let raw = "\x1b[58:2::255:0:0mraw\ntext";
        assert_eq!(
            format_as_escapes(vec![FormatItem::Text(raw.into())]).unwrap(),
            raw
        );
    }

    #[test]
    fn hyperlinks_survive_attribute_changes_and_close_at_reset_or_end() {
        let link = Hyperlink::new("https://wezterm.org");
        for reset in [
            None,
            Some(FormatItem::ResetAttributes),
            Some(FormatItem::Attribute(AttributeChange::Hyperlink(None))),
        ] {
            let closes_before_y = reset.is_some();
            let mut items = vec![
                FormatItem::Attribute(AttributeChange::Hyperlink(Some(Arc::new(link.clone())))),
                FormatItem::Attribute(AttributeChange::Italic(false)),
                FormatItem::Text("x".into()),
            ];
            if let Some(reset) = reset {
                items.push(reset);
            }
            items.push(FormatItem::Text("y".into()));
            let text = format_as_escapes(items).unwrap();
            let actions: Vec<_> = Parser::new()
                .parse_as_vec(text.as_bytes())
                .into_iter()
                .filter(|action| !matches!(action, Action::Esc(_)))
                .collect();
            let links: Vec<_> = actions
                .iter()
                .filter_map(|action| match action {
                    Action::OperatingSystemCommand(osc) => match &**osc {
                        OperatingSystemCommand::SetHyperlink(link) => Some(link.clone()),
                        _ => None,
                    },
                    _ => None,
                })
                .collect();
            assert_eq!(links, vec![Some(link.clone()), None]);
            assert_eq!(actions[1], Action::CSI(CSI::Sgr(Sgr::Italic(false))));
            assert_eq!(actions[2], Action::Print('x'));
            let close = actions.iter().position(|action| matches!(action,
                Action::OperatingSystemCommand(osc) if matches!(&**osc, OperatingSystemCommand::SetHyperlink(None))
            )).unwrap();
            let y = actions
                .iter()
                .position(|action| *action == Action::Print('y'))
                .unwrap();
            assert_eq!(close < y, closes_before_y);
        }
        // A hyperlink-only fragment should not reset outer graphical styles.
        let text = format_as_escapes(vec![
            FormatItem::Attribute(AttributeChange::Hyperlink(Some(Arc::new(link)))),
            FormatItem::Text("x".into()),
        ])
        .unwrap();
        assert!(!Parser::new()
            .parse_as_vec(text.as_bytes())
            .contains(&Action::CSI(CSI::Sgr(Sgr::Reset))));
    }
}

pub fn pad_right(mut result: String, width: usize) -> String {
    let mut len = unicode_column_width(&result, None);
    while len < width {
        result.push(' ');
        len += 1;
    }

    result
}

pub fn pad_left(mut result: String, width: usize) -> String {
    let mut len = unicode_column_width(&result, None);
    while len < width {
        result.insert(0, ' ');
        len += 1;
    }

    result
}

pub fn truncate_left(s: &str, max_width: usize) -> String {
    let mut result = vec![];
    let mut len = 0;
    let graphemes: Vec<_> = Graphemes::new(s).collect();
    for &g in graphemes.iter().rev() {
        let g_len = grapheme_column_width(g, None);
        if g_len + len > max_width {
            break;
        }
        result.push(g);
        len += g_len;
    }

    result.reverse();
    result.join("")
}

pub fn truncate_right(s: &str, max_width: usize) -> String {
    let mut result = String::new();
    let mut len = 0;
    for g in Graphemes::new(s) {
        let g_len = grapheme_column_width(g, None);
        if g_len + len > max_width {
            break;
        }
        result.push_str(g);
        len += g_len;
    }
    result
}

fn permute_mods<'lua>(
    lua: &'lua Lua,
    item: mlua::Table,
    allow_none: bool,
) -> mlua::Result<Vec<mlua::Value<'lua>>> {
    use wezterm_input_types::Modifiers;

    let mut result = vec![];
    for ctrl in &[Modifiers::NONE, Modifiers::CTRL] {
        for shift in &[Modifiers::NONE, Modifiers::SHIFT] {
            for alt in &[Modifiers::NONE, Modifiers::ALT] {
                for sup in &[Modifiers::NONE, Modifiers::SUPER] {
                    let flags = *ctrl | *shift | *alt | *sup;
                    if flags == Modifiers::NONE && !allow_none {
                        continue;
                    }

                    let new_item = lua.create_table()?;
                    for pair in item.clone().pairs::<mlua::Value, mlua::Value>() {
                        let (k, v) = pair?;
                        new_item.set(k, v)?;
                    }
                    new_item.set("mods", flags.to_string())?;
                    result.push(new_item.into_lua(lua)?);
                }
            }
        }
    }
    Ok(result)
}

fn permute_any_mods<'lua>(
    lua: &'lua Lua,
    item: mlua::Table,
) -> mlua::Result<Vec<mlua::Value<'lua>>> {
    permute_mods(lua, item, false)
}

fn permute_any_or_no_mods<'lua>(
    lua: &'lua Lua,
    item: mlua::Table,
) -> mlua::Result<Vec<mlua::Value<'lua>>> {
    permute_mods(lua, item, true)
}

lazy_static::lazy_static! {
    static ref CAPS: Capabilities = {
        let data = include_bytes!("../../../termwiz/data/xterm-256color");
        let db = terminfo::Database::from_buffer(&data[..]).unwrap();
        Capabilities::new_with_hints(
            ProbeHints::new_from_env()
                .term(Some("xterm-256color".into()))
                .terminfo_db(Some(db))
                .color_level(Some(ColorLevel::TrueColor))
                .colorterm(None)
                .colorterm_bce(None)
                .term_program(Some("WezTerm".into()))
                .term_program_version(Some(config::wezterm_version().into())),
        )
        .expect("cannot fail to make internal Capabilities")
    };
}

pub fn new_wezterm_terminfo_renderer() -> TerminfoRenderer {
    TerminfoRenderer::new(CAPS.clone())
}

pub fn lines_to_escapes(lines: Vec<Line>) -> anyhow::Result<String> {
    let mut changes = vec![];
    let mut attr = CellAttributes::blank();
    for line in lines {
        changes.append(&mut line.changes(&attr));
        changes.push(Change::Text("\r\n".to_string()));
        if let Some(a) = line.visible_cells().last().map(|cell| cell.attrs().clone()) {
            attr = a;
        }
    }
    changes.push(Change::AllAttributes(CellAttributes::blank()));
    let mut renderer = new_wezterm_terminfo_renderer();

    struct Target {
        target: Vec<u8>,
    }

    impl std::io::Write for Target {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            std::io::Write::write(&mut self.target, buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl termwiz::render::RenderTty for Target {
        fn get_size_in_cells(&mut self) -> termwiz::Result<(usize, usize)> {
            Ok((80, 24))
        }
    }

    let mut target = Target { target: vec![] };
    renderer.render_to(&changes, &mut target)?;
    Ok(String::from_utf8(target.target)?)
}
