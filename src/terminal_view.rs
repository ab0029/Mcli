use crate::terminal::{Session, Size, palette};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::selection::SelectionRange;
use alacritty_terminal::term::TermDamage;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor, Rgb};
use rune::{Color, Family, GridRow, Shape, Span, TextStyle};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
mod lines;
use lines::Decoration;

pub const HEIGHT: f32 = 19.0;
pub fn font() -> TextStyle {
    TextStyle::new(15.0, 400.0)
        .family(Family::SystemMono)
        .lh(HEIGHT)
}
pub fn color(rgb: Rgb) -> Color {
    static LINEAR: std::sync::LazyLock<[f32; 256]> = std::sync::LazyLock::new(|| {
        std::array::from_fn(|i| rune::color::srgb_decode(i as f32 / 255.))
    });
    Color {
        r: LINEAR[usize::from(rgb.r)],
        g: LINEAR[usize::from(rgb.g)],
        b: LINEAR[usize::from(rgb.b)],
        a: 1.0,
    }
}
fn resolve(c: AnsiColor, colors: &Colors) -> Rgb {
    let idx = match c {
        AnsiColor::Spec(rgb) => return rgb,
        AnsiColor::Named(n) => n as usize,
        AnsiColor::Indexed(i) => usize::from(i),
    };
    colors[idx].unwrap_or_else(|| palette(idx))
}

// Windows Terminal defaults to intenseTextStyle=bright. SGR 1 brightens legacy
// foreground indices but leaves explicit 256-colour and RGB values untouched.
pub(super) fn foreground(c: AnsiColor, flags: Flags, colors: &Colors) -> Rgb {
    if flags.contains(Flags::BOLD) {
        if let AnsiColor::Named(name) = c {
            if (name as usize) < 8 {
                return resolve(AnsiColor::Named(name.to_bright()), colors);
            }
            if name == NamedColor::Foreground {
                let default = resolve(c, colors);
                if let Some(i) =
                    (0..8).find(|&i| colors[i].unwrap_or_else(|| palette(i)) == default)
                {
                    return colors[i + 8].unwrap_or_else(|| palette(i + 8));
                }
            }
        }
    }
    resolve(c, colors)
}

pub fn viewport(w: f32, h: f32, cell: f32, scale: f32) -> Option<Size> {
    if w < cell * 2. || h < HEIGHT || !w.is_finite() || !h.is_finite() {
        return None;
    }
    Some(Size {
        cols: (w / cell).floor().clamp(2., 32767.) as u16,
        lines: (h / HEIGHT).floor().clamp(1., 32767.) as u16,
        cell_w: (cell * scale).round().max(1.) as u16,
        cell_h: (HEIGHT * scale).round().max(1.) as u16,
    })
}

type RowSnapshot = (Vec<Cell>, Option<usize>);

const ROW_POOL_LIMIT: usize = 256;

fn selection_color() -> Color {
    color(Rgb {
        r: 58,
        g: 79,
        b: 112,
    })
}

fn hash_ansi_color(c: AnsiColor, h: &mut impl Hasher) {
    match c {
        AnsiColor::Named(n) => {
            0u8.hash(h);
            (n as usize).hash(h);
        }
        AnsiColor::Indexed(i) => {
            1u8.hash(h);
            i.hash(h);
        }
        AnsiColor::Spec(rgb) => {
            2u8.hash(h);
            (rgb.r, rgb.g, rgb.b).hash(h);
        }
    }
}

fn is_default_blank(cell: &Cell) -> bool {
    cell.c == ' '
        && cell.flags.is_empty()
        && cell.fg == AnsiColor::Named(NamedColor::Foreground)
        && cell.bg == AnsiColor::Named(NamedColor::Background)
        && cell.underline_color().is_none()
        && cell.zerowidth().is_none()
}

fn trim_trailing_default_blanks(cells: &mut Vec<Cell>, block: Option<usize>) {
    let min_len = block.map_or(0, |x| x + 1).min(cells.len());
    let mut end = cells.len();
    while end > min_len && is_default_blank(&cells[end - 1]) {
        end -= 1;
    }
    cells.truncate(end);
}

fn hash_row(cells: &[Cell], block: Option<usize>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    cells.len().hash(&mut h);
    block.hash(&mut h);
    for cell in cells {
        cell.c.hash(&mut h);
        cell.flags.bits().hash(&mut h);
        hash_ansi_color(cell.fg, &mut h);
        hash_ansi_color(cell.bg, &mut h);
        if let Some(u) = cell.underline_color() {
            1u8.hash(&mut h);
            hash_ansi_color(u, &mut h);
        } else {
            0u8.hash(&mut h);
        }
        cell.zerowidth().hash(&mut h);
    }
    h.finish()
}

fn colors_eq(a: &Colors, b: &Colors) -> bool {
    (0..alacritty_terminal::term::color::COUNT).all(|i| a[i] == b[i])
}

pub struct Screen {
    pub rows: Vec<Rc<GridRow>>,
    pub selection_fills: Vec<(u16, u16, u16, Color)>,
    pub caret: Option<(u16, u16, CursorShape)>,
    pub bg: Color,
    pub cursor_color: Color,
    display_offset: usize,
    selection: Option<SelectionRange>,
    last_colors: Option<Colors>,
    last_metrics: Option<(u32, u32)>,
    snapshots: Vec<Option<RowSnapshot>>,
    row_pool: HashMap<u64, Vec<(RowSnapshot, Rc<GridRow>)>>,
    row_pool_count: usize,
}
impl Default for Screen {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            selection_fills: Vec::new(),
            caret: None,
            bg: color(palette(257)),
            cursor_color: color(palette(258)),
            display_offset: 0,
            selection: None,
            last_colors: None,
            last_metrics: None,
            snapshots: Vec::new(),
            row_pool: HashMap::new(),
            row_pool_count: 0,
        }
    }
}

fn build_selection_fills(
    selection: Option<SelectionRange>,
    caret: Option<(u16, u16, CursorShape)>,
    offset: i32,
    lines: usize,
    cols: usize,
) -> Vec<(u16, u16, u16, Color)> {
    let Some(sel) = selection else {
        return Vec::new();
    };
    if lines == 0 || cols == 0 {
        return Vec::new();
    }
    let fill = selection_color();
    let block = caret.and_then(|(x, y, shape)| (shape == CursorShape::Block).then_some((x, y)));
    let lo = (sel.start.line.0.min(sel.end.line.0) + offset).max(0) as usize;
    let hi = (sel.start.line.0.max(sel.end.line.0) + offset).min(lines as i32 - 1);
    if lo as i32 > hi {
        return Vec::new();
    }
    let mut out = Vec::new();
    for y in lo..=(hi as usize) {
        let line = Line(y as i32 - offset);
        let skip = block.and_then(|(bx, by)| (usize::from(by) == y).then_some(usize::from(bx)));
        let mut run_start = None;
        for x in 0..cols {
            let active = skip != Some(x) && sel.contains(Point::new(line, Column(x)));
            match (run_start, active) {
                (None, true) => run_start = Some(x as u16),
                (Some(s), false) => {
                    out.push((y as u16, s, x as u16, fill));
                    run_start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = run_start {
            out.push((y as u16, s, cols as u16, fill));
        }
    }
    out
}
impl Screen {
    pub fn refresh(&mut self, session: &Session, force: bool, cell_width: f32, scale: f32) -> bool {
        let mut term = session.term.lock().unwrap();
        let batch = self.extract_updates(&mut term, force, cell_width, scale);
        drop(term);
        self.apply_updates(batch, cell_width, scale)
    }

    #[cfg(test)]
    fn refresh_term<T: alacritty_terminal::event::EventListener>(
        &mut self,
        term: &mut alacritty_terminal::term::Term<T>,
        force: bool,
        cell_width: f32,
        scale: f32,
    ) -> bool {
        let batch = self.extract_updates(term, force, cell_width, scale);
        self.apply_updates(batch, cell_width, scale)
    }

    fn extract_updates<T: alacritty_terminal::event::EventListener>(
        &mut self,
        term: &mut alacritty_terminal::term::Term<T>,
        force: bool,
        cell_width: f32,
        scale: f32,
    ) -> (usize, Colors, bool, Vec<(usize, Vec<Cell>)>) {
        let (cols, lines) = (term.columns(), term.screen_lines());
        let metrics = (cell_width.to_bits(), scale.to_bits());
        let metrics_changed = self.last_metrics != Some(metrics);
        let mut full = force || metrics_changed || self.rows.len() != lines;
        let mut dirty_rows = Vec::new();
        if !full {
            match term.damage() {
                TermDamage::Full => full = true,
                TermDamage::Partial(iter) => dirty_rows.extend(iter.map(|b| b.line)),
            }
        }
        let content = term.renderable_content();
        let colors = *content.colors;
        let colors_changed = self
            .last_colors
            .as_ref()
            .is_none_or(|prev| !colors_eq(prev, &colors));
        if colors_changed || metrics_changed {
            self.snapshots.clear();
            self.row_pool.clear();
            self.row_pool_count = 0;
            self.last_colors = Some(colors);
            self.last_metrics = Some(metrics);
            full = true;
        }
        let display_offset = content.display_offset;
        let offset = display_offset as i32;
        if !full && self.display_offset != display_offset {
            full = true;
        }
        let p = content.cursor.point;
        let viewport_line = p.line.0 + offset;
        let next_caret = (0..lines as i32).contains(&viewport_line).then_some((
            p.column.0 as u16,
            viewport_line as u16,
            content.cursor.shape,
        ));
        if !full && self.caret != next_caret {
            if let Some((_, y, CursorShape::Block)) = self.caret {
                dirty_rows.push(y as usize);
            }
            if let Some((_, y, CursorShape::Block)) = next_caret {
                dirty_rows.push(y as usize);
            }
        }
        let selection = content.selection;
        let next_selection_fills =
            build_selection_fills(selection, next_caret, offset, lines, cols);
        let next_bg = color(colors[257].unwrap_or_else(|| palette(257)));
        let next_cursor_color = color(colors[258].unwrap_or_else(|| palette(258)));
        let meta_changed = self.caret != next_caret
            || self.bg != next_bg
            || self.cursor_color != next_cursor_color
            || self.display_offset != display_offset
            || self.selection != selection
            || self.selection_fills != next_selection_fills;
        self.caret = next_caret;
        self.bg = next_bg;
        self.cursor_color = next_cursor_color;
        self.display_offset = display_offset;
        self.selection = selection;
        self.selection_fills = next_selection_fills;
        let changed: Vec<usize> = if full {
            (0..lines).collect()
        } else {
            dirty_rows.sort_unstable();
            dirty_rows.dedup();
            dirty_rows.retain(|&y| y < lines);
            dirty_rows
        };
        // Copy only damaged viewport rows. Font shaping and GPU data stay on the UI thread outside the lock.
        let updates: Vec<_> = changed
            .into_iter()
            .map(|y| {
                let line = Line(y as i32 - offset);
                let cells = term.grid()[line][..].to_vec();
                (y, cells)
            })
            .collect();
        term.reset_damage();
        (lines, colors, meta_changed, updates)
    }

    fn apply_updates(
        &mut self,
        (lines, colors, meta_changed, updates): (usize, Colors, bool, Vec<(usize, Vec<Cell>)>),
        cell_width: f32,
        scale: f32,
    ) -> bool {
        let resized = self.rows.len() != lines;
        if resized {
            self.rows
                .resize_with(lines, || Rc::new(GridRow::new("", vec![], font())));
        }
        if self.snapshots.len() != lines {
            self.snapshots.resize_with(lines, || None);
        }
        let mut rows_changed = false;
        for (y, mut cells) in updates {
            let block = self.caret.and_then(|(x, cy, shape)| {
                (cy as usize == y && shape == CursorShape::Block).then_some(x as usize)
            });
            trim_trailing_default_blanks(&mut cells, block);
            if self.snapshots[y]
                .as_ref()
                .is_some_and(|(prev_cells, prev_block)| {
                    *prev_block == block && *prev_cells == cells
                })
            {
                continue;
            }
            let hash = hash_row(&cells, block);
            if let Some(cached) = self.row_pool.get(&hash).and_then(|bucket| {
                bucket
                    .iter()
                    .find_map(|((c, b), row)| (*b == block && *c == cells).then(|| Rc::clone(row)))
            }) {
                self.rows[y] = cached;
                self.snapshots[y] = Some((cells, block));
                rows_changed = true;
                continue;
            }
            let built = Rc::new(draw_row(&cells, &colors, None, block, cell_width, scale));
            if self.row_pool_count >= ROW_POOL_LIMIT {
                self.row_pool.clear();
                self.row_pool_count = 0;
                for (row, snap) in self.rows.iter().zip(self.snapshots.iter()) {
                    if let Some((c, b)) = snap {
                        let key = hash_row(c, *b);
                        self.row_pool
                            .entry(key)
                            .or_default()
                            .push(((c.clone(), *b), Rc::clone(row)));
                        self.row_pool_count += 1;
                    }
                }
            }
            self.rows[y] = Rc::clone(&built);
            self.snapshots[y] = Some((cells.clone(), block));
            self.row_pool
                .entry(hash)
                .or_default()
                .push(((cells, block), built));
            self.row_pool_count += 1;
            rows_changed = true;
        }
        resized || rows_changed || meta_changed
    }
}

fn draw_row(
    cells: &[Cell],
    colors: &Colors,
    selection: Option<&[bool]>,
    block: Option<usize>,
    cell_width: f32,
    scale: f32,
) -> GridRow {
    let style = font();
    let mut line = String::with_capacity(cells.len());
    let mut spans = Vec::new();
    let mut fills: Vec<(u16, u16, Rgb)> = Vec::new();
    let mut pinned = Vec::new();
    let mut last_style = None;
    let mut runs = Vec::new();
    let mut shape = Shape::new();
    let mut glyph_batch = lines::GridGlyphBatch::new();
    let mut decoration: Option<Decoration> = None;
    let mut decoration_start = 0.;
    let default_bg = colors[257].unwrap_or_else(|| palette(257));
    for (x, cell) in cells.iter().enumerate() {
        let f = cell.flags;
        let (mut fg, mut bg) = (foreground(cell.fg, f, colors), resolve(cell.bg, colors));
        if f.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if f.contains(Flags::DIM) {
            fg = Rgb {
                r: fg.r / 2,
                g: fg.g / 2,
                b: fg.b / 2,
            };
        }
        if selection.is_some_and(|s| s[x]) {
            bg = Rgb {
                r: 58,
                g: 79,
                b: 112,
            };
        }
        if block == Some(x) {
            fg = default_bg;
            bg = colors[258].unwrap_or_else(|| palette(258));
        }
        let col = x as u16;
        if bg != default_bg {
            if let Some((_, end, c)) = fills.last_mut()
                && *end == col
                && *c == bg
            {
                *end += 1;
            } else {
                fills.push((col, col + 1, bg));
            }
        }
        let left = x as f32 * cell_width;
        let next = Decoration::from_cell(f, fg, cell.underline_color().map(|c| resolve(c, colors)));
        if next != decoration {
            if let Some(previous) = decoration {
                shape = previous.paint(shape, decoration_start, left);
            }
            decoration = next;
            decoration_start = left;
        }
        let flags = f & Flags::ITALIC;
        let run = (fg, flags);
        if last_style != Some(run) {
            runs.push((line.len(), run));
            last_style = Some(run);
        }
        // Alacritty has already advanced to the tab stop and keeps HT for copying.
        // Rune's text shaper must see exactly one narrow placeholder for this cell.
        if cell.c.is_ascii_control()
            || f.intersects(
                Flags::HIDDEN | Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER,
            )
        {
            glyph_batch.flush(&mut shape, cell_width, scale);
            line.push(' ');
            continue;
        }
        if !cell.c.is_ascii() || cell.zerowidth().is_some() {
            let glyph_ink = color(fg);
            let vector = cell.zerowidth().is_none()
                && glyph_batch.push(&mut shape, cell.c, left, cell_width, scale, glyph_ink);
            if !vector {
                let mut value = cell.c.to_string();
                if let Some(marks) = cell.zerowidth() {
                    value.extend(marks);
                }
                let mut fs = style;
                if f.contains(Flags::ITALIC) {
                    fs = fs.italic();
                }
                pinned.push((
                    col,
                    if f.contains(Flags::WIDE_CHAR) { 2 } else { 1 },
                    value,
                    fs,
                    glyph_ink,
                ));
            }
            line.push(' ');
        } else {
            glyph_batch.flush(&mut shape, cell_width, scale);
            line.push(cell.c);
        }
    }
    glyph_batch.flush(&mut shape, cell_width, scale);
    for (i, (start, (fg, flags))) in runs.iter().enumerate() {
        let end = runs.get(i + 1).map_or(line.len(), |r| r.0);
        let mut span = Span::new(*start, end).color(color(*fg));
        if flags.contains(Flags::ITALIC) {
            span = span.italic();
        }
        spans.push(span);
    }
    if let Some(last) = decoration {
        shape = last.paint(shape, decoration_start, cells.len() as f32 * cell_width);
    }
    let wraps = cells
        .last()
        .is_some_and(|c| c.flags.contains(Flags::WRAPLINE));
    let mut row = GridRow::new(line, spans, style).wraps(wraps);
    for (a, b, c) in fills {
        row = row.fill(a, b, color(c));
    }
    for (x, w, s, fs, c) in pinned {
        row = row.cell(x, w, s, fs, c);
    }
    row.shape(shape)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::{Config, Term};
    use alacritty_terminal::vte::ansi::{CursorStyle, Processor, StdSyncHandler};

    #[test]
    fn tab_stops_align_text_fills_and_wide_glyphs_like_equivalent_spaces() {
        for (prefix, spaces) in [
            ("Limit: 5h:\x1b[42m▊ \x1b[0m", 4),
            ("中文e\u{301}🙂", 1),
            ("\x1b[3g\x1b[23G\x1bH\rAB", 20),
        ] {
            let render = |gap: &str| {
                let mut term = Term::new(Config::default(), &Size::default(), VoidListener);
                let seq = format!(
                    "{prefix}{gap} 1%\x1b[34G\x1b[42m▎\x1b[0m 12%\x1b[61G\x1b[42m█\x1b[0m 20%"
                );
                Processor::<StdSyncHandler>::new().advance(&mut term, seq.as_bytes());
                let cells = &term.grid()[Line(0)][..];
                let x = cells.iter().position(|c| c.c == '1').unwrap();
                if prefix.starts_with("Limit") {
                    assert_eq!(
                        (x, cells[10].c, cells[33].c, cells[60].c),
                        (17, '▊', '▎', '█')
                    );
                    if gap == "\t" {
                        assert_eq!(cells[12].c, '\t');
                        let copied = term.bounds_to_string(
                            Point::new(Line(0), Column(0)),
                            Point::new(Line(0), Column(18)),
                        );
                        assert!(copied.contains("\t 1%"), "{copied:?}");
                    }
                }
                let selected: Vec<_> = (0..cells.len()).map(|x| (12..38).contains(&x)).collect();
                let rows = [(None, None), (Some(selected.as_slice()), Some(17))].map(
                    |(selection, caret)| {
                        Rc::new(draw_row(
                            cells,
                            &Colors::default(),
                            selection,
                            caret,
                            9.,
                            1.,
                        ))
                    },
                );
                let mut rt = rune::Runtime::new(
                    rows,
                    |rows, _| rune::grid(80, 9., HEIGHT, rows.to_vec()).w_full().h_full(),
                    (720., HEIGHT * 2.),
                    rune::Env::new(true, 1.),
                );
                rt.frame(0.);
                assert!(rt.instances.len() > 8);
                (
                    x,
                    rt.instances
                        .iter()
                        .map(|instance| instance.rect)
                        .collect::<Vec<_>>(),
                )
            };
            let (col, tab_rects) = render("\t");
            let (space_col, space_rects) = render(&" ".repeat(spaces));
            assert_eq!(col, space_col);
            assert_eq!(tab_rects.len(), space_rects.len());
            if let Some((index, (actual, expected))) = tab_rects
                .iter()
                .zip(&space_rects)
                .enumerate()
                .find(|(_, (a, b))| a != b)
            {
                panic!(
                    "drawing instance {index} after tab at {prefix:?}: {actual:?}, expected {expected:?}"
                );
            }
        }
    }

    #[test]
    fn horizontal_vector_rules_coalesce_without_transparent_cells() {
        let mut term = Term::new(Config::default(), &Size::default(), VoidListener);
        let rule = "─".repeat(40);
        let blocks = "█".repeat(20);
        let seq = format!("{rule}\r\n{blocks}");
        Processor::<StdSyncHandler>::new().advance(&mut term, seq.as_bytes());
        let rows = [Line(0), Line(1)].map(|line| {
            Rc::new(draw_row(
                &term.grid()[line][..],
                &Colors::default(),
                None,
                None,
                9.,
                1.,
            ))
        });
        let mut rt = rune::Runtime::new(
            rows,
            |rows, _| rune::grid(80, 9., HEIGHT, rows.to_vec()).w_full().h_full(),
            (720., HEIGHT * 2.),
            rune::Env::new(true, 1.),
        );
        rt.frame(0.);
        // One merged vector stroke for 40 '─' cells + one merged filled quad (2 triangles)
        // for 20 '█' cells, with zero transparent pinned glyph instances.
        assert_eq!(rt.instances.len(), 3, "{:?}", rt.instances);
        assert!((rt.instances[0].radii[2] - 40. * 9.).abs() <= 1.0);
        assert!((rt.instances[1].rect[2] - 20. * 9.).abs() <= 1.0);
    }

    #[test]
    fn unchanged_damaged_rows_keep_cached_grid_row_and_skip_refresh() {
        let mut screen = Screen::default();
        let size = Size {
            cols: 40,
            lines: 4,
            cell_w: 9,
            cell_h: 19,
        };
        let config = Config {
            default_cursor_style: CursorStyle {
                shape: CursorShape::Beam,
                ..CursorStyle::default()
            },
            ..Config::default()
        };
        let mut term = Term::new(config, &size, VoidListener);
        Processor::<StdSyncHandler>::new().advance(&mut term, b"hello codex");
        assert!(screen.refresh_term(&mut term, true, 9., 1.));
        let row0 = Rc::clone(&screen.rows[0]);
        // Re-write the exact same bytes to line 0 so alacritty marks it damaged without visual change.
        Processor::<StdSyncHandler>::new().advance(&mut term, b"\rhello codex");
        assert!(!screen.refresh_term(&mut term, false, 9., 1.));
        assert!(Rc::ptr_eq(&row0, &screen.rows[0]));
        // Actual content change rebuilds the row.
        Processor::<StdSyncHandler>::new().advance(&mut term, b"!");
        assert!(screen.refresh_term(&mut term, false, 9., 1.));
        assert!(!Rc::ptr_eq(&row0, &screen.rows[0]));
    }

    #[test]
    fn scrolling_reuses_shifted_chinese_grid_rows_from_pool() {
        let mut screen = Screen::default();
        let size = Size {
            cols: 40,
            lines: 4,
            cell_w: 9,
            cell_h: 19,
        };
        let config = Config {
            default_cursor_style: CursorStyle {
                shape: CursorShape::Beam,
                ..CursorStyle::default()
            },
            ..Config::default()
        };
        let mut term = Term::new(config, &size, VoidListener);
        Processor::<StdSyncHandler>::new().advance(
            &mut term,
            "第一行中文\r\n第二行中文\r\n第三行中文\r\n第四行中文".as_bytes(),
        );
        assert!(screen.refresh_term(&mut term, true, 9., 1.));
        let row1 = Rc::clone(&screen.rows[1]);
        let row2 = Rc::clone(&screen.rows[2]);
        let row3 = Rc::clone(&screen.rows[3]);
        // Append one new line so lines 1..=3 scroll up into rows 0..=2.
        Processor::<StdSyncHandler>::new().advance(&mut term, "\r\n第五行中文".as_bytes());
        assert!(screen.refresh_term(&mut term, false, 9., 1.));
        assert!(Rc::ptr_eq(&row1, &screen.rows[0]));
        assert!(Rc::ptr_eq(&row2, &screen.rows[1]));
        assert!(Rc::ptr_eq(&row3, &screen.rows[2]));
    }

    #[test]
    fn horizontal_resize_reuses_cached_rows_when_only_trailing_blanks_change() {
        let mut screen = Screen::default();
        let narrow = Size {
            cols: 40,
            lines: 4,
            cell_w: 9,
            cell_h: 19,
        };
        let wide = Size { cols: 90, ..narrow };
        let config = Config {
            default_cursor_style: CursorStyle {
                shape: CursorShape::Beam,
                ..CursorStyle::default()
            },
            ..Config::default()
        };
        let mut term = Term::new(config, &narrow, VoidListener);
        Processor::<StdSyncHandler>::new().advance(
            &mut term,
            "短标题行\r\nPS L:\\AI\\Mcli> cargo test".as_bytes(),
        );
        assert!(screen.refresh_term(&mut term, true, 9., 1.));
        let row0 = Rc::clone(&screen.rows[0]);
        let row1 = Rc::clone(&screen.rows[1]);
        term.resize(wide);
        let _ = screen.refresh_term(&mut term, true, 9., 1.);
        assert!(
            Rc::ptr_eq(&row0, &screen.rows[0]),
            "水平扩宽后未折行行 0 应复用缓存 GridRow"
        );
        assert!(
            Rc::ptr_eq(&row1, &screen.rows[1]),
            "水平扩宽后未折行行 1 应复用缓存 GridRow"
        );
    }
}
