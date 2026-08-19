//! How the code sits inside the frame.
//!
//! This feeds the alignment guidance in the UI. The measure that dominates is
//! `pixels_per_module`: below about eight and a half pixels per module — a
//! measured figure, see [`MIN_PIXELS_PER_MODULE`] — the detector starts failing
//! no matter how well centred the code is, and no other adjustment compensates.
//! That is why "move closer" is almost always the right advice when something is
//! wrong.

/// A point in frame coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    fn dist(self, other: Self) -> f32 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        (dx * dx + dy * dy).sqrt()
    }
}

/// Where and how the code is seen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QrGeometry {
    /// Corners in order: top-left, top-right, bottom-right, bottom-left.
    pub corners: [Point; 4],
    /// Mean side length in pixels.
    pub side_px: f32,
    /// Rotation from horizontal, in degrees, within -180..=180.
    pub rotation_deg: f32,
    /// How far it departs from a square, in 0..=1.
    ///
    /// Zero is a perfect square; it grows as the code is seen at an angle
    /// because the displays are not facing each other squarely.
    pub perspective_error: f32,
    /// Fraction of the frame area the code occupies, in 0..=1.
    pub frame_coverage: f32,
    /// Displacement of the centre from the frame centre, in 0..=1, where 1 is a
    /// corner.
    pub offset: f32,
    /// Modules per side of the detected code.
    pub modules: u32,
    /// Image pixels per module. The measure that decides readability.
    pub pixels_per_module: f32,
}

impl QrGeometry {
    /// Computes geometry from the four corners and the frame size.
    #[must_use]
    pub fn from_corners(corners: [Point; 4], modules: u32, frame_w: u32, frame_h: u32) -> Self {
        let [tl, tr, br, bl] = corners;

        let top = tl.dist(tr);
        let right = tr.dist(br);
        let bottom = br.dist(bl);
        let left = bl.dist(tl);
        let side_px = (top + right + bottom + left) / 4.0;

        // Foreshortening is estimated by comparing opposite sides: in a square
        // seen head-on they are equal, and the relative difference grows with
        // the angle.
        let perspective_error = if side_px > 0.0 {
            let h = (top - bottom).abs() / side_px;
            let v = (left - right).abs() / side_px;
            (h.max(v)).min(1.0)
        } else {
            1.0
        };

        let rotation_deg = (tr.y - tl.y).atan2(tr.x - tl.x).to_degrees();

        let frame_area = (frame_w as f32) * (frame_h as f32);
        let frame_coverage = if frame_area > 0.0 {
            (side_px * side_px / frame_area).min(1.0)
        } else {
            0.0
        };

        let cx = (tl.x + tr.x + br.x + bl.x) / 4.0;
        let cy = (tl.y + tr.y + br.y + bl.y) / 4.0;
        let offset = if frame_w > 0 && frame_h > 0 {
            let dx = (cx - frame_w as f32 / 2.0) / (frame_w as f32 / 2.0);
            let dy = (cy - frame_h as f32 / 2.0) / (frame_h as f32 / 2.0);
            (dx * dx + dy * dy).sqrt().min(1.0)
        } else {
            0.0
        };

        let pixels_per_module = if modules > 0 {
            side_px / modules as f32
        } else {
            0.0
        };

        Self {
            corners,
            side_px,
            rotation_deg,
            perspective_error,
            frame_coverage,
            offset,
            modules,
            pixels_per_module,
        }
    }
}

/// Pixels per module at which reading becomes reliable **in real conditions**.
///
/// **Measured, not assumed** — and measured twice, because the first
/// measurement was wrong in an instructive way. Sweeping fractional scales
/// through the synthetic camera
/// (`cargo run -p optical-codec --example threshold`):
///
/// | px/module | ideal capture | typical capture |
/// |---|---|---|
/// | 2.0-3.0 | 24-40% | 0% |
/// | 3.0-6.0 | 60-87% | 10-67% |
/// | 6.0-7.0 | 91-100% | 64-65% |
/// | 7.0-8.5 | 93-100% | 64-94% |
/// | 8.5+    | 100%   | 100%  |
///
/// The first pass measured only the ideal column and concluded 6.0. That number
/// is real but describes a capture with no blur, no noise and no tilt, which is
/// not a capture. Under conditions a webcam on a desk actually produces, the
/// same 6.0 px/module reads barely two frames in three — and a link dropping a
/// third of its frames is not a link, it is a retry loop.
///
/// The standard quotes 2 as an absolute minimum. That assumes a grid aligned to
/// the pixel; a camera scales fractionally, module edges land mid-pixel, and the
/// detector — which samples the module centre — gets confused. Four times the
/// theoretical minimum is what reality costs.
///
/// Practical consequence: at 720p with the code filling 70% of the height, about
/// 59 modules fit, which is roughly 270 B per frame at correction level L.
pub const MIN_PIXELS_PER_MODULE: f32 = 8.5;

/// What suffices when the capture really is clean — a still device, good light,
/// displays square to each other.
///
/// Kept as a separate constant rather than folded into the one above because
/// calibration can discover that a particular link is this good and push
/// density accordingly. What it must not do is *assume* it.
pub const IDEAL_PIXELS_PER_MODULE: f32 = 6.0;

/// Below this, reading fails more often than it succeeds.
///
/// Between this value and [`MIN_PIXELS_PER_MODULE`] the link works
/// intermittently: useful for not cutting the session at the first stumble, not
/// for operating.
pub const MARGINAL_PIXELS_PER_MODULE: f32 = 3.0;

/// Above this coverage the code brushes the frame edges and gets clipped at the
/// slightest movement.
pub const MAX_COVERAGE: f32 = 0.75;

/// Foreshortening beyond which the displays are worth straightening.
pub const MAX_PERSPECTIVE_ERROR: f32 = 0.20;

/// Centre displacement beyond which re-centring is worth suggesting.
pub const MAX_OFFSET: f32 = 0.35;

/// Laplacian variance below which the image is out of focus.
pub const MIN_SHARPNESS: f32 = 50.0;

/// What to tell whoever is holding the devices.
///
/// One piece of advice at a time, and the one that dominates: a list of five
/// things to fix at once does not get followed, and some fix themselves when
/// another is addressed — moving closer usually improves focus and coverage
/// along the way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advice {
    /// The link is in good shape.
    Ok,
    /// Far too few pixels per module.
    MoveCloser,
    /// The code fills the frame and will clip at the slightest movement.
    MoveAway,
    /// Off centre.
    Center,
    /// Too much foreshortening: the displays are not facing each other.
    Straighten,
    /// The image is blurry.
    Focus,
}

impl Advice {
    /// Short message for the UI.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Ok => "Optical link stable",
            Self::MoveCloser => "Move the devices closer",
            Self::MoveAway => "Move the devices apart",
            Self::Center => "Centre the code in the camera",
            Self::Straighten => "Face the displays to each other",
            Self::Focus => "Insufficient focus",
        }
    }
}

/// Picks the most urgent advice.
///
/// The order matters and is not arbitrary: without enough pixels per module
/// nothing else can be fixed, so it comes first. Focus comes before centring
/// because a blurry image makes the corners — and therefore every other measure
/// — untrustworthy.
#[must_use]
pub fn advise(geom: &QrGeometry, sharpness: f32) -> Advice {
    if geom.pixels_per_module < MIN_PIXELS_PER_MODULE {
        return Advice::MoveCloser;
    }
    if geom.frame_coverage > MAX_COVERAGE {
        return Advice::MoveAway;
    }
    if sharpness < MIN_SHARPNESS {
        return Advice::Focus;
    }
    if geom.perspective_error > MAX_PERSPECTIVE_ERROR {
        return Advice::Straighten;
    }
    if geom.offset > MAX_OFFSET {
        return Advice::Center;
    }
    Advice::Ok
}

/// Laplacian variance over a region: the usual sharpness measure.
///
/// A focused image has sharp edges, and the Laplacian — which responds to abrupt
/// changes — spikes at them. As focus softens, the edges smear and the variance
/// falls. A QR code is almost all edges, so the indicator is especially clear on
/// this class of image.
///
/// **Known limitation:** noise also raises the variance. A noisy, blurry image
/// can score higher than a clean, slightly blurry one. Using this as a focus
/// criterion in production calls for denoising first, or combining it with
/// another measure.
#[must_use]
pub fn sharpness(
    width: usize,
    height: usize,
    pixels: &[u8],
    region: Option<(usize, usize, usize, usize)>,
) -> f32 {
    if width < 3 || height < 3 || pixels.len() < width * height {
        return 0.0;
    }
    let (x0, y0, x1, y1) = region.unwrap_or((0, 0, width, height));
    let x0 = x0.max(1);
    let y0 = y0.max(1);
    let x1 = x1.min(width.saturating_sub(1));
    let y1 = y1.min(height.saturating_sub(1));
    if x1 <= x0 || y1 <= y0 {
        return 0.0;
    }

    let mut sum = 0.0f64;
    let mut sum_squares = 0.0f64;
    let mut n = 0u64;

    for y in y0..y1 {
        for x in x0..x1 {
            let i = y * width + x;
            // Four-neighbour Laplacian kernel.
            let lap = 4.0 * f64::from(pixels[i])
                - f64::from(pixels[i - 1])
                - f64::from(pixels[i + 1])
                - f64::from(pixels[i - width])
                - f64::from(pixels[i + width]);
            sum += lap;
            sum_squares += lap * lap;
            n += 1;
        }
    }

    if n == 0 {
        return 0.0;
    }
    let mean = sum / n as f64;
    ((sum_squares / n as f64) - mean * mean).max(0.0) as f32
}
