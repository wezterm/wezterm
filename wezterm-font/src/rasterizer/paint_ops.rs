//! Cairo-free paint operation types for COLR color font rendering.
//! These replace the cairo::Matrix, cairo::Operator, cairo::Extend
//! types used in the old colr.rs with pure-Rust equivalents backed
//! by tiny-skia.

use wezterm_color_types::SrgbaPixel;

/// 2D affine transform matrix (row-major), replacing cairo::Matrix.
/// Layout: [xx, yx, xy, yy, x0, y0] maps to:
///   | xx  xy  x0 |
///   | yx  yy  y0 |
///   |  0   0   1 |
#[derive(Debug, Clone, Copy)]
pub struct Transform {
    pub xx: f64,
    pub yx: f64,
    pub xy: f64,
    pub yy: f64,
    pub x0: f64,
    pub y0: f64,
}

impl Transform {
    pub fn identity() -> Self {
        Self {
            xx: 1.0,
            yx: 0.0,
            xy: 0.0,
            yy: 1.0,
            x0: 0.0,
            y0: 0.0,
        }
    }

    pub fn new(xx: f64, yx: f64, xy: f64, yy: f64, x0: f64, y0: f64) -> Self {
        Self {
            xx,
            yx,
            xy,
            yy,
            x0,
            y0,
        }
    }

    pub fn translate(&mut self, tx: f64, ty: f64) {
        self.x0 += self.xx * tx + self.xy * ty;
        self.y0 += self.yx * tx + self.yy * ty;
    }

    pub fn scale(&mut self, sx: f64, sy: f64) {
        self.xx *= sx;
        self.yx *= sx;
        self.xy *= sy;
        self.yy *= sy;
    }

    pub fn rotate(&mut self, angle: f64) {
        let s = angle.sin();
        let c = angle.cos();
        let new_xx = self.xx * c + self.xy * s;
        let new_yx = self.yx * c + self.yy * s;
        let new_xy = self.xx * -s + self.xy * c;
        let new_yy = self.yx * -s + self.yy * c;
        self.xx = new_xx;
        self.yx = new_yx;
        self.xy = new_xy;
        self.yy = new_yy;
    }

    /// Multiply: self = self * other
    pub fn multiply(&self, other: &Transform) -> Transform {
        Transform {
            xx: self.xx * other.xx + self.xy * other.yx,
            yx: self.yx * other.xx + self.yy * other.yx,
            xy: self.xx * other.xy + self.xy * other.yy,
            yy: self.yx * other.xy + self.yy * other.yy,
            x0: self.xx * other.x0 + self.xy * other.y0 + self.x0,
            y0: self.yx * other.x0 + self.yy * other.y0 + self.y0,
        }
    }

    /// Convert to tiny_skia::Transform
    pub fn to_tiny_skia(&self) -> tiny_skia::Transform {
        tiny_skia::Transform::from_row(
            self.xx as f32,
            self.yx as f32,
            self.xy as f32,
            self.yy as f32,
            self.x0 as f32,
            self.y0 as f32,
        )
    }
}

/// Porter-Duff and blend composite modes, replacing cairo::Operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositeMode {
    Clear,
    Source,
    Dest,
    Over,
    DestOver,
    In,
    DestIn,
    Out,
    DestOut,
    Atop,
    DestAtop,
    Xor,
    Add,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Multiply,
    HslHue,
    HslSaturation,
    HslColor,
    HslLuminosity,
}

impl CompositeMode {
    /// Convert to tiny_skia::BlendMode where possible.
    /// Some modes (Hsl*) are approximated as SrcOver.
    pub fn to_tiny_skia(&self) -> tiny_skia::BlendMode {
        match self {
            CompositeMode::Clear => tiny_skia::BlendMode::Clear,
            CompositeMode::Source => tiny_skia::BlendMode::Source,
            CompositeMode::Dest => tiny_skia::BlendMode::Destination,
            CompositeMode::Over => tiny_skia::BlendMode::SourceOver,
            CompositeMode::DestOver => tiny_skia::BlendMode::DestinationOver,
            CompositeMode::In => tiny_skia::BlendMode::SourceIn,
            CompositeMode::DestIn => tiny_skia::BlendMode::DestinationIn,
            CompositeMode::Out => tiny_skia::BlendMode::SourceOut,
            CompositeMode::DestOut => tiny_skia::BlendMode::DestinationOut,
            CompositeMode::Atop => tiny_skia::BlendMode::SourceAtop,
            CompositeMode::DestAtop => tiny_skia::BlendMode::DestinationAtop,
            CompositeMode::Xor => tiny_skia::BlendMode::Xor,
            CompositeMode::Add => tiny_skia::BlendMode::Plus,
            CompositeMode::Screen => tiny_skia::BlendMode::Screen,
            CompositeMode::Overlay => tiny_skia::BlendMode::Overlay,
            CompositeMode::Darken => tiny_skia::BlendMode::Darken,
            CompositeMode::Lighten => tiny_skia::BlendMode::Lighten,
            CompositeMode::ColorDodge => tiny_skia::BlendMode::ColorDodge,
            CompositeMode::ColorBurn => tiny_skia::BlendMode::ColorBurn,
            CompositeMode::HardLight => tiny_skia::BlendMode::HardLight,
            CompositeMode::SoftLight => tiny_skia::BlendMode::SoftLight,
            CompositeMode::Difference => tiny_skia::BlendMode::Difference,
            CompositeMode::Exclusion => tiny_skia::BlendMode::Exclusion,
            CompositeMode::Multiply => tiny_skia::BlendMode::Multiply,
            CompositeMode::HslHue => tiny_skia::BlendMode::Hue,
            CompositeMode::HslSaturation => tiny_skia::BlendMode::Saturation,
            CompositeMode::HslColor => tiny_skia::BlendMode::Color,
            CompositeMode::HslLuminosity => tiny_skia::BlendMode::Luminosity,
        }
    }
}

/// Gradient extend mode, replacing cairo::Extend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtendMode {
    Pad,
    Repeat,
    Reflect,
}

impl ExtendMode {
    pub fn to_tiny_skia(&self) -> tiny_skia::SpreadMode {
        match self {
            ExtendMode::Pad => tiny_skia::SpreadMode::Pad,
            ExtendMode::Repeat => tiny_skia::SpreadMode::Repeat,
            ExtendMode::Reflect => tiny_skia::SpreadMode::Reflect,
        }
    }
}

/// A color stop in a gradient.
#[derive(Clone, Debug)]
pub struct ColorStop {
    pub offset: f64,
    pub color: SrgbaPixel,
}

/// A color line (gradient definition with stops and extend mode).
#[derive(Clone, Debug)]
pub struct ColorLine {
    pub color_stops: Vec<ColorStop>,
    pub extend: ExtendMode,
}

/// Drawing operations for glyph outlines (Cairo-free).
#[derive(Debug, Clone)]
pub enum DrawOp {
    MoveTo {
        to_x: f32,
        to_y: f32,
    },
    LineTo {
        to_x: f32,
        to_y: f32,
    },
    QuadTo {
        control_x: f32,
        control_y: f32,
        to_x: f32,
        to_y: f32,
    },
    CubicTo {
        control1_x: f32,
        control1_y: f32,
        control2_x: f32,
        control2_y: f32,
        to_x: f32,
        to_y: f32,
    },
    ClosePath,
}

/// Paint operations for COLR color font rendering.
/// These are collected during the COLR table walk and then
/// rasterized by skia_colr::rasterize_paint_ops().
#[derive(Debug, Clone)]
pub enum PaintOp {
    PushTransform(Transform),
    PopTransform,
    PushClip(Vec<DrawOp>),
    PushRectClip {
        xmin: f32,
        ymin: f32,
        xmax: f32,
        ymax: f32,
    },
    PopClip,
    PaintSolid(SrgbaPixel),
    PaintLinearGradient {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        color_line: ColorLine,
    },
    PaintRadialGradient {
        x0: f32,
        y0: f32,
        r0: f32,
        x1: f32,
        y1: f32,
        r1: f32,
        color_line: ColorLine,
    },
    PaintSweepGradient {
        x0: f32,
        y0: f32,
        start_angle: f32,
        end_angle: f32,
        color_line: ColorLine,
    },
    PaintImage {
        data: Vec<u8>,
        width: u32,
        height: u32,
        is_png: bool,
        slant: f32,
        extents: Option<ImageExtents>,
    },
    PushGroup,
    PopGroup(CompositeMode),
}

/// Glyph image extents for PaintImage.
#[derive(Debug, Clone, Copy)]
pub struct ImageExtents {
    pub x_bearing: f32,
    pub y_bearing: f32,
    pub width: f32,
    pub height: f32,
}

/// Convert draw ops to a tiny_skia path.
pub fn draw_ops_to_path(ops: &[DrawOp]) -> tiny_skia::Path {
    let mut pb = tiny_skia::PathBuilder::new();
    for op in ops {
        match op {
            DrawOp::MoveTo { to_x, to_y } => {
                pb.move_to(*to_x, *to_y);
            }
            DrawOp::LineTo { to_x, to_y } => {
                pb.line_to(*to_x, *to_y);
            }
            DrawOp::QuadTo {
                control_x,
                control_y,
                to_x,
                to_y,
            } => {
                pb.quad_to(*control_x, *control_y, *to_x, *to_y);
            }
            DrawOp::CubicTo {
                control1_x,
                control1_y,
                control2_x,
                control2_y,
                to_x,
                to_y,
            } => {
                pb.cubic_to(
                    *control1_x,
                    *control1_y,
                    *control2_x,
                    *control2_y,
                    *to_x,
                    *to_y,
                );
            }
            DrawOp::ClosePath => {
                pb.close();
            }
        }
    }
    pb.finish().unwrap_or_else(|| {
        // Return an empty path as fallback
        let mut pb2 = tiny_skia::PathBuilder::new();
        pb2.move_to(0.0, 0.0);
        pb2.finish().unwrap()
    })
}
