//! The operations-explorer start-screen pictogram: a small animated node constellation.
//!
//! Deliberately *not* the FLUX wordmark (that is [`crate::splash`]) — this is a graph of `◆` nodes
//! joined by box-drawing edges, with a pulse travelling the edges in sequence and nodes sparkling
//! as ops "fire". It reuses the splash kit ([`Rgb`], [`Cell`], [`lerp`], [`Pcg32`], the glow sine)
//! rather than growing a second animation stack.
//!
//! Every frame is a pure function of `(seed, frame index)`, so a test can pin the animation without
//! a terminal or a clock. Under a colorless theme the caller asks for [`Pictogram::static_frame`]
//! instead of stepping — the shape still reads, nothing moves and nothing is colored.

use crate::splash::{lerp, Cell, Pcg32, Rgb};

/// Canvas size. Small on purpose: it sits above a centered input on the start screen, so it has to
/// survive an 80×24 terminal with room to spare.
pub(crate) const PICTO_W: usize = 29;
pub(crate) const PICTO_H: usize = 9;

/// Sine period of the node shimmer, in frames — the splash's glow cadence.
const GLOW_PERIOD: u32 = 80;
/// Frames a single edge pulse takes to travel its full length.
const PULSE_FRAMES: u32 = 14;
/// Frames between one edge's pulse starting and the next edge's.
const PULSE_STAGGER: u32 = 5;

const NODE_HOT: Rgb = Rgb(0xff, 0xff, 0xff);
const NODE_BASE: Rgb = Rgb(0x00, 0xd7, 0xff);
const EDGE_HOT: Rgb = Rgb(0x00, 0xd7, 0xff);
const EDGE_BASE: Rgb = Rgb(0x00, 0x3f, 0x5c);

const BLANK: Cell = Cell {
    ch: ' ',
    fg: Rgb(0, 0, 0),
    bold: false,
};

/// Node positions on the [`PICTO_W`]×[`PICTO_H`] grid.
///
/// Hand-placed rather than generated: the constellation has to read as a *graph* at a glance, and a
/// random layout at this size produces mush about as often as it produces something legible.
/// The lattice is on a strict 4:2 pitch so every edge is either a pure horizontal or a diagonal
/// spanning exactly one intermediate row — the only two shapes [`edge_cells`] draws.
const NODES: [(usize, usize); 9] = [
    (14, 0), // apex
    (10, 2),
    (18, 2),
    (6, 4),
    (14, 4), // hub
    (22, 4),
    (10, 6),
    (18, 6),
    (14, 8), // base
];

/// Edges as node-index pairs. Order is the pulse order, so it reads as a signal propagating out
/// from the apex rather than as nine independent blinkers.
const EDGES: [(usize, usize); 14] = [
    (0, 1),
    (0, 2),
    (1, 3),
    (1, 4),
    (2, 4),
    (2, 5),
    (3, 4), // the horizontal spine through the hub
    (4, 5),
    (3, 6),
    (4, 6),
    (4, 7),
    (5, 7),
    (6, 8),
    (7, 8),
];

/// One composed pictogram frame. Background is the caller's; `' '` cells are transparent.
pub(crate) struct PictoFrame(pub [[Cell; PICTO_W]; PICTO_H]);

impl PictoFrame {
    fn set(&mut self, x: usize, y: usize, ch: char, fg: Rgb, bold: bool) {
        if x < PICTO_W && y < PICTO_H {
            self.0[y][x] = Cell { ch, fg, bold };
        }
    }
}

/// The cells an edge occupies, in travel order from `a` to `b`.
///
/// Restricted to the two shapes the box-drawing set renders cleanly at cell aspect: pure horizontal
/// and the 4:2 diagonal. [`NODES`] is laid out to only ever need those, so this never approximates
/// a slope. Only the interior cells are returned — the endpoints belong to the nodes, drawn last.
///
/// The step count is derived from the span rather than walked until it happens to land on the
/// target: a layout edit that produces a slope this cannot draw must fail an assertion, not spin.
fn edge_cells(a: (usize, usize), b: (usize, usize)) -> Vec<(usize, usize, char)> {
    let (ax, ay) = (a.0 as isize, a.1 as isize);
    let (bx, by) = (b.0 as isize, b.1 as isize);
    let (dx, dy) = (bx - ax, by - ay);
    debug_assert!(
        (dy == 0 && dx != 0) || (dy.abs() == 2 && dx.abs() == 4),
        "pictogram edges are horizontal or 4:2 diagonals; {a:?}->{b:?} is neither"
    );
    if dy == 0 {
        let ch = '─';
        let step = dx.signum();
        return (1..dx.abs())
            .map(|i| ((ax + step * i) as usize, ay as usize, ch))
            .collect();
    }
    // A 4:2 diagonal has exactly one intermediate row, so it draws exactly one glyph — at the
    // midpoint. Anything denser reads as a smear at this size.
    let ch = if (dx > 0) == (dy > 0) { '╲' } else { '╱' };
    vec![((ax + dx / 2) as usize, (ay + dy / 2) as usize, ch)]
}

/// A seeded, frame-indexed constellation animation.
pub(crate) struct Pictogram {
    seed: u64,
    frame: u32,
    edges: Vec<Vec<(usize, usize, char)>>,
}

impl Pictogram {
    pub(crate) fn new(seed: u64) -> Self {
        let edges = EDGES
            .iter()
            .map(|&(a, b)| edge_cells(NODES[a], NODES[b]))
            .collect();
        Pictogram {
            seed,
            frame: 0,
            edges,
        }
    }

    /// Advance one frame and compose it.
    pub(crate) fn next_frame(&mut self) -> PictoFrame {
        let frame = self.frame;
        self.frame = self.frame.wrapping_add(1);
        self.compose(frame, true)
    }

    /// The frame a colorless theme gets: shape only, no pulse, no sparkle, no color.
    pub(crate) fn static_frame(&self) -> PictoFrame {
        self.compose(0, false)
    }

    fn compose(&self, frame: u32, animate: bool) -> PictoFrame {
        let mut f = PictoFrame([[BLANK; PICTO_W]; PICTO_H]);

        for (i, cells) in self.edges.iter().enumerate() {
            // Each edge's pulse starts `PULSE_STAGGER` frames after the previous one's, and the
            // whole sequence loops once the last edge has finished travelling.
            let cycle = PULSE_FRAMES + PULSE_STAGGER * self.edges.len() as u32;
            let start = PULSE_STAGGER * i as u32;
            let local = (frame + cycle - start % cycle) % cycle;
            let head = if animate && local < PULSE_FRAMES {
                Some(local as f64 / f64::from(PULSE_FRAMES) * cells.len() as f64)
            } else {
                None
            };
            for (n, &(x, y, ch)) in cells.iter().enumerate() {
                let fg = match head {
                    // Brightest at the pulse head, falling off over ~3 cells behind it.
                    Some(h) => {
                        let d = (n as f64 - h).abs();
                        lerp(EDGE_HOT, EDGE_BASE, (d / 3.0).min(1.0))
                    }
                    None if animate => EDGE_BASE,
                    None => Rgb(0, 0, 0),
                };
                f.set(x, y, ch, fg, false);
            }
        }

        // Nodes last so an edge can never overwrite one.
        let mut rng = Pcg32::new(self.seed ^ u64::from(frame / 7));
        let glow = if animate {
            let t = f64::from(frame) / f64::from(GLOW_PERIOD) * std::f64::consts::TAU;
            (t.sin() * 0.5 + 0.5) * 0.55
        } else {
            0.0
        };
        for &(x, y) in NODES.iter() {
            // One node per 7-frame window sparkles; the seed decides which, so the sequence is
            // reproducible and the choice still looks unpatterned.
            let sparkle = animate && rng.gen_range(NODES.len()) == 0;
            let fg = if !animate {
                Rgb(0, 0, 0)
            } else if sparkle {
                NODE_HOT
            } else {
                lerp(NODE_BASE, NODE_HOT, glow)
            };
            f.set(x, y, '◆', fg, sparkle);
        }
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C-643 acceptance: animation frames are deterministic for a seed. Two pictograms seeded alike
    /// must produce byte-identical frame sequences — that is what lets the explorer's start screen
    /// be asserted at all, since otherwise every render is a fresh roll.
    #[test]
    fn frames_deterministic_for_seed() {
        let (mut a, mut b) = (Pictogram::new(0xC643), Pictogram::new(0xC643));
        for i in 0..40 {
            let (fa, fb) = (a.next_frame(), b.next_frame());
            assert_eq!(fa.0, fb.0, "frame {i} diverged for identical seeds");
        }
        // A different seed must actually differ somewhere, or "deterministic" would be satisfied
        // by a constant and the test would prove nothing.
        let mut c = Pictogram::new(0x1234);
        let mut d = Pictogram::new(0xC643);
        let differs = (0..40).any(|_| c.next_frame().0 != d.next_frame().0);
        assert!(differs, "two different seeds produced identical animations");
    }

    /// The shape must survive with no color and no motion: every node and every edge cell is still
    /// drawn, and nothing carries a color.
    #[test]
    fn static_frame_is_uncolored_and_still_a_graph() {
        let f = Pictogram::new(7).static_frame();
        let nodes = f.0.iter().flatten().filter(|c| c.ch == '◆').count();
        let edges =
            f.0.iter()
                .flatten()
                .filter(|c| matches!(c.ch, '─' | '│' | '╱' | '╲'))
                .count();
        assert_eq!(
            nodes,
            NODES.len(),
            "every node is drawn in the static frame"
        );
        assert!(edges > 20, "the edges are still drawn: {edges} cells");
        assert!(
            f.0.iter()
                .flatten()
                .all(|c| c.fg == Rgb(0, 0, 0) && !c.bold),
            "the static frame carries no color and no bold"
        );
    }

    /// The layout only ever needs h/v/2:1-diagonal edges. If someone moves a node such that an edge
    /// needs a slope this renderer cannot draw, the glyph run stops being uniform — catch it here
    /// rather than as a visual smear.
    #[test]
    fn every_edge_draws_a_single_uniform_glyph() {
        for &(a, b) in EDGES.iter() {
            let cells = edge_cells(NODES[a], NODES[b]);
            assert!(!cells.is_empty(), "edge {a}->{b} drew nothing");
            let first = cells[0].2;
            assert!(
                cells.iter().all(|c| c.2 == first),
                "edge {a}->{b} mixed glyphs: {cells:?}"
            );
            assert!(
                cells.iter().all(|&(x, y, _)| x < PICTO_W && y < PICTO_H),
                "edge {a}->{b} left the canvas"
            );
        }
    }
}
