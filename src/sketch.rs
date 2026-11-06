//! Hand-drawn geometry: the path commands that draw a mark the way a pen would have.
//!
//! Everything here is in canvas coordinates and free of tiny-skia, `Screen` and the document types, so
//! the wander can be tested without a pixmap and the renderer keeps the only canvas-to-screen
//! conversion. A mark is generated from an element's stored geometry and a seed hashed from that
//! element's anchor, so the same element draws the same mark on every frame, at every zoom, on every
//! output, and after a restart. Nothing here is ever stored.

/// Rough.js's `roughness` default: the single knob the look is scaled by, kept at its reference value.
pub const SKETCH_ROUGHNESS: f32 = 1.0;

/// The wander of a trace away from the stored geometry, in canvas pixels: Rough.js's
/// `maxRandomnessOffset` at the reference roughness.
pub const SKETCH_OFFSET: f32 = 2.0 * SKETCH_ROUGHNESS;

/// The wander is floored at this fraction of the pen's own weight: a 2 px wander is invisible on a
/// 12 px pen, and the wander is what the eye reads as hand-drawn.
pub const SKETCH_WEIGHT: f32 = 0.5;

/// The cap on the wander, so a very heavy pen is rough rather than scribbled over.
pub const SKETCH_OFFSET_MAX: f32 = 6.0;

/// The cap on how far a straight edge bows away from the line between its endpoints, in canvas pixels.
pub const SKETCH_BOW_MAX: f32 = 6.0;

/// Rough.js's divisor for the bow: a longer edge bows further, up to the cap above.
pub const SKETCH_BOW_DIVISOR: f32 = 200.0;

/// The samples a wandering closed loop is built from. Higher than Rough.js's nine because this
/// project joins samples through their midpoints rather than running a spline through them, and nine
/// midpoints let a large oval sag visibly towards a polygon.
pub const SKETCH_OVAL_STEPS: usize = 24;

/// How far any sketched ink can reach outside the geometry it was generated from. `canvas.rs` grows an
/// element's bounds by this, and `2.0 * SKETCH_OFFSET` covers the two traces' endpoint wander while
/// `SKETCH_BOW_MAX` covers the bow; a cubic's bulge is three quarters of its control points' offset, so
/// the sum is an upper bound rather than a near miss.
pub const SKETCH_SLACK: f32 = 2.0 * SKETCH_OFFSET + SKETCH_BOW_MAX;

/// One path command. `render` walks these and emits them through the view; the module itself never
/// touches a `tiny_skia` type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    Move([f32; 2]),
    Line([f32; 2]),
    Quad([f32; 2], [f32; 2]),
    Cubic([f32; 2], [f32; 2], [f32; 2]),
}

/// The two traces of a straight edge between two points. Each trace is one cubic whose control points
/// carry the bow and the wander, so a trace is one command rather than a sampled chain.
pub fn line(from: [f32; 2], to: [f32; 2], width: f32, seed: u64) -> Vec<Seg> {
    let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
    let len = (dx * dx + dy * dy).sqrt();
    if len <= f32::EPSILON {
        return Vec::new();
    }
    let offset = wander(width, len);
    let bow = (offset * len / SKETCH_BOW_DIVISOR).min(SKETCH_BOW_MAX);
    // Both traces bow to the same side: a hand that goes over a line twice bulges it the same way, and
    // the side is taken from the seed rather than from the stream so it does not depend on draw order.
    let side = if seed & 0x80 == 0 { -1.0 } else { 1.0 };
    let (nx, ny) = (-dy / len * side, dx / len * side);
    let mut segs = Vec::with_capacity(4);
    for pass in 0..2u64 {
        // The second trace is displaced by half as much, which is Rough.js's `_doubleLine`: two ends
        // that differ, and a middle where the two cross.
        let amount = if pass == 0 { offset } else { offset * 0.5 };
        let mut rng = Rng::new(seed ^ PASS_SALT[pass as usize]);
        let control = |frac: f32, rng: &mut Rng| {
            [
                from[0] + dx * frac + nx * bow + rng.bipolar(amount),
                from[1] + dy * frac + ny * bow + rng.bipolar(amount),
            ]
        };
        let start = [from[0] + rng.bipolar(amount), from[1] + rng.bipolar(amount)];
        let first = control(1.0 / 3.0, &mut rng);
        let second = control(2.0 / 3.0, &mut rng);
        let end = [to[0] + rng.bipolar(amount), to[1] + rng.bipolar(amount)];
        segs.push(Seg::Move(start));
        segs.push(Seg::Cubic(first, second, end));
    }
    segs
}

/// The per-trace salts, so trace two sees a different stream from trace one under the same seed.
const PASS_SALT: [u64; 2] = [0x243f_6a88_85a3_08d3, 0x1319_8a2e_0370_7344];

/// A closed shape as each of its edges traced twice. `rect` and `diamond` are the same function given
/// different corners, so which corners a kind has stays in the one place that already knows the kind.
/// The edges are separate traces rather than one closed loop, which is what a hand does and what the
/// round caps join.
pub fn closed(corners: &[[f32; 2]], width: f32, seed: u64) -> Vec<Seg> {
    if corners.len() < 3 {
        return Vec::new();
    }
    let mut segs = Vec::with_capacity(corners.len() * 4);
    for (index, (from, to)) in corners
        .iter()
        .zip(corners.iter().cycle().skip(1))
        .take(corners.len())
        .enumerate()
    {
        segs.extend(line(
            *from,
            *to,
            width,
            seed ^ EDGE_SALT.wrapping_mul(index as u64 + 1),
        ));
    }
    segs
}

const EDGE_SALT: u64 = 0x2545_f491_4f6c_dd1d;

/// A closed loop around a centre, traced twice. The loop is sampled at `SKETCH_OVAL_STEPS` points whose
/// radius wanders with two harmonics of the angle and whose centre drifts a little, then joined through
/// the midpoints of consecutive samples — the same construction a freehand stroke uses, which keeps the
/// whole loop inside the hull of its samples and therefore inside the slack the renderer reserves.
/// Running one spline through nine jittered points instead (Rough.js's sampling) bulges a large oval
/// outside the element's bounds.
pub fn ellipse(centre: [f32; 2], radii: [f32; 2], width: f32, seed: u64) -> Vec<Seg> {
    let (rx, ry) = (radii[0].abs(), radii[1].abs());
    if rx <= f32::EPSILON && ry <= f32::EPSILON {
        return Vec::new();
    }
    let offset = wander(width, 2.0 * rx.max(ry));
    let mut segs = Vec::with_capacity(SKETCH_OVAL_STEPS * 4);
    for pass in 0..2u64 {
        let amount = if pass == 0 { offset } else { offset * 0.5 };
        let mut rng = Rng::new(seed ^ PASS_SALT[pass as usize]);
        let phase = [rng.unit() * TAU, rng.unit() * TAU];
        let drift = [rng.bipolar(amount * 0.5), rng.bipolar(amount * 0.5)];
        let samples: Vec<[f32; 2]> = (0..SKETCH_OVAL_STEPS)
            .map(|step| {
                let angle = step as f32 / SKETCH_OVAL_STEPS as f32 * TAU;
                let wobble =
                    0.6 * (3.0 * angle + phase[0]).sin() + 0.4 * (5.0 * angle + phase[1]).sin();
                let wander = amount * wobble;
                [
                    centre[0] + drift[0] + (rx + wander) * angle.cos(),
                    centre[1] + drift[1] + (ry + wander) * angle.sin(),
                ]
            })
            .collect();
        segs.extend(ring(&samples));
    }
    segs
}

const TAU: f32 = std::f32::consts::TAU;

/// A closed chain through the midpoints of consecutive samples, with the samples as control points.
fn ring(samples: &[[f32; 2]]) -> Vec<Seg> {
    let Some(first) = samples.first() else {
        return Vec::new();
    };
    let last = samples[samples.len() - 1];
    let mut segs = Vec::with_capacity(samples.len() + 1);
    segs.push(Seg::Move(midpoint(last, *first)));
    for (index, sample) in samples.iter().enumerate() {
        let next = samples[(index + 1) % samples.len()];
        segs.push(Seg::Quad(*sample, midpoint(*sample, next)));
    }
    segs
}

fn midpoint(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0]
}

/// A freehand stroke traced twice, along the same quad-through-midpoints construction the exact path
/// uses, with every sample displaced. A stroke of fewer than two samples draws nothing here: a dot has
/// no direction to wander, so `render` keeps drawing it as an exact disc of the pen's own width.
pub fn stroke(points: &[[f32; 2]], width: f32, seed: u64) -> Vec<Seg> {
    if points.len() < 2 {
        return Vec::new();
    }
    let offset = wander(width, polyline_length(points));
    let mut segs = Vec::with_capacity(points.len() * 4);
    for pass in 0..2u64 {
        let amount = if pass == 0 { offset } else { offset * 0.5 };
        let mut rng = Rng::new(seed ^ PASS_SALT[pass as usize]);
        let jittered: Vec<[f32; 2]> = points
            .iter()
            .map(|p| [p[0] + rng.bipolar(amount), p[1] + rng.bipolar(amount)])
            .collect();
        segs.push(Seg::Move(jittered[0]));
        for index in 1..jittered.len() - 1 {
            segs.push(Seg::Quad(
                jittered[index],
                midpoint(jittered[index], jittered[index + 1]),
            ));
        }
        segs.push(Seg::Line(jittered[jittered.len() - 1]));
    }
    segs
}

fn polyline_length(points: &[[f32; 2]]) -> f32 {
    points
        .windows(2)
        .map(|w| ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2)).sqrt())
        .sum()
}

/// How far a trace may wander from the geometry it is generated from: the style's offset, raised to a
/// fraction of a heavy pen's weight and capped, then shrunk for a mark so short that the offset would
/// be its whole length.
fn wander(width: f32, len: f32) -> f32 {
    let offset = SKETCH_OFFSET
        .max(width * SKETCH_WEIGHT)
        .min(SKETCH_OFFSET_MAX);
    if offset * offset * 100.0 > len * len {
        len / 10.0
    } else {
        offset
    }
}

/// The seed a mark is drawn from: the element's kind, colour and width, and the single anchor it was
/// started from — the first sample of a stroke, the `from` point of a shape. Not the whole geometry,
/// because a stroke gains a sample per motion event and the mark must not re-roll under the hand.
pub fn seed_for(kind: u8, color: &str, width: f32, anchor: [f32; 2]) -> u64 {
    // FNV-1a over the element's own description. Nothing derived is stored, so this hash is what makes
    // the mark reproducible across frames, views and runs.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    feed(&[kind]);
    feed(color.as_bytes());
    feed(&width.to_bits().to_le_bytes());
    feed(&anchor[0].to_bits().to_le_bytes());
    feed(&anchor[1].to_bits().to_le_bytes());
    hash
}

/// A deterministic stream. `SplitMix64`, so a jitter is a pure function of the seed and how many values
/// were drawn before it — no shared mutable state anywhere.
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Rng(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A value in `[0, 1)`.
    pub(crate) fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u32 << 24) as f32
    }

    /// A value uniformly spread over `[-offset, offset]`.
    pub(crate) fn bipolar(&mut self, offset: f32) -> f32 {
        (self.unit() * 2.0 - 1.0) * offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTH: f32 = 3.0;
    const FROM: [f32; 2] = [10.0, 20.0];
    const TO: [f32; 2] = [210.0, 220.0];

    #[test]
    fn the_same_seed_draws_the_same_mark() {
        let seed = seed_for(0, "#e01b24", WIDTH, FROM);
        assert_eq!(line(FROM, TO, WIDTH, seed), line(FROM, TO, WIDTH, seed));
    }

    #[test]
    fn a_different_seed_draws_a_different_mark() {
        let one = line(FROM, TO, WIDTH, seed_for(0, "#e01b24", WIDTH, FROM));
        let other = line(FROM, TO, WIDTH, seed_for(0, "#e01b24", WIDTH, TO));
        assert_ne!(one, other);
    }

    #[test]
    fn a_mark_is_not_disturbed_by_another_mark_drawn_between_them() {
        let seed = seed_for(0, "#e01b24", WIDTH, FROM);
        let first = line(FROM, TO, WIDTH, seed);
        let _other = line([0.0, 0.0], [1.0, 1.0], WIDTH, seed ^ 0x5eed);
        assert_eq!(first, line(FROM, TO, WIDTH, seed));
    }

    #[test]
    fn the_seed_follows_kind_colour_weight_and_anchor() {
        let base = seed_for(0, "#e01b24", WIDTH, FROM);
        assert_eq!(base, seed_for(0, "#e01b24", WIDTH, FROM));
        assert_ne!(base, seed_for(1, "#e01b24", WIDTH, FROM));
        assert_ne!(base, seed_for(0, "#33d17a", WIDTH, FROM));
        assert_ne!(base, seed_for(0, "#e01b24", WIDTH + 1.0, FROM));
        assert_ne!(base, seed_for(0, "#e01b24", WIDTH, TO));
    }

    /// Every point the emitted commands actually draw, with each curve sampled — not just the control
    /// points, which legitimately sit further out than the curve they bend.
    fn drawn(segs: &[Seg]) -> Vec<[f32; 2]> {
        let mut out = Vec::new();
        let mut at = [0.0, 0.0];
        for seg in segs {
            match *seg {
                Seg::Move(p) | Seg::Line(p) => {
                    at = p;
                    out.push(p);
                }
                Seg::Quad(c, p) => {
                    for step in 0..=16 {
                        let t = step as f32 / 16.0;
                        out.push(lerp2(lerp2(at, c, t), lerp2(c, p, t), t));
                    }
                    at = p;
                }
                Seg::Cubic(c1, c2, p) => {
                    for step in 0..=16 {
                        let t = step as f32 / 16.0;
                        let a = lerp2(at, c1, t);
                        let b = lerp2(c1, c2, t);
                        let c = lerp2(c2, p, t);
                        out.push(lerp2(lerp2(a, b, t), lerp2(b, c, t), t));
                    }
                    at = p;
                }
            }
        }
        out
    }

    fn lerp2(a: [f32; 2], b: [f32; 2], t: f32) -> [f32; 2] {
        [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
    }

    /// How far the furthest drawn point sits from the line between `from` and `to`.
    fn reach(segs: &[Seg], from: [f32; 2], to: [f32; 2]) -> f32 {
        let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
        let len = (dx * dx + dy * dy).sqrt().max(f32::EPSILON);
        drawn(segs)
            .into_iter()
            .map(|p| ((p[0] - from[0]) * dy - (p[1] - from[1]) * dx).abs() / len)
            .fold(0.0f32, f32::max)
    }

    fn line_reach(from: [f32; 2], to: [f32; 2], width: f32) -> f32 {
        let seed = seed_for(0, "#e01b24", width, from);
        reach(&line(from, to, width, seed), from, to)
    }

    #[test]
    fn a_line_is_traced_twice() {
        let segs = line(FROM, TO, WIDTH, seed_for(0, "#e01b24", WIDTH, FROM));
        let traces = segs.iter().filter(|s| matches!(s, Seg::Move(_))).count();
        assert_eq!(traces, 2, "a mark is a pen going over the same line twice");
        assert!(
            line_reach(FROM, TO, WIDTH) > 0.0,
            "and neither trace is exact"
        );
    }

    #[test]
    fn a_line_stays_inside_the_slack_it_declares() {
        for (from, to) in [
            (FROM, TO),
            ([0.0, 0.0], [4000.0, 0.0]),
            ([0.0, 0.0], [6.0, 3.0]),
            ([100.0, 100.0], [100.0, 140.0]),
        ] {
            for width in [1.0, 3.0, 20.0] {
                assert!(
                    line_reach(from, to, width) <= SKETCH_SLACK,
                    "{from:?}..{to:?} at width {width} reached further than the declared slack"
                );
            }
        }
    }

    #[test]
    fn a_short_edge_is_not_moved_like_a_long_one() {
        let short = line_reach([0.0, 0.0], [10.0, 0.0], WIDTH);
        let long = line_reach([0.0, 0.0], [1000.0, 0.0], WIDTH);
        assert!(short < long, "short {short} long {long}");
        assert!(
            short < 2.0,
            "a 10 px edge is not thickened into a blob: {short}"
        );
    }

    #[test]
    fn a_heavy_line_wanders_further_than_a_light_one() {
        let light = line_reach(FROM, TO, 1.0);
        let heavy = line_reach(FROM, TO, 24.0);
        assert!(heavy > light, "heavy {heavy} light {light}");
    }

    #[test]
    fn a_line_with_no_length_draws_nothing() {
        assert!(line(FROM, FROM, WIDTH, 7).is_empty());
    }

    fn box_corners(from: [f32; 2], to: [f32; 2]) -> Vec<[f32; 2]> {
        vec![from, [to[0], from[1]], to, [from[0], to[1]]]
    }

    #[test]
    fn a_closed_shape_is_its_edges_traced_twice() {
        let corners = box_corners(FROM, TO);
        let segs = closed(&corners, WIDTH, seed_for(1, "#e01b24", WIDTH, FROM));
        let traces = segs.iter().filter(|s| matches!(s, Seg::Move(_))).count();
        assert_eq!(traces, corners.len() * 2);
    }

    #[test]
    fn a_closed_shape_stays_inside_the_box_and_its_slack() {
        let corners = box_corners(FROM, TO);
        let segs = closed(&corners, WIDTH, seed_for(1, "#e01b24", WIDTH, FROM));
        for p in drawn(&segs) {
            assert!(
                p[0] >= FROM[0] - SKETCH_SLACK
                    && p[0] <= TO[0] + SKETCH_SLACK
                    && p[1] >= FROM[1] - SKETCH_SLACK
                    && p[1] <= TO[1] + SKETCH_SLACK,
                "{p:?} left the box's slack"
            );
        }
    }

    #[test]
    fn a_closed_shape_actually_reaches_every_corner() {
        let corners = box_corners(FROM, TO);
        let segs = closed(&corners, WIDTH, seed_for(1, "#e01b24", WIDTH, FROM));
        let points = drawn(&segs);
        for corner in &corners {
            let nearest = points
                .iter()
                .map(|p| ((p[0] - corner[0]).powi(2) + (p[1] - corner[1]).powi(2)).sqrt())
                .fold(f32::INFINITY, f32::min);
            assert!(
                nearest <= SKETCH_SLACK,
                "{corner:?} was missed by {nearest}"
            );
        }
    }

    #[test]
    fn a_closed_shape_with_two_corners_draws_nothing() {
        assert!(closed(&[FROM, TO], WIDTH, 7).is_empty());
    }

    const CENTRE: [f32; 2] = [400.0, 300.0];
    const RADII: [f32; 2] = [200.0, 120.0];

    fn oval_segs() -> Vec<Seg> {
        ellipse(CENTRE, RADII, WIDTH, seed_for(2, "#e01b24", WIDTH, FROM))
    }

    /// The loop's traced points, with the exact oval's normalised radius beside each.
    fn oval_reach(p: [f32; 2]) -> f32 {
        let x = (p[0] - CENTRE[0]) / RADII[0];
        let y = (p[1] - CENTRE[1]) / RADII[1];
        (x * x + y * y).sqrt()
    }

    #[test]
    fn an_oval_is_a_closed_loop_traced_twice() {
        let segs = oval_segs();
        let traces = segs.iter().filter(|s| matches!(s, Seg::Move(_))).count();
        assert_eq!(traces, 2);
        // Each loop has to come back to where it started, or the wander leaves a visible gap.
        let mut start = None;
        let mut last = [0.0, 0.0];
        for seg in &segs {
            match *seg {
                Seg::Move(p) => {
                    if let Some((first, end)) = start {
                        assert!(
                            close(first, end),
                            "a loop came back to {end:?} not {first:?}"
                        );
                    }
                    start = Some((p, p));
                    last = p;
                }
                Seg::Quad(_, p) | Seg::Line(p) | Seg::Cubic(_, _, p) => {
                    last = p;
                }
            }
        }
        let first = start.map(|(first, _)| first).unwrap_or(last);
        assert!(
            close(first, last),
            "the loops came back to {last:?} not {first:?}"
        );
    }

    fn close(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 0.01 && (a[1] - b[1]).abs() < 0.01
    }

    #[test]
    fn an_oval_wanders_off_the_exact_oval() {
        let furthest = drawn(&oval_segs())
            .into_iter()
            .map(|p| (oval_reach(p) - 1.0).abs())
            .fold(0.0f32, f32::max);
        assert!(furthest > 0.005, "the loop was the exact oval: {furthest}");
    }

    #[test]
    fn an_oval_stays_inside_its_box_and_its_slack() {
        for p in drawn(&oval_segs()) {
            let (dx, dy) = ((p[0] - CENTRE[0]).abs(), (p[1] - CENTRE[1]).abs());
            assert!(
                dx <= RADII[0] + SKETCH_SLACK && dy <= RADII[1] + SKETCH_SLACK,
                "{p:?} left the box's slack"
            );
        }
    }

    #[test]
    fn an_oval_with_no_size_draws_nothing() {
        assert!(ellipse(CENTRE, [0.0, 0.0], WIDTH, 7).is_empty());
    }

    fn sample_points() -> Vec<[f32; 2]> {
        (0..24)
            .map(|i| {
                let t = i as f32 / 23.0;
                [20.0 + t * 400.0, 300.0 + (t * 6.0).sin() * 80.0]
            })
            .collect()
    }

    fn distance_to_polyline(p: [f32; 2], points: &[[f32; 2]]) -> f32 {
        points
            .windows(2)
            .map(|w| {
                let (a, b) = (w[0], w[1]);
                let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                let len2 = dx * dx + dy * dy;
                let t = if len2 <= f32::EPSILON {
                    0.0
                } else {
                    (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0)
                };
                let (x, y) = (a[0] + dx * t, a[1] + dy * t);
                ((p[0] - x).powi(2) + (p[1] - y).powi(2)).sqrt()
            })
            .fold(f32::INFINITY, f32::min)
    }

    #[test]
    fn a_stroke_is_traced_twice() {
        let points = sample_points();
        let segs = stroke(&points, WIDTH, seed_for(0, "#e01b24", WIDTH, points[0]));
        let traces = segs.iter().filter(|s| matches!(s, Seg::Move(_))).count();
        assert_eq!(traces, 2);
    }

    #[test]
    fn a_stroke_stays_near_its_samples() {
        let points = sample_points();
        for width in [1.0, 3.0, 24.0] {
            let segs = stroke(&points, width, seed_for(0, "#e01b24", width, points[0]));
            let furthest = drawn(&segs)
                .into_iter()
                .map(|p| distance_to_polyline(p, &points))
                .fold(0.0f32, f32::max);
            assert!(
                furthest <= SKETCH_SLACK,
                "width {width} wandered {furthest} off its samples"
            );
        }
    }

    #[test]
    fn a_stroke_reaches_its_two_ends() {
        let points = sample_points();
        let segs = stroke(&points, WIDTH, seed_for(0, "#e01b24", WIDTH, points[0]));
        let drawn = drawn(&segs);
        for end in [points[0], points[points.len() - 1]] {
            let nearest = drawn
                .iter()
                .map(|p| ((p[0] - end[0]).powi(2) + (p[1] - end[1]).powi(2)).sqrt())
                .fold(f32::INFINITY, f32::min);
            assert!(
                nearest <= SKETCH_SLACK,
                "the trace missed {end:?} by {nearest}"
            );
        }
    }

    #[test]
    fn a_stroke_with_fewer_than_two_samples_is_the_callers_dot() {
        // A dot has no direction to wander: `render` keeps drawing it as an exact disc of the pen's
        // width, and this module has nothing to add to it.
        assert!(stroke(&[], WIDTH, 7).is_empty());
        assert!(stroke(&[FROM], WIDTH, 7).is_empty());
    }
}
