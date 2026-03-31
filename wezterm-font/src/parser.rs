use crate::locator::{FontDataHandle, FontDataSource, FontOrigin};
use crate::shaper::GlyphInfo;
use config::{FontAttributes, FontStyle, FreeTypeLoadFlags, FreeTypeLoadTarget};
pub use config::{FontStretch, FontWeight};
use rangeset::RangeSet;
use read_fonts::types::GlyphId16;
use read_fonts::types::NameId;
use read_fonts::TableProvider as ReadFontsTableProvider;
use skrifa::MetadataProvider;
use std::cmp::Ordering;
use std::sync::Mutex;

#[derive(Debug)]
pub enum MaybeShaped {
    Resolved(GlyphInfo),
    Unresolved { raw: String, slice_start: usize },
}

#[derive(Debug, Clone)]
pub struct FontPaletteInfo {
    pub name: String,
    pub palette_index: usize,
    pub usable_with_light_bg: bool,
    pub usable_with_dark_bg: bool,
}

/// Represents a parsed font
pub struct ParsedFont {
    names: Names,
    weight: FontWeight,
    stretch: FontStretch,
    style: FontStyle,
    cap_height: Option<f64>,
    pub handle: FontDataHandle,
    coverage: Mutex<RangeSet<u32>>,
    pub synthesize_italic: bool,
    pub synthesize_bold: bool,
    pub synthesize_dim: bool,
    pub assume_emoji_presentation: bool,
    pub pixel_sizes: Vec<u16>,
    pub is_built_in_fallback: bool,
    pub palettes: Vec<FontPaletteInfo>,

    pub harfbuzz_features: Option<Vec<String>>,
    pub freetype_load_target: Option<FreeTypeLoadTarget>,
    pub freetype_render_target: Option<FreeTypeLoadTarget>,
    pub freetype_load_flags: Option<FreeTypeLoadFlags>,
    pub scale: Option<f64>,
}

impl std::fmt::Debug for ParsedFont {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        fmt.debug_struct("ParsedFont")
            .field("names", &self.names)
            .field("weight", &self.weight)
            .field("stretch", &self.stretch)
            .field("style", &self.style)
            .field("handle", &self.handle)
            .field("cap_height", &self.cap_height)
            .field("synthesize_italic", &self.synthesize_italic)
            .field("synthesize_bold", &self.synthesize_bold)
            .field("synthesize_dim", &self.synthesize_dim)
            .field("assume_emoji_presentation", &self.assume_emoji_presentation)
            .field("pixel_sizes", &self.pixel_sizes)
            .field("harfbuzz_features", &self.harfbuzz_features)
            .field("freetype_load_target", &self.freetype_load_target)
            .field("freetype_render_target", &self.freetype_render_target)
            .field("freetype_load_flags", &self.freetype_load_flags)
            .field("scale", &self.scale)
            .finish()
    }
}

impl Clone for ParsedFont {
    fn clone(&self) -> Self {
        Self {
            names: self.names.clone(),
            weight: self.weight,
            stretch: self.stretch,
            style: self.style,
            synthesize_italic: self.synthesize_italic,
            synthesize_bold: self.synthesize_bold,
            synthesize_dim: self.synthesize_dim,
            assume_emoji_presentation: self.assume_emoji_presentation,
            handle: self.handle.clone(),
            cap_height: self.cap_height,
            coverage: Mutex::new(self.coverage.lock().unwrap().clone()),
            pixel_sizes: self.pixel_sizes.clone(),
            harfbuzz_features: self.harfbuzz_features.clone(),
            freetype_load_target: self.freetype_load_target,
            freetype_render_target: self.freetype_render_target,
            freetype_load_flags: self.freetype_load_flags,
            is_built_in_fallback: self.is_built_in_fallback,
            scale: self.scale,
            palettes: self.palettes.clone(),
        }
    }
}

impl Eq for ParsedFont {}

impl PartialEq for ParsedFont {
    fn eq(&self, rhs: &Self) -> bool {
        self.stretch == rhs.stretch
            && self.weight == rhs.weight
            && self.style == rhs.style
            && self.names == rhs.names
    }
}

impl Ord for ParsedFont {
    fn cmp(&self, rhs: &Self) -> Ordering {
        match self.names.family.cmp(&rhs.names.family) {
            o @ Ordering::Less | o @ Ordering::Greater => o,
            Ordering::Equal => match self.stretch.cmp(&rhs.stretch) {
                o @ Ordering::Less | o @ Ordering::Greater => o,
                Ordering::Equal => match self.weight.cmp(&rhs.weight) {
                    o @ Ordering::Less | o @ Ordering::Greater => o,
                    Ordering::Equal => match self.style.cmp(&rhs.style) {
                        o @ Ordering::Less | o @ Ordering::Greater => o,
                        Ordering::Equal => self.handle.cmp(&rhs.handle),
                    },
                },
            },
        }
    }
}

impl PartialOrd for ParsedFont {
    fn partial_cmp(&self, rhs: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(rhs))
    }
}

#[derive(Debug, PartialEq, Eq, Clone, PartialOrd, Ord)]
pub struct Names {
    pub full_name: String,
    pub family: String,
    pub sub_family: Option<String>,
    pub postscript_name: Option<String>,
    pub aliases: Vec<String>,
}

/// Helper: load font data from a FontDataHandle
fn load_font_data(handle: &FontDataHandle) -> anyhow::Result<Vec<u8>> {
    Ok(handle.source.load_data()?.into_owned())
}

/// Helper: create a skrifa FontRef from raw data and index
fn make_font_ref<'a>(data: &'a [u8], index: u32) -> anyhow::Result<skrifa::FontRef<'a>> {
    skrifa::FontRef::from_index(data, index)
        .map_err(|e| anyhow::anyhow!("Failed to load font at index {}: {}", index, e))
}

/// Count the number of font faces in font data (TTC collection or single font)
fn count_faces(data: &[u8]) -> u32 {
    let mut count = 0;
    while skrifa::FontRef::from_index(data, count).is_ok() {
        count += 1;
    }
    count
}

/// Find the best localized string for a set of name IDs.
/// Tries each ID in order, preferring English.
fn find_name(font_ref: &skrifa::FontRef<'_>, ids: &[NameId]) -> Option<String> {
    for &id in ids {
        // Prefer English
        for ls in font_ref.localized_strings(id) {
            if ls.language() == Some("en") {
                let text: String = ls.chars().collect();
                if !text.is_empty() {
                    return Some(text);
                }
            }
        }
        // Fall back to any language
        if let Some(ls) = font_ref.localized_strings(id).next() {
            let text: String = ls.chars().collect();
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

/// Find name by a specific NameId
fn find_name_by_string_id(font_ref: &skrifa::FontRef<'_>, id: NameId) -> Option<String> {
    // Prefer English
    for ls in font_ref.localized_strings(id) {
        if ls.language() == Some("en") {
            let text: String = ls.chars().collect();
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    // Fall back to any language
    if let Some(ls) = font_ref.localized_strings(id).next() {
        let text: String = ls.chars().collect();
        if !text.is_empty() {
            return Some(text);
        }
    }
    None
}

/// Collect all name strings for given IDs (for aliases)
fn collect_all_names(font_ref: &skrifa::FontRef<'_>, ids: &[NameId]) -> Vec<String> {
    let mut result = vec![];
    for &id in ids {
        for ls in font_ref.localized_strings(id) {
            let text: String = ls.chars().collect();
            if !text.is_empty() {
                result.push(text);
            }
        }
    }
    result.sort();
    result.dedup();
    result
}

/// Check if a font's table directory contains a given table tag.
/// Works for both standalone fonts and TrueType Collections.
fn has_table_tag(data: &[u8], index: u32, tag: [u8; 4]) -> bool {
    let tag_u32 = u32::from_be_bytes(tag);

    // Determine the offset of the table directory for this font index
    let dir_offset = if data.len() >= 12 && data[0..4] == *b"ttcf" {
        // TrueType Collection
        let num_fonts =
            u32::from_be_bytes([data[8], data[9], data[10], data[11]]) as usize;
        let idx = index as usize;
        if idx >= num_fonts {
            return false;
        }
        let off_pos = 12 + idx * 4;
        if off_pos + 4 > data.len() {
            return false;
        }
        u32::from_be_bytes([
            data[off_pos],
            data[off_pos + 1],
            data[off_pos + 2],
            data[off_pos + 3],
        ]) as usize
    } else {
        0
    };

    if dir_offset + 12 > data.len() {
        return false;
    }

    let num_tables =
        u16::from_be_bytes([data[dir_offset + 4], data[dir_offset + 5]]) as usize;
    let records_start = dir_offset + 12;

    for i in 0..num_tables {
        let rec_offset = records_start + i * 16;
        if rec_offset + 4 > data.len() {
            return false;
        }
        let rec_tag = u32::from_be_bytes([
            data[rec_offset],
            data[rec_offset + 1],
            data[rec_offset + 2],
            data[rec_offset + 3],
        ]);
        if rec_tag == tag_u32 {
            return true;
        }
    }
    false
}

/// Map a width percentage to OpenType width class (1-9).
fn stretch_pct_to_width_class(pct: u16) -> u16 {
    if pct <= 56 {
        1
    } else if pct <= 69 {
        2
    } else if pct <= 81 {
        3
    } else if pct <= 93 {
        4
    } else if pct <= 106 {
        5
    } else if pct <= 118 {
        6
    } else if pct <= 137 {
        7
    } else if pct <= 174 {
        8
    } else {
        9
    }
}

/// Map skrifa Stretch (percentage as f32) to OpenType width class (1-9).
fn skrifa_stretch_to_width_class(stretch: skrifa::attribute::Stretch) -> u16 {
    let pct = stretch.percentage() as u16;
    stretch_pct_to_width_class(pct)
}

/// Look up the name of a glyph by its position/ID.
/// Uses the 'post' table from the font via read-fonts.
pub fn get_glyph_name(handle: &FontDataHandle, glyph_pos: u32) -> Option<String> {
    let data = handle.source.load_data().ok()?;
    let font = read_fonts::FontRef::from_index(&data, handle.index).ok()?;
    let post = font.post().ok()?;
    post.glyph_name(GlyphId16::new(glyph_pos as u16))
        .map(|s| s.to_string())
}

/// Compute codepoint coverage from font charmap.
/// Uses charmap.mappings() to walk only mapped codepoints from the cmap table
/// rather than probing all ~1.1M Unicode codepoints individually.
pub(crate) fn compute_coverage(font_ref: &skrifa::FontRef<'_>) -> RangeSet<u32> {
    let charmap = font_ref.charmap();
    let mut coverage = RangeSet::new();
    for (cp, _glyph_id) in charmap.mappings() {
        coverage.add(cp);
    }
    coverage
}

impl Names {
    pub fn from_font_ref(font_ref: &skrifa::FontRef<'_>) -> Names {
        let family = find_name(
            font_ref,
            &[NameId::TYPOGRAPHIC_FAMILY_NAME, NameId::FAMILY_NAME],
        )
        .unwrap_or_default();

        let sub_family = find_name(
            font_ref,
            &[NameId::TYPOGRAPHIC_SUBFAMILY_NAME, NameId::SUBFAMILY_NAME],
        )
        .unwrap_or_default();

        let postscript_name =
            find_name(font_ref, &[NameId::POSTSCRIPT_NAME]).unwrap_or_default();

        let full_name = find_name(font_ref, &[NameId::FULL_NAME]).unwrap_or_else(|| {
            if sub_family.is_empty() {
                family.clone()
            } else {
                format!("{} {}", family, sub_family)
            }
        });

        let mut aliases =
            collect_all_names(font_ref, &[NameId::TYPOGRAPHIC_FAMILY_NAME, NameId::FAMILY_NAME]);
        aliases.retain(|n| *n != full_name && *n != family);

        Names {
            full_name,
            family,
            sub_family: Some(sub_family),
            postscript_name: Some(postscript_name),
            aliases,
        }
    }
}

impl ParsedFont {
    pub fn from_locator(handle: &FontDataHandle) -> anyhow::Result<Self> {
        let data = load_font_data(handle)?;
        let font_ref = make_font_ref(&data, handle.index)?;
        Self::from_font_ref(&font_ref, handle.clone(), &data)
    }

    pub fn aka(&self) -> String {
        if self.names.aliases.is_empty() {
            String::new()
        } else {
            format!("(AKA: {}) ", self.names.aliases.join(", "))
        }
    }

    pub fn lua_name(&self) -> String {
        format!(
            "wezterm.font(\"{}\", {{weight={}, stretch=\"{}\", style=\"{}\"}})",
            self.names.family, self.weight, self.stretch, self.style
        )
    }

    pub fn lua_fallback(handles: &[Self]) -> String {
        let mut code = "wezterm.font_with_fallback({\n".to_string();

        for p in handles {
            code.push_str(&format!("  -- {}\n", p.handle.diagnostic_string()));
            if p.synthesize_italic {
                code.push_str("  -- Will synthesize italics\n");
            }
            if p.synthesize_bold {
                code.push_str("  -- Will synthesize bold\n");
            } else if p.synthesize_dim {
                code.push_str("  -- Will synthesize dim\n");
            }
            if p.assume_emoji_presentation {
                code.push_str("  -- Assumed to have Emoji Presentation\n");
            }
            if !p.pixel_sizes.is_empty() {
                code.push_str(&format!("  -- Pixel sizes: {:?}\n", p.pixel_sizes));
            }
            if !p.palettes.is_empty() {
                for pal in &p.palettes {
                    let mut info = format!(
                        "  -- Palette: {} {}",
                        pal.palette_index,
                        pal.name.to_string()
                    );
                    if pal.usable_with_light_bg {
                        info.push_str(" (with light bg)");
                    }
                    if pal.usable_with_dark_bg {
                        info.push_str(" (with dark bg)");
                    }
                    info.push('\n');
                    code.push_str(&info);
                }
            }
            for aka in &p.names.aliases {
                code.push_str(&format!("  -- AKA: \"{}\"\n", aka));
            }

            if p.weight == FontWeight::REGULAR
                && p.stretch == FontStretch::Normal
                && p.style == FontStyle::Normal
                && p.freetype_render_target.is_none()
                && p.freetype_load_target.is_none()
                && p.freetype_load_flags.is_none()
                && p.harfbuzz_features.is_none()
                && p.scale.is_none()
            {
                code.push_str(&format!("  \"{}\",\n", p.names.family));
            } else {
                code.push_str(&format!("  {{family=\"{}\"", p.names.family));
                if p.weight != FontWeight::REGULAR {
                    code.push_str(&format!(", weight={}", p.weight));
                }
                if p.stretch != FontStretch::Normal {
                    code.push_str(&format!(", stretch=\"{}\"", p.stretch));
                }
                if p.style != FontStyle::Normal {
                    code.push_str(&format!(", style=\"{}\"", p.style));
                }
                if let Some(scale) = p.scale {
                    code.push_str(&format!(", scale={}", scale));
                }
                if let Some(item) = p.freetype_load_flags {
                    code.push_str(&format!(", freetype_load_flags=\"{}\"", item.to_string()));
                }
                if let Some(item) = p.freetype_load_target {
                    code.push_str(&format!(", freetype_load_target=\"{:?}\"", item));
                }
                if let Some(item) = p.freetype_render_target {
                    code.push_str(&format!(", freetype_render_target=\"{:?}\"", item));
                }
                if let Some(feat) = &p.harfbuzz_features {
                    code.push_str(", harfbuzz_features={");
                    for (idx, f) in feat.iter().enumerate() {
                        if idx > 0 {
                            code.push_str(", ");
                        }
                        code.push('"');
                        code.push_str(f);
                        code.push('"');
                    }
                    code.push('}');
                }
                code.push_str("},\n")
            }
            code.push_str("\n");
        }
        code.push_str("})");
        code
    }

    pub fn from_font_ref(
        font_ref: &skrifa::FontRef<'_>,
        handle: FontDataHandle,
        data: &[u8],
    ) -> anyhow::Result<Self> {
        let attrs = font_ref.attributes();

        // Style from skrifa attributes
        let style = match attrs.style {
            skrifa::attribute::Style::Normal => FontStyle::Normal,
            skrifa::attribute::Style::Italic => FontStyle::Italic,
            skrifa::attribute::Style::Oblique(_) => FontStyle::Oblique,
        };

        // Weight and width from skrifa attributes
        let ot_weight = attrs.weight.value() as u16;
        let weight = FontWeight::from_opentype_weight(ot_weight);
        let width = skrifa_stretch_to_width_class(attrs.stretch);
        let stretch = FontStretch::from_opentype_stretch(width);

        // Cap height from metrics
        let metrics = font_ref.metrics(skrifa::instance::Size::unscaled(), skrifa::instance::LocationRef::default());
        let cap_height = metrics.cap_height.and_then(|ch| {
            if ch > 0.0 {
                Some(ch as f64 / metrics.units_per_em as f64)
            } else {
                None
            }
        });

        // Pixel sizes from bitmap strikes (unified in skrifa)
        let mut pixel_sizes: Vec<u16> = vec![];
        for strike in font_ref.bitmap_strikes().iter() {
            let ppem = strike.ppem() as u16;
            if !pixel_sizes.contains(&ppem) {
                pixel_sizes.push(ppem);
            }
        }
        pixel_sizes.sort();
        pixel_sizes.dedup();

        // Palette data from CPAL table via read-fonts
        let palettes: Vec<FontPaletteInfo> = if let Ok(cpal) = font_ref.cpal() {
            let num_palettes = cpal.num_palettes();
            let labels = cpal.palette_labels_array();
            let types = cpal.palette_types_array();
            (0..num_palettes)
                .map(|idx| {
                    let name = labels
                        .as_ref()
                        .and_then(|arr| arr.as_ref().ok())
                        .and_then(|arr| arr.get(idx as usize))
                        .and_then(|nid| {
                            let nid = nid.get();
                            if nid != NameId::COPYRIGHT_NOTICE {
                                find_name_by_string_id(font_ref, nid)
                            } else {
                                None
                            }
                        })
                        .unwrap_or_else(|| format!("Palette {}", idx));
                    let (usable_with_light_bg, usable_with_dark_bg) = types
                        .as_ref()
                        .and_then(|arr| arr.as_ref().ok())
                        .and_then(|arr| arr.get(idx as usize))
                        .map(|flags| {
                            use read_fonts::tables::cpal::PaletteType;
                            let pt = flags.get();
                            (
                                pt.contains(PaletteType::USABLE_WITH_LIGHT_BACKGROUND),
                                pt.contains(PaletteType::USABLE_WITH_DARK_BACKGROUND),
                            )
                        })
                        .unwrap_or((false, false));
                    FontPaletteInfo {
                        name,
                        palette_index: idx as usize,
                        usable_with_light_bg,
                        usable_with_dark_bg,
                    }
                })
                .collect()
        } else {
            vec![]
        };

        // SVG table detection: check for "SVG " table tag in the font
        let has_svg = has_table_tag(data, handle.index, *b"SVG ");

        if has_svg && config::configuration().ignore_svg_fonts {
            anyhow::bail!("skipping svg font because ignore_svg_fonts=true");
        }

        let has_cpal = font_ref.cpal().is_ok();
        let has_color_bitmaps =
            has_table_tag(data, handle.index, *b"CBDT") || has_table_tag(data, handle.index, *b"sbix");
        let has_color = has_cpal || has_color_bitmaps;
        let assume_emoji_presentation = has_color;

        let names = Names::from_font_ref(font_ref);

        // Style refinement from name (same heuristic as before)
        let style = match style {
            FontStyle::Normal => {
                let lower = names.full_name.to_lowercase();
                if lower.contains("italic") || lower.contains("kursiv") {
                    FontStyle::Italic
                } else if lower.contains("oblique") {
                    FontStyle::Oblique
                } else {
                    FontStyle::Normal
                }
            }
            FontStyle::Italic => {
                let lower = names.full_name.to_lowercase();
                if lower.contains("oblique") {
                    FontStyle::Oblique
                } else {
                    FontStyle::Italic
                }
            }
            FontStyle::Oblique => FontStyle::Oblique,
        };

        // Weight refinement from name
        let weight = match weight {
            FontWeight::REGULAR => {
                let lower = names.full_name.to_lowercase();
                let mut weight = weight;
                for (label, candidate) in &[
                    ("extrablack", FontWeight::EXTRABLACK),
                    // must match after other black variants
                    ("black", FontWeight::BLACK),
                    ("extrabold", FontWeight::EXTRABOLD),
                    ("demibold", FontWeight::DEMIBOLD),
                    // must match after other bold variants
                    ("bold", FontWeight::BOLD),
                    ("medium", FontWeight::MEDIUM),
                    ("book", FontWeight::BOOK),
                    ("demilight", FontWeight::DEMILIGHT),
                    ("extralight", FontWeight::EXTRALIGHT),
                    // must match after other light variants
                    ("light", FontWeight::LIGHT),
                    ("thin", FontWeight::THIN),
                ] {
                    if lower.contains(label) {
                        weight = *candidate;
                        break;
                    }
                }
                weight
            }
            weight => weight,
        };

        // Stretch refinement from name
        let stretch = match stretch {
            FontStretch::Normal => {
                let lower = names.full_name.to_lowercase();
                let mut stretch = stretch;
                for (label, value) in &[
                    ("ultracondensed", FontStretch::UltraCondensed),
                    ("extracondensed", FontStretch::ExtraCondensed),
                    ("semicondensed", FontStretch::SemiCondensed),
                    // must match after other condensed variants
                    ("condensed", FontStretch::Condensed),
                    ("semiexpanded", FontStretch::SemiExpanded),
                    ("extraexpanded", FontStretch::ExtraExpanded),
                    ("ultraexpanded", FontStretch::UltraExpanded),
                    // must match after other expanded variants
                    ("expanded", FontStretch::Expanded),
                ] {
                    if lower.contains(label) {
                        stretch = *value;
                        break;
                    }
                }

                stretch
            }
            stretch => stretch,
        };

        let initial_coverage = handle.coverage.clone().unwrap_or_default();

        Ok(Self {
            names,
            weight,
            stretch,
            style,
            synthesize_italic: false,
            synthesize_bold: false,
            synthesize_dim: false,
            is_built_in_fallback: false,
            assume_emoji_presentation,
            handle,
            coverage: Mutex::new(initial_coverage),
            cap_height,
            pixel_sizes,
            harfbuzz_features: None,
            freetype_render_target: None,
            freetype_load_target: None,
            freetype_load_flags: None,
            scale: None,
            palettes,
        })
    }

    /// Computes the intersection of the wanted set of codepoints with
    /// the set of codepoints covered by this font entry.
    /// Computes the codepoint coverage for this font entry if we haven't
    /// already done so.
    pub fn coverage_intersection(&self, wanted: &RangeSet<u32>) -> anyhow::Result<RangeSet<u32>> {
        let mut cov = self.coverage.lock().unwrap();
        if cov.is_empty() {
            let t = std::time::Instant::now();
            let data = load_font_data(&self.handle)?;
            let font_ref = make_font_ref(&data, self.handle.index)?;
            *cov = compute_coverage(&font_ref);
            let elapsed = t.elapsed();
            metrics::histogram!("font.compute.codepoint.coverage").record(elapsed);
            log::debug!(
                "{} codepoint coverage computed in {:?}",
                self.names.full_name,
                elapsed
            );
        }
        Ok(wanted.intersection(&cov))
    }

    pub fn names(&self) -> &Names {
        &self.names
    }

    pub fn weight(&self) -> FontWeight {
        self.weight
    }

    pub fn stretch(&self) -> FontStretch {
        self.stretch
    }

    pub fn style(&self) -> FontStyle {
        self.style
    }

    pub fn matches_name(&self, attr: &FontAttributes) -> bool {
        if attr.family == self.names.family {
            return true;
        }
        if let Some(path) = self.handle.path_str() {
            if attr.family == path {
                return true;
            }
        }
        self.matches_full_or_ps_name(attr) || self.matches_alias(attr)
    }

    pub fn matches_alias(&self, attr: &FontAttributes) -> bool {
        for a in &self.names.aliases {
            if *a == attr.family {
                return true;
            }
        }
        false
    }

    pub fn matches_full_or_ps_name(&self, attr: &FontAttributes) -> bool {
        if attr.family == self.names.full_name {
            return true;
        }
        if let Some(ps) = self.names.postscript_name.as_ref() {
            if attr.family == *ps {
                return true;
            }
        }
        false
    }

    /// Perform CSS Fonts Level 3 font matching.
    /// This implementation is derived from the `find_best_match` function
    /// in the font-kit crate which is
    /// Copyright © 2018 The Pathfinder Project Developers.
    /// https://drafts.csswg.org/css-fonts-3/#font-style-matching says
    pub fn best_matching_index<P: std::ops::Deref<Target = Self> + std::fmt::Debug>(
        attr: &FontAttributes,
        fonts: &[P],
        pixel_size: u16,
    ) -> Option<usize> {
        if fonts.is_empty() {
            return None;
        }

        let mut candidates: Vec<usize> = (0..fonts.len()).collect();

        // First, filter by stretch
        let stretch_value = attr.stretch.to_opentype_stretch();
        let stretch = if candidates
            .iter()
            .any(|&idx| fonts[idx].stretch == attr.stretch)
        {
            attr.stretch
        } else if attr.stretch <= FontStretch::Normal {
            match candidates
                .iter()
                .filter(|&&idx| fonts[idx].stretch < attr.stretch)
                .min_by_key(|&&idx| stretch_value - fonts[idx].stretch.to_opentype_stretch())
            {
                Some(&idx) => fonts[idx].stretch,
                None => {
                    let idx = *candidates.iter().min_by_key(|&&idx| {
                        fonts[idx].stretch.to_opentype_stretch() - stretch_value
                    })?;
                    fonts[idx].stretch
                }
            }
        } else {
            match candidates
                .iter()
                .filter(|&&idx| fonts[idx].stretch > attr.stretch)
                .min_by_key(|&&idx| fonts[idx].stretch.to_opentype_stretch() - stretch_value)
            {
                Some(&idx) => fonts[idx].stretch,
                None => {
                    let idx = *candidates.iter().min_by_key(|&&idx| {
                        stretch_value - fonts[idx].stretch.to_opentype_stretch()
                    })?;
                    fonts[idx].stretch
                }
            }
        };

        // Reduce to matching stretches
        candidates.retain(|&idx| fonts[idx].stretch == stretch);

        // Now match style: italics.
        let styles = match attr.style {
            FontStyle::Normal => [FontStyle::Normal, FontStyle::Italic, FontStyle::Oblique],
            FontStyle::Italic => [FontStyle::Italic, FontStyle::Oblique, FontStyle::Normal],
            FontStyle::Oblique => [FontStyle::Oblique, FontStyle::Italic, FontStyle::Normal],
        };
        let style = *styles
            .iter()
            .find(|&&style| candidates.iter().any(|&idx| fonts[idx].style == style))?;

        // Reduce to matching italics
        candidates.retain(|&idx| fonts[idx].style == style);

        // And now match by font weight
        let query_weight = attr.weight.to_opentype_weight();
        let weight = if candidates
            .iter()
            .any(|&idx| fonts[idx].weight == attr.weight)
        {
            // Exact match for the requested weight
            attr.weight
        } else if attr.weight == FontWeight::REGULAR
            && candidates
                .iter()
                .any(|&idx| fonts[idx].weight == FontWeight::MEDIUM)
        {
            FontWeight::MEDIUM
        } else if attr.weight == FontWeight::MEDIUM
            && candidates
                .iter()
                .any(|&idx| fonts[idx].weight == FontWeight::REGULAR)
        {
            FontWeight::REGULAR
        } else if attr.weight <= FontWeight::MEDIUM {
            match candidates
                .iter()
                .filter(|&&idx| fonts[idx].weight <= attr.weight)
                .min_by_key(|&&idx| query_weight - fonts[idx].weight.to_opentype_weight())
            {
                Some(&idx) => fonts[idx].weight,
                None => {
                    let idx = *candidates.iter().min_by_key(|&&idx| {
                        fonts[idx].weight.to_opentype_weight() - query_weight
                    })?;
                    fonts[idx].weight
                }
            }
        } else {
            match candidates
                .iter()
                .filter(|&&idx| fonts[idx].weight >= attr.weight)
                .min_by_key(|&&idx| fonts[idx].weight.to_opentype_weight() - query_weight)
            {
                Some(&idx) => fonts[idx].weight,
                None => {
                    let idx = *candidates.iter().min_by_key(|&&idx| {
                        query_weight - fonts[idx].weight.to_opentype_weight()
                    })?;
                    fonts[idx].weight
                }
            }
        };

        // Reduce to matching weight
        candidates.retain(|&idx| fonts[idx].weight == weight);

        // Check for best matching pixel strike
        if candidates
            .iter()
            .all(|&idx| !fonts[idx].pixel_sizes.is_empty())
        {
            if let Some((_distance, idx)) = candidates
                .iter()
                .map(|&idx| {
                    let distance = fonts[idx]
                        .pixel_sizes
                        .iter()
                        .map(|&size| ((pixel_size as i32) - (size as i32)).abs())
                        .min()
                        .unwrap_or(i32::MAX);
                    (distance, idx)
                })
                .min()
            {
                return Some(idx);
            }
        }

        candidates.into_iter().next()
    }

    pub fn best_match(
        attr: &FontAttributes,
        pixel_size: u16,
        mut fonts: Vec<Self>,
    ) -> Option<Self> {
        let refs: Vec<&Self> = fonts.iter().collect();
        let idx = Self::best_matching_index(attr, &refs, pixel_size)?;
        fonts.drain(idx..=idx).next().map(|p| p.synthesize(attr))
    }

    /// Update self to reflect whether the rasterizer might need to synthesize
    /// italic for this font.
    pub fn synthesize(mut self, attr: &FontAttributes) -> Self {
        self.harfbuzz_features = attr.harfbuzz_features.clone();
        self.freetype_render_target = attr.freetype_render_target;
        self.freetype_load_target = attr.freetype_load_target;
        self.freetype_load_flags = attr.freetype_load_flags;
        self.scale = attr.scale.map(|f| *f);

        self.synthesize_italic = self.style == FontStyle::Normal && attr.style != FontStyle::Normal;
        self.synthesize_bold = attr.weight >= FontWeight::DEMIBOLD
            && attr.weight > self.weight
            && self.weight <= FontWeight::REGULAR;
        self.synthesize_dim = attr.weight < FontWeight::REGULAR
            && attr.weight < self.weight
            && self.weight >= FontWeight::REGULAR;

        match attr.assume_emoji_presentation {
            Some(assume) => {
                self.assume_emoji_presentation = assume;
            }
            None => {
                if !self.is_built_in_fallback
                    && !attr.is_synthetic
                    && self.names.full_name.to_lowercase().contains("moji")
                {
                    self.assume_emoji_presentation = true;
                }
            }
        }

        self
    }
}

/// In case the user has a broken configuration, or no configuration,
/// we bundle JetBrains Mono and Noto Color Emoji to act as reasonably
/// sane fallback fonts.
/// This function loads those.
pub(crate) fn load_built_in_fonts(font_info: &mut Vec<ParsedFont>) -> anyhow::Result<()> {
    #[allow(unused_macros)]
    macro_rules! font {
        ($font:literal) => {
            (include_bytes!($font) as &'static [u8], $font)
        };
    }

    let built_ins: &[&[(&[u8], &str)]] = &[
        #[cfg(any(test, feature = "vendor-jetbrains"))]
        &[
            font!("../../assets/fonts/JetBrainsMono-BoldItalic.ttf"),
            font!("../../assets/fonts/JetBrainsMono-Bold.ttf"),
            font!("../../assets/fonts/JetBrainsMono-ExtraBoldItalic.ttf"),
            font!("../../assets/fonts/JetBrainsMono-ExtraBold.ttf"),
            font!("../../assets/fonts/JetBrainsMono-ExtraLightItalic.ttf"),
            font!("../../assets/fonts/JetBrainsMono-ExtraLight.ttf"),
            font!("../../assets/fonts/JetBrainsMono-Italic.ttf"),
            font!("../../assets/fonts/JetBrainsMono-LightItalic.ttf"),
            font!("../../assets/fonts/JetBrainsMono-Light.ttf"),
            font!("../../assets/fonts/JetBrainsMono-MediumItalic.ttf"),
            font!("../../assets/fonts/JetBrainsMono-Medium.ttf"),
            font!("../../assets/fonts/JetBrainsMono-Regular.ttf"),
            font!("../../assets/fonts/JetBrainsMono-SemiBoldItalic.ttf"),
            font!("../../assets/fonts/JetBrainsMono-SemiBold.ttf"),
            font!("../../assets/fonts/JetBrainsMono-ThinItalic.ttf"),
            font!("../../assets/fonts/JetBrainsMono-Thin.ttf"),
        ],
        #[cfg(any(test, feature = "vendor-roboto"))]
        &[
            font!("../../assets/fonts/Roboto-Black.ttf"),
            font!("../../assets/fonts/Roboto-BlackItalic.ttf"),
            font!("../../assets/fonts/Roboto-Bold.ttf"),
            font!("../../assets/fonts/Roboto-BoldItalic.ttf"),
            font!("../../assets/fonts/Roboto-Italic.ttf"),
            font!("../../assets/fonts/Roboto-Light.ttf"),
            font!("../../assets/fonts/Roboto-LightItalic.ttf"),
            font!("../../assets/fonts/Roboto-Medium.ttf"),
            font!("../../assets/fonts/Roboto-MediumItalic.ttf"),
            font!("../../assets/fonts/Roboto-Regular.ttf"),
            font!("../../assets/fonts/Roboto-Thin.ttf"),
            font!("../../assets/fonts/Roboto-ThinItalic.ttf"),
        ],
        #[cfg(any(test, feature = "vendor-noto-emoji"))]
        &[font!("../../assets/fonts/NotoColorEmoji.ttf")],
        #[cfg(any(test, feature = "vendor-nerd-font-symbols"))]
        &[font!("../../assets/fonts/SymbolsNerdFontMono-Regular.ttf")],
    ];
    for bundle in built_ins {
        for (data, name) in bundle.iter() {
            let locator = FontDataHandle {
                source: FontDataSource::BuiltIn { data, name },
                index: 0,
                variation: 0,
                origin: FontOrigin::BuiltIn,
                coverage: None,
            };
            let font_ref = skrifa::FontRef::from_index(data, 0)
                .map_err(|e| anyhow::anyhow!("Failed to load built-in font {}: {}", name, e))?;
            let mut parsed = ParsedFont::from_font_ref(&font_ref, locator, data)?;
            parsed.is_built_in_fallback = true;
            font_info.push(parsed);
        }
    }

    Ok(())
}

pub fn best_matching_font(
    source: &FontDataSource,
    font_attr: &FontAttributes,
    origin: FontOrigin,
    pixel_size: u16,
) -> anyhow::Result<Option<ParsedFont>> {
    let mut font_info = vec![];
    parse_and_collect_font_info(source, &mut font_info, origin)?;
    font_info.retain(|font| font.matches_name(font_attr));
    Ok(ParsedFont::best_match(font_attr, pixel_size, font_info))
}

pub(crate) fn parse_and_collect_font_info(
    source: &FontDataSource,
    font_info: &mut Vec<ParsedFont>,
    origin: FontOrigin,
) -> anyhow::Result<()> {
    let data = source.load_data()?;
    let num_faces = count_faces(&data);

    fn load_one(
        data: &[u8],
        source: &FontDataSource,
        index: u32,
        font_info: &mut Vec<ParsedFont>,
        origin: &FontOrigin,
    ) -> anyhow::Result<()> {
        let font_ref = make_font_ref(data, index)?;

        // Check for named instances (variable fonts)
        let instances = font_ref.named_instances();
        if instances.len() > 0 {
            let axes: Vec<_> = font_ref.axes().iter().collect();
            for var_idx in 0..instances.len() {
                let instance = match instances.get(var_idx) {
                    Some(inst) => inst,
                    None => continue,
                };
                let var_handle = FontDataHandle {
                    source: source.clone(),
                    index,
                    variation: (var_idx + 1) as u32,
                    origin: origin.clone(),
                    coverage: None,
                };
                match ParsedFont::from_font_ref(&font_ref, var_handle, data) {
                    Ok(mut parsed) => {
                        // Override weight/style from instance axis values.
                        for (i, val) in instance.user_coords().enumerate() {
                            if i >= axes.len() {
                                break;
                            }
                            let tag_bytes = axes[i].tag().to_be_bytes();
                            if &tag_bytes == b"wght" {
                                parsed.weight =
                                    FontWeight::from_opentype_weight(val as u16);
                            } else if &tag_bytes == b"wdth" {
                                let width_class =
                                    stretch_pct_to_width_class(val as u16);
                                parsed.stretch =
                                    FontStretch::from_opentype_stretch(width_class);
                            } else if &tag_bytes == b"ital" {
                                if val > 0.5 {
                                    parsed.style = FontStyle::Italic;
                                }
                            } else if &tag_bytes == b"slnt" {
                                if val.abs() > 0.5 {
                                    parsed.style = FontStyle::Oblique;
                                }
                            }
                        }
                        // Update names from instance subfamily
                        let name_id = instance.subfamily_name_id();
                        if let Some(instance_name) = find_name_by_string_id(&font_ref, name_id) {
                            if !instance_name.is_empty() {
                                parsed.names.full_name =
                                    format!("{} {}", parsed.names.family, instance_name);
                                parsed.names.sub_family = Some(instance_name);
                            }
                        }
                        font_info.push(parsed);
                    }
                    Err(err) => {
                        log::trace!("error parsing variation {}: {}", var_idx, err);
                    }
                }
            }
        } else {
            let locator = FontDataHandle {
                source: source.clone(),
                index,
                variation: 0,
                origin: origin.clone(),
                coverage: None,
            };
            let parsed = ParsedFont::from_font_ref(&font_ref, locator, data)?;
            font_info.push(parsed);
        }
        Ok(())
    }

    for index in 0..num_faces {
        if let Err(err) = load_one(&data, &source, index, font_info, &origin) {
            log::trace!("error while parsing {:?} index {}: {}", source, index, err);
        }
    }

    Ok(())
}
