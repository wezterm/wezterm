//! Font locator using fontdb + fontconfig-parser (pure Rust).
//! Replaces font_config.rs / fcwrap.rs for Linux font discovery.

use crate::locator::{FontDataHandle, FontDataSource, FontLocator, FontOrigin};
use crate::parser::ParsedFont;
use config::FontAttributes;
use rangeset::RangeSet;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

pub struct FontDbLocator {
    db: Mutex<fontdb::Database>,
    /// Lazily computed codepoint coverage per font face.
    /// Populated on first fallback lookup; subsequent lookups are
    /// in-memory set intersections with no disk I/O.
    coverage_cache: Mutex<HashMap<fontdb::ID, RangeSet<u32>>>,
}

impl FontDbLocator {
    pub fn new() -> Self {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        // Also load fontconfig paths via fontconfig-parser
        let fc_config = fontconfig_parser::FontConfig::default();
        for dir in &fc_config.dirs {
            db.load_fonts_dir(&dir.path);
        }
        Self {
            db: Mutex::new(db),
            coverage_cache: Mutex::new(HashMap::new()),
        }
    }
}

impl FontLocator for FontDbLocator {
    fn load_fonts(
        &self,
        fonts_selection: &[FontAttributes],
        loaded: &mut HashSet<FontAttributes>,
        _pixel_size: u16,
    ) -> anyhow::Result<Vec<ParsedFont>> {
        let db = self.db.lock().unwrap();
        let mut fonts = vec![];

        for attr in fonts_selection {
            if loaded.contains(attr) {
                continue;
            }

            let query = fontdb::Query {
                families: &[fontdb::Family::Name(&attr.family)],
                weight: fontdb::Weight(attr.weight.to_opentype_weight()),
                stretch: map_stretch(attr.stretch),
                style: match attr.style {
                    config::FontStyle::Normal => fontdb::Style::Normal,
                    config::FontStyle::Italic => fontdb::Style::Italic,
                    config::FontStyle::Oblique => fontdb::Style::Oblique,
                },
            };

            if let Some(id) = db.query(&query) {
                if let Some(face_info) = db.face(id) {
                    let source = face_source_to_font_data_source(face_info, &attr.family);

                    let handle = FontDataHandle {
                        source,
                        index: face_info.index,
                        variation: 0,
                        origin: FontOrigin::FontConfig,
                        coverage: None,
                    };

                    match ParsedFont::from_locator(&handle) {
                        Ok(parsed) => {
                            loaded.insert(attr.clone());
                            fonts.push(parsed);
                        }
                        Err(err) => {
                            log::warn!(
                                "Failed to parse font for {:?}: {:#}",
                                attr,
                                err
                            );
                        }
                    }
                }
            }
        }

        Ok(fonts)
    }

    fn enumerate_all_fonts(&self) -> anyhow::Result<Vec<ParsedFont>> {
        let db = self.db.lock().unwrap();
        let mut fonts = vec![];

        for face_info in db.faces() {
            let source = face_source_to_font_data_source(face_info, "enumerated");

            let handle = FontDataHandle {
                source,
                index: face_info.index,
                variation: 0,
                origin: FontOrigin::FontConfig,
                coverage: None,
            };

            match ParsedFont::from_locator(&handle) {
                Ok(parsed) => fonts.push(parsed),
                Err(err) => {
                    log::trace!(
                        "Failed to parse enumerated font {:?}: {:#}",
                        face_info.source,
                        err
                    );
                }
            }
        }

        Ok(fonts)
    }

    fn locate_fallback_for_codepoints(
        &self,
        codepoints: &[char],
    ) -> anyhow::Result<Vec<ParsedFont>> {
        let db = self.db.lock().unwrap();
        let mut cache = self.coverage_cache.lock().unwrap();
        let mut fonts = vec![];
        let mut seen = HashSet::new();

        let mut wanted = RangeSet::new();
        for &c in codepoints {
            wanted.add(c as u32);
        }

        for face_info in db.faces() {
            if seen.contains(&face_info.id) {
                continue;
            }
            seen.insert(face_info.id);

            // Lazily compute and cache coverage for this face.
            // For uncached fonts, do a quick codepoint probe first to avoid
            // computing full coverage for fonts that don't have any of the
            // wanted glyphs.
            let coverage = match cache.get(&face_info.id) {
                Some(cov) => cov,
                None => {
                    let source = face_source_to_font_data_source(face_info, "fallback");
                    let data = match source.load_data() {
                        Ok(d) => d,
                        Err(err) => {
                            log::trace!(
                                "Failed to load font data {:?}: {:#}",
                                face_info.source,
                                err
                            );
                            continue;
                        }
                    };
                    let font_ref =
                        match skrifa::FontRef::from_index(&data, face_info.index) {
                            Ok(f) => f,
                            Err(_) => continue,
                        };

                    // Quick check: probe just the wanted codepoints before
                    // computing full coverage. This avoids the cost of
                    // enumerating the entire cmap for non-matching fonts.
                    use skrifa::MetadataProvider;
                    let charmap = font_ref.charmap();
                    let has_any = codepoints.iter().any(|&c| charmap.map(c as u32).is_some());
                    if !has_any {
                        continue;
                    }

                    let t = std::time::Instant::now();
                    let cov = crate::parser::compute_coverage(&font_ref);
                    let elapsed = t.elapsed();
                    metrics::histogram!("font.compute.codepoint.coverage").record(elapsed);
                    log::trace!(
                        "fontdb: coverage for {:?} computed in {:?}",
                        face_info.source,
                        elapsed
                    );
                    cache.insert(face_info.id, cov);
                    cache.get(&face_info.id).unwrap()
                }
            };

            if wanted.intersection(coverage).is_empty() {
                continue;
            }

            let source = face_source_to_font_data_source(face_info, "fallback");
            let handle = FontDataHandle {
                source,
                index: face_info.index,
                variation: 0,
                origin: FontOrigin::FontConfig,
                coverage: Some(coverage.clone()),
            };

            match ParsedFont::from_locator(&handle) {
                Ok(parsed) => {
                    fonts.push(parsed);
                }
                Err(err) => {
                    log::trace!(
                        "Failed to parse fallback font {:?}: {:#}",
                        face_info.source,
                        err
                    );
                }
            }
        }

        Ok(fonts)
    }
}

fn face_source_to_font_data_source(face_info: &fontdb::FaceInfo, label: &str) -> FontDataSource {
    match &face_info.source {
        fontdb::Source::File(path) => FontDataSource::OnDisk(path.clone()),
        fontdb::Source::SharedFile(path, _) => FontDataSource::OnDisk(path.clone()),
        fontdb::Source::Binary(data) => {
            let data_vec: Vec<u8> = data.as_ref().as_ref().to_vec();
            FontDataSource::Memory {
                name: format!("fontdb:{}", label),
                data: std::sync::Arc::new(data_vec.into_boxed_slice()),
            }
        }
    }
}

fn map_stretch(stretch: config::FontStretch) -> fontdb::Stretch {
    match stretch {
        config::FontStretch::UltraCondensed => fontdb::Stretch::UltraCondensed,
        config::FontStretch::ExtraCondensed => fontdb::Stretch::ExtraCondensed,
        config::FontStretch::Condensed => fontdb::Stretch::Condensed,
        config::FontStretch::SemiCondensed => fontdb::Stretch::SemiCondensed,
        config::FontStretch::Normal => fontdb::Stretch::Normal,
        config::FontStretch::SemiExpanded => fontdb::Stretch::SemiExpanded,
        config::FontStretch::Expanded => fontdb::Stretch::Expanded,
        config::FontStretch::ExtraExpanded => fontdb::Stretch::ExtraExpanded,
        config::FontStretch::UltraExpanded => fontdb::Stretch::UltraExpanded,
    }
}
