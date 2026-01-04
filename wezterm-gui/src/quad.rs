// Clippy hates the implement_vertex macro and won't let me scope
// this warning to its use
#![allow(clippy::unneeded_field_pattern)]

use crate::renderstate::{BorrowedLayers, Cairo2DCacheData};
use ::window::bitmaps::TextureRect;
use ::window::color::LinearRgba;
use config::HsbTransform;
use std::cell::RefCell;

/// Each cell is composed of two triangles built from 4 vertices.
/// The buffer is organized row by row.
pub const VERTICES_PER_CELL: usize = 4;
pub const V_TOP_LEFT: usize = 0;
pub const V_TOP_RIGHT: usize = 1;
pub const V_BOT_LEFT: usize = 2;
pub const V_BOT_RIGHT: usize = 3;

/// a regular monochrome text glyph
const IS_GLYPH: f32 = 0.0;
/// a color emoji glyph
const IS_COLOR_EMOJI: f32 = 1.0;
/// a full color texture attached as the
/// background image of the window
const IS_BG_IMAGE: f32 = 2.0;
/// like 2.0, except that instead of an
/// image, we use the solid bg color
const IS_SOLID_COLOR: f32 = 3.0;
/// Grayscale poly quad for non-aa text render layers
const IS_GRAY_SCALE: f32 = 4.0;

#[repr(C)]
#[derive(Copy, Clone, Default, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    // Physical position of the corner of the character cell
    pub position: [f32; 2],
    // glyph texture
    pub tex: [f32; 2],
    pub fg_color: [f32; 4],
    pub alt_color: [f32; 4],
    pub hsv: [f32; 3],
    pub has_color: f32,
    pub mix_value: f32,
}
::window::glium::implement_vertex!(
    Vertex, position, tex, fg_color, alt_color, hsv, has_color, mix_value
);

impl Vertex {
    const ATTRIBS: [wgpu::VertexAttribute; 7] = wgpu::vertex_attr_array![
    0 => Float32x2,  // position
    1 => Float32x2,  // tex
    2 => Float32x4,  // fg_color
    3 => Float32x4,  // alt_color
    4 => Float32x3,  // hsv
    5 => Float32,    // has_color
    6 => Float32,    // mix_value
    ];

    pub fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

pub trait QuadTrait {
    /// Assign the texture coordinates
    fn set_texture(&mut self, coords: TextureRect) {
        let x1 = coords.min_x();
        let x2 = coords.max_x();
        let y1 = coords.min_y();
        let y2 = coords.max_y();
        self.set_texture_discrete(x1, x2, y1, y2);
    }
    fn set_texture_discrete(&mut self, x1: f32, x2: f32, y1: f32, y2: f32);
    fn set_has_color_impl(&mut self, has_color: f32);

    /// Set the color glyph "flag"
    fn set_has_color(&mut self, has_color: bool) {
        self.set_has_color_impl(if has_color { IS_COLOR_EMOJI } else { IS_GLYPH });
    }

    /// Mark as a grayscale polyquad; color and alpha will be
    /// multipled with those in the texture
    fn set_grayscale(&mut self) {
        self.set_has_color_impl(IS_GRAY_SCALE);
    }

    /// Mark this quad as a background image.
    /// Mutually exclusive with set_has_color.
    fn set_is_background_image(&mut self) {
        self.set_has_color_impl(IS_BG_IMAGE);
    }

    fn set_is_background(&mut self) {
        self.set_has_color_impl(IS_SOLID_COLOR);
    }

    fn set_fg_color(&mut self, color: LinearRgba);

    /// Must be called after set_fg_color
    fn set_alt_color_and_mix_value(&mut self, color: LinearRgba, mix_value: f32);

    fn set_hsv(&mut self, hsv: Option<HsbTransform>);
    fn set_position(&mut self, left: f32, top: f32, right: f32, bottom: f32);

    /// Set glyph identifier for Cairo2D caching (ignored by GPU backends)
    fn set_glyph_id(&mut self, id: u32);

    /// Set background color for Cairo2D caching (ignored by GPU backends)
    fn set_bg_color(&mut self, color: LinearRgba);

    /// Set cell boundaries for Cairo2D background fill (ignored by GPU backends)
    fn set_cell_bounds(&mut self, cell_y: f32, cell_height: f32);
}

pub enum QuadImpl<'a> {
    Vert(Quad<'a>),
    Boxed(&'a mut BoxedQuad),
    Cairo2D(Cairo2DQuad<'a>),
    /// Heap-allocated quad with parallel Cairo2D cache data (for HeapQuadAllocator)
    HeapCairo2D(HeapCairo2DQuad<'a>),
}

/// A heap-allocated quad wrapper that stores Cairo2D cache data in parallel.
/// Used by HeapQuadAllocator to maintain cache data for Cairo2D line caching.
pub struct HeapCairo2DQuad<'a> {
    quad: &'a mut BoxedQuad,
    cache_data: &'a mut Cairo2DCacheData,
}

impl<'a> QuadTrait for HeapCairo2DQuad<'a> {
    fn set_texture_discrete(&mut self, x1: f32, x2: f32, y1: f32, y2: f32) {
        self.quad.set_texture_discrete(x1, x2, y1, y2);
    }

    fn set_has_color_impl(&mut self, has_color: f32) {
        self.quad.set_has_color_impl(has_color);
    }

    fn set_fg_color(&mut self, color: LinearRgba) {
        self.quad.set_fg_color(color);
    }

    fn set_alt_color_and_mix_value(&mut self, color: LinearRgba, mix_value: f32) {
        self.quad.set_alt_color_and_mix_value(color, mix_value);
    }

    fn set_hsv(&mut self, hsv: Option<HsbTransform>) {
        self.quad.set_hsv(hsv);
    }

    fn set_position(&mut self, left: f32, top: f32, right: f32, bottom: f32) {
        self.quad.set_position(left, top, right, bottom);
    }

    fn set_glyph_id(&mut self, id: u32) {
        self.cache_data.glyph_id = id;
    }

    fn set_bg_color(&mut self, color: LinearRgba) {
        let (r, g, b, a) = color.tuple();
        self.cache_data.bg_color = [r, g, b, a];
    }

    fn set_cell_bounds(&mut self, cell_y: f32, cell_height: f32) {
        self.cache_data.cell_y = cell_y;
        self.cache_data.cell_height = cell_height;
    }
}

impl<'a> QuadTrait for QuadImpl<'a> {
    fn set_texture_discrete(&mut self, x1: f32, x2: f32, y1: f32, y2: f32) {
        match self {
            Self::Vert(q) => q.set_texture_discrete(x1, x2, y1, y2),
            Self::Boxed(q) => q.set_texture_discrete(x1, x2, y1, y2),
            Self::Cairo2D(q) => q.set_texture_discrete(x1, x2, y1, y2),
            Self::HeapCairo2D(q) => q.set_texture_discrete(x1, x2, y1, y2),
        }
    }

    fn set_has_color_impl(&mut self, has_color: f32) {
        match self {
            Self::Vert(q) => q.set_has_color_impl(has_color),
            Self::Boxed(q) => q.set_has_color_impl(has_color),
            Self::Cairo2D(q) => q.set_has_color_impl(has_color),
            Self::HeapCairo2D(q) => q.set_has_color_impl(has_color),
        }
    }

    fn set_fg_color(&mut self, color: LinearRgba) {
        match self {
            Self::Vert(q) => q.set_fg_color(color),
            Self::Boxed(q) => q.set_fg_color(color),
            Self::Cairo2D(q) => q.set_fg_color(color),
            Self::HeapCairo2D(q) => q.set_fg_color(color),
        }
    }

    fn set_alt_color_and_mix_value(&mut self, color: LinearRgba, mix_value: f32) {
        match self {
            Self::Vert(q) => q.set_alt_color_and_mix_value(color, mix_value),
            Self::Boxed(q) => q.set_alt_color_and_mix_value(color, mix_value),
            Self::Cairo2D(q) => q.set_alt_color_and_mix_value(color, mix_value),
            Self::HeapCairo2D(q) => q.set_alt_color_and_mix_value(color, mix_value),
        }
    }

    fn set_hsv(&mut self, hsv: Option<HsbTransform>) {
        match self {
            Self::Vert(q) => q.set_hsv(hsv),
            Self::Boxed(q) => q.set_hsv(hsv),
            Self::Cairo2D(q) => q.set_hsv(hsv),
            Self::HeapCairo2D(q) => q.set_hsv(hsv),
        }
    }

    fn set_position(&mut self, left: f32, top: f32, right: f32, bottom: f32) {
        match self {
            Self::Vert(q) => q.set_position(left, top, right, bottom),
            Self::Boxed(q) => q.set_position(left, top, right, bottom),
            Self::Cairo2D(q) => q.set_position(left, top, right, bottom),
            Self::HeapCairo2D(q) => q.set_position(left, top, right, bottom),
        }
    }

    fn set_glyph_id(&mut self, id: u32) {
        match self {
            Self::Vert(q) => q.set_glyph_id(id),
            Self::Boxed(q) => q.set_glyph_id(id),
            Self::Cairo2D(q) => q.set_glyph_id(id),
            Self::HeapCairo2D(q) => q.set_glyph_id(id),
        }
    }

    fn set_bg_color(&mut self, color: LinearRgba) {
        match self {
            Self::Vert(q) => q.set_bg_color(color),
            Self::Boxed(q) => q.set_bg_color(color),
            Self::Cairo2D(q) => q.set_bg_color(color),
            Self::HeapCairo2D(q) => q.set_bg_color(color),
        }
    }

    fn set_cell_bounds(&mut self, cell_y: f32, cell_height: f32) {
        match self {
            Self::Vert(q) => q.set_cell_bounds(cell_y, cell_height),
            Self::Boxed(q) => q.set_cell_bounds(cell_y, cell_height),
            Self::Cairo2D(q) => q.set_cell_bounds(cell_y, cell_height),
            Self::HeapCairo2D(q) => q.set_cell_bounds(cell_y, cell_height),
        }
    }
}

/// A helper for updating the 4 vertices that compose a glyph cell
pub struct Quad<'a> {
    pub(crate) vert: &'a mut [Vertex],
}

impl<'a> QuadTrait for Quad<'a> {
    fn set_texture_discrete(&mut self, x1: f32, x2: f32, y1: f32, y2: f32) {
        self.vert[V_TOP_LEFT].tex = [x1, y1];
        self.vert[V_TOP_RIGHT].tex = [x2, y1];
        self.vert[V_BOT_LEFT].tex = [x1, y2];
        self.vert[V_BOT_RIGHT].tex = [x2, y2];
    }

    fn set_has_color_impl(&mut self, has_color: f32) {
        for v in self.vert.iter_mut() {
            v.has_color = has_color;
        }
    }

    fn set_fg_color(&mut self, color: LinearRgba) {
        for v in self.vert.iter_mut() {
            v.fg_color = color.into();
        }
        self.set_alt_color_and_mix_value(color, 0.);
    }

    /// Must be called after set_fg_color
    fn set_alt_color_and_mix_value(&mut self, color: LinearRgba, mix_value: f32) {
        for v in self.vert.iter_mut() {
            v.alt_color = color.into();
            v.mix_value = mix_value;
        }
    }

    fn set_hsv(&mut self, hsv: Option<HsbTransform>) {
        let (h, s, v) = hsv
            .map(|t| (t.hue, t.saturation, t.brightness))
            .unwrap_or((1., 1., 1.));
        for vert in self.vert.iter_mut() {
            vert.hsv = [h, s, v];
        }
    }

    fn set_position(&mut self, left: f32, top: f32, right: f32, bottom: f32) {
        self.vert[V_TOP_LEFT].position = [left, top];
        self.vert[V_TOP_RIGHT].position = [right, top];
        self.vert[V_BOT_LEFT].position = [left, bottom];
        self.vert[V_BOT_RIGHT].position = [right, bottom];
    }

    // These methods are no-ops for GPU backends.
    // For Cairo2D, use Cairo2DQuad which stores this data separately.
    fn set_glyph_id(&mut self, _id: u32) {}
    fn set_bg_color(&mut self, _color: LinearRgba) {}
    fn set_cell_bounds(&mut self, _cell_y: f32, _cell_height: f32) {}
}

/// A quad wrapper for Cairo2D that stores cache data separately from vertices.
/// This allows the Vertex struct to remain GPU-friendly while Cairo2D gets the
/// additional metadata it needs for glyph caching and background rendering.
pub struct Cairo2DQuad<'a> {
    /// The underlying vertex quad
    quad: Quad<'a>,
    /// Reference to the cache data storage (parallel to vertices)
    cache_data: &'a RefCell<Vec<Cairo2DCacheData>>,
    /// Index into cache_data for this quad
    quad_index: usize,
}

impl<'a> Cairo2DQuad<'a> {
    pub fn new(
        quad: Quad<'a>,
        cache_data: &'a RefCell<Vec<Cairo2DCacheData>>,
        quad_index: usize,
    ) -> Self {
        Self {
            quad,
            cache_data,
            quad_index,
        }
    }
}

impl<'a> QuadTrait for Cairo2DQuad<'a> {
    fn set_texture_discrete(&mut self, x1: f32, x2: f32, y1: f32, y2: f32) {
        self.quad.set_texture_discrete(x1, x2, y1, y2);
    }

    fn set_has_color_impl(&mut self, has_color: f32) {
        self.quad.set_has_color_impl(has_color);
    }

    fn set_fg_color(&mut self, color: LinearRgba) {
        self.quad.set_fg_color(color);
    }

    fn set_alt_color_and_mix_value(&mut self, color: LinearRgba, mix_value: f32) {
        self.quad.set_alt_color_and_mix_value(color, mix_value);
    }

    fn set_hsv(&mut self, hsv: Option<HsbTransform>) {
        self.quad.set_hsv(hsv);
    }

    fn set_position(&mut self, left: f32, top: f32, right: f32, bottom: f32) {
        self.quad.set_position(left, top, right, bottom);
    }

    fn set_glyph_id(&mut self, id: u32) {
        let mut cache = self.cache_data.borrow_mut();
        if let Some(data) = cache.get_mut(self.quad_index) {
            data.glyph_id = id;
        }
    }

    fn set_bg_color(&mut self, color: LinearRgba) {
        let (r, g, b, a) = color.tuple();
        let mut cache = self.cache_data.borrow_mut();
        if let Some(data) = cache.get_mut(self.quad_index) {
            data.bg_color = [r, g, b, a];
        }
    }

    fn set_cell_bounds(&mut self, cell_y: f32, cell_height: f32) {
        let mut cache = self.cache_data.borrow_mut();
        if let Some(data) = cache.get_mut(self.quad_index) {
            data.cell_y = cell_y;
            data.cell_height = cell_height;
        }
    }
}

pub trait QuadAllocator {
    fn allocate(&mut self) -> anyhow::Result<QuadImpl<'_>>;
    fn extend_with(&mut self, vertices: &[Vertex]);
    /// Cairo2D specific - extends with vertices and cache data
    fn extend_with_cairo2d(&mut self, vertices: &[Vertex], cache_data: &[Cairo2DCacheData]);
}

pub trait TripleLayerQuadAllocatorTrait {
    fn allocate(&mut self, layer_num: usize) -> anyhow::Result<QuadImpl<'_>>;
    /// Original GPU method - extends with vertices only
    fn extend_with(&mut self, layer_num: usize, vertices: &[Vertex]);
    /// Cairo2D specific - extends with vertices and cache data
    fn extend_with_cairo2d(
        &mut self,
        layer_num: usize,
        vertices: &[Vertex],
        cache_data: &[Cairo2DCacheData],
    );
}

/// We prefer to allocate a quad at a time for HeapQuadAllocator
/// because we tend to end up with fairly large arrays of Vertex
/// and the total amount of contiguous memory is in the MB range,
/// which is a bit gnarly to reallocate, and can waste several MB
/// in unused capacity
#[derive(Default)]
pub struct BoxedQuad {
    position: (f32, f32, f32, f32),
    fg_color: [f32; 4],
    alt_color: [f32; 4],
    tex: (f32, f32, f32, f32),
    hsv: [f32; 3],
    has_color: f32,
    mix_value: f32,
}

impl QuadTrait for BoxedQuad {
    fn set_texture_discrete(&mut self, x1: f32, x2: f32, y1: f32, y2: f32) {
        self.tex = (x1, x2, y1, y2);
    }

    fn set_has_color_impl(&mut self, has_color: f32) {
        self.has_color = has_color;
    }

    fn set_fg_color(&mut self, color: LinearRgba) {
        self.fg_color = color.into();
    }
    fn set_alt_color_and_mix_value(&mut self, color: LinearRgba, mix_value: f32) {
        self.alt_color = color.into();
        self.mix_value = mix_value;
    }
    fn set_hsv(&mut self, hsv: Option<HsbTransform>) {
        let (h, s, v) = hsv
            .map(|t| (t.hue, t.saturation, t.brightness))
            .unwrap_or((1., 1., 1.));
        self.hsv = [h, s, v];
    }

    fn set_position(&mut self, left: f32, top: f32, right: f32, bottom: f32) {
        self.position = (left, top, right, bottom);
    }

    // These methods are no-ops for HeapQuadAllocator (used for caching).
    // Cairo2D uses Cairo2DQuad which stores this data separately.
    fn set_glyph_id(&mut self, _id: u32) {}
    fn set_bg_color(&mut self, _color: LinearRgba) {}
    fn set_cell_bounds(&mut self, _cell_y: f32, _cell_height: f32) {}
}

impl BoxedQuad {
    fn from_vertices(verts: &[Vertex; VERTICES_PER_CELL]) -> Self {
        let [x1, y1] = verts[V_TOP_LEFT].tex;
        let [x2, y2] = verts[V_BOT_RIGHT].tex;

        let [left, top] = verts[V_TOP_LEFT].position;
        let [right, bottom] = verts[V_BOT_RIGHT].position;
        Self {
            tex: (x1, x2, y1, y2),
            position: (left, top, right, bottom),
            has_color: verts[V_TOP_LEFT].has_color,
            alt_color: verts[V_TOP_LEFT].alt_color,
            fg_color: verts[V_TOP_LEFT].fg_color,
            hsv: verts[V_TOP_LEFT].hsv,
            mix_value: verts[V_TOP_LEFT].mix_value,
        }
    }

    fn to_vertices(&self) -> [Vertex; VERTICES_PER_CELL] {
        let mut vert: [Vertex; VERTICES_PER_CELL] = Default::default();
        let mut quad = Quad { vert: &mut vert };

        let (x1, x2, y1, y2) = self.tex;
        quad.set_texture_discrete(x1, x2, y1, y2);

        let (left, top, right, bottom) = self.position;
        quad.set_position(left, top, right, bottom);

        quad.set_has_color_impl(self.has_color);
        let [hue, saturation, brightness] = self.hsv;
        quad.set_hsv(Some(HsbTransform {
            hue,
            saturation,
            brightness,
        }));
        quad.set_fg_color(LinearRgba::with_components(
            self.fg_color[0],
            self.fg_color[1],
            self.fg_color[2],
            self.fg_color[3],
        ));
        quad.set_alt_color_and_mix_value(self.alt_color.into(), self.mix_value);

        vert
    }
}

pub struct HeapQuadAllocator {
    layer0: Vec<Box<BoxedQuad>>,
    layer1: Vec<Box<BoxedQuad>>,
    layer2: Vec<Box<BoxedQuad>>,
    // Parallel Cairo2D cache data storage - only used when caching for Cairo2D backend
    cache0: Vec<Cairo2DCacheData>,
    cache1: Vec<Cairo2DCacheData>,
    cache2: Vec<Cairo2DCacheData>,
    // Controls whether to use Cairo2D-specific caching behavior
    uses_cairo2d: bool,
}

impl std::fmt::Debug for HeapQuadAllocator {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt.debug_struct("HeapQuadAllocator").finish()
    }
}

impl HeapQuadAllocator {
    pub fn new(uses_cairo2d: bool) -> Self {
        Self {
            layer0: Vec::new(),
            layer1: Vec::new(),
            layer2: Vec::new(),
            cache0: Vec::new(),
            cache1: Vec::new(),
            cache2: Vec::new(),
            uses_cairo2d,
        }
    }

    pub fn apply_to(&self, other: &mut TripleLayerQuadAllocator) -> anyhow::Result<()> {
        let start = std::time::Instant::now();
        if self.uses_cairo2d {
            for (layer_num, quads, cache) in [
                (0, &self.layer0, &self.cache0),
                (1, &self.layer1, &self.cache1),
                (2, &self.layer2, &self.cache2),
            ] {
                for (idx, quad) in quads.iter().enumerate() {
                    let cache_slice = if idx < cache.len() {
                        &cache[idx..idx + 1]
                    } else {
                        &[]
                    };
                    other.extend_with_cairo2d(layer_num, &quad.to_vertices(), cache_slice);
                }
            }
        } else {
            // Original GPU path - no cache data
            for (layer_num, quads) in [(0, &self.layer0), (1, &self.layer1), (2, &self.layer2)] {
                for quad in quads {
                    other.extend_with(layer_num, &quad.to_vertices());
                }
            }
        }
        metrics::histogram!("quad_buffer_apply").record(start.elapsed());
        Ok(())
    }
}

impl TripleLayerQuadAllocatorTrait for HeapQuadAllocator {
    fn allocate(&mut self, layer_num: usize) -> anyhow::Result<QuadImpl<'_>> {
        let (quads, cache) = match layer_num {
            0 => (&mut self.layer0, &mut self.cache0),
            1 => (&mut self.layer1, &mut self.cache1),
            2 => (&mut self.layer2, &mut self.cache2),
            _ => unreachable!(),
        };

        quads.push(Box::new(BoxedQuad::default()));

        if self.uses_cairo2d {
            cache.push(Cairo2DCacheData::default());
            let quad = quads.last_mut().unwrap();
            let cache_data = cache.last_mut().unwrap();
            Ok(QuadImpl::HeapCairo2D(HeapCairo2DQuad { quad, cache_data }))
        } else {
            // Original GPU behavior
            let quad = quads.last_mut().unwrap();
            Ok(QuadImpl::Boxed(quad))
        }
    }

    /// Original GPU method - extends with vertices only
    fn extend_with(&mut self, layer_num: usize, vertices: &[Vertex]) {
        if vertices.is_empty() {
            return;
        }

        let dest_quads = match layer_num {
            0 => &mut self.layer0,
            1 => &mut self.layer1,
            2 => &mut self.layer2,
            _ => unreachable!(),
        };

        assert_eq!(vertices.len() % VERTICES_PER_CELL, 0);
        let src_quads: &[[Vertex; VERTICES_PER_CELL]] =
            unsafe { std::slice::from_raw_parts(vertices.as_ptr().cast(), vertices.len() / 4) };

        for quad in src_quads {
            dest_quads.push(Box::new(BoxedQuad::from_vertices(quad)));
        }
    }

    /// Cairo2D specific - delegates to extend_with then handles cache
    fn extend_with_cairo2d(
        &mut self,
        layer_num: usize,
        vertices: &[Vertex],
        cache_data: &[Cairo2DCacheData],
    ) {
        // First do the vertex extension
        self.extend_with(layer_num, vertices);

        // Then handle cache data if we're in Cairo2D mode
        if self.uses_cairo2d && !cache_data.is_empty() {
            let dest_cache = match layer_num {
                0 => &mut self.cache0,
                1 => &mut self.cache1,
                2 => &mut self.cache2,
                _ => unreachable!(),
            };
            for cd in cache_data {
                dest_cache.push(*cd);
            }
        }
    }
}

pub enum TripleLayerQuadAllocator<'a> {
    Gpu(BorrowedLayers),
    Heap(&'a mut HeapQuadAllocator),
}

impl<'a> TripleLayerQuadAllocatorTrait for TripleLayerQuadAllocator<'a> {
    fn allocate(&mut self, layer_num: usize) -> anyhow::Result<QuadImpl<'_>> {
        match self {
            Self::Gpu(b) => b.allocate(layer_num),
            Self::Heap(h) => h.allocate(layer_num),
        }
    }

    fn extend_with(&mut self, layer_num: usize, vertices: &[Vertex]) {
        match self {
            Self::Gpu(b) => b.extend_with(layer_num, vertices),
            Self::Heap(h) => h.extend_with(layer_num, vertices),
        }
    }

    fn extend_with_cairo2d(
        &mut self,
        layer_num: usize,
        vertices: &[Vertex],
        cache_data: &[Cairo2DCacheData],
    ) {
        match self {
            Self::Gpu(b) => b.extend_with_cairo2d(layer_num, vertices, cache_data),
            Self::Heap(h) => h.extend_with_cairo2d(layer_num, vertices, cache_data),
        }
    }
}

#[cfg(test)]
#[test]
fn size() {
    // Vertex: 68 bytes (GPU-only data, Cairo2D cache data stored separately)
    // 4 vertices per cell = 272 bytes
    assert_eq!(std::mem::size_of::<Vertex>() * VERTICES_PER_CELL, 272);
    // BoxedQuad: 84 bytes (GPU-only data)
    assert_eq!(std::mem::size_of::<BoxedQuad>(), 84);
}
