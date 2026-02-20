//! Text shaper using the harfrust crate (pure-Rust HarfBuzz port).
//! Replaces shaper/harfbuzz.rs.

use crate::parser::ParsedFont;
use crate::shaper::{FallbackIdx, FontMetrics, FontShaper, GlyphInfo, PresentationWidth};
use crate::units::*;
use anyhow::anyhow;
use config::ConfigHandle;
use finl_unicode::grapheme_clusters::Graphemes;
use log::error;
use ordered_float::NotNan;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use skrifa::MetadataProvider;
use termwiz::cell::{unicode_column_width, Presentation};
use wezterm_bidi::Direction;

#[derive(Clone, Debug)]
struct Info {
    cluster: usize,
    len: usize,
    codepoint: u32,
    x_advance: i32,
    y_advance: i32,
    x_offset: i32,
    y_offset: i32,
}

fn get_only_char(s: &str) -> Option<char> {
    let mut chars = s.chars();
    let first_char = chars.next()?;
    if chars.next().is_some() {
        None
    } else {
        Some(first_char)
    }
}

fn make_glyphinfo(text: &str, num_cells: u8, font_idx: usize, info: &Info) -> GlyphInfo {
    let is_space = text == " ";
    let only_char = get_only_char(text);
    GlyphInfo {
        #[cfg(any(debug_assertions, test))]
        text: text.into(),
        only_char,
        is_space,
        num_cells,
        font_idx,
        glyph_pos: info.codepoint,
        cluster: info.cluster as u32,
        x_advance: PixelLength::new(f64::from(info.x_advance) / 64.0),
        y_advance: PixelLength::new(f64::from(info.y_advance) / 64.0),
        x_offset: PixelLength::new(f64::from(info.x_offset) / 64.0),
        y_offset: PixelLength::new(f64::from(info.y_offset) / 64.0),
    }
}

struct HarfrustFontPair {
    font_data: Arc<Vec<u8>>,
    font_index: u32,
    shaper_data: harfrust::ShaperData,
    shaped_any: bool,
    presentation: Presentation,
    features: Vec<harfrust::Feature>,
    scale: f64,
    units_per_em: u16,
}

impl HarfrustFontPair {
    fn harfrust_font_ref(&self) -> Result<harfrust::FontRef<'_>, anyhow::Error> {
        harfrust::FontRef::from_index(&self.font_data, self.font_index)
            .map_err(|e| anyhow::anyhow!("harfrust font ref error: {e:?}"))
    }

    fn skrifa_font_ref(&self) -> Option<skrifa::FontRef<'_>> {
        skrifa::FontRef::from_index(&self.font_data, self.font_index).ok()
    }
}

#[derive(Debug, Hash, PartialEq, Eq, Clone)]
struct MetricsKey {
    font_idx: usize,
    size: NotNan<f64>,
    dpi: u32,
}

pub struct HarfrustShaper {
    handles: Vec<ParsedFont>,
    fonts: Vec<RefCell<Option<HarfrustFontPair>>>,
    metrics: RefCell<HashMap<MetricsKey, FontMetrics>>,
    features: Vec<harfrust::Feature>,
    lang: harfrust::Language,
}

fn make_question_string(s: &str) -> String {
    let len = Graphemes::new(s).count();
    let c = if !is_question_string(s) {
        std::char::REPLACEMENT_CHARACTER
    } else {
        '?'
    };
    std::iter::repeat(c).take(len).collect()
}

fn is_question_string(s: &str) -> bool {
    s.chars().all(|c| c == std::char::REPLACEMENT_CHARACTER)
}

impl HarfrustShaper {
    pub fn new(config: &ConfigHandle, handles: &[ParsedFont]) -> anyhow::Result<Self> {
        let handles = handles.to_vec();
        let fonts: Vec<_> = (0..handles.len()).map(|_| RefCell::new(None)).collect();

        let lang: harfrust::Language = "en"
            .parse()
            .map_err(|_| anyhow!("failed to parse language 'en'"))?;

        let features: Vec<harfrust::Feature> = config
            .harfbuzz_features
            .iter()
            .filter_map(|s| s.parse().ok())
            .collect();

        Ok(Self {
            fonts,
            handles,
            metrics: RefCell::new(HashMap::new()),
            features,
            lang,
        })
    }

    fn load_fallback(
        &self,
        font_idx: FallbackIdx,
    ) -> anyhow::Result<Option<std::cell::RefMut<'_, HarfrustFontPair>>> {
        if font_idx >= self.handles.len() {
            return Ok(None);
        }
        match self.fonts.get(font_idx) {
            None => Ok(None),
            Some(opt_pair) => {
                let mut opt_pair = opt_pair.borrow_mut();
                if opt_pair.is_none() {
                    let handle = &self.handles[font_idx];
                    log::trace!("harfrust shaper wants {} {:?}", font_idx, handle);

                    let data = handle.handle.source.load_data()?;
                    let font_data = Arc::new(data.into_owned());

                    let font_ref =
                        harfrust::FontRef::from_index(&font_data, handle.handle.index)
                            .map_err(|e| {
                                anyhow::anyhow!("failed to load font for shaping: {e:?}")
                            })?;

                    let units_per_em = {
                        use read_fonts::TableProvider;
                        font_ref
                            .head()
                            .map(|h: read_fonts::tables::head::Head| h.units_per_em())
                            .unwrap_or(1000)
                    };

                    let shaper_data = harfrust::ShaperData::new(&font_ref);

                    let features = match &handle.harfbuzz_features {
                        Some(features) => features
                            .iter()
                            .filter_map(|s| s.parse().ok())
                            .collect(),
                        None => self.features.clone(),
                    };

                    *opt_pair = Some(HarfrustFontPair {
                        font_data,
                        font_index: handle.handle.index,
                        shaper_data,
                        shaped_any: false,
                        presentation: if handle.assume_emoji_presentation {
                            Presentation::Emoji
                        } else {
                            Presentation::Text
                        },
                        features,
                        scale: handle.scale.unwrap_or(1.0),
                        units_per_em,
                    });
                }

                Ok(Some(std::cell::RefMut::map(opt_pair, |opt_pair| {
                    opt_pair.as_mut().unwrap()
                })))
            }
        }
    }

    fn do_shape(
        &self,
        mut font_idx: FallbackIdx,
        s: &str,
        font_size: f64,
        dpi: u32,
        no_glyphs: &mut Vec<char>,
        presentation: Option<Presentation>,
        direction: Direction,
        range: Range<usize>,
        presentation_width: Option<&PresentationWidth>,
    ) -> anyhow::Result<Vec<GlyphInfo>> {
        // We set this to true when we've run out of fallback fonts to try.
        // In that case, we accept shaper info with codepoint==0 and
        // will use the notdef glyph from the base font.
        let mut no_more_fallbacks = false;
        let shaped_any;

        loop {
            match self.load_fallback(font_idx)? {
                Some(pair) => {
                    if let Some(p) = presentation {
                        if pair.presentation != p {
                            log::trace!(
                                "wanted presentation {p:?} != font presentation {:?}, skip idx {font_idx}",
                                pair.presentation
                            );
                            font_idx += 1;
                            continue;
                        }
                    }

                    let font_ref = match pair.harfrust_font_ref() {
                        Ok(f) => f,
                        Err(_) => {
                            font_idx += 1;
                            continue;
                        }
                    };

                    let pixel_size = font_size * pair.scale * dpi as f64 / 72.0;
                    // harfrust uses font units internally; we scale the output
                    let ppem_scale = pixel_size / pair.units_per_em as f64;

                    let shaper = pair
                        .shaper_data
                        .shaper(&font_ref)
                        .point_size(Some(font_size as f32))
                        .build();

                    let mut buffer = harfrust::UnicodeBuffer::new();
                    buffer.set_direction(match direction {
                        Direction::LeftToRight => harfrust::Direction::LeftToRight,
                        Direction::RightToLeft => harfrust::Direction::RightToLeft,
                    });
                    buffer.set_language(self.lang.clone());

                    // Add text with byte-indexed clusters
                    let substr = &s[range.clone()];
                    for (byte_idx, ch) in substr.char_indices() {
                        buffer.add(ch, (range.start + byte_idx) as u32);
                    }
                    buffer.guess_segment_properties();
                    buffer.set_cluster_level(
                        harfrust::BufferClusterLevel::MonotoneGraphemes,
                    );

                    let glyph_buf = shaper.shape(buffer, &pair.features);

                    let infos = glyph_buf.glyph_infos();
                    let positions = glyph_buf.glyph_positions();

                    // Scale from font units to 26.6 fixed point pixels
                    let scale_26_6 = ppem_scale * 64.0;

                    shaped_any = pair.shaped_any;
                    drop(pair);

                    // Build cluster-resolved info list with byte lengths
                    let mut cluster_resolver = ClusterResolver {
                        presentation_width,
                        ..Default::default()
                    };
                    cluster_resolver.build(infos, s, &range);

                    // Group glyph infos by cluster, tracking incomplete clusters
                    let mut info_clusters: Vec<Vec<Info>> = Vec::with_capacity(s.len());

                    let info_iter = infos.iter().zip(positions.iter());
                    for (glyph_info, pos) in info_iter {
                        let cluster_info =
                            match cluster_resolver.get_mut(glyph_info.cluster as usize) {
                                Some(i) => i,
                                None => {
                                    log::warn!(
                                        "unexpected cluster {} not in resolver",
                                        glyph_info.cluster
                                    );
                                    continue;
                                }
                            };
                        let len = cluster_info.byte_len;

                        let mut info = Info {
                            cluster: cluster_info.start,
                            len,
                            codepoint: glyph_info.glyph_id,
                            x_advance: (pos.x_advance as f64 * scale_26_6) as i32,
                            y_advance: (pos.y_advance as f64 * scale_26_6) as i32,
                            x_offset: (pos.x_offset as f64 * scale_26_6) as i32,
                            y_offset: (pos.y_offset as f64 * scale_26_6) as i32,
                        };

                        if info.codepoint == 0 && !no_more_fallbacks {
                            cluster_info.incomplete = true;
                        }

                        if let Some(ref mut cluster) = info_clusters.last_mut() {
                            // Don't fragment runs of unresolved codepoints; they could
                            // be a sequence that shapes together in a fallback font.
                            if info.codepoint == 0 && !no_more_fallbacks {
                                let prior = cluster.last_mut().unwrap();
                                if prior.codepoint == 0 || prior.cluster == info.cluster {
                                    if prior.cluster + prior.len == info.cluster {
                                        // Coalesce with prior
                                        prior.len += info.len;
                                        continue;
                                    } else if info.cluster + info.len == prior.cluster {
                                        // We precede prior; re-arrange and coalesce
                                        std::mem::swap(&mut info, prior);
                                        prior.len += info.len;
                                        continue;
                                    } else if info.cluster + info.len
                                        == prior.cluster + prior.len
                                    {
                                        // Overlaps and coincides with end of prior
                                        continue;
                                    }
                                }
                            }

                            if cluster.last().unwrap().cluster == info.cluster {
                                cluster.push(info);
                                continue;
                            }
                        }
                        info_clusters.push(vec![info]);
                    }

                    // Now process each cluster group
                    let mut cluster = Vec::with_capacity(s.len());
                    let mut direct_clusters = 0;

                    for infos_group in &info_clusters {
                        let cluster_info = cluster_resolver
                            .get(infos_group[0].cluster)
                            .expect("assigned above");
                        let sub_range =
                            cluster_info.start..cluster_info.start + cluster_info.byte_len;
                        let substr = &s[sub_range.clone()];

                        if cluster_info.incomplete {
                            // One or more entries didn't have a corresponding glyph,
                            // so try a fallback
                            let first_info = &infos_group[0];

                            let mut shape = match self.do_shape(
                                font_idx + 1,
                                s,
                                font_size,
                                dpi,
                                no_glyphs,
                                presentation,
                                direction,
                                first_info.cluster..first_info.cluster + first_info.len,
                                presentation_width,
                            ) {
                                Ok(shape) => Ok(shape),
                                Err(e) => {
                                    error!("{:?} for {:?}", e, substr);
                                    self.do_shape(
                                        0,
                                        &make_question_string(substr),
                                        font_size,
                                        dpi,
                                        no_glyphs,
                                        presentation,
                                        direction,
                                        sub_range,
                                        presentation_width,
                                    )
                                }
                            }?;

                            cluster.append(&mut shape);
                            continue;
                        }

                        let total_width: f64 =
                            infos_group.iter().map(|info| info.x_advance as f64).sum();
                        let mut remaining_cells = cluster_info.cell_width;

                        for info in infos_group.iter() {
                            let weighted_cell_width = if total_width == 0. {
                                1
                            } else {
                                (cluster_info.cell_width as f64 * info.x_advance as f64
                                    / total_width)
                                    .ceil() as u8
                            };
                            let weighted_cell_width = weighted_cell_width.min(remaining_cells);
                            remaining_cells =
                                remaining_cells.saturating_sub(weighted_cell_width);

                            let glyph =
                                make_glyphinfo(substr, weighted_cell_width, font_idx, info);
                            cluster.push(glyph);
                            direct_clusters += 1;
                        }
                    }

                    if !shaped_any {
                        if let Some(opt_pair) = self.fonts.get(font_idx) {
                            if direct_clusters == 0 {
                                log::trace!(
                                    "Shaper didn't resolve glyphs from {:?}, so unload it",
                                    self.handles[font_idx]
                                );
                                opt_pair.borrow_mut().take();
                            } else if let Some(pair) = &mut *opt_pair.borrow_mut() {
                                pair.shaped_any = true;
                            }
                        }
                    }

                    return Ok(cluster);
                }
                None => {
                    for c in s[range.clone()].chars() {
                        no_glyphs.push(c);
                    }

                    if presentation.is_some() {
                        log::debug!(
                            "Ran out of fallback options, retry shape with no presentation"
                        );
                        return self.do_shape(
                            0,
                            s,
                            font_size,
                            dpi,
                            no_glyphs,
                            None,
                            direction,
                            range,
                            presentation_width,
                        );
                    }

                    // One more go around to pick up the base font and
                    // accept using the notdef glyph from that.
                    no_more_fallbacks = true;
                    font_idx = 0;
                    continue;
                }
            }
        }
    }
}

#[derive(Debug)]
struct ClusterInfo {
    start: usize,
    byte_len: usize,
    cell_width: u8,
    incomplete: bool,
}

#[derive(Default, Debug)]
struct ClusterResolver<'a> {
    map: HashMap<usize, ClusterInfo>,
    presentation_width: Option<&'a PresentationWidth<'a>>,
    start_by_cell_idx: HashMap<usize, usize>,
}

impl<'a> ClusterResolver<'a> {
    pub fn build(
        &mut self,
        hb_infos: &[harfrust::GlyphInfo],
        s: &str,
        range: &Range<usize>,
    ) {
        #[derive(PartialOrd, Ord, Eq, PartialEq, Copy, Clone)]
        struct Item {
            cell_idx: Option<usize>,
            start: usize,
        }

        let mut map = HashMap::new();

        for info in hb_infos.iter() {
            let start = info.cluster as usize;

            let cell_idx = match self.presentation_width {
                Some(pw) => {
                    let cell_idx = pw.byte_to_cell_idx(start);
                    let entry = self.start_by_cell_idx.entry(cell_idx).or_insert(start);
                    *entry = (*entry).min(start);
                    Some(cell_idx)
                }
                None => None,
            };

            map.entry(start).or_insert_with(|| Item { start, cell_idx });
        }

        let mut cluster_starts: Vec<Item> = map.into_values().collect();
        cluster_starts.sort();

        // If we have multiple entries with the same starting cell index,
        // remove the duplicates.
        cluster_starts.dedup_by(|a, b| match (a.cell_idx, b.cell_idx) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        });

        let mut iter = cluster_starts.iter().peekable();
        while let Some(item) = iter.next().copied() {
            let start = item.start;
            let next_start = iter.peek().map(|&&s| s.start).unwrap_or(range.end);
            let byte_len = next_start - start;
            let cell_width = match self.presentation_width {
                Some(p) => p.num_cells(start..next_start),
                None => unicode_column_width(&s[start..next_start], None) as u8,
            };
            self.map.entry(start).or_insert_with(|| ClusterInfo {
                start,
                byte_len,
                cell_width,
                incomplete: false,
            });
        }
    }

    pub fn get_mut(&mut self, start: usize) -> Option<&mut ClusterInfo> {
        match self.presentation_width {
            Some(pw) => {
                let cell_idx = pw.byte_to_cell_idx(start);
                let actual_start = self.start_by_cell_idx.get(&cell_idx)?;
                self.map.get_mut(actual_start)
            }
            None => self.map.get_mut(&start),
        }
    }

    pub fn get(&self, start: usize) -> Option<&ClusterInfo> {
        match self.presentation_width {
            Some(pw) => {
                let cell_idx = pw.byte_to_cell_idx(start);
                let actual_start = self.start_by_cell_idx.get(&cell_idx)?;
                self.map.get(actual_start)
            }
            None => self.map.get(&start),
        }
    }
}

impl FontShaper for HarfrustShaper {
    fn shape(
        &self,
        text: &str,
        size: f64,
        dpi: u32,
        no_glyphs: &mut Vec<char>,
        presentation: Option<Presentation>,
        direction: Direction,
        range: Option<Range<usize>>,
        presentation_width: Option<&PresentationWidth>,
    ) -> anyhow::Result<Vec<GlyphInfo>> {
        let range = range.unwrap_or(0..text.len());
        let start = std::time::Instant::now();

        let result = self.do_shape(
            0,
            text,
            size,
            dpi,
            no_glyphs,
            presentation,
            direction,
            range,
            presentation_width,
        );

        if let Err(err) = &result {
            error!("harfrust shaping error: {:#}", err);
        }

        metrics::histogram!("shape.harfrust").record(start.elapsed());
        result
    }

    fn metrics_for_idx(
        &self,
        font_idx: usize,
        size: f64,
        dpi: u32,
    ) -> anyhow::Result<FontMetrics> {
        let pair = self
            .load_fallback(font_idx)?
            .ok_or_else(|| anyhow!("metrics_for_idx: no font with idx={font_idx}"))?;

        let key = MetricsKey {
            font_idx,
            size: NotNan::new(size).unwrap(),
            dpi,
        };
        if let Some(metrics) = self.metrics.borrow().get(&key) {
            return Ok(*metrics);
        }

        let scale = self.handles[font_idx].scale.unwrap_or(1.0);
        let pixel_size = size * scale * dpi as f64 / 72.0;

        // Use skrifa for metrics
        let skrifa_ref = pair
            .skrifa_font_ref()
            .ok_or_else(|| anyhow!("failed to get skrifa font ref for metrics"))?;

        let skrifa_metrics = skrifa_ref.metrics(
            skrifa::instance::Size::unscaled(),
            skrifa::instance::LocationRef::default(),
        );
        let upem = skrifa_metrics.units_per_em as f64;
        let scale_factor = pixel_size / upem;

        // Note: skrifa reports descent as negative (OpenType convention)
        let ascent = skrifa_metrics.ascent as f64 * scale_factor;
        let descent = (-skrifa_metrics.descent as f64) * scale_factor;
        let leading = skrifa_metrics.leading as f64 * scale_factor;
        let cell_height = ascent + descent + leading;

        // Compute cell width
        let cell_width = if let Some(avg_width) = skrifa_metrics.average_width {
            if avg_width > 0.0 {
                avg_width as f64 * scale_factor
            } else {
                pixel_size * 0.5
            }
        } else {
            let cmap = skrifa_ref.charmap();
            if let Some(space_gid) = cmap.map(' ') {
                let gm = skrifa_ref.glyph_metrics(
                    skrifa::instance::Size::new(pixel_size as f32),
                    skrifa::instance::LocationRef::default(),
                );
                gm.advance_width(space_gid).map(|w| w as f64).unwrap_or(pixel_size * 0.5)
            } else {
                pixel_size * 0.5
            }
        };

        let (underline_thickness, underline_position) =
            if let Some(ref ul) = skrifa_metrics.underline {
                (
                    (ul.thickness as f64 * scale_factor).max(1.0),
                    ul.offset as f64 * scale_factor,
                )
            } else {
                (1.0, 0.0)
            };

        let cap_height = skrifa_metrics.cap_height.and_then(|ch| {
            if ch > 0.0 {
                Some(ch as f64 * scale_factor)
            } else {
                None
            }
        });
        let cap_height_ratio = skrifa_metrics.cap_height.and_then(|ch| {
            if ch > 0.0 && upem > 0.0 {
                Some(ch as f64 / upem)
            } else {
                None
            }
        });

        let mut metrics = FontMetrics {
            cell_width: PixelLength::new(cell_width),
            cell_height: PixelLength::new(cell_height),
            descender: PixelLength::new(-descent), // descent is positive here (already negated above)
            underline_thickness: PixelLength::new(underline_thickness),
            underline_position: PixelLength::new(underline_position),
            cap_height_ratio,
            cap_height: cap_height.map(PixelLength::new),
            is_scaled: true,
            presentation: pair.presentation,
            force_y_adjust: PixelLength::new(0.0),
        };

        if scale != 1.0 && metrics.is_scaled {
            let diff = metrics.descender - (metrics.descender / scale);
            metrics.force_y_adjust = diff;
        }

        self.metrics.borrow_mut().insert(key, metrics);

        log::trace!(
            "metrics_for_idx={}, size={}, dpi={} -> {:?}",
            font_idx,
            size,
            dpi,
            metrics
        );

        Ok(metrics)
    }

    fn metrics(&self, size: f64, dpi: u32) -> anyhow::Result<FontMetrics> {
        let theoretical_height = size * dpi as f64 / 72.0;
        let mut metrics_idx = 0;

        log::trace!(
            "compute metrics for size={}, dpi={}, theoretical height {}",
            size,
            dpi,
            theoretical_height
        );

        while let Ok(Some(pair)) = self.load_fallback(metrics_idx) {
            let scale = self.handles[metrics_idx].scale.unwrap_or(1.0);
            let pixel_size = size * scale * dpi as f64 / 72.0;

            if let Some(skrifa_ref) = pair.skrifa_font_ref() {
                let m = skrifa_ref.metrics(
                    skrifa::instance::Size::unscaled(),
                    skrifa::instance::LocationRef::default(),
                );
                let upem = m.units_per_em as f64;
                let sf = pixel_size / upem;
                // Note: skrifa descent is negative, so ascent + (-descent) = ascent - descent
                let cell_height = (m.ascent - m.descent + m.leading) as f64 * sf;
                let diff = (theoretical_height - cell_height).abs();
                let factor = diff / theoretical_height;

                drop(pair);

                if factor < 2.0 {
                    break;
                }

                if metrics_idx + 1 >= self.handles.len() {
                    log::warn!(
                        "metrics: wanted to skip idx {} but no more fallbacks",
                        metrics_idx
                    );
                    break;
                }
                metrics_idx += 1;
            } else {
                drop(pair);
                break;
            }

        }

        self.metrics_for_idx(metrics_idx, size, dpi)
    }
}
