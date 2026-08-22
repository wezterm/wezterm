use crate::color::LinearRgba;
use crate::glyphcache::LoadState;
use crate::quad::{QuadAllocator, QuadTrait};
use crate::termwindow::RenderState;
use crate::utilsprites::RenderMetrics;
use crate::Dimensions;
use anyhow::Context;
use config::CommandSource;
use config::{
    BackgroundHorizontalAlignment, BackgroundLayer, BackgroundRepeat, BackgroundSize,
    BackgroundSource, BackgroundVerticalAlignment, ConfigHandle, DimensionContext, Gradient,
    GradientOrientation,
};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use termwiz::image::{ImageData, ImageDataType};
use wezterm_term::StableRowIndex;

use crate::termwindow::live_command::{LiveCommand, LiveCommandKey};

lazy_static::lazy_static! {
    static ref IMAGE_CACHE: Mutex<HashMap<String, CachedImage>> = Mutex::new(HashMap::new());
    static ref GRADIENT_CACHE: Mutex<Vec<CachedGradient>> = Mutex::new(vec![]);
}

impl crate::TermWindow {
    /// Looks up (or spawns) the `LiveCommand` for this layer's command +
    /// current pixel size, keyed on the resulting terminal grid dimensions
    /// so that a resize naturally spawns a freshly-sized instance.
    ///
    /// This lives on `TermWindow` (rather than in a module-level cache like
    /// `IMAGE_CACHE`/`GRADIENT_CACHE` above) because `LiveCommand` holds a
    /// `GlyphCache`, which is `Rc`-based and therefore not `Send`/`Sync` —
    /// it can't live behind a `lazy_static`, which requires `Sync`. That's
    /// fine: unlike a static image, a `LiveCommand` is inherently tied to
    /// one window's rendering thread anyway.
    ///
    /// Config-level removal of a Command layer is swept separately (see
    /// the call to `reload_background_image`'s caller in `mod.rs`, keyed
    /// only on argv/cwd, which doesn't depend on window size); a resize of
    /// a layer that's still configured is handled right here instead,
    /// since only this call site knows both the old and new size for the
    /// same argv/cwd.
    fn resolve_live_command(
        &self,
        cmd: &CommandSource,
        width: usize,
        height: usize,
    ) -> anyhow::Result<Rc<LiveCommand>> {
        // Keyed on the layer's raw pixel size (rather than a cols/rows
        // derived from it) because the cell size used to derive cols/rows
        // depends on `font_scale`, which `LiveCommand::spawn` resolves
        // internally -- this call site doesn't need to duplicate that.
        let key = LiveCommandKey {
            argv: cmd.argv.clone(),
            cwd: cmd.cwd.clone(),
            width,
            height,
            font_scale_bits: cmd.font_scale.to_bits(),
        };

        let mut cache = self.live_command_cache.borrow_mut();
        if let Some(existing) = cache.get(&key) {
            return Ok(Rc::clone(existing));
        }

        // Drop any other instance of this same command at a different size
        // (eg: left over from before a window resize) so it doesn't linger
        // running in the background forever.
        cache.retain(|k, _| !(k.argv == key.argv && k.cwd == key.cwd));

        let live = Rc::new(LiveCommand::spawn(
            cmd,
            width,
            height,
            &self.render_metrics,
            &self.fonts,
        )?);
        cache.insert(key, Rc::clone(&live));
        Ok(live)
    }

    /// Kills off any live command whose (argv, cwd) is no longer
    /// referenced by any configured background layer. Called after a
    /// config reload; a resize of a layer that's still configured is
    /// handled separately, at resolve time (see `resolve_live_command`),
    /// since the cache key also includes the terminal grid size, which
    /// isn't meaningful independent of the window's current dimensions the
    /// way argv/cwd are.
    pub fn sweep_dead_live_commands(&self, config: &ConfigHandle) {
        let active: std::collections::HashSet<(Vec<String>, Option<String>)> = config
            .background
            .iter()
            .filter_map(|layer| match &layer.source {
                BackgroundSource::Command(cmd) => Some((cmd.argv.clone(), cmd.cwd.clone())),
                _ => None,
            })
            .collect();
        self.live_command_cache
            .borrow_mut()
            .retain(|key, _| active.contains(&(key.argv.clone(), key.cwd.clone())));
    }
}

struct CachedGradient {
    g: Gradient,
    width: u32,
    height: u32,
    image: Arc<ImageData>,
    marked: bool,
}

impl CachedGradient {
    fn compute(g: &Gradient, width: u32, height: u32) -> anyhow::Result<Arc<ImageData>> {
        let grad = g
            .build()
            .with_context(|| format!("building gradient {:?}", g))?;

        let mut imgbuf = image::RgbaImage::new(width, height);
        let fw = width as f64;
        let fh = height as f64;

        fn to_pixel(c: colorgrad::Color) -> image::Rgba<u8> {
            image::Rgba(c.to_rgba8())
        }

        // Map t which is in range [a, b] to range [c, d]
        fn remap(t: f64, a: f64, b: f64, c: f64, d: f64) -> f64 {
            (t - a) * ((d - c) / (b - a)) + c
        }

        let (dmin, dmax) = grad.domain();

        let mut rng = fastrand::Rng::new();

        // We add some randomness to the position that we use to
        // index into the color gradient, so that we can avoid
        // visible color banding.  The default 64 was selected
        // because it it was the smallest value on my mac where
        // the banding wasn't obvious.
        let noise_amount = g.noise.unwrap_or_else(|| {
            if matches!(g.orientation, GradientOrientation::Radial { .. }) {
                16
            } else {
                64
            }
        });

        fn noise(rng: &mut fastrand::Rng, noise_amount: usize) -> f64 {
            if noise_amount == 0 {
                0.
            } else {
                rng.usize(0..noise_amount) as f64 * -1.
            }
        }

        match g.orientation {
            GradientOrientation::Horizontal => {
                for (x, _, pixel) in imgbuf.enumerate_pixels_mut() {
                    *pixel = to_pixel(grad.at(remap(
                        x as f64 + noise(&mut rng, noise_amount),
                        0.0,
                        fw,
                        dmin,
                        dmax,
                    )));
                }
            }
            GradientOrientation::Vertical => {
                for (_, y, pixel) in imgbuf.enumerate_pixels_mut() {
                    *pixel = to_pixel(grad.at(remap(
                        y as f64 + noise(&mut rng, noise_amount),
                        0.0,
                        fh,
                        dmin,
                        dmax,
                    )));
                }
            }
            GradientOrientation::Linear { angle } => {
                let angle = angle.unwrap_or(0.0).to_radians();
                for (x, y, pixel) in imgbuf.enumerate_pixels_mut() {
                    let (x, y) = (x as f64, y as f64);
                    let (x, y) = (x - fw / 2., y - fh / 2.);
                    let t = x * f64::cos(angle) - y * f64::sin(angle);
                    *pixel = to_pixel(grad.at(remap(
                        t + noise(&mut rng, noise_amount),
                        -fw / 2.,
                        fw / 2.,
                        dmin,
                        dmax,
                    )));
                }
            }
            GradientOrientation::Radial { radius, cx, cy } => {
                let radius = fw * radius.unwrap_or(0.5);
                let cx = fw * cx.unwrap_or(0.5);
                let cy = fh * cy.unwrap_or(0.5);

                for (x, y, pixel) in imgbuf.enumerate_pixels_mut() {
                    let x = x as f64;
                    let y = y as f64;

                    // If we are close to the center, stop applying noise,
                    // as the noise can wrap around and start using the
                    // color from the other end of the gradient and look weird
                    let nx = if ((cx - x).abs() as usize) < noise_amount {
                        0.
                    } else {
                        noise(&mut rng, noise_amount)
                    };
                    let ny = if ((cy - y).abs() as usize) < noise_amount {
                        0.
                    } else {
                        noise(&mut rng, noise_amount)
                    };

                    let t = (nx + (x - cx).powi(2) + (ny + y - cy).powi(2)).sqrt() / radius;
                    *pixel = to_pixel(grad.at(t));
                }
            }
        }

        let data = imgbuf.into_vec();
        let image = Arc::new(ImageData::with_data(ImageDataType::new_single_frame(
            width, height, data,
        )));

        Ok(image)
    }

    fn load(g: &Gradient, width: u32, height: u32) -> anyhow::Result<Arc<ImageData>> {
        let mut cache = GRADIENT_CACHE.lock().unwrap();

        if let Some(entry) = cache
            .iter_mut()
            .find(|entry| entry.g == *g && entry.width == width && entry.height == height)
        {
            entry.marked = false;
            return Ok(Arc::clone(&entry.image));
        }

        let image = Self::compute(g, width, height)?;

        cache.push(Self {
            g: g.clone(),
            width,
            height,
            image: Arc::clone(&image),
            marked: false,
        });
        Ok(image)
    }

    fn mark() {
        let mut cache = GRADIENT_CACHE.lock().unwrap();
        for entry in cache.iter_mut() {
            entry.marked = true;
        }
    }

    fn sweep() {
        let mut cache = GRADIENT_CACHE.lock().unwrap();
        cache.retain(|entry| !entry.marked);
    }
}

struct CachedImage {
    modified: SystemTime,
    image: Arc<ImageData>,
    marked: bool,
    speed: f32,
}

impl CachedImage {
    fn load(path: &str, speed: f32) -> anyhow::Result<Arc<ImageData>> {
        let modified = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .with_context(|| format!("getting metadata for {}", path))?;
        let mut cache = IMAGE_CACHE.lock().unwrap();
        if let Some(cached) = cache.get_mut(path) {
            if cached.modified == modified && cached.speed == speed {
                cached.marked = false;
                return Ok(Arc::clone(&cached.image));
            }
        }

        let data = std::fs::read(path)
            .with_context(|| format!("Failed to load window_background_image {}", path))?;
        log::trace!("loaded {}", path);
        let mut data = ImageDataType::EncodedFile(data);
        data.adjust_speed(speed);
        let image = Arc::new(ImageData::with_data(data));

        cache.insert(
            path.to_string(),
            Self {
                modified,
                image: Arc::clone(&image),
                marked: false,
                speed,
            },
        );

        Ok(image)
    }

    fn mark() {
        let mut cache = IMAGE_CACHE.lock().unwrap();
        for entry in cache.values_mut() {
            entry.marked = true;
        }
    }

    fn sweep() {
        let mut cache = IMAGE_CACHE.lock().unwrap();
        cache.retain(|k, entry| {
            if entry.marked {
                log::trace!("Unloading {} from cache", k);
            }
            !entry.marked
        });
    }
}

pub struct LoadedBackgroundLayer {
    pub source: Arc<ImageData>,
    pub def: BackgroundLayer,
}

fn load_background_layer(
    layer: &BackgroundLayer,
    dimensions: &Dimensions,
    render_metrics: &RenderMetrics,
) -> anyhow::Result<LoadedBackgroundLayer> {
    let h_context = DimensionContext {
        dpi: dimensions.dpi as f32,
        pixel_max: dimensions.pixel_width as f32,
        pixel_cell: render_metrics.cell_size.width as f32,
    };
    let v_context = DimensionContext {
        dpi: dimensions.dpi as f32,
        pixel_max: dimensions.pixel_height as f32,
        pixel_cell: render_metrics.cell_size.height as f32,
    };

    let data = match &layer.source {
        BackgroundSource::Gradient(g) => {
            let mut width = match layer.width {
                BackgroundSize::Dimension(d) => d.evaluate_as_pixels(h_context),
                unsup => anyhow::bail!(
                    "{unsup:?} is not implemented for background gradients. \
                     Use e.g. `width = '100%'` instead"
                ),
            } as u32;
            let mut height = match layer.height {
                BackgroundSize::Dimension(d) => d.evaluate_as_pixels(v_context),
                unsup => anyhow::bail!(
                    "{unsup:?} is not implemented for background gradients. \
                     Use e.g. `height = '100%'` instead"
                ),
            } as u32;

            if matches!(g.orientation, GradientOrientation::Radial { .. }) {
                // To simplify the math, we compute a perfect circle
                // for the radial gradient, and let the texture sampler
                // perturb it to fill the window
                width = width.min(height);
                height = height.min(width);
            }

            CachedGradient::load(g, width, height)?
        }
        BackgroundSource::Color(color) => {
            // In theory we could just make a 1x1 texture and allow
            // the shader to stretch it, but if we do that, it'll blend
            // around the edges and look weird.
            // So we make a square texture in the ballpark of the window
            // surface.
            // It's not ideal.
            let width = match layer.width {
                BackgroundSize::Dimension(d) => d.evaluate_as_pixels(h_context),
                unsup => anyhow::bail!(
                    "{unsup:?} is not implemented for background color. \
                     Use e.g. `width = '100%'` instead"
                ),
            } as u32;
            let height = match layer.height {
                BackgroundSize::Dimension(d) => d.evaluate_as_pixels(v_context),
                unsup => anyhow::bail!(
                    "{unsup:?} is not implemented for background color. \
                     Use e.g. `height = '100%'` instead"
                ),
            } as u32;

            let size = width.min(height);

            let mut imgbuf = image::RgbaImage::new(size, size);
            let src_pixel = {
                let (r, g, b, a) = color.to_srgb_u8();
                image::Rgba([r, g, b, a])
            };
            for (_x, _y, pixel) in imgbuf.enumerate_pixels_mut() {
                *pixel = src_pixel;
            }
            let data = imgbuf.into_vec();
            Arc::new(ImageData::with_data(ImageDataType::new_single_frame(
                size, size, data,
            )))
        }
        BackgroundSource::File(source) => CachedImage::load(&source.path, source.speed)?,
        BackgroundSource::Command(_cmd) => {
            let width = match layer.width {
                BackgroundSize::Dimension(d) => d.evaluate_as_pixels(h_context),
                unsup => anyhow::bail!(
                    "{unsup:?} is not implemented for background commands. \
                     Use e.g. `width = '100%'` instead"
                ),
            } as u32;
            let height = match layer.height {
                BackgroundSize::Dimension(d) => d.evaluate_as_pixels(v_context),
                unsup => anyhow::bail!(
                    "{unsup:?} is not implemented for background commands. \
                     Use e.g. `height = '100%'` instead"
                ),
            } as u32;

            // Placeholder only: a running process isn't a bounded,
            // cacheable image the way a gradient/color/file is, so
            // `render_background` below replaces this on every paint with
            // a freshly rasterized frame from the live command. This just
            // avoids a flash of transparency before the first real frame.
            let mut data = vec![0u8; (width as usize) * (height as usize) * 4];
            for px in data.chunks_exact_mut(4) {
                px[3] = 0xff;
            }
            Arc::new(ImageData::with_data(ImageDataType::new_single_frame(
                width, height, data,
            )))
        }
    };

    Ok(LoadedBackgroundLayer {
        source: data,
        def: layer.clone(),
    })
}

pub fn load_background_image(
    config: &ConfigHandle,
    dimensions: &Dimensions,
    render_metrics: &RenderMetrics,
) -> Vec<LoadedBackgroundLayer> {
    let mut layers = vec![];
    for layer in &config.background {
        let load_start = std::time::Instant::now();
        match load_background_layer(layer, dimensions, render_metrics) {
            Ok(layer) => {
                log::trace!("loaded layer in {:?}", load_start.elapsed());
                layers.push(layer);
            }
            Err(err) => {
                log::error!("Failed to load background: {:#}", err);
            }
        }
    }
    layers
}

pub fn reload_background_image(
    config: &ConfigHandle,
    existing: &[LoadedBackgroundLayer],
    dimensions: &Dimensions,
    render_metrics: &RenderMetrics,
) -> Vec<LoadedBackgroundLayer> {
    // We want to reuse the existing version of the image where possible
    // so that the textures we may have cached can be re-used and so that
    // animation state can be preserved across the reload.
    let map: HashMap<_, _> = existing
        .iter()
        .map(|layer| (layer.source.hash(), &layer.source))
        .collect();

    CachedImage::mark();
    CachedGradient::mark();

    let result = load_background_image(config, dimensions, render_metrics)
        .into_iter()
        .map(|mut layer| {
            let hash = layer.source.hash();

            if let Some(existing) = map.get(&hash) {
                layer.source = Arc::clone(existing);
            }

            layer
        })
        .collect();

    CachedImage::sweep();
    CachedGradient::sweep();

    result
}

impl crate::TermWindow {
    pub fn render_backgrounds(
        &self,
        bg_color: LinearRgba,
        top: StableRowIndex,
    ) -> anyhow::Result<bool> {
        let gl_state = self.render_state.as_ref().unwrap();
        let mut layer_idx = -127;
        let mut loaded_any = false;
        for layer in self.window_background.iter() {
            if self.render_background(gl_state, bg_color, layer, layer_idx, top)? {
                loaded_any = true;
                layer_idx = layer_idx.saturating_add(1);
            }
        }
        Ok(loaded_any)
    }

    fn render_background(
        &self,
        gl_state: &RenderState,
        bg_color: LinearRgba,
        layer: &LoadedBackgroundLayer,
        layer_index: i8,
        top: StableRowIndex,
    ) -> anyhow::Result<bool> {
        let render_layer = gl_state.layer_for_zindex(layer_index)?;
        let vbs = render_layer.vb.borrow();
        let mut layer0 = vbs[0].map();

        let color = bg_color.mul_alpha(layer.def.opacity);

        let pixel_width = self.dimensions.pixel_width as f32;
        let pixel_height = self.dimensions.pixel_height as f32;

        let h_context = DimensionContext {
            dpi: self.dimensions.dpi as f32,
            pixel_max: pixel_width,
            pixel_cell: self.render_metrics.cell_size.width as f32,
        };
        let v_context = DimensionContext {
            dpi: self.dimensions.dpi as f32,
            pixel_max: pixel_height,
            pixel_cell: self.render_metrics.cell_size.height as f32,
        };

        // A `Command` source is a live, unbounded process rather than a
        // bounded, cacheable image, so its current frame is resolved (and,
        // if the configured fps interval has elapsed, re-rasterized) on
        // every paint, with our own repaint scheduled explicitly: unlike
        // an animated gif/png, `cached_image` below has no bounded frame
        // list to infer a "next frame due" time from for this source.
        let live_source;
        let source: &Arc<ImageData> = if let BackgroundSource::Command(cmd) = &layer.def.source {
            let width = match layer.def.width {
                BackgroundSize::Dimension(d) => d.evaluate_as_pixels(h_context) as usize,
                _ => pixel_width as usize,
            };
            let height = match layer.def.height {
                BackgroundSize::Dimension(d) => d.evaluate_as_pixels(v_context) as usize,
                _ => pixel_height as usize,
            };

            let live = self.resolve_live_command(cmd, width, height)?;
            let (image, due) = live.get_frame(cmd.fps, width, height);
            self.update_next_frame_time(Some(due));
            live_source = image;
            &live_source
        } else {
            &layer.source
        };

        let (sprite, next_due, load_state) =
            gl_state
                .glyph_cache
                .borrow_mut()
                .cached_image(source, None, self.allow_images)?;
        self.update_next_frame_time(next_due);

        if load_state == LoadState::Loading {
            return Ok(false);
        }

        let tex_width = sprite.coords.width() as f32;
        let tex_height = sprite.coords.height() as f32;

        let scale_width = pixel_width / tex_width as f32;
        let scale_height = pixel_height / tex_height as f32;

        // log::info!("tex {tex_width}x{tex_height} aspect={aspect}");

        // Compute the smallest aspect-preserved size that will fit the space
        let (min_aspect_width, min_aspect_height) = {
            let scale = scale_width.min(scale_height);
            (tex_width * scale, tex_height * scale)
        };
        // Compute the largest aspect-preserved size that will fill the space
        let (max_aspect_width, max_aspect_height) = {
            let scale = scale_width.max(scale_height);
            (tex_width * scale, tex_height * scale)
        };

        let width = match layer.def.width {
            BackgroundSize::Contain => min_aspect_width as f32,
            BackgroundSize::Cover => max_aspect_width as f32,
            BackgroundSize::Dimension(n) => n.evaluate_as_pixels(h_context),
        };

        let height = match layer.def.height {
            BackgroundSize::Contain => min_aspect_height as f32,
            BackgroundSize::Cover => max_aspect_height as f32,
            BackgroundSize::Dimension(n) => n.evaluate_as_pixels(v_context),
        };

        let mut origin_x = pixel_width / -2.;
        let top_pixel = pixel_height / -2.;
        let mut origin_y = top_pixel;

        match layer.def.vertical_align {
            BackgroundVerticalAlignment::Top => {}
            BackgroundVerticalAlignment::Bottom => {
                origin_y += pixel_height - height;
            }
            BackgroundVerticalAlignment::Middle => {
                origin_y += (pixel_height - height) / 2.;
            }
        }
        match layer.def.horizontal_align {
            BackgroundHorizontalAlignment::Left => {}
            BackgroundHorizontalAlignment::Right => {
                origin_x += pixel_width - width;
            }
            BackgroundHorizontalAlignment::Center => {
                origin_x += (pixel_width - width) / 2.;
            }
        }

        let vertical_offset = layer
            .def
            .vertical_offset
            .map(|d| d.evaluate_as_pixels(v_context))
            .unwrap_or(0.);
        origin_y += vertical_offset;

        let horizontal_offset = layer
            .def
            .horizontal_offset
            .map(|d| d.evaluate_as_pixels(h_context))
            .unwrap_or(0.);
        origin_x += horizontal_offset;

        let repeat_x = layer
            .def
            .repeat_x_size
            .map(|size| size.evaluate_as_pixels(h_context))
            .unwrap_or(width);
        let repeat_y = layer
            .def
            .repeat_y_size
            .map(|size| size.evaluate_as_pixels(v_context))
            .unwrap_or(height);

        // log::info!("computed {width}x{height}");

        let mut start_tile = 0;
        if let Some(factor) = layer.def.attachment.scroll_factor() {
            let distance = top as f32 * self.render_metrics.cell_size.height as f32 * factor;
            let num_tiles = distance / repeat_y;
            origin_y -= (num_tiles.fract() * repeat_y).floor();
            start_tile = num_tiles.floor() as usize;
        }

        let limit_y = top_pixel + pixel_height;

        let mut emitted = false;

        for y_step in start_tile.. {
            let offset_y = (y_step - start_tile) as f32 * repeat_y;
            let origin_y = origin_y + offset_y;
            if origin_y >= limit_y
                || (y_step > start_tile && layer.def.repeat_y == BackgroundRepeat::NoRepeat)
            {
                break;
            }

            for x_step in 0.. {
                let offset_x = x_step as f32 * repeat_x;
                if offset_x >= pixel_width
                    || (x_step > 0 && layer.def.repeat_x == BackgroundRepeat::NoRepeat)
                {
                    break;
                }
                let origin_x = origin_x + offset_x;
                let mut quad = layer0.allocate()?;
                emitted = true;
                // log::info!("quad {origin_x},{origin_y} {width}x{height}");
                quad.set_position(origin_x, origin_y, origin_x + width, origin_y + height);

                let coords = sprite.texture_coords();
                let mut x1 = coords.min_x();
                let mut x2 = coords.max_x();
                let mut y1 = coords.min_y();
                let mut y2 = coords.max_y();
                if layer.def.repeat_x == BackgroundRepeat::Mirror && x_step % 2 == 1 {
                    std::mem::swap(&mut x1, &mut x2);
                }
                if layer.def.repeat_y == BackgroundRepeat::Mirror && y_step % 2 == 1 {
                    std::mem::swap(&mut y1, &mut y2);
                }

                quad.set_texture_discrete(x1, x2, y1, y2);
                quad.set_is_background_image();
                quad.set_hsv(Some(layer.def.hsb));
                quad.set_fg_color(color);
            }
        }

        Ok(emitted)
    }
}
