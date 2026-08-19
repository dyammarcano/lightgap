//! Matching a visual profile to the two devices actually involved.
//!
//! The protocol is symmetric, but the hardware is not. A laptop has a big
//! display and a mediocre fixed-focus webcam; a phone has a small display and an
//! excellent autofocus rear camera. Every pairing works, but each one has a
//! different sweet spot, and picking one profile for all of them wastes most of
//! whichever advantage is present.
//!
//! The key asymmetry: **the transmit profile is set by my display and the
//! peer's camera**, never by my own camera. My camera constrains what I can
//! receive, which is the peer's problem to solve. Getting this backwards is easy
//! and produces a link that is mysteriously worse in one direction.
//!
//! That is also why a phone paired with a laptop tends to be the best
//! combination: the laptop contributes the large display and the phone
//! contributes the good camera, so each device is used for its strength.

use crate::encode::{encode, Ecc};
use crate::geometry::MIN_PIXELS_PER_MODULE;

/// What kind of device this is.
///
/// Used only for sensible defaults and for wording the UI. Nothing in the
/// protocol branches on it: the numbers below are what actually decide, and a
/// device that misreports its own kind still gets a correct profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormFactor {
    /// External display, typically with a separate webcam.
    Desktop,
    /// Built-in display, front-facing webcam above it.
    Laptop,
    Tablet,
    Phone,
}

impl FormFactor {
    /// Whether the device's best camera and its display face the same way.
    ///
    /// They never do on any current form factor, which is why no device can see
    /// its own screen and every pairing needs two physical devices. Stated as
    /// code rather than as a comment because it is the constraint people keep
    /// trying to design around.
    #[must_use]
    pub const fn can_see_own_display(self) -> bool {
        false
    }
}

/// What a device brings to the visual channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualCapabilities {
    /// Pixels available to draw the code into, after UI chrome. Usually the
    /// shorter side is the binding one, since the code is square.
    pub display_px: (u32, u32),
    /// Camera capture resolution.
    pub camera_px: (u32, u32),
    pub form_factor: FormFactor,
}

impl VisualCapabilities {
    /// The side of the largest square that fits the display area.
    #[must_use]
    pub fn display_square_px(&self) -> u32 {
        self.display_px.0.min(self.display_px.1)
    }

    /// The side of the largest square the camera can resolve.
    #[must_use]
    pub fn camera_square_px(&self) -> u32 {
        self.camera_px.0.min(self.camera_px.1)
    }

    /// Plausible defaults per form factor, for before real values are known.
    ///
    /// Deliberately conservative: a first profile that undershoots costs some
    /// throughput for a few seconds until calibration corrects it, whereas one
    /// that overshoots means the peer reads nothing and calibration has no
    /// signal to work from.
    #[must_use]
    pub fn typical(form_factor: FormFactor) -> Self {
        match form_factor {
            FormFactor::Desktop => Self {
                display_px: (1920, 1080),
                camera_px: (1280, 720),
                form_factor,
            },
            FormFactor::Laptop => Self {
                display_px: (1512, 982),
                camera_px: (1280, 720),
                form_factor,
            },
            FormFactor::Tablet => Self {
                display_px: (1640, 2360),
                camera_px: (1920, 1080),
                form_factor,
            },
            FormFactor::Phone => Self {
                display_px: (1080, 2340),
                // A modern phone's rear camera comfortably out-resolves any
                // laptop webcam. This is the single biggest reason to support
                // mobile at all.
                camera_px: (1920, 1080),
                form_factor,
            },
        }
    }
}

/// Quiet-zone modules on each side, as the standard requires.
///
/// Counted when sizing because it occupies camera pixels exactly like the code
/// does. Ignoring it makes every profile optimistic by `(modules + 8) / modules`
/// — about 14% at 57 modules — which is enough to put a link that the numbers
/// say is comfortable below the threshold in practice.
pub const QUIET_MODULES: u32 = 4;

/// Fraction of the receiving camera's frame the code is expected to occupy once
/// the user has framed it reasonably.
///
/// Not 1.0: the quiet zone, the alignment margin and the fact that nobody holds
/// two devices perfectly still all eat into it. Measured framings that people
/// actually achieve sit around 0.6 to 0.8.
pub const EXPECTED_FRAME_FILL: f32 = 0.7;

/// A negotiated visual profile for one direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VisualProfile {
    /// Modules per side of the code to emit.
    pub modules: u32,
    /// Error correction level.
    pub ecc: Ecc,
    /// Usable payload bytes per frame at that size and level.
    pub payload_bytes: usize,
    /// Side length in display pixels the code should be drawn at.
    pub display_side_px: u32,
    /// Pixels per module the peer's camera is expected to resolve.
    pub expected_pixels_per_module: f32,
}

/// Largest QR version whose module count fits within `modules`.
///
/// Versions run 1 to 40 and a version `v` has `17 + 4v` modules per side.
fn version_for_modules(modules: u32) -> Option<u8> {
    if modules < 21 {
        return None;
    }
    let v = (modules.saturating_sub(17)) / 4;
    Some(v.clamp(1, 40) as u8)
}

/// Module count of a given version.
#[must_use]
pub fn modules_for_version(version: u8) -> u32 {
    17 + 4 * u32::from(version.clamp(1, 40))
}

/// Payload capacity of a version at a correction level, in bytes.
///
/// Measured by probing rather than looked up in a table, for the same reason
/// [`crate::encode::max_payload`] is: it reports what the encoder in use will
/// actually accept for incompressible data, which is what our PDUs are.
#[must_use]
pub fn capacity(version: u8, ecc: Ecc) -> usize {
    let modules = modules_for_version(version);
    let mut lo = 0usize;
    let mut hi = 3000usize;
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        match encode(&vec![0u8; mid], ecc) {
            // The encoder picks the smallest version that fits, so a payload
            // that produces a larger code than we budgeted for does not fit our
            // budget even though it encoded successfully.
            Ok(m) if m.size() as u32 <= modules => lo = mid,
            _ => hi = mid - 1,
        }
    }
    lo
}

/// Chooses a transmit profile from my display and the peer's camera.
///
/// Returns `None` when the peer's camera cannot resolve even the smallest code
/// at the expected framing. That is a real outcome, not an edge case: a 480p
/// webcam looking at a phone held at arm's length genuinely cannot do this, and
/// answering with a profile anyway would start a session that can never work.
#[must_use]
pub fn suggest_profile(
    mine: &VisualCapabilities,
    peer: &VisualCapabilities,
    ecc: Ecc,
) -> Option<VisualProfile> {
    // How many camera pixels will land on the code.
    let camera_px_on_code = peer.camera_square_px() as f32 * EXPECTED_FRAME_FILL;

    // How many modules those pixels can resolve, at the measured reliability
    // threshold rather than the standard's theoretical minimum — and counting
    // the quiet zone, which consumes camera pixels just as the code does.
    let resolvable_total = (camera_px_on_code / MIN_PIXELS_PER_MODULE).floor() as u32;
    let resolvable_modules = resolvable_total.saturating_sub(QUIET_MODULES * 2);

    // My display also has to be able to draw them. This constraint is far weaker
    // — one pixel per module suffices to render — but on a small display showing
    // a very dense code it can bind.
    let drawable_modules = mine.display_square_px();

    let modules = resolvable_modules.min(drawable_modules);
    let version = version_for_modules(modules)?;
    let actual_modules = modules_for_version(version);

    let payload_bytes = capacity(version, ecc);
    if payload_bytes == 0 {
        return None;
    }

    // Draw as large as the display allows: the peer's camera cannot resolve what
    // was never drawn, and unused display area is throughput left on the table.
    let display_side_px = mine.display_square_px();

    let expected_pixels_per_module = (peer.camera_square_px() as f32 * EXPECTED_FRAME_FILL)
        / (actual_modules + QUIET_MODULES * 2) as f32;

    Some(VisualProfile {
        modules: actual_modules,
        ecc,
        payload_bytes,
        display_side_px,
        expected_pixels_per_module,
    })
}

/// Chooses the correction level that maximises payload while still resolving.
///
/// Tries from most capacity to most robustness and keeps the first that yields a
/// profile. Higher correction is not automatically safer here: it shrinks the
/// payload for the same module count, so it only pays off when the link is
/// actually marginal — which calibration, not this function, is what discovers.
#[must_use]
pub fn suggest_best_profile(
    mine: &VisualCapabilities,
    peer: &VisualCapabilities,
) -> Option<VisualProfile> {
    Ecc::all()
        .iter()
        .filter_map(|ecc| suggest_profile(mine, peer, *ecc))
        .max_by_key(|p| p.payload_bytes)
}

/// Both directions of a pairing, which are generally different.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PairProfile {
    pub a_to_b: Option<VisualProfile>,
    pub b_to_a: Option<VisualProfile>,
}

impl PairProfile {
    /// Whether the pairing can carry data at all, in either direction.
    #[must_use]
    pub fn usable(&self) -> bool {
        self.a_to_b.is_some() || self.b_to_a.is_some()
    }

    /// Whether both directions work, which is what a bidirectional transfer or a
    /// visual-only acknowledgement path needs.
    #[must_use]
    pub fn bidirectional(&self) -> bool {
        self.a_to_b.is_some() && self.b_to_a.is_some()
    }
}

/// Works out both directions of a pairing.
#[must_use]
pub fn pair_profile(a: &VisualCapabilities, b: &VisualCapabilities) -> PairProfile {
    PairProfile {
        a_to_b: suggest_best_profile(a, b),
        b_to_a: suggest_best_profile(b, a),
    }
}
