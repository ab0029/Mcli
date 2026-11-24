use super::{HEIGHT, color};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::Rgb;
use rune::{Color, LineCap, Shape};

#[derive(Clone, Copy, PartialEq)]
pub(super) struct Decoration {
    underline: Flags,
    ink: Rgb,
    strike: Option<Rgb>,
}

impl Decoration {
    pub(super) fn from_cell(flags: Flags, fg: Rgb, underline_ink: Option<Rgb>) -> Option<Self> {
        let underline = flags & Flags::ALL_UNDERLINES;
        let strike = flags.contains(Flags::STRIKEOUT).then_some(fg);
        if flags.contains(Flags::HIDDEN) || (underline.is_empty() && strike.is_none()) {
            return None;
        }
        Some(Self {
            underline,
            ink: underline_ink.unwrap_or(fg),
            strike,
        })
    }

    /// Paint an uninterrupted run, including spaces and the second column of wide glyphs.
    pub(super) fn paint(self, mut shape: Shape, left: f32, right: f32) -> Shape {
        let ink = color(self.ink);
        let bottom = HEIGHT - 2.5;
        if let Some(strike) = self.strike {
            shape = shape.line(
                [left, HEIGHT * 0.53],
                [right, HEIGHT * 0.53],
                1.,
                LineCap::Butt,
                color(strike),
            );
        }
        if self.underline.contains(Flags::UNDERCURL) {
            let step = 2.;
            let mut pts = vec![[left, bottom - 1.]];
            let mut x = left + step;
            let mut rise = true;
            while x < right {
                pts.push([x, if rise { bottom - 2. } else { bottom }]);
                rise = !rise;
                x += step;
            }
            pts.push([right, bottom - 1.]);
            return shape.stroke_with(pts, false, 1., LineCap::Round, ink);
        }
        if self
            .underline
            .intersects(Flags::DOTTED_UNDERLINE | Flags::DASHED_UNDERLINE)
        {
            let (dash, gap) = if self.underline.contains(Flags::DOTTED_UNDERLINE) {
                (1., 2.)
            } else {
                (4., 3.)
            };
            let mut x = left;
            while x < right {
                shape = shape.line(
                    [x, bottom],
                    [(x + dash).min(right), bottom],
                    1.,
                    LineCap::Butt,
                    ink,
                );
                x += dash + gap;
            }
            return shape;
        }
        if self.underline.contains(Flags::DOUBLE_UNDERLINE) {
            shape = shape.line(
                [left, bottom - 2.],
                [right, bottom - 2.],
                1.,
                LineCap::Butt,
                ink,
            );
        }
        if !self.underline.is_empty() {
            shape = shape.line([left, bottom], [right, bottom], 1., LineCap::Butt, ink);
        }
        shape
    }
}

/// Mosaic blocks and box-drawing strokes meet at cell boundaries regardless of font bearings.
/// Consecutive horizontal bars and full-width blocks of the same colour coalesce into one shape.
pub(super) struct GridGlyphBatch {
    run: Option<HorizontalRun>,
}

#[derive(Clone, Copy)]
struct HorizontalRun {
    kind: RunKind,
    start: f32,
    end: f32,
    ink: Color,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RunKind {
    Rule(u8),
    Block(char),
}

fn horizontal_run_kind(ch: char) -> Option<RunKind> {
    match ch {
        '─' => Some(RunKind::Rule(1)),
        '━' => Some(RunKind::Rule(2)),
        '▀' | '▁'..='█' | '▔' => Some(RunKind::Block(ch)),
        _ => None,
    }
}

fn block_vertical_bounds(ch: char) -> Option<(f32, f32)> {
    match ch {
        '▀' => Some((0., 0.5)),
        '▁'..='█' => {
            let fraction = (ch as u32 - '▁' as u32 + 1) as f32 / 8.;
            Some((1. - fraction, 1.))
        }
        '▔' => Some((0., 0.125)),
        _ => None,
    }
}

impl GridGlyphBatch {
    pub(super) fn new() -> Self {
        Self { run: None }
    }

    pub(super) fn flush(&mut self, shape: &mut Shape, cell: f32, scale: f32) {
        let Some(run) = self.run.take() else {
            return;
        };
        let snap = |v: f32| (v * scale).round() / scale;
        let (l, r) = (snap(run.start), snap(run.end));
        let mut path = std::mem::take(shape);
        match run.kind {
            RunKind::Rule(weight) => {
                let stroke_px = ((cell * scale).round() / 6.).round().max(1.) * f32::from(weight);
                let weight = stroke_px / scale;
                let mid_y = snap(HEIGHT / 2. - weight / 2.) + weight / 2.;
                path = path.line([l, mid_y], [r, mid_y], weight, LineCap::Butt, run.ink);
            }
            RunKind::Block(ch) => {
                if let Some((top, bottom)) = block_vertical_bounds(ch) {
                    let (t, b) = (snap(top * HEIGHT), snap(bottom * HEIGHT));
                    path = path.fill([[l, t], [r, t], [r, b], [l, b]], run.ink);
                }
            }
        }
        *shape = path;
    }

    pub(super) fn push(
        &mut self,
        shape: &mut Shape,
        ch: char,
        x: f32,
        cell: f32,
        scale: f32,
        ink: Color,
    ) -> bool {
        if let Some(kind) = horizontal_run_kind(ch) {
            if let Some(run) = &mut self.run
                && run.kind == kind
                && run.ink == ink
                && (run.end - x).abs() < 0.01
            {
                run.end = x + cell;
                return true;
            }
            self.flush(shape, cell, scale);
            self.run = Some(HorizontalRun {
                kind,
                start: x,
                end: x + cell,
                ink,
            });
            return true;
        }
        self.flush(shape, cell, scale);
        grid_glyph(shape, ch, x, cell, scale, ink)
    }
}

pub(super) fn grid_glyph(
    shape: &mut Shape,
    ch: char,
    x: f32,
    cell: f32,
    scale: f32,
    ink: Color,
) -> bool {
    let snap = |v: f32| (v * scale).round() / scale;
    let rect = match ch {
        '▀' => Some([0., 0., 1., 0.5]),
        '▁'..='█' => {
            let fraction = (ch as u32 - '▁' as u32 + 1) as f32 / 8.;
            Some([0., 1. - fraction, 1., 1.])
        }
        '▉'..='▏' => {
            let fraction = ('▏' as u32 - ch as u32 + 1) as f32 / 8.;
            Some([0., 0., fraction, 1.])
        }
        '▐' => Some([0.5, 0., 1., 1.]),
        '▔' => Some([0., 0., 1., 0.125]),
        '▕' => Some([0.875, 0., 1., 1.]),
        _ => None,
    };
    if let Some([l, t, r, b]) = rect {
        let (l, r) = (snap(x + l * cell), snap(x + r * cell));
        let (t, b) = (snap(t * HEIGHT), snap(b * HEIGHT));
        *shape = std::mem::take(shape).fill([[l, t], [r, t], [r, b], [l, b]], ink);
        return true;
    }
    const LEFT: u8 = 1;
    const RIGHT: u8 = 2;
    const UP: u8 = 4;
    const DOWN: u8 = 8;
    let (arms, weight) = match ch {
        '─' => (LEFT | RIGHT, 1.),
        '━' => (LEFT | RIGHT, 2.),
        '│' => (UP | DOWN, 1.),
        '┃' => (UP | DOWN, 2.),
        '┌' | '╭' => (RIGHT | DOWN, 1.),
        '┏' => (RIGHT | DOWN, 2.),
        '┐' | '╮' => (LEFT | DOWN, 1.),
        '┓' => (LEFT | DOWN, 2.),
        '└' | '╰' => (RIGHT | UP, 1.),
        '┗' => (RIGHT | UP, 2.),
        '┘' | '╯' => (LEFT | UP, 1.),
        '┛' => (LEFT | UP, 2.),
        '├' => (RIGHT | UP | DOWN, 1.),
        '┣' => (RIGHT | UP | DOWN, 2.),
        '┤' => (LEFT | UP | DOWN, 1.),
        '┫' => (LEFT | UP | DOWN, 2.),
        '┬' => (LEFT | RIGHT | DOWN, 1.),
        '┳' => (LEFT | RIGHT | DOWN, 2.),
        '┴' => (LEFT | RIGHT | UP, 1.),
        '┻' => (LEFT | RIGHT | UP, 2.),
        '┼' => (LEFT | RIGHT | UP | DOWN, 1.),
        '╋' => (LEFT | RIGHT | UP | DOWN, 2.),
        _ => return false,
    };
    // Atlas sizes a light stroke from the device cell width, then snaps its edges.
    let stroke_px = ((cell * scale).round() / 6.).round().max(1.) * weight;
    let weight = stroke_px / scale;
    let center = |v: f32| snap(v - weight / 2.) + weight / 2.;
    let mid = [center(x + cell / 2.), center(HEIGHT / 2.)];
    let mut path = std::mem::take(shape);
    if matches!(ch, '╭' | '╮' | '╰' | '╯') {
        let sx = if arms & LEFT != 0 { -1. } else { 1. };
        let sy = if arms & UP != 0 { -1. } else { 1. };
        let radius = (cell / 2.).min(HEIGHT / 2.);
        let points = vec![
            [mid[0] + sx * cell / 2., mid[1]],
            [mid[0] + sx * radius, mid[1]],
            [mid[0] + sx * radius * 0.293, mid[1] + sy * radius * 0.293],
            [mid[0], mid[1] + sy * radius],
            [mid[0], mid[1] + sy * HEIGHT / 2.],
        ];
        path = path.stroke_with(points, false, weight, LineCap::Butt, ink);
    } else {
        if arms & (LEFT | RIGHT) != 0 {
            let start = if arms & LEFT != 0 { snap(x) } else { mid[0] };
            let end = if arms & RIGHT != 0 {
                snap(x + cell)
            } else {
                mid[0]
            };
            path = path.line([start, mid[1]], [end, mid[1]], weight, LineCap::Butt, ink);
        }
        if arms & (UP | DOWN) != 0 {
            let start = if arms & UP != 0 { 0. } else { mid[1] };
            let end = if arms & DOWN != 0 { HEIGHT } else { mid[1] };
            path = path.line([mid[0], start], [mid[0], end], weight, LineCap::Butt, ink);
        }
    }
    *shape = path;
    true
}
