//! Synthetic camera: turns a module matrix into the blurry, skewed, noisy frame
//! a webcam actually captures when pointed at a display.
//!
//! Without it, testing the visual channel requires two laptops, a room and a
//! pair of hands. With it, the whole thing fits in a test that runs in CI — and
//! the space of conditions (focus, angle, distance, light) can be swept
//! systematically, which never happens by hand.
//!
//! What is modelled, and why each piece:
//!
//! - **Perspective.** Two displays are never perfectly square to each other.
//! - **Blur.** A webcam's autofocus hunts, and fails at close range.
//! - **Noise.** In low light the sensor raises gain and gets dirty.
//! - **Contrast.** Display brightness against camera exposure; a black that
//!   arrives as mid grey ruins thresholding.
//! - **Moiré.** The display's pixel grid beating against the sensor's. It is the
//!   artefact specific to this medium and does not appear when photographing
//!   paper.

use crate::encode::Modules;

/// Quiet-zone modules around the code. Four is the standard's minimum; with less
/// the detector cannot tell where the code begins.
const QUIET_MODULES: usize = 4;

/// Capture conditions.
#[derive(Debug, Clone, PartialEq)]
pub struct Conditions {
    /// Camera frame size.
    pub frame_w: usize,
    pub frame_h: usize,
    /// Fraction of the frame's shorter side the code occupies, in 0..=1.
    pub fill: f32,
    /// Centre displacement, as a fraction of half the frame.
    pub offset_x: f32,
    pub offset_y: f32,
    /// Rotation in degrees.
    pub rotation_deg: f32,
    /// Horizontal and vertical tilt, in 0..=1. Zero is head-on.
    pub tilt_x: f32,
    pub tilt_y: f32,
    /// Gaussian blur radius, in pixels. Zero is perfect focus.
    pub blur: f32,
    /// Noise standard deviation, in grey levels.
    pub noise: f32,
    /// Contrast in 0..=1: 1 preserves pure black and white, lower values pull
    /// them toward grey.
    pub contrast: f32,
    /// Brightness offset, in grey levels, positive or negative.
    pub brightness: f32,
    /// Moiré strength, in 0..=1.
    pub moire: f32,
    /// Noise seed, so a failure reproduces exactly.
    pub seed: u64,
}

impl Default for Conditions {
    fn default() -> Self {
        Self {
            frame_w: 1280,
            frame_h: 720,
            fill: 0.7,
            offset_x: 0.0,
            offset_y: 0.0,
            rotation_deg: 0.0,
            tilt_x: 0.0,
            tilt_y: 0.0,
            blur: 0.0,
            noise: 0.0,
            contrast: 1.0,
            brightness: 0.0,
            moire: 0.0,
            seed: 1,
        }
    }
}

impl Conditions {
    /// Ideal capture: head-on, focused, noise-free. The control case.
    #[must_use]
    pub fn ideal() -> Self {
        Self::default()
    }

    /// A decent webcam on a desk: some tilt, imperfect focus and light noise,
    /// with the code well framed.
    ///
    /// Blur is relative to module size, not absolute: a 1.2 px radius is
    /// harmless over 8 px modules and devastating over 3 px ones. These values
    /// assume a framing that respects
    /// [`crate::geometry::MIN_PIXELS_PER_MODULE`].
    #[must_use]
    pub fn typical() -> Self {
        Self {
            fill: 0.75,
            tilt_x: 0.05,
            tilt_y: 0.03,
            rotation_deg: 2.0,
            blur: 0.8,
            noise: 4.0,
            contrast: 0.9,
            moire: 0.08,
            ..Self::default()
        }
    }

    /// Bad but still plausible conditions: unsteady hands, poor light, displays
    /// badly squared to each other.
    #[must_use]
    pub fn harsh() -> Self {
        Self {
            fill: 0.65,
            offset_x: 0.12,
            tilt_x: 0.14,
            tilt_y: 0.10,
            rotation_deg: 6.0,
            blur: 1.6,
            noise: 10.0,
            contrast: 0.75,
            brightness: 10.0,
            moire: 0.18,
            ..Self::default()
        }
    }
}

/// Reproducible noise generator. Cryptographic quality is not needed; producing
/// the same image from the same seed is.
struct Noise(u64);

impl Noise {
    fn next_u32(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 32) as u32
    }

    /// Gaussian approximation by summing uniforms: by the central limit theorem,
    /// twelve uniforms minus six give mean 0 and variance 1. The classic trick,
    /// and plenty for dirtying an image.
    fn gaussian(&mut self) -> f32 {
        let mut acc = 0.0f32;
        for _ in 0..12 {
            acc += self.next_u32() as f32 / u32::MAX as f32;
        }
        acc - 6.0
    }
}

/// A 3x3 matrix in row-major order.
type Mat3 = [f32; 9];

/// Homography from the unit square to the given quadrilateral.
///
/// The classic Heckbert construction. The affine case is handled separately
/// because the general one divides by a determinant that vanishes there.
fn unit_square_to_quad(q: [(f32, f32); 4]) -> Mat3 {
    let (x0, y0) = q[0];
    let (x1, y1) = q[1];
    let (x2, y2) = q[2];
    let (x3, y3) = q[3];

    let sx = x0 - x1 + x2 - x3;
    let sy = y0 - y1 + y2 - y3;

    if sx.abs() < 1e-6 && sy.abs() < 1e-6 {
        return [x1 - x0, x2 - x1, x0, y1 - y0, y2 - y1, y0, 0.0, 0.0, 1.0];
    }

    let dx1 = x1 - x2;
    let dx2 = x3 - x2;
    let dy1 = y1 - y2;
    let dy2 = y3 - y2;
    let den = dx1 * dy2 - dx2 * dy1;
    if den.abs() < 1e-9 {
        // Degenerate quadrilateral: fall back to affine rather than produce
        // infinities.
        return [x1 - x0, x2 - x1, x0, y1 - y0, y2 - y1, y0, 0.0, 0.0, 1.0];
    }

    let g = (sx * dy2 - dx2 * sy) / den;
    let h = (dx1 * sy - sx * dy1) / den;

    [
        x1 - x0 + g * x1,
        x3 - x0 + h * x3,
        x0,
        y1 - y0 + g * y1,
        y3 - y0 + h * y3,
        y0,
        g,
        h,
        1.0,
    ]
}

fn invert3(m: &Mat3) -> Option<Mat3> {
    let [a, b, c, d, e, f, g, h, i] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        (e * i - f * h) * inv,
        (c * h - b * i) * inv,
        (b * f - c * e) * inv,
        (f * g - d * i) * inv,
        (a * i - c * g) * inv,
        (c * d - a * f) * inv,
        (d * h - e * g) * inv,
        (b * g - a * h) * inv,
        (a * e - b * d) * inv,
    ])
}

fn apply(m: &Mat3, x: f32, y: f32) -> (f32, f32) {
    let w = m[6] * x + m[7] * y + m[8];
    if w.abs() < 1e-9 {
        return (f32::NAN, f32::NAN);
    }
    (
        (m[0] * x + m[1] * y + m[2]) / w,
        (m[3] * x + m[4] * y + m[5]) / w,
    )
}

/// Bilinear sampling, so resampling does not add steps the detector would
/// mistake for modules.
fn sample(src: &[u8], w: usize, h: usize, x: f32, y: f32) -> f32 {
    if x < 0.0 || y < 0.0 || x > (w - 1) as f32 || y > (h - 1) as f32 {
        return 255.0;
    }
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;

    let p00 = f32::from(src[y0 * w + x0]);
    let p10 = f32::from(src[y0 * w + x1]);
    let p01 = f32::from(src[y1 * w + x0]);
    let p11 = f32::from(src[y1 * w + x1]);

    let top = p00 + (p10 - p00) * fx;
    let bot = p01 + (p11 - p01) * fx;
    top + (bot - top) * fy
}

/// Separable Gaussian blur. Two one-dimensional passes instead of one
/// two-dimensional pass: same result, cost linear in the radius rather than
/// quadratic.
fn blur(buf: &mut [f32], w: usize, h: usize, sigma: f32) {
    if sigma <= 0.0 {
        return;
    }
    let radius = (sigma * 3.0).ceil() as isize;
    let mut kernel: Vec<f32> = (-radius..=radius)
        .map(|i| {
            let x = i as f32;
            (-(x * x) / (2.0 * sigma * sigma)).exp()
        })
        .collect();
    let sum: f32 = kernel.iter().sum();
    for k in &mut kernel {
        *k /= sum;
    }

    let mut tmp = vec![0.0f32; buf.len()];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (ki, k) in kernel.iter().enumerate() {
                let sx = (x as isize + ki as isize - radius).clamp(0, w as isize - 1) as usize;
                acc += buf[y * w + sx] * k;
            }
            tmp[y * w + x] = acc;
        }
    }
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (ki, k) in kernel.iter().enumerate() {
                let sy = (y as isize + ki as isize - radius).clamp(0, h as isize - 1) as usize;
                acc += tmp[sy * w + x] * k;
            }
            buf[y * w + x] = acc;
        }
    }
}

/// Synthetic capture: module matrix to greyscale camera frame.
///
/// Returns `(width, height, pixels)`.
#[must_use]
pub fn capture(modules: &Modules, cond: &Conditions) -> (usize, usize, Vec<u8>) {
    let (fw, fh) = (cond.frame_w, cond.frame_h);
    let side = (fw.min(fh) as f32) * cond.fill.clamp(0.05, 1.0);

    // The raster resolution is matched to the size the code will occupy in the
    // frame, rather than fixed high and shrunk afterwards.
    //
    // Shrinking a lot is exactly what produces aliasing, and aliasing has a
    // deceptive signature: the code fails at perfect focus and reads once
    // blurred, because blur acts as an anti-alias filter. That would lead to
    // concluding the link improves when defocused — the opposite of reality.
    // Keeping the source close to the destination lets the supersampling below
    // suffice.
    let total_modules = (modules.size() + QUIET_MODULES * 2) as f32;
    let scale = ((2.5 * side / total_modules).ceil() as usize).clamp(2, 12);
    let (sw, sh, src) = modules.render_greyscale(scale, QUIET_MODULES);

    let cx = fw as f32 / 2.0 + cond.offset_x * fw as f32 / 2.0;
    let cy = fh as f32 / 2.0 + cond.offset_y * fh as f32 / 2.0;

    // A centred square, rotated and then tilted. Tilt is applied by pulling two
    // corners in: that is what a display seen at an angle does.
    let r = side / 2.0;
    let rot = cond.rotation_deg.to_radians();
    let (sin, cos) = rot.sin_cos();
    let rotate = |dx: f32, dy: f32| (cx + dx * cos - dy * sin, cy + dx * sin + dy * cos);

    let tx = cond.tilt_x.clamp(0.0, 0.9);
    let ty = cond.tilt_y.clamp(0.0, 0.9);
    let quad = [
        rotate(-r, -r),
        rotate(r * (1.0 - tx), -r * (1.0 - ty)),
        rotate(r, r),
        rotate(-r * (1.0 - tx), r * (1.0 - ty)),
    ];

    let m = unit_square_to_quad(quad);
    let Some(inv) = invert3(&m) else {
        return (fw, fh, vec![255u8; fw * fh]);
    };

    // A light background: a lit display in an ordinary room is not surrounded by
    // black.
    let mut buf = vec![235.0f32; fw * fh];

    // Supersampling: each destination pixel averages SS x SS samples spread over
    // its area.
    //
    // Not a luxury. A real sensor INTEGRATES over the pixel's area; point
    // sampling produces aliasing when shrinking, with the deceptive signature
    // described above.
    const SS: usize = 3;

    // Only the quadrilateral's bounding box is walked: the rest of the frame is
    // background and supersampling it is wasted work. In a typical framing that
    // is a third of the pixels.
    let xs = quad.map(|p| p.0);
    let ys = quad.map(|p| p.1);
    let bx0 = xs
        .iter()
        .cloned()
        .fold(f32::INFINITY, f32::min)
        .floor()
        .max(0.0) as usize;
    let by0 = ys
        .iter()
        .cloned()
        .fold(f32::INFINITY, f32::min)
        .floor()
        .max(0.0) as usize;
    let bx1 = (xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max).ceil() as usize + 1).min(fw);
    let by1 = (ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max).ceil() as usize + 1).min(fh);

    for py in by0..by1 {
        for px in bx0..bx1 {
            let mut acc = 0.0f32;
            let mut inside = 0u32;
            for sy in 0..SS {
                for sx in 0..SS {
                    // Centres of SS x SS subcells covering the WHOLE pixel. With
                    // `1/(SS+1)` the samples bunch up in the middle and leave the
                    // pixel's real footprint — what the sensor integrates —
                    // uncovered.
                    let fx = px as f32 + (sx as f32 + 0.5) / SS as f32;
                    let fy = py as f32 + (sy as f32 + 0.5) / SS as f32;
                    let (u, v) = apply(&inv, fx, fy);
                    if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                        continue;
                    }
                    acc += sample(&src, sw, sh, u * (sw - 1) as f32, v * (sh - 1) as f32);
                    inside += 1;
                }
            }
            if inside == 0 {
                continue;
            }
            // Samples landing outside the code contribute background, so the
            // edge comes out smooth rather than stepped.
            let total = (SS * SS) as f32;
            let outside = total - inside as f32;
            buf[py * fw + px] = (acc + outside * 235.0) / total;
        }
    }

    // Moiré arises from the beat between the display grid and the sensor grid,
    // so it is modelled as a modulation near the pixel pitch rather than as
    // loose noise.
    if cond.moire > 0.0 {
        let amp = cond.moire.clamp(0.0, 1.0) * 40.0;
        for py in 0..fh {
            for px in 0..fw {
                let wave = ((px as f32 * 0.83).sin() * (py as f32 * 0.79).sin()) * amp;
                buf[py * fw + px] += wave;
            }
        }
    }

    blur(&mut buf, fw, fh, cond.blur);

    let mut rng = Noise(cond.seed | 1);
    let contrast = cond.contrast.clamp(0.05, 2.0);
    let mut out = vec![0u8; fw * fh];
    for (i, v) in buf.iter().enumerate() {
        // Contrast is applied around mid grey, which is where the detector's
        // decision threshold sits.
        let mut p = (v - 128.0) * contrast + 128.0 + cond.brightness;
        if cond.noise > 0.0 {
            p += rng.gaussian() * cond.noise;
        }
        out[i] = p.clamp(0.0, 255.0) as u8;
    }

    (fw, fh, out)
}
