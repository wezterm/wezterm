use crate::db::FontDatabase;
use crate::locator::{new_locator, FontLocator};
use crate::parser::ParsedFont;
use crate::rasterizer::{new_rasterizer, FontRasterizer};
use crate::shaper::{new_shaper, FontShaper, PresentationWidth};
use anyhow::{Context, Error};
use config::{
    configuration, BoldBrightening, Config, ConfigHandle, DisplayPixelGeometry, FontAttributes,
    FontRasterizerSelection, FontStretch, FontStyle, FontWeight, TextStyle,
};
use rangeset::RangeSet;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::{Rc, Weak};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use termwiz::cell::Presentation;
use thiserror::Error;
use wezterm_bidi::Direction;
use wezterm_term::{CellAttributes, Intensity};
use wezterm_toast_notification::ToastNotification;

mod hbwrap;

pub mod db;
pub mod ftwrap;
pub mod locator;
pub mod parser;
pub mod rasterizer;
pub mod shaper;
pub mod units;

#[cfg(all(unix, not(target_os = "macos")))]
pub mod fcwrap;

pub use crate::rasterizer::RasterizedGlyph;
pub use crate::shaper::{FallbackIdx, FontMetrics, GlyphInfo};

#[derive(Debug, Error)]
#[error("Font fallback recalculated")]
pub struct ClearShapeCache {}

static FONT_ID: ::std::sync::atomic::AtomicUsize = ::std::sync::atomic::AtomicUsize::new(0);
pub type LoadedFontId = usize;
pub fn alloc_font_id() -> LoadedFontId {
    FONT_ID.fetch_add(1, ::std::sync::atomic::Ordering::Relaxed)
}

lazy_static::lazy_static! {
    static ref LAST_WARNING: Mutex<Option<(Instant, usize)>> = Mutex::new(None);
}

pub struct LoadedFont {
    rasterizers: RefCell<HashMap<FallbackIdx, Box<dyn FontRasterizer>>>,
    handles: RefCell<Vec<ParsedFont>>,
    shaper: RefCell<Box<dyn FontShaper>>,
    metrics: FontMetrics,
    pixel_geometry: DisplayPixelGeometry,
    font_size: f64,
    dpi: u32,
    font_config: Weak<FontConfigInner>,
    pending_fallback: Arc<Mutex<Vec<ParsedFont>>>,
    text_style: TextStyle,
    id: LoadedFontId,
    /// Glyphs for which no font was found and for which we should
    /// stop searching
    tried_glyphs: RefCell<HashSet<char>>,
}

impl std::fmt::Debug for LoadedFont {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        fmt.debug_struct("LoadedFont")
            .field("handles", &self.handles)
            .field("metrics", &self.metrics)
            .field("font_size", &self.font_size)
            .field("dpi", &self.dpi)
            .field("pending_fallback", &self.pending_fallback)
            .field("text_style", &self.text_style)
            .finish()
    }
}

impl LoadedFont {
    pub fn metrics(&self) -> FontMetrics {
        self.metrics
    }

    pub fn style(&self) -> &TextStyle {
        &self.text_style
    }

    pub fn id(&self) -> LoadedFontId {
        self.id
    }

    fn insert_fallback_handles(&self, extra_handles: Vec<ParsedFont>) -> anyhow::Result<bool> {
        let mut loaded = false;
        {
            let mut handles = self.handles.borrow_mut();
            for h in extra_handles {
                if !handles.iter().any(|existing| *existing == h) {
                    handles.push(h);
                    loaded = true;
                }
            }
            if loaded {
                log::trace!("revised fallback: {:#?}", handles);
            }
        }
        if loaded {
            if let Some(font_config) = self.font_config.upgrade() {
                *self.shaper.borrow_mut() =
                    new_shaper(&*font_config.config.borrow(), &self.handles.borrow())?;
            }
        }
        Ok(loaded)
    }

    pub fn blocking_shape(
        &self,
        text: &str,
        presentation: Option<Presentation>,
        direction: Direction,
        range: Option<Range<usize>>,
        presentation_width: Option<&PresentationWidth>,
    ) -> anyhow::Result<Vec<GlyphInfo>> {
        loop {
            let (tx, rx) = channel();

            let (async_resolve, res) = match self.shape_impl(
                text,
                move || {
                    let _ = tx.send(());
                },
                |_| {},
                presentation,
                direction,
                range.clone(),
                presentation_width,
            ) {
                Ok(tuple) => tuple,
                Err(err) if err.downcast_ref::<ClearShapeCache>().is_some() => {
                    continue;
                }
                Err(err) => return Err(err),
            };

            if !async_resolve {
                return Ok(res);
            }
            if rx.recv().is_err() {
                return Ok(res);
            }
        }
    }

    pub fn shape<F: FnOnce() + Send + 'static, FS: FnOnce(&mut Vec<char>)>(
        &self,
        text: &str,
        completion: F,
        filter_out_synthetic: FS,
        presentation: Option<Presentation>,
        direction: Direction,
        range: Option<Range<usize>>,
        presentation_width: Option<&PresentationWidth>,
    ) -> anyhow::Result<Vec<GlyphInfo>> {
        let (_async_resolve, res) = self.shape_impl(
            text,
            completion,
            filter_out_synthetic,
            presentation,
            direction,
            range,
            presentation_width,
        )?;
        Ok(res)
    }

    fn shape_impl<F: FnOnce() + Send + 'static, FS: FnOnce(&mut Vec<char>)>(
        &self,
        text: &str,
        completion: F,
        filter_out_synthetic: FS,
        presentation: Option<Presentation>,
        direction: Direction,
        range: Option<Range<usize>>,
        presentation_width: Option<&PresentationWidth>,
    ) -> anyhow::Result<(bool, Vec<GlyphInfo>)> {
        let mut no_glyphs = vec![];

        {
            let mut pending = self.pending_fallback.lock().unwrap();
            if !pending.is_empty() {
                match self.insert_fallback_handles(pending.split_off(0)) {
                    Ok(true) => return Err(ClearShapeCache {})?,
                    Ok(false) => {}
                    Err(err) => {
                        log::error!("Error adding fallback: {:#}", err);
                    }
                }
            }
        }

        let result = self.shaper.borrow().shape(
            text,
            self.font_size,
            self.dpi,
            &mut no_glyphs,
            presentation,
            direction,
            range,
            presentation_width,
        );

        no_glyphs.retain(|&c| c != '\u{FE0F}' && c != '\u{FE0E}');
        filter_out_synthetic(&mut no_glyphs);

        let mut tried_glyphs = self.tried_glyphs.borrow_mut();
        no_glyphs.retain(|c| !tried_glyphs.contains(c));
        for c in &no_glyphs {
            tried_glyphs.insert(*c);
        }

        no_glyphs.sort();
        no_glyphs.dedup();

        let mut async_resolve = false;

        if !no_glyphs.is_empty() {
            if let Some(font_config) = self.font_config.upgrade() {
                font_config.schedule_fallback_resolve(
                    no_glyphs,
                    &self.pending_fallback,
                    completion,
                );
                async_resolve = true;
            }
        }

        result.map(|r| (async_resolve, r))
    }

    pub fn metrics_for_idx(&self, font_idx: usize) -> anyhow::Result<FontMetrics> {
        self.shaper
            .borrow()
            .metrics_for_idx(font_idx, self.font_size, self.dpi)
    }

    pub fn brightness_adjust(&self, font_idx: usize) -> f32 {
        let synthesize_dim = self
            .handles
            .borrow()
            .get(font_idx)
            .map(|p| p.synthesize_dim)
            .unwrap_or(false);
        if synthesize_dim {
            0.5
        } else {
            1.0
        }
    }

    pub fn rasterize_glyph(
        &self,
        glyph_pos: u32,
        fallback: FallbackIdx,
    ) -> anyhow::Result<RasterizedGlyph> {
        let mut rasterizers = self.rasterizers.borrow_mut();
        if let Some(raster) = rasterizers.get(&fallback) {
            raster.rasterize_glyph(glyph_pos, self.font_size, self.dpi)
        } else {
            let raster_selection = self
                .font_config
                .upgrade()
                .map_or(FontRasterizerSelection::default(), |c| {
                    c.config.borrow().font_rasterizer
                });
            let raster = new_rasterizer(
                raster_selection,
                &(self.handles.borrow())[fallback],
                self.pixel_geometry,
            )?;
            let result = raster.rasterize_glyph(glyph_pos, self.font_size, self.dpi);
            rasterizers.insert(fallback, raster);
            result
        }
    }

    pub fn clone_handles(&self) -> Vec<ParsedFont> {
        self.handles.borrow().clone()
    }
}

struct FallbackResolveInfo {
    no_glyphs: Vec<char>,
    pending: Arc<Mutex<Vec<ParsedFont>>>,
    completion: Box<dyn FnOnce() + Send>,
    font_dirs: Arc<FontDatabase>,
    built_in: Arc<FontDatabase>,
    locator: Arc<dyn FontLocator + Send + Sync>,
    config: ConfigHandle,
}

impl FallbackResolveInfo {
    fn process(self) {
        let fallback_str = self.no_glyphs.iter().collect::<String>();
        let mut extra_handles = vec![];

        log::trace!(
            "Looking for {} in fallback fonts",
            fallback_str.escape_unicode()
        );

        match self.locator.locate_fallback_for_codepoints(&self.no_glyphs) {
            Ok(ref mut handles) => extra_handles.append(handles),
            Err(err) => log::error!(
                "Error: {:#} while resolving fallback for {} from font-locator",
                err,
                fallback_str.escape_unicode()
            ),
        }

        if self.config.search_font_dirs_for_fallback {
            match self
                .font_dirs
                .locate_fallback_for_codepoints(&self.no_glyphs)
            {
                Ok(ref mut handles) => extra_handles.append(handles),
                Err(err) => log::error!(
                    "Error: {:#} while resolving fallback for {} from font_dirs",
                    err,
                    fallback_str.escape_unicode()
                ),
            }
        }

        match self
            .built_in
            .locate_fallback_for_codepoints(&self.no_glyphs)
        {
            Ok(ref mut handles) => extra_handles.append(handles),
            Err(err) => log::error!(
                "Error: {:#} while resolving fallback for {} for built-in fonts",
                err,
                fallback_str.escape_unicode()
            ),
        }

        let mut wanted = RangeSet::new();
        for c in self.no_glyphs {
            wanted.add(c as u32);
        }
        log::trace!(
            "Fallback fonts that match {} before sorting are: {:#?}",
            fallback_str.escape_unicode(),
            extra_handles
        );

        if wanted.len() > 1 && self.config.sort_fallback_fonts_by_coverage {
            // Sort by ascending coverage
            extra_handles.sort_by_cached_key(|p| {
                p.coverage_intersection(&wanted)
                    .map(|r| r.len())
                    .unwrap_or(0)
            });
            // Re-arrange to descending coverage
            extra_handles.reverse();
            log::trace!(
                "Fallback fonts that match {} after sorting are: {:#?}",
                fallback_str.escape_unicode(),
                extra_handles
            );
        }

        // iteratively reduce to just the fonts that we need
        extra_handles.retain(|p| match p.coverage_intersection(&wanted) {
            Ok(cov) if cov.is_empty() => false,
            Ok(cov) => {
                // Remove the matches from the set, so that we avoid
                // picking up multiple fonts for the same glyphs
                wanted = wanted.difference(&cov);
                true
            }
            Err(_) => false,
        });

        if !extra_handles.is_empty() {
            let mut pending = self.pending.lock().unwrap();
            pending.append(&mut extra_handles);
            (self.completion)();
        }

        if !wanted.is_empty() {
            // There were some glyphs we couldn't resolve!
            let fallback_str = wanted
                .iter_values()
                .map(|c| std::char::from_u32(c).unwrap_or(' '))
                .collect::<String>();

            let current_gen = self.config.generation();
            let show_warning = self.config.warn_about_missing_glyphs
                && LAST_WARNING
                    .lock()
                    .unwrap()
                    .map(|(instant, generation)| {
                        generation != current_gen
                            || instant.elapsed() > Duration::from_secs(60 * 60)
                    })
                    .unwrap_or(true);

            if show_warning {
                LAST_WARNING
                    .lock()
                    .unwrap()
                    .replace((Instant::now(), self.config.generation()));
                let url = "https://wezterm.org/config/fonts.html";
                log::warn!(
                    "No fonts contain glyphs for these codepoints: {}.\n\
                     Placeholder glyphs are being displayed instead.\n\
                     You may wish to install additional fonts, or adjust your\n\
                     configuration so that it can find them.\n\
                     {} has more information about configuring fonts.\n\
                     Set warn_about_missing_glyphs=false to suppress this message.",
                    fallback_str.escape_unicode(),
                    url,
                );

                ToastNotification {
                    title: "Font problem".to_string(),
                    message: format!(
                        "No fonts contain glyphs for these codepoints: {}.\n\
                            Placeholder glyphs are being displayed instead.\n\
                            You may wish to install additional fonts, or adjust\n\
                            your configuration so that it can find them.\n\
                            Set warn_about_missing_glyphs=false to suppress this\n\
                            message.",
                        fallback_str.escape_unicode()
                    ),
                    url: Some(url.to_string()),
                    timeout: Some(Duration::from_secs(15)),
                }
                .show();
            } else {
                log::debug!(
                    "No fonts contain glyphs for these codepoints: {}",
                    fallback_str.escape_unicode()
                );
            }
        }
    }
}

#[derive(PartialEq, Eq)]
enum Entity {
    Title,
    CommandPalette,
    CharSelect,
    PaneSelect,
}

struct FontConfigInner {
    fonts: RefCell<HashMap<TextStyle, Rc<LoadedFont>>>,
    metrics: RefCell<Option<FontMetrics>>,
    dpi: RefCell<usize>,
    font_scale: RefCell<f64>,
    config: RefCell<ConfigHandle>,
    locator: Arc<dyn FontLocator + Send + Sync>,
    font_dirs: RefCell<Arc<FontDatabase>>,
    built_in: RefCell<Arc<FontDatabase>>,
    title_font: RefCell<Option<Rc<LoadedFont>>>,
    pane_select_font: RefCell<Option<Rc<LoadedFont>>>,
    char_select_font: RefCell<Option<Rc<LoadedFont>>>,
    command_palette_font: RefCell<Option<Rc<LoadedFont>>>,
    fallback_channel: RefCell<Option<Sender<FallbackResolveInfo>>>,
}

/// Matches and loads fonts for a given input style
pub struct FontConfiguration {
    inner: Rc<FontConfigInner>,
}

impl FontConfigInner {
    /// Create a new empty configuration
    pub fn new(config: Option<ConfigHandle>, dpi: usize) -> anyhow::Result<Self> {
        let config = config.unwrap_or_else(configuration);
        let locator = new_locator(config.font_locator);
        Ok(Self {
            fonts: RefCell::new(HashMap::new()),
            locator,
            metrics: RefCell::new(None),
            title_font: RefCell::new(None),
            pane_select_font: RefCell::new(None),
            char_select_font: RefCell::new(None),
            command_palette_font: RefCell::new(None),
            font_scale: RefCell::new(1.0),
            dpi: RefCell::new(dpi),
            config: RefCell::new(config.clone()),
            font_dirs: RefCell::new(Arc::new(FontDatabase::with_font_dirs(&config)?)),
            built_in: RefCell::new(Arc::new(FontDatabase::with_built_in()?)),
            fallback_channel: RefCell::new(None),
        })
    }

    fn config_changed(&self, config: &ConfigHandle) -> anyhow::Result<()> {
        let mut fonts = self.fonts.borrow_mut();
        *self.config.borrow_mut() = config.clone();
        // Config was reloaded, invalidate our caches
        fonts.clear();
        self.title_font.borrow_mut().take();
        self.pane_select_font.borrow_mut().take();
        self.char_select_font.borrow_mut().take();
        self.command_palette_font.borrow_mut().take();
        self.metrics.borrow_mut().take();
        *self.font_dirs.borrow_mut() = Arc::new(FontDatabase::with_font_dirs(config)?);
        Ok(())
    }

    fn schedule_fallback_resolve<F: FnOnce() + Send + 'static>(
        &self,
        no_glyphs: Vec<char>,
        pending: &Arc<Mutex<Vec<ParsedFont>>>,
        completion: F,
    ) {
        if no_glyphs.is_empty() {
            return;
        }

        let info = FallbackResolveInfo {
            completion: Box::new(completion),
            no_glyphs,
            pending: Arc::clone(pending),
            font_dirs: Arc::clone(&*self.font_dirs.borrow()),
            built_in: Arc::clone(&*self.built_in.borrow()),
            locator: Arc::clone(&self.locator),
            config: self.config.borrow().clone(),
        };

        let mut fallback = self.fallback_channel.borrow_mut();

        if fallback.is_none() {
            let (tx, rx) = channel::<FallbackResolveInfo>();

            std::thread::spawn(move || {
                for info in rx {
                    info.process();
                }
            });

            fallback.replace(tx);
        }

        if let Err(err) = fallback.as_mut().expect("channel to exist").send(info) {
            log::error!("Failed to schedule font fallback resolve: {:#}", err);
        }
    }

    fn compute_title_font(&self, config: &ConfigHandle, make_bold: bool) -> (TextStyle, f64) {
        fn bold(family: &str) -> FontAttributes {
            FontAttributes {
                family: family.to_string(),
                weight: FontWeight::BOLD,
                ..Default::default()
            }
        }

        let mut fonts = vec![if make_bold {
            bold("Roboto")
        } else {
            FontAttributes::new("Roboto")
        }];

        // Fallback to their main font selection, so that we can pick up
        // any fallback fonts they might have configured in the main
        // config and so that they don't have to replicate that list for
        // the title font.
        for font in &config.font.font {
            let mut font = font.clone();
            font.is_fallback = true;
            fonts.push(font);
        }

        let font_size = if cfg!(windows) { 10. } else { 12. };

        (
            TextStyle {
                foreground: None,
                font: fonts,
            },
            font_size,
        )
    }

    fn make_entity_font_impl(
        &self,
        myself: &Rc<Self>,
        entity: Entity,
    ) -> anyhow::Result<Rc<LoadedFont>> {
        let config = self.config.borrow();
        let make_bold = entity != Entity::CommandPalette;
        let (sys_font, sys_size) = self.compute_title_font(&config, make_bold);

        let (font_size, text_style) = match entity {
            Entity::Title => (config.window_frame.font_size.unwrap_or(sys_size), None),
            Entity::CommandPalette => (
                config.command_palette_font_size,
                config.command_palette_font.as_ref(),
            ),
            Entity::CharSelect => (
                config.char_select_font_size,
                config.char_select_font.as_ref(),
            ),
            Entity::PaneSelect => (
                config.pane_select_font_size,
                config.pane_select_font.as_ref(),
            ),
        };

        let text_style =
            text_style.unwrap_or(config.window_frame.font.as_ref().unwrap_or(&sys_font));

        let dpi = *self.dpi.borrow() as u32;
        let pixel_size = (font_size * dpi as f64 / 72.0) as u16;

        let attributes = text_style.font_with_fallback();
        let (handles, _loaded) = self.resolve_font_helper_impl(&attributes, pixel_size)?;

        let shaper = new_shaper(&*config, &handles)?;

        let metrics = shaper.metrics(font_size, dpi).with_context(|| {
            format!(
                "obtaining metrics for font_size={} @ dpi {}",
                font_size, dpi
            )
        })?;

        let loaded = Rc::new(LoadedFont {
            rasterizers: RefCell::new(HashMap::new()),
            handles: RefCell::new(handles),
            shaper: RefCell::new(shaper),
            metrics,
            font_size,
            dpi,
            font_config: Rc::downgrade(myself),
            pending_fallback: Arc::new(Mutex::new(vec![])),
            text_style: text_style.clone(),
            id: alloc_font_id(),
            tried_glyphs: RefCell::new(HashSet::new()),
            pixel_geometry: config.display_pixel_geometry,
        });

        Ok(loaded)
    }

    fn title_font(&self, myself: &Rc<Self>) -> anyhow::Result<Rc<LoadedFont>> {
        let mut title_font = self.title_font.borrow_mut();

        if let Some(entry) = title_font.as_ref() {
            return Ok(Rc::clone(entry));
        }

        let loaded = self.make_entity_font_impl(myself, Entity::Title)?;

        title_font.replace(Rc::clone(&loaded));

        Ok(loaded)
    }

    fn command_palette_font(&self, myself: &Rc<Self>) -> anyhow::Result<Rc<LoadedFont>> {
        let mut command_palette_font = self.command_palette_font.borrow_mut();

        if let Some(entry) = command_palette_font.as_ref() {
            return Ok(Rc::clone(entry));
        }

        let loaded = self.make_entity_font_impl(myself, Entity::CommandPalette)?;

        command_palette_font.replace(Rc::clone(&loaded));

        Ok(loaded)
    }

    fn char_select_font(&self, myself: &Rc<Self>) -> anyhow::Result<Rc<LoadedFont>> {
        let mut char_select_font = self.char_select_font.borrow_mut();

        if let Some(entry) = char_select_font.as_ref() {
            return Ok(Rc::clone(entry));
        }

        let loaded = self.make_entity_font_impl(myself, Entity::CharSelect)?;

        char_select_font.replace(Rc::clone(&loaded));

        Ok(loaded)
    }

    fn pane_select_font(&self, myself: &Rc<Self>) -> anyhow::Result<Rc<LoadedFont>> {
        let mut pane_select_font = self.pane_select_font.borrow_mut();

        if let Some(entry) = pane_select_font.as_ref() {
            return Ok(Rc::clone(entry));
        }

        let loaded = self.make_entity_font_impl(myself, Entity::PaneSelect)?;

        pane_select_font.replace(Rc::clone(&loaded));

        Ok(loaded)
    }

    fn resolve_font_helper_impl(
        &self,
        attributes: &[FontAttributes],
        pixel_size: u16,
    ) -> anyhow::Result<(Vec<ParsedFont>, HashSet<FontAttributes>)> {
        let preferred_attributes = attributes
            .iter()
            .filter(|a| !a.is_fallback)
            .cloned()
            .collect::<Vec<_>>();
        let fallback_attributes = attributes
            .iter()
            .filter(|a| a.is_fallback)
            .cloned()
            .collect::<Vec<_>>();
        let mut loaded = HashSet::new();
        let mut handles = vec![];

        for &attrs in &[&preferred_attributes, &fallback_attributes] {
            let mut candidates = vec![];

            let font_dirs = self.font_dirs.borrow();
            for attr in attrs {
                candidates.append(&mut font_dirs.candidates(attr));
            }

            let mut loaded_ignored = HashSet::new();
            let located = self
                .locator
                .load_fonts(attrs, &mut loaded_ignored, pixel_size)?;
            for font in &located {
                candidates.push(font);
            }

            let built_in = self.built_in.borrow();
            for attr in attrs {
                candidates.append(&mut built_in.candidates(attr));
            }

            let mut is_fallback = false;

            for attr in attrs {
                if attr.is_fallback {
                    is_fallback = true;
                }

                if loaded.contains(attr) {
                    continue;
                }
                let named_candidates: Vec<&ParsedFont> = candidates
                    .iter()
                    .filter_map(|&p| if p.matches_name(attr) { Some(p) } else { None })
                    .collect();
                if let Some(idx) =
                    ParsedFont::best_matching_index(attr, &named_candidates, pixel_size)
                {
                    named_candidates.get(idx).map(|&p| {
                        loaded.insert(attr.clone());
                        handles.push(p.clone().synthesize(attr))
                    });
                }
            }

            if !is_fallback && loaded.is_empty() {
                // We didn't explicitly match any names.
                // When using fontconfig, the system may have expanded a family name
                // like "monospace" into the real font, in which case we wouldn't have
                // found a match in the `named_candidates` vec above, because of the
                // name mismatch.
                // So what we do now is make a second pass over all the located candidates,
                // ignoring their names, and just match based on font attributes.
                let located_candidates: Vec<_> = located.iter().collect();
                for attr in attrs {
                    if let Some(idx) =
                        ParsedFont::best_matching_index(attr, &located_candidates, pixel_size)
                    {
                        located_candidates.get(idx).map(|&p| {
                            loaded.insert(attr.clone());
                            handles.push(p.clone().synthesize(attr))
                        });
                    }
                }
            }
        }

        Ok((handles, loaded))
    }

    fn resolve_font_helper(
        &self,
        style: &TextStyle,
        config: &ConfigHandle,
        pixel_size: u16,
    ) -> anyhow::Result<(Box<dyn FontShaper>, Vec<ParsedFont>)> {
        let attributes = style.font_with_fallback();

        let (handles, loaded) = self.resolve_font_helper_impl(&attributes, pixel_size)?;

        for attr in &attributes {
            if !attr.is_synthetic && !attr.is_fallback && !loaded.contains(attr) {
                let styled_extra = if attr.weight != FontWeight::default()
                    || attr.style != FontStyle::default()
                    || attr.stretch != FontStretch::default()
                {
                    ". An alternative variant of the font was requested; \
                    TrueType and OpenType fonts don't have an automatic way to \
                    produce these font variants, so a separate font file containing \
                    the bold or italic variant must be installed"
                } else {
                    ""
                };

                let is_primary = config.font.font.iter().any(|a| a == attr);
                let derived_from_primary = config.font.font.iter().any(|a| a.family == attr.family);

                let explanation = if is_primary {
                    // This is the primary font selection
                    format!(
                        "Unable to load a font specified by your font={} configuration",
                        attr
                    )
                } else if derived_from_primary {
                    // it came from font_rules and may have been derived from
                    // their primary font (we can't know for sure)
                    format!(
                        "Unable to load a font matching one of your font_rules: {}. \
                        Note that wezterm will synthesize font_rules to select bold \
                        and italic fonts based on your primary font configuration",
                        attr
                    )
                } else {
                    format!(
                        "Unable to load a font matching one of your font_rules: {}",
                        attr
                    )
                };

                config::show_error(&format!(
                    "{}. Fallback(s) are being used instead, and the terminal \
                    may not render as intended{}. See \
                    https://wezterm.org/config/fonts.html for more information",
                    explanation, styled_extra
                ));
            }
        }

        Ok((new_shaper(&*config, &handles)?, handles))
    }

    /// Given a text style, load (with caching) the font that best
    /// matches according to the fontconfig pattern.
    fn resolve_font(&self, myself: &Rc<Self>, style: &TextStyle) -> anyhow::Result<Rc<LoadedFont>> {
        let config = self.config.borrow();
        let is_default = *style == config.font;
        let def_font = if !is_default && config.use_cap_height_to_scale_fallback_fonts {
            Some(self.default_font(myself)?)
        } else {
            None
        };

        let mut fonts = self.fonts.borrow_mut();

        if let Some(entry) = fonts.get(style) {
            return Ok(Rc::clone(entry));
        }

        let mut font_size = config.font_size * *self.font_scale.borrow();
        let dpi = *self.dpi.borrow() as u32;
        let pixel_size = (font_size * dpi as f64 / 72.0) as u16;

        let (mut shaper, mut handles) = self.resolve_font_helper(style, &config, pixel_size)?;

        let mut metrics = shaper.metrics(font_size, dpi).with_context(|| {
            format!(
                "obtaining metrics for font_size={} @ dpi {}",
                font_size, dpi
            )
        })?;

        if let Some(def_font) = def_font {
            let def_metrics = def_font.metrics();
            match (def_metrics.cap_height, metrics.cap_height) {
                (Some(d), Some(m)) => {
                    // Scale by the ratio of the pixel heights of the default
                    // and this font; this causes the `I` glyphs to appear to
                    // have the same height.
                    let scale = d.get() / m.get();
                    if scale != 1.0 {
                        let scaled_pixel_size = (pixel_size as f64 * scale) as u16;
                        let scaled_font_size = font_size * scale;
                        log::trace!(
                            "using cap height adjusted: pixel_size {} -> {}, font_size {} -> {}, {:?}",
                            pixel_size,
                            scaled_pixel_size,
                            font_size,
                            scaled_font_size,
                            metrics,
                        );
                        let (alt_shaper, alt_handles) =
                            self.resolve_font_helper(style, &config, scaled_pixel_size)?;
                        shaper = alt_shaper;
                        handles = alt_handles;

                        metrics = shaper.metrics(scaled_font_size, dpi).with_context(|| {
                            format!(
                                "obtaining cap-height adjusted metrics for font_size={} @ dpi {}",
                                scaled_font_size, dpi
                            )
                        })?;

                        font_size = scaled_font_size;
                    }
                }
                _ => {}
            }
        }

        let loaded = Rc::new(LoadedFont {
            rasterizers: RefCell::new(HashMap::new()),
            handles: RefCell::new(handles),
            shaper: RefCell::new(shaper),
            metrics,
            font_size,
            dpi,
            font_config: Rc::downgrade(myself),
            pending_fallback: Arc::new(Mutex::new(vec![])),
            text_style: style.clone(),
            id: alloc_font_id(),
            tried_glyphs: RefCell::new(HashSet::new()),
            pixel_geometry: config.display_pixel_geometry,
        });

        fonts.insert(style.clone(), Rc::clone(&loaded));

        Ok(loaded)
    }

    pub fn change_scaling(&self, font_scale: f64, dpi: usize) -> (f64, usize) {
        let prior_font = *self.font_scale.borrow();
        let prior_dpi = *self.dpi.borrow();

        *self.dpi.borrow_mut() = dpi;
        *self.font_scale.borrow_mut() = font_scale;
        self.fonts.borrow_mut().clear();
        self.metrics.borrow_mut().take();
        self.title_font.borrow_mut().take();

        (prior_font, prior_dpi)
    }

    /// Returns the baseline font specified in the configuration
    pub fn default_font(&self, myself: &Rc<Self>) -> anyhow::Result<Rc<LoadedFont>> {
        self.resolve_font(myself, &self.config.borrow().font)
    }

    pub fn get_font_scale(&self) -> f64 {
        *self.font_scale.borrow()
    }

    pub fn get_dpi(&self) -> usize {
        *self.dpi.borrow()
    }

    pub fn default_font_metrics(&self, myself: &Rc<Self>) -> Result<FontMetrics, Error> {
        {
            let metrics = self.metrics.borrow();
            if let Some(metrics) = metrics.as_ref() {
                return Ok(*metrics);
            }
        }

        let font = self.default_font(myself)?;
        let metrics = font.metrics();

        *self.metrics.borrow_mut() = Some(metrics);

        Ok(metrics)
    }

    /// Apply the defined font_rules from the user configuration to
    /// produce the text style that best matches the supplied input
    /// cell attributes.
    pub fn match_style<'a>(
        &self,
        config: &'a ConfigHandle,
        attrs: &CellAttributes,
    ) -> &'a TextStyle {
        match_style(config, attrs)
    }
}

/// Apply the defined font_rules from the user configuration to produce the
/// text style that best matches the supplied input cell attributes.
///
/// Expects a config that has been through `Config::compute_extra_defaults`.
fn match_style<'a>(config: &'a Config, attrs: &CellAttributes) -> &'a TextStyle {
    // a little macro to avoid boilerplate for matching the rules.
    // If the rule doesn't specify a value for an attribute then
    // it will implicitly match.  If it specifies an attribute
    // then it has to have the same value as that in the input attrs.
    macro_rules! attr_match {
        ($ident:ident, $rule:expr) => {
            if let Some($ident) = $rule.$ident {
                if $ident != attrs.$ident() {
                    // Does not match
                    continue;
                }
            }
            // matches so far...
        };
    }

    // Palette indices below 8 are the standard colours, and are the only ones
    // BrightOnly brightens in place of using a bold font.
    let would_bright = match attrs.foreground() {
        wezterm_term::color::ColorAttribute::PaletteIndex(idx) if idx < 8 => config.is_bold(attrs),
        _ => false,
    };

    // What the conditions compare against: under BrightOnly on a standard
    // palette the colour is brightened in place of a bold font, so bold
    // is suppressed for matching.
    let (effective_bold, effective_intensity) = match config.bold_brightens_ansi_colors {
        BoldBrightening::BrightOnly if would_bright => (false, Intensity::Normal),
        BoldBrightening::No | BoldBrightening::BrightAndBold | BoldBrightening::BrightOnly => {
            (config.is_bold(attrs), attrs.intensity())
        }
    };

    // Never rewritten, but it still goes through `config.is_dim`, whose answer
    // depends on the tracking mode.
    let effective_dim = config.is_dim(attrs);

    for rule in &config.font_rules {
        if let Some(intensity) = rule.intensity {
            debug_assert!(
                !config.track_bold_and_dim_separately,
                "a font rule states `intensity` under \
                 track_bold_and_dim_separately; Config::compute_extra_defaults \
                 is what removes those, so this config has not been through it"
            );
            if intensity != effective_intensity {
                // Rule does not match
                continue;
            }
            // matches so far
        }
        if let Some(bold) = rule.bold {
            if bold != effective_bold {
                continue;
            }
        }
        if let Some(dim) = rule.dim {
            if dim != effective_dim {
                continue;
            }
        }
        attr_match!(underline, &rule);
        attr_match!(italic, &rule);
        attr_match!(blink, &rule);
        attr_match!(reverse, &rule);
        attr_match!(strikethrough, &rule);
        attr_match!(invisible, &rule);

        // If we get here, then none of the rules didn't match,
        // so we therefore assume that it did match overall.
        return &rule.font;
    }
    &config.font
}

impl FontConfiguration {
    /// Create a new empty configuration
    pub fn new(config: Option<ConfigHandle>, dpi: usize) -> anyhow::Result<Self> {
        let inner = Rc::new(FontConfigInner::new(config, dpi)?);
        Ok(Self { inner })
    }

    pub fn config_changed(&self, config: &ConfigHandle) -> anyhow::Result<()> {
        self.inner.config_changed(config)
    }

    pub fn config(&self) -> ConfigHandle {
        self.inner.config.borrow().clone()
    }

    pub fn title_font(&self) -> anyhow::Result<Rc<LoadedFont>> {
        self.inner.title_font(&self.inner)
    }

    pub fn command_palette_font(&self) -> anyhow::Result<Rc<LoadedFont>> {
        self.inner.command_palette_font(&self.inner)
    }

    pub fn pane_select_font(&self) -> anyhow::Result<Rc<LoadedFont>> {
        self.inner.pane_select_font(&self.inner)
    }

    pub fn char_select_font(&self) -> anyhow::Result<Rc<LoadedFont>> {
        self.inner.char_select_font(&self.inner)
    }

    /// Given a text style, load (with caching) the font that best
    /// matches according to the fontconfig pattern.
    pub fn resolve_font(&self, style: &TextStyle) -> anyhow::Result<Rc<LoadedFont>> {
        self.inner.resolve_font(&self.inner, style)
    }

    pub fn change_scaling(&self, font_scale: f64, dpi: usize) -> (f64, usize) {
        self.inner.change_scaling(font_scale, dpi)
    }

    /// Returns the baseline font specified in the configuration
    pub fn default_font(&self) -> anyhow::Result<Rc<LoadedFont>> {
        self.inner.default_font(&self.inner)
    }

    pub fn get_font_scale(&self) -> f64 {
        self.inner.get_font_scale()
    }

    pub fn get_dpi(&self) -> usize {
        self.inner.get_dpi()
    }

    pub fn default_font_metrics(&self) -> Result<FontMetrics, Error> {
        self.inner.default_font_metrics(&self.inner)
    }

    pub fn list_fonts_in_font_dirs(&self) -> Vec<ParsedFont> {
        let mut font_dirs = self.inner.font_dirs.borrow().list_available();
        let mut built_in = self.inner.built_in.borrow().list_available();

        font_dirs.append(&mut built_in);
        font_dirs.sort();
        font_dirs
    }

    pub fn list_system_fonts(&self) -> anyhow::Result<Vec<ParsedFont>> {
        self.inner.locator.enumerate_all_fonts()
    }

    /// Apply the defined font_rules from the user configuration to
    /// produce the text style that best matches the supplied input
    /// cell attributes.
    pub fn match_style<'a>(
        &self,
        config: &'a ConfigHandle,
        attrs: &CellAttributes,
    ) -> &'a TextStyle {
        self.inner.match_style(config, attrs)
    }
}

#[cfg(test)]
mod match_style_test {
    use super::match_style;
    use config::{
        BoldBrightening, Config, FontAttributes, FontStyle, FontWeight, StyleRule, TextStyle,
    };
    use wezterm_term::color::{AnsiColor, ColorAttribute};
    use wezterm_term::{CellAttributes, Intensity};

    const STANDARD_PALETTE_RED: u8 = AnsiColor::Maroon as u8;

    fn base_config() -> Config {
        Config::default().compute_extra_defaults(None).0
    }

    fn weight_and_style(style: &TextStyle) -> (u16, FontStyle) {
        let attr = style.font.first().expect("a TextStyle always has a font");
        (attr.weight.to_opentype_weight(), attr.style)
    }

    fn family(style: &TextStyle) -> &str {
        let attr = style.font.first().expect("a TextStyle always has a font");
        &attr.family
    }

    fn cell(intensity: Intensity, italic: bool) -> CellAttributes {
        let mut attrs = CellAttributes::default();
        attrs.set_intensity(intensity).set_italic(italic);
        attrs
    }

    #[test]
    fn built_in_rules_at_defaults() {
        let config = base_config();
        let m =
            |intensity, italic| weight_and_style(match_style(&config, &cell(intensity, italic)));

        // Regular is 400; bolder() adds 400, lighter() subtracts 300.
        assert_eq!(m(Intensity::Normal, false), (400, FontStyle::Normal));
        assert_eq!(m(Intensity::Normal, true), (400, FontStyle::Italic));
        assert_eq!(m(Intensity::Bold, false), (800, FontStyle::Normal));
        assert_eq!(m(Intensity::Bold, true), (800, FontStyle::Italic));
        assert_eq!(m(Intensity::Half, false), (100, FontStyle::Normal));
        assert_eq!(m(Intensity::Half, true), (100, FontStyle::Italic));
    }

    #[test]
    fn bright_only_suppresses_bold_weight_on_standard_palette() {
        let mut config = base_config();
        config.bold_brightens_ansi_colors = BoldBrightening::BrightOnly;

        let mut attrs = cell(Intensity::Bold, false);
        attrs.set_foreground(ColorAttribute::PaletteIndex(STANDARD_PALETTE_RED));

        // The rewrite makes the effective intensity Normal, so the Bold rule
        // does not fire.
        assert_eq!(
            weight_and_style(match_style(&config, &attrs)),
            (400, FontStyle::Normal)
        );
    }

    #[test]
    fn bright_and_bold_keeps_bold_weight() {
        let mut config = base_config();
        config.bold_brightens_ansi_colors = BoldBrightening::BrightAndBold;

        let mut attrs = cell(Intensity::Bold, false);
        attrs.set_foreground(ColorAttribute::PaletteIndex(STANDARD_PALETTE_RED));

        assert_eq!(
            weight_and_style(match_style(&config, &attrs)),
            (800, FontStyle::Normal)
        );
    }

    #[test]
    fn bold_brightening_off_keeps_bold_weight() {
        let mut config = base_config();
        config.bold_brightens_ansi_colors = BoldBrightening::No;

        let mut attrs = cell(Intensity::Bold, false);
        attrs.set_foreground(ColorAttribute::PaletteIndex(STANDARD_PALETTE_RED));

        assert_eq!(
            weight_and_style(match_style(&config, &attrs)),
            (800, FontStyle::Normal)
        );
    }

    #[test]
    fn a_user_rule_beats_the_built_ins() {
        let mut config = Config::default();
        config.font_rules.push(StyleRule {
            intensity: Some(Intensity::Bold),
            font: TextStyle {
                font: vec![FontAttributes::new("UserChosenFace")],
                ..Default::default()
            },
            ..Default::default()
        });
        let config = config.compute_extra_defaults(None).0;

        let style = match_style(&config, &cell(Intensity::Bold, false));
        assert_eq!(family(style), "UserChosenFace");
    }

    #[test]
    fn no_default_rule_matches_plain_text() {
        let config = base_config();
        let style = match_style(&config, &cell(Intensity::Normal, false));
        // Identity, not equality: this pins that *no rule matched*, not that
        // the winner happened to equal the base font.
        assert!(std::ptr::eq(style, &config.font));
    }

    #[test]
    fn bright_only_rewrite_only_affects_normal_non_bright_colors() {
        let mut config = base_config();
        config.bold_brightens_ansi_colors = BoldBrightening::BrightOnly;

        let bold_on = |idx: u8| {
            let mut attrs = cell(Intensity::Bold, false);
            attrs.set_foreground(ColorAttribute::PaletteIndex(idx));
            weight_and_style(match_style(&config, &attrs))
        };

        assert_eq!(bold_on(AnsiColor::Silver as u8), (400, FontStyle::Normal));
        assert_eq!(bold_on(AnsiColor::Grey as u8), (800, FontStyle::Normal));
    }

    #[test]
    fn a_default_foreground_is_never_suppressed() {
        for brightening in [
            BoldBrightening::No,
            BoldBrightening::BrightAndBold,
            BoldBrightening::BrightOnly,
        ] {
            let mut config = base_config();
            config.bold_brightens_ansi_colors = brightening;

            let mut attrs = cell(Intensity::Bold, false);
            attrs.set_foreground(ColorAttribute::Default);

            assert_eq!(
                weight_and_style(match_style(&config, &attrs)),
                (800, FontStyle::Normal),
                "with bold_brightens_ansi_colors = {brightening:?}"
            );
        }
    }

    #[test]
    fn the_bright_only_rewrite_is_per_condition_not_per_rule() {
        let mut config = Config::default();
        config.bold_brightens_ansi_colors = BoldBrightening::BrightOnly;

        // States `intensity`, so the rewrite is consulted for it.
        config.font_rules.push(StyleRule {
            intensity: Some(Intensity::Bold),
            italic: Some(false),
            font: TextStyle {
                font: vec![FontAttributes::new("StatesIntensity")],
                ..Default::default()
            },
            ..Default::default()
        });
        // States no `intensity`, so the rewrite is never consulted for it.
        config.font_rules.push(StyleRule {
            italic: Some(true),
            font: TextStyle {
                font: vec![FontAttributes::new("StatesNoIntensity")],
                ..Default::default()
            },
            ..Default::default()
        });
        let config = config.compute_extra_defaults(None).0;

        // Bold on a standard-palette colour: the rewrite makes the effective
        // intensity Normal, so the intensity-stating rule does not fire.
        let mut bold = cell(Intensity::Bold, false);
        bold.set_foreground(ColorAttribute::PaletteIndex(STANDARD_PALETTE_RED));
        assert!(std::ptr::eq(match_style(&config, &bold), &config.font));

        // The same cell with italic added. The rule that states no intensity
        // is unaffected by the rewrite and fires.
        let mut bold_italic = cell(Intensity::Bold, true);
        bold_italic.set_foreground(ColorAttribute::PaletteIndex(STANDARD_PALETTE_RED));
        assert_eq!(
            family(match_style(&config, &bold_italic)),
            "StatesNoIntensity"
        );
    }

    #[test]
    fn the_fall_through_font_keeps_its_family_but_rule_fonts_are_reduced() {
        let mut config = Config::default();
        config.font = TextStyle {
            font: vec![FontAttributes {
                weight: FontWeight::MEDIUM,
                ..FontAttributes::new("Iosevka Light")
            }],
            ..Default::default()
        };
        let config = config.compute_extra_defaults(None).0;

        // compute_extra_defaults derives the built-in rule fonts from
        // `self.font.reduce_first_font_to_family()`, but leaves `cfg.font`
        // itself alone. So plain text keeps the configured family verbatim...
        let plain = match_style(&config, &cell(Intensity::Normal, false));
        assert!(std::ptr::eq(plain, &config.font));
        let attr = plain.font.first().expect("a font");
        assert_eq!(attr.family, "Iosevka Light");
        assert_eq!(attr.weight, FontWeight::MEDIUM);
        assert!(!attr.is_synthetic);

        // ...while bold text matches a rule built from the reduced family,
        // and so resolves to a *different family* than the plain text does.
        let bold = match_style(&config, &cell(Intensity::Bold, false));
        let attr = bold.font.first().expect("a font");
        assert_eq!(attr.family, "Iosevka");
        assert_eq!(attr.weight, FontWeight::MEDIUM.bolder());
        assert!(attr.is_synthetic);
    }

    #[test]
    fn a_rule_naming_fewer_conditions_matches_more_text() {
        let mut config = Config::default();
        // This rule states only italic, so it also claims bold-italic text.
        config.font_rules.push(StyleRule {
            italic: Some(true),
            font: TextStyle {
                font: vec![FontAttributes::new("AnyItalic")],
                ..Default::default()
            },
            ..Default::default()
        });
        let config = config.compute_extra_defaults(None).0;

        let style = match_style(&config, &cell(Intensity::Bold, true));
        assert_eq!(family(style), "AnyItalic");
    }

    fn config_with_tracking(rule: StyleRule, separately: bool) -> Config {
        let mut config = Config::default();
        config.track_bold_and_dim_separately = separately;
        config.dim_opacity = Some(1.0);
        config.font_rules.push(rule);
        config.compute_extra_defaults(None).0
    }

    fn config_with_rule(rule: StyleRule) -> Config {
        config_with_tracking(rule, true)
    }

    fn marked_rule(bold: Option<bool>, dim: Option<bool>) -> StyleRule {
        StyleRule {
            bold,
            dim,
            font: TextStyle {
                font: vec![FontAttributes::new("RuleFired")],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn rule_fired(config: &Config, attrs: &CellAttributes) -> bool {
        match_style(config, attrs)
            .font
            .first()
            .map(|f| f.family == "RuleFired")
            .unwrap_or(false)
    }

    fn both_attributes(dim_last: bool) -> CellAttributes {
        let mut attrs = CellAttributes::default();
        if dim_last {
            attrs.apply_sgr_intensity(Intensity::Bold);
            attrs.apply_sgr_intensity(Intensity::Half);
        } else {
            attrs.apply_sgr_intensity(Intensity::Half);
            attrs.apply_sgr_intensity(Intensity::Bold);
        }
        attrs
    }

    #[test]
    fn bold_condition_matches_the_bold_attribute_in_either_order() {
        let config = config_with_rule(marked_rule(Some(true), None));
        assert!(rule_fired(&config, &both_attributes(true)));
        assert!(rule_fired(&config, &both_attributes(false)));
    }

    #[test]
    fn dim_condition_matches_the_dim_attribute_in_either_order() {
        let config = config_with_rule(marked_rule(None, Some(true)));
        assert!(rule_fired(&config, &both_attributes(true)));
        assert!(rule_fired(&config, &both_attributes(false)));
    }

    #[test]
    fn both_conditions_together_require_both_attributes() {
        let config = config_with_rule(marked_rule(Some(true), Some(true)));
        assert!(rule_fired(&config, &both_attributes(true)));
        assert!(rule_fired(&config, &both_attributes(false)));
        assert!(!rule_fired(&config, &cell(Intensity::Bold, false)));
        assert!(!rule_fired(&config, &cell(Intensity::Half, false)));
    }

    #[test]
    fn negative_spellings_match_the_absence_of_an_attribute() {
        let config = config_with_rule(marked_rule(Some(false), Some(false)));
        assert!(rule_fired(&config, &cell(Intensity::Normal, false)));
        assert!(!rule_fired(&config, &both_attributes(true)));
        assert!(!rule_fired(&config, &cell(Intensity::Bold, false)));
        assert!(!rule_fired(&config, &cell(Intensity::Half, false)));
    }

    #[test]
    fn bright_only_rewrite_applies_to_the_bold_condition() {
        let mut attrs = cell(Intensity::Bold, false);
        attrs.set_foreground(ColorAttribute::PaletteIndex(STANDARD_PALETTE_RED));

        let mut positive = config_with_rule(marked_rule(Some(true), None));
        positive.bold_brightens_ansi_colors = BoldBrightening::BrightOnly;
        assert!(
            !rule_fired(&positive, &attrs),
            "BrightOnly suppresses the bold attribute for matching"
        );

        let mut negative = config_with_rule(marked_rule(Some(false), None));
        negative.bold_brightens_ansi_colors = BoldBrightening::BrightOnly;
        assert!(
            rule_fired(&negative, &attrs),
            "and so the negative spelling matches instead"
        );
    }

    #[test]
    fn no_other_brightening_setting_suppresses_the_bold_condition() {
        let mut attrs = cell(Intensity::Bold, false);
        attrs.set_foreground(ColorAttribute::PaletteIndex(STANDARD_PALETTE_RED));

        for brightening in [BoldBrightening::No, BoldBrightening::BrightAndBold] {
            let mut config = config_with_rule(marked_rule(Some(true), None));
            config.bold_brightens_ansi_colors = brightening;
            assert!(
                rule_fired(&config, &attrs),
                "with bold_brightens_ansi_colors = {brightening:?}"
            );
        }
    }

    #[test]
    fn the_rewrite_leaves_dim_alone() {
        let mut config = config_with_rule(marked_rule(None, Some(true)));
        config.bold_brightens_ansi_colors = BoldBrightening::BrightOnly;

        let fg = |mut attrs: CellAttributes| {
            attrs.set_foreground(ColorAttribute::PaletteIndex(STANDARD_PALETTE_RED));
            attrs
        };

        // Both attributes, bold most recent: bold is suppressed for matching
        // and the colour brightened, but `dim = true` still fires.
        assert!(rule_fired(&config, &fg(both_attributes(false))));

        // Both attributes, dim most recent.
        assert!(rule_fired(&config, &fg(both_attributes(true))));

        // Dim alone also fires.
        assert!(rule_fired(&config, &fg(cell(Intensity::Half, false))));
    }

    #[test]
    fn a_not_dim_rule_does_not_fire_on_suppressed_bold_dim_text() {
        let mut config = config_with_rule(marked_rule(Some(false), Some(false)));
        config.bold_brightens_ansi_colors = BoldBrightening::BrightOnly;

        let mut attrs = both_attributes(false);
        attrs.set_foreground(ColorAttribute::PaletteIndex(STANDARD_PALETTE_RED));

        assert!(!rule_fired(&config, &attrs));
    }

    #[test]
    fn the_rewrite_does_not_apply_off_the_standard_palette() {
        let mut attrs = cell(Intensity::Bold, false);
        attrs.set_foreground(ColorAttribute::PaletteIndex(9));

        let mut config = config_with_rule(marked_rule(Some(true), None));
        config.bold_brightens_ansi_colors = BoldBrightening::BrightOnly;
        assert!(rule_fired(&config, &attrs));
    }

    #[test]
    fn a_rule_may_mix_the_old_vocabulary_with_the_new() {
        // `intensity: Bold` with `dim: true` asks for two things that cannot
        // both hold once `dim` means "Half arrived last".
        let mut contradictory = marked_rule(None, Some(true));
        contradictory.intensity = Some(Intensity::Bold);
        let config = config_with_tracking(contradictory, false);
        assert!(!rule_fired(&config, &both_attributes(false)));
        assert!(!rule_fired(&config, &both_attributes(true)));

        // The agreeing pairing fires exactly where either half alone would.
        let mut agreeing = marked_rule(None, Some(true));
        agreeing.intensity = Some(Intensity::Half);
        let config = config_with_tracking(agreeing, false);
        // Dim arrived last, so intensity() reads Half and `dim` aliases it.
        assert!(rule_fired(&config, &both_attributes(true)));
        // Bold arrived last, so neither half holds.
        assert!(!rule_fired(&config, &both_attributes(false)));
    }

    fn faded_config() -> Config {
        let mut config = Config::default();
        config.track_bold_and_dim_separately = true;
        config.dim_opacity = Some(0.5);
        config.compute_extra_defaults(None).0
    }

    #[test]
    fn weight_follows_bold_alone_under_separate_tracking() {
        let config = faded_config();
        let m = |attrs: &CellAttributes| weight_and_style(match_style(&config, attrs));

        // The four states without dim keep the default weights.
        assert_eq!(m(&cell(Intensity::Normal, false)), (400, FontStyle::Normal));
        assert_eq!(m(&cell(Intensity::Normal, true)), (400, FontStyle::Italic));
        assert_eq!(m(&cell(Intensity::Bold, false)), (800, FontStyle::Normal));
        assert_eq!(m(&cell(Intensity::Bold, true)), (800, FontStyle::Italic));

        // Dim without bold loses the lighter weight; the fade expresses it.
        assert_eq!(m(&cell(Intensity::Half, false)), (400, FontStyle::Normal));
        assert_eq!(m(&cell(Intensity::Half, true)), (400, FontStyle::Italic));

        // Bold and dim together draw bold, in either arrival order.
        let mut both = both_attributes(true);
        assert_eq!(m(&both), (800, FontStyle::Normal));
        both = both_attributes(false);
        assert_eq!(m(&both), (800, FontStyle::Normal));

        let mut both_italic = both_attributes(true);
        both_italic.set_italic(true);
        assert_eq!(m(&both_italic), (800, FontStyle::Italic));
        let mut both_italic = both_attributes(false);
        both_italic.set_italic(true);
        assert_eq!(m(&both_italic), (800, FontStyle::Italic));
    }

    #[test]
    fn plain_text_still_falls_through_when_faded() {
        let config = faded_config();
        let style = match_style(&config, &cell(Intensity::Normal, false));
        assert!(
            std::ptr::eq(style, &config.font),
            "plain non-italic text must reach the base font, not a rule"
        );
    }

    #[test]
    fn unified_tracking_with_a_fade_retires_only_the_lighter_weight() {
        let mut config = Config::default();
        config.dim_opacity = Some(0.5);
        let config = config.compute_extra_defaults(None).0;
        assert!(!config.track_bold_and_dim_separately);

        let m =
            |intensity, italic| weight_and_style(match_style(&config, &cell(intensity, italic)));

        // The four states without dim keep the default weights.
        assert_eq!(m(Intensity::Normal, false), (400, FontStyle::Normal));
        assert_eq!(m(Intensity::Normal, true), (400, FontStyle::Italic));
        assert_eq!(m(Intensity::Bold, false), (800, FontStyle::Normal));
        assert_eq!(m(Intensity::Bold, true), (800, FontStyle::Italic));

        // Dim loses the lighter weight; dim italic keeps its italic.
        assert_eq!(m(Intensity::Half, false), (400, FontStyle::Normal));
        assert_eq!(m(Intensity::Half, true), (400, FontStyle::Italic));

        // Identity, not equality: dim non-italic text matches no rule at all.
        assert!(std::ptr::eq(
            match_style(&config, &cell(Intensity::Half, false)),
            &config.font
        ));
    }

    #[test]
    fn bright_only_keys_on_the_bold_attribute_when_faded() {
        let mut config = faded_config();
        config.bold_brightens_ansi_colors = BoldBrightening::BrightOnly;

        for dim_last in [true, false] {
            let mut attrs = both_attributes(dim_last);
            attrs.set_foreground(ColorAttribute::PaletteIndex(STANDARD_PALETTE_RED));
            assert_eq!(
                weight_and_style(match_style(&config, &attrs)),
                (400, FontStyle::Normal),
                "bold weight is suppressed regardless of arrival order (dim_last = {dim_last})"
            );
        }
    }

    #[test]
    fn bright_only_keys_on_recency_when_not_faded() {
        let mut config = base_config();
        config.bold_brightens_ansi_colors = BoldBrightening::BrightOnly;

        let mut dim_last = both_attributes(true);
        dim_last.set_foreground(ColorAttribute::PaletteIndex(STANDARD_PALETTE_RED));
        // intensity() reads Half, so the rewrite does not fire and the
        // Half rule matches: the lighter weight.
        assert_eq!(
            weight_and_style(match_style(&config, &dim_last)),
            (100, FontStyle::Normal)
        );

        let mut bold_last = both_attributes(false);
        bold_last.set_foreground(ColorAttribute::PaletteIndex(STANDARD_PALETTE_RED));
        // intensity() reads Bold, so the rewrite fires and weight is
        // suppressed.
        assert_eq!(
            weight_and_style(match_style(&config, &bold_last)),
            (400, FontStyle::Normal)
        );
    }

    #[test]
    fn a_user_rule_still_beats_the_restated_built_ins() {
        let mut config = Config::default();
        config.track_bold_and_dim_separately = true;
        config.dim_opacity = Some(0.5);
        config.font_rules.push(marked_rule(Some(true), Some(true)));
        let config = config.compute_extra_defaults(None).0;

        assert!(rule_fired(&config, &both_attributes(true)));
        assert!(rule_fired(&config, &both_attributes(false)));
    }

    #[test]
    fn bright_only_selects_plain_italic_when_faded() {
        let mut config = faded_config();
        config.bold_brightens_ansi_colors = BoldBrightening::BrightOnly;

        let red_italic = |mut attrs: CellAttributes| {
            attrs.set_italic(true);
            attrs.set_foreground(ColorAttribute::PaletteIndex(STANDARD_PALETTE_RED));
            attrs
        };

        for dim_last in [true, false] {
            assert_eq!(
                weight_and_style(match_style(&config, &red_italic(both_attributes(dim_last)))),
                (400, FontStyle::Italic),
                "plain italic, not bold italic (dim_last = {dim_last})"
            );
        }

        // The same row without dim.
        assert_eq!(
            weight_and_style(match_style(
                &config,
                &red_italic(cell(Intensity::Bold, false))
            )),
            (400, FontStyle::Italic)
        );
    }

    #[test]
    fn bright_and_bold_keeps_the_bold_font_when_faded() {
        let mut config = faded_config();
        config.bold_brightens_ansi_colors = BoldBrightening::BrightAndBold;

        for dim_last in [true, false] {
            let mut attrs = both_attributes(dim_last);
            attrs.set_foreground(ColorAttribute::PaletteIndex(STANDARD_PALETTE_RED));

            assert_eq!(
                weight_and_style(match_style(&config, &attrs)),
                (800, FontStyle::Normal),
                "the bold rule fires (dim_last = {dim_last})"
            );

            attrs.set_italic(true);
            assert_eq!(
                weight_and_style(match_style(&config, &attrs)),
                (800, FontStyle::Italic),
                "the bold-italic rule fires (dim_last = {dim_last})"
            );
        }
    }

    #[test]
    fn the_conditions_alias_the_derived_value_under_unified_tracking() {
        let dim_last = both_attributes(true);
        let config = config_with_tracking(marked_rule(None, Some(true)), false);

        // dim arrived last, so unified tracking calls this dim.
        assert!(
            rule_fired(&config, &dim_last),
            "dim = true fires where intensity = Half would"
        );

        let bold_last = both_attributes(false);
        assert!(
            !rule_fired(&config, &bold_last),
            "bold arrived last, so unified tracking does not call this dim"
        );
    }

    #[test]
    fn the_bold_condition_aliases_the_derived_value_under_unified_tracking() {
        let config = config_with_tracking(marked_rule(Some(true), None), false);

        // Bold arrived last, so unified tracking calls this bold.
        assert!(
            rule_fired(&config, &both_attributes(false)),
            "bold = true fires where intensity = Bold would"
        );

        // Dim arrived last. The bold record is still set, so this is the
        // assertion that separates the derived value from the record.
        assert!(
            !rule_fired(&config, &both_attributes(true)),
            "dim arrived last, so unified tracking does not call this bold"
        );
    }

    #[test]
    fn the_conditions_read_the_records_under_separate_tracking() {
        let config = config_with_tracking(marked_rule(None, Some(true)), true);

        for (label, first, second) in [
            ("dim last", Intensity::Bold, Intensity::Half),
            ("bold last", Intensity::Half, Intensity::Bold),
        ] {
            let mut attrs = CellAttributes::default();
            attrs.apply_sgr_intensity(first);
            attrs.apply_sgr_intensity(second);
            assert!(
                rule_fired(&config, &attrs),
                "{label}: separate tracking reads the record, not the order"
            );
        }
    }

    #[test]
    #[should_panic(expected = "states `intensity`")]
    fn an_intensity_rule_under_separate_tracking_trips_the_guard() {
        let mut config = Config::default();
        config.track_bold_and_dim_separately = true;
        let mut rule = marked_rule(None, None);
        rule.intensity = Some(Intensity::Half);
        // Not run through `compute_extra_defaults`, which is what would remove
        // this rule.
        config.font_rules.push(rule);

        let _ = match_style(&config, &cell(Intensity::Half, false));
    }
}
