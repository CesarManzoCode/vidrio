//! Table layout (css-tables-3 with CSS 2 §17): the table wrapper box
//! with its captions above or below the table grid box (`caption-side`),
//! columns and column groups (with `span`), the slots of the grid with
//! `rowspan` and `colspan` (a `rowspan` of 0 reaches the end of the row
//! group; the first header group goes first and the first footer group
//! last), the automatic layout (the min-content and max-content widths
//! of cells and columns, cells that span several columns, percentages,
//! and the width distribution of css-tables-3 §3.9.3 between the
//! min-content, percentage, specified and max-content guesses) and the
//! fixed layout (`table-layout: fixed`: columns and the cells of the
//! first row), row heights with the baselines of cells
//! (`vertical-align`: `top`, `middle`, `bottom` and the rest as
//! `baseline`) and the rows that `rowspan` crosses, the specified height
//! of the table, the separated borders model with `border-spacing` and
//! `empty-cells`, the collapsing borders model (conflict resolution
//! between cells, rows, row groups, columns, column groups and the
//! table; each cell takes half of the widest border on each of its
//! sides, the table half of its outer ones) and `visibility: collapse`
//! on rows, row groups, columns and column groups (their space is
//! removed after the widths are computed).
//!
//! Positions are physical offsets from the border box of the table grid
//! box (columns are mirrored in `rtl`); `layout` returns the captions and
//! the table grid box relative to the content box of the wrapper, as
//! `block::layout_independent` returns them.

use std::ops::Range;
use std::rc::Rc;

use crate::css::style::ComputedStyle;

use super::Ctx;
use super::block::{self, Independent};
use super::boxtree::{BoxKind, BoxNode, Contents, GroupKind, InlineItem, TableRole, role_of};
use super::fragment::{CollapsedSide, Fragment, FragmentKind, Rect, Side, Sides};
use super::values::{self, Lp, Size, kw, value_lp};

const EPS: f64 = 1e-7;

// The origins of a border, by precedence in conflicts (CSS 2 §17.6.2.1).
const TABLE: u8 = 0;
const COLUMN_GROUP: u8 = 1;
const COLUMN: u8 = 2;
const ROW_GROUP: u8 = 3;
const ROW: u8 = 4;
const CELL: u8 = 5;

/// The box is a table wrapper box.
pub(super) fn is_wrapper(b: &BoxNode) -> bool {
    role_of(b) == TableRole::Wrapper
}

fn children(b: &BoxNode) -> &[BoxNode] {
    match &b.kind {
        BoxKind::Container(c) => match &c.contents {
            Contents::Blocks(v) => v,
            Contents::Inline(_) => &[],
        },
        BoxKind::Replaced(_) => &[],
    }
}

/// The captions and the table grid box of a table wrapper box.
fn parts(wrapper: &BoxNode) -> (Vec<&BoxNode>, Option<&BoxNode>) {
    let mut captions = Vec::new();
    let mut grid = None;
    for ch in children(wrapper) {
        match role_of(ch) {
            TableRole::Caption => captions.push(ch),
            TableRole::Table => grid = Some(ch),
            _ => {}
        }
    }
    (captions, grid)
}

fn is_collapsed(s: &ComputedStyle) -> bool {
    kw(s, "visibility") == "collapse"
}

/// The used `border-spacing`: horizontal, vertical.
fn border_spacing(s: &ComputedStyle) -> (f64, f64) {
    let lengths: Vec<f64> = s
        .get("border-spacing")
        .and_then(|v| v.values.as_ref())
        .map(|vs| {
            vs.iter()
                .filter_map(value_lp)
                .map(|l| l.resolve(0.0).max(0.0))
                .collect()
        })
        .unwrap_or_default();
    match lengths.as_slice() {
        [h] => (*h, *h),
        [h, v, ..] => (*h, *v),
        _ => (0.0, 0.0),
    }
}

/// A border-box size from a specified size `w` of a box whose borders
/// and paddings add up to `bp`.
fn outer(s: &ComputedStyle, w: f64, bp: f64) -> f64 {
    if values::border_box_sizing(s) {
        w.max(bp)
    } else {
        w + bp
    }
}

/// A fixed length of `name` (no percentages, no keywords).
fn fixed_length(s: &ComputedStyle, name: &str) -> Option<f64> {
    match values::size(s, name) {
        Size::Lp(l) if !l.has_percent() => Some(l.resolve(0.0).max(0.0)),
        _ => None,
    }
}

/// A cell has no in-flow content (CSS 2 §17.6.1.1).
fn is_empty(b: &BoxNode) -> bool {
    match &b.kind {
        BoxKind::Container(c) => match &c.contents {
            Contents::Inline(ic) => {
                !ic.is_significant() && !ic.items.iter().any(|i| matches!(i, InlineItem::Float(_)))
            }
            Contents::Blocks(v) => v.iter().all(|b| values::out_of_flow(&b.style)),
        },
        BoxKind::Replaced(_) => false,
    }
}

fn same(a: Option<&BoxNode>, b: Option<&BoxNode>) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => std::ptr::eq(x, y),
        (None, None) => true,
        _ => false,
    }
}

// ----- collapsing borders ------------------------------------------------------------------

/// A border that takes part in a conflict on one edge of the grid.
#[derive(Clone, Debug)]
struct Edge {
    width: f64,
    /// The precedence of its style (`none` is 0).
    rank: u8,
    origin: u8,
    hidden: bool,
    source: Option<Rc<ComputedStyle>>,
    side: Side,
}

impl Edge {
    fn none() -> Edge {
        Edge {
            width: 0.0,
            rank: 0,
            origin: TABLE,
            hidden: false,
            source: None,
            side: Side::Top,
        }
    }

    fn of(style: &Rc<ComputedStyle>, side: Side, origin: u8) -> Edge {
        let name = side.name();
        let st = kw(style, &format!("border-{name}-style"));
        let rank = match st.as_str() {
            "double" => 8,
            "solid" => 7,
            "dashed" => 6,
            "dotted" => 5,
            "ridge" => 4,
            "outset" => 3,
            "groove" => 2,
            "inset" => 1,
            _ => 0,
        };
        let width = if rank == 0 {
            0.0
        } else {
            values::px(style, &format!("border-{name}-width")).max(0.0)
        };
        Edge {
            width,
            rank,
            origin,
            hidden: st == "hidden",
            source: Some(style.clone()),
            side,
        }
    }

    /// The border wins over `o`, which came first (CSS 2 §17.6.2.1):
    /// `hidden` wins, then the wider border, then the style by
    /// precedence, then the origin; on a full tie the first one (the
    /// one further up or to the left) stays.
    fn beats(&self, o: &Edge) -> bool {
        if o.hidden {
            return false;
        }
        if self.hidden {
            return true;
        }
        if self.width > o.width + EPS {
            return true;
        }
        if self.width < o.width - EPS {
            return false;
        }
        if self.rank != o.rank {
            return self.rank > o.rank;
        }
        self.origin > o.origin
    }

    fn used(&self) -> f64 {
        if self.hidden { 0.0 } else { self.width }
    }

    fn painted(&self) -> CollapsedSide {
        let width = self.used();
        CollapsedSide {
            width,
            source: if width > 0.0 {
                self.source.clone()
            } else {
                None
            },
            side: self.side,
        }
    }
}

/// The winner of a conflict between `candidates`, in precedence order
/// for ties.
fn resolve(candidates: Vec<Edge>) -> Edge {
    let mut best = Edge::none();
    for c in candidates {
        if c.beats(&best) {
            best = c;
        }
    }
    best
}

/// The widest of `edges` (the first on a tie) as it is painted.
fn widest<'e>(edges: impl Iterator<Item = &'e Edge>) -> CollapsedSide {
    let mut best: Option<&Edge> = None;
    for e in edges {
        if best.is_none_or(|b| e.used() > b.used() + EPS) {
            best = Some(e);
        }
    }
    best.map_or(
        CollapsedSide {
            width: 0.0,
            source: None,
            side: Side::Top,
        },
        Edge::painted,
    )
}

// ----- the table grid ------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Column<'a> {
    /// The `table-column` box of the column, if any.
    col: Option<&'a BoxNode>,
    group: Option<&'a BoxNode>,
}

struct Row<'a> {
    b: &'a BoxNode,
    group: usize,
}

struct Group<'a> {
    b: &'a BoxNode,
    /// The rows of the group.
    start: usize,
    end: usize,
}

struct Cell<'a> {
    b: &'a BoxNode,
    row: usize,
    col: usize,
    rowspan: usize,
    colspan: usize,
}

/// The width measures of a column (or a cell): min-content and
/// max-content border-box widths, a percentage, and whether its width
/// is specified as a length.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Measure {
    min: f64,
    max: f64,
    pct: Option<f64>,
    fixed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VAlign {
    Top,
    Middle,
    Bottom,
    Baseline,
}

/// The alignment of a cell's content (CSS 2 §17.5.3): values other than
/// `top`, `middle` and `bottom` behave as `baseline`.
fn valign(s: &ComputedStyle) -> VAlign {
    match kw(s, "vertical-align").as_str() {
        "top" => VAlign::Top,
        "middle" => VAlign::Middle,
        "bottom" => VAlign::Bottom,
        _ => VAlign::Baseline,
    }
}

/// A cell laid out in its columns.
struct Laid {
    children: Vec<Fragment>,
    border: Sides,
    padding: Sides,
    x: f64,
    width: f64,
    /// The height of the content box the content needs.
    content: f64,
    /// The border-box height the cell needs (at least its `height`).
    need: f64,
    /// The baseline from the top of the border box (the bottom of the
    /// content box without line boxes).
    baseline: f64,
    align: VAlign,
}

struct Table<'a> {
    b: &'a BoxNode,
    style: &'a Rc<ComputedStyle>,
    collapse: bool,
    spacing: (f64, f64),
    cols: Vec<Column<'a>>,
    rows: Vec<Row<'a>>,
    groups: Vec<Group<'a>>,
    cells: Vec<Cell<'a>>,
    /// The cell in each slot, by row and column.
    slots: Vec<Vec<Option<usize>>>,
    /// Out-of-flow children of the table parts.
    oof: Vec<&'a BoxNode>,
    /// The resolved collapsed borders above each slot (one more row at
    /// the bottom) and left of each slot (one more column at the end).
    h: Vec<Vec<Edge>>,
    v: Vec<Vec<Edge>>,
}

impl<'a> Table<'a> {
    /// The grid of table grid box `grid` (css-tables-3 §3.2, the HTML
    /// table processing model for the slots).
    fn new(grid: &'a BoxNode) -> Table<'a> {
        let style = &grid.style;
        let collapse = kw(style, "border-collapse") == "collapse";
        let spacing = if collapse {
            (0.0, 0.0)
        } else {
            border_spacing(style)
        };
        let mut cols = Vec::new();
        let mut oof = Vec::new();
        let (mut head, mut foot, mut bodies) = (None, None, Vec::new());
        for ch in children(grid) {
            if values::out_of_flow(&ch.style) {
                oof.push(ch);
                continue;
            }
            match role_of(ch) {
                TableRole::ColumnGroup { span } => {
                    let inner: Vec<&BoxNode> = children(ch)
                        .iter()
                        .filter(|c| matches!(role_of(c), TableRole::Column { .. }))
                        .collect();
                    if inner.is_empty() {
                        for _ in 0..span {
                            cols.push(Column {
                                col: None,
                                group: Some(ch),
                            });
                        }
                    }
                    for c in inner {
                        if let TableRole::Column { span } = role_of(c) {
                            for _ in 0..span {
                                cols.push(Column {
                                    col: Some(c),
                                    group: Some(ch),
                                });
                            }
                        }
                    }
                }
                TableRole::Column { span } => {
                    for _ in 0..span {
                        cols.push(Column {
                            col: Some(ch),
                            group: None,
                        });
                    }
                }
                TableRole::RowGroup(GroupKind::Header) if head.is_none() => head = Some(ch),
                TableRole::RowGroup(GroupKind::Footer) if foot.is_none() => foot = Some(ch),
                TableRole::RowGroup(_) => bodies.push(ch),
                _ => {}
            }
        }
        let order: Vec<&BoxNode> = head.into_iter().chain(bodies).chain(foot).collect();
        let mut rows = Vec::new();
        let mut groups = Vec::new();
        for g in order {
            let start = rows.len();
            for r in children(g) {
                if values::out_of_flow(&r.style) {
                    oof.push(r);
                } else if role_of(r) == TableRole::Row {
                    rows.push(Row {
                        b: r,
                        group: groups.len(),
                    });
                }
            }
            groups.push(Group {
                b: g,
                start,
                end: rows.len(),
            });
        }
        let mut slots: Vec<Vec<Option<usize>>> = vec![Vec::new(); rows.len()];
        let mut cells = Vec::new();
        for g in &groups {
            for r in g.start..g.end {
                let mut c = 0;
                for cell in children(rows[r].b) {
                    if values::out_of_flow(&cell.style) {
                        oof.push(cell);
                        continue;
                    }
                    let TableRole::Cell { colspan, rowspan } = role_of(cell) else {
                        continue;
                    };
                    while slots[r].get(c).is_some_and(|s| s.is_some()) {
                        c += 1;
                    }
                    let rs = if rowspan == 0 {
                        g.end - r
                    } else {
                        rowspan.min(g.end - r)
                    }
                    .max(1);
                    let i = cells.len();
                    for row in slots.iter_mut().skip(r).take(rs) {
                        if row.len() < c + colspan {
                            row.resize(c + colspan, None);
                        }
                        for s in &mut row[c..c + colspan] {
                            if s.is_none() {
                                *s = Some(i);
                            }
                        }
                    }
                    cells.push(Cell {
                        b: cell,
                        row: r,
                        col: c,
                        rowspan: rs,
                        colspan,
                    });
                    c += colspan;
                }
            }
        }
        let ncols = slots
            .iter()
            .map(Vec::len)
            .max()
            .unwrap_or(0)
            .max(cols.len());
        cols.resize(
            ncols,
            Column {
                col: None,
                group: None,
            },
        );
        for row in &mut slots {
            row.resize(ncols, None);
        }
        let mut t = Table {
            b: grid,
            style,
            collapse,
            spacing,
            cols,
            rows,
            groups,
            cells,
            slots,
            oof,
            h: Vec::new(),
            v: Vec::new(),
        };
        if collapse {
            t.resolve_borders();
        }
        t
    }

    /// Resolves the border conflicts on every edge of the grid (CSS 2
    /// §17.6.2.1). An edge inside a cell that spans it has no border.
    fn resolve_borders(&mut self) {
        let (nr, nc) = (self.rows.len(), self.cols.len());
        let table = self.style.clone();
        let mut h = vec![vec![Edge::none(); nc]; nr + 1];
        for (r, line) in h.iter_mut().enumerate() {
            for (c, edge) in line.iter_mut().enumerate() {
                let above = if r > 0 { self.slots[r - 1][c] } else { None };
                let below = if r < nr { self.slots[r][c] } else { None };
                if above.is_some() && above == below {
                    continue;
                }
                let mut cands = Vec::new();
                if let Some(i) = above {
                    cands.push(Edge::of(&self.cells[i].b.style, Side::Bottom, CELL));
                }
                if let Some(i) = below {
                    cands.push(Edge::of(&self.cells[i].b.style, Side::Top, CELL));
                }
                if r > 0 {
                    cands.push(Edge::of(&self.rows[r - 1].b.style, Side::Bottom, ROW));
                }
                if r < nr {
                    cands.push(Edge::of(&self.rows[r].b.style, Side::Top, ROW));
                }
                let ga = (r > 0).then(|| self.rows[r - 1].group);
                let gb = (r < nr).then(|| self.rows[r].group);
                if ga != gb {
                    if let Some(g) = ga {
                        cands.push(Edge::of(&self.groups[g].b.style, Side::Bottom, ROW_GROUP));
                    }
                    if let Some(g) = gb {
                        cands.push(Edge::of(&self.groups[g].b.style, Side::Top, ROW_GROUP));
                    }
                }
                if r == 0 || r == nr {
                    let side = if r == 0 { Side::Top } else { Side::Bottom };
                    if let Some(b) = self.cols[c].col {
                        cands.push(Edge::of(&b.style, side, COLUMN));
                    }
                    if let Some(b) = self.cols[c].group {
                        cands.push(Edge::of(&b.style, side, COLUMN_GROUP));
                    }
                    cands.push(Edge::of(&table, side, TABLE));
                }
                *edge = resolve(cands);
            }
        }
        let mut v = vec![vec![Edge::none(); nc + 1]; nr];
        for (r, line) in v.iter_mut().enumerate() {
            for (c, edge) in line.iter_mut().enumerate() {
                let left = if c > 0 { self.slots[r][c - 1] } else { None };
                let right = if c < nc { self.slots[r][c] } else { None };
                if left.is_some() && left == right {
                    continue;
                }
                let mut cands = Vec::new();
                if let Some(i) = left {
                    cands.push(Edge::of(&self.cells[i].b.style, Side::Right, CELL));
                }
                if let Some(i) = right {
                    cands.push(Edge::of(&self.cells[i].b.style, Side::Left, CELL));
                }
                let outer_side = if c == 0 {
                    Some(Side::Left)
                } else if c == nc {
                    Some(Side::Right)
                } else {
                    None
                };
                if let Some(side) = outer_side {
                    let row = &self.rows[r];
                    cands.push(Edge::of(&row.b.style, side, ROW));
                    cands.push(Edge::of(&self.groups[row.group].b.style, side, ROW_GROUP));
                }
                if c > 0 {
                    if let Some(b) = self.cols[c - 1].col {
                        cands.push(Edge::of(&b.style, Side::Right, COLUMN));
                    }
                }
                if c < nc {
                    if let Some(b) = self.cols[c].col {
                        cands.push(Edge::of(&b.style, Side::Left, COLUMN));
                    }
                }
                let gl = if c > 0 { self.cols[c - 1].group } else { None };
                let gr = if c < nc { self.cols[c].group } else { None };
                if !same(gl, gr) {
                    if let Some(b) = gl {
                        cands.push(Edge::of(&b.style, Side::Right, COLUMN_GROUP));
                    }
                    if let Some(b) = gr {
                        cands.push(Edge::of(&b.style, Side::Left, COLUMN_GROUP));
                    }
                }
                if let Some(side) = outer_side {
                    cands.push(Edge::of(&table, side, TABLE));
                }
                *edge = resolve(cands);
            }
        }
        self.h = h;
        self.v = v;
    }

    /// The edges along the sides of cell `i`: top, right, bottom, left.
    fn cell_edges(&self, i: usize) -> [Vec<&Edge>; 4] {
        let c = &self.cells[i];
        let cols = c.col..c.col + c.colspan;
        let rows = c.row..c.row + c.rowspan;
        [
            cols.clone().map(|k| &self.h[c.row][k]).collect(),
            rows.clone()
                .map(|r| &self.v[r][c.col + c.colspan])
                .collect(),
            cols.map(|k| &self.h[c.row + c.rowspan][k]).collect(),
            rows.map(|r| &self.v[r][c.col]).collect(),
        ]
    }

    /// The edges along the outer sides of the grid: top, right, bottom,
    /// left.
    fn outer_edges(&self) -> [Vec<&Edge>; 4] {
        let nc = self.cols.len();
        match (self.h.first(), self.h.last()) {
            (Some(top), Some(bottom)) => [
                top.iter().collect(),
                self.v.iter().map(|l| &l[nc]).collect(),
                bottom.iter().collect(),
                self.v.iter().map(|l| &l[0]).collect(),
            ],
            _ => [Vec::new(), Vec::new(), Vec::new(), Vec::new()],
        }
    }

    /// The used borders of the table grid box: its own in the separated
    /// model, half of the widest outer collapsed border on each side in
    /// the collapsing one (css-tables-3 §3.6.2.2).
    fn border(&self) -> Sides {
        if !self.collapse {
            return values::border(self.style);
        }
        if self.rows.is_empty() || self.cols.is_empty() {
            let b = values::border(self.style);
            return Sides::new(b.top / 2.0, b.right / 2.0, b.bottom / 2.0, b.left / 2.0);
        }
        let half = |es: &Vec<&Edge>| es.iter().map(|e| e.used() / 2.0).fold(0.0, f64::max);
        let [t, r, b, l] = self.outer_edges();
        Sides::new(half(&t), half(&r), half(&b), half(&l))
    }

    /// The used paddings of the table grid box (none when borders
    /// collapse).
    fn padding(&self, cbw: f64) -> Sides {
        if self.collapse {
            Sides::default()
        } else {
            values::padding(self.style, cbw)
        }
    }

    /// The used borders of cell `i`.
    fn cell_border(&self, i: usize) -> Sides {
        let c = &self.cells[i];
        if !self.collapse {
            return values::border(&c.b.style);
        }
        let half = |es: &Vec<&Edge>| es.iter().map(|e| e.used() / 2.0).fold(0.0, f64::max);
        let [t, r, b, l] = self.cell_edges(i);
        Sides::new(half(&t), half(&r), half(&b), half(&l))
    }

    /// The horizontal space of the table grid box that is not columns:
    /// borders, paddings and the spacing around and between columns.
    fn extra_width(&self, cbw: f64) -> f64 {
        let bp = self.border().horizontal() + self.padding(cbw).horizontal();
        let n = self.cols.len();
        if n == 0 {
            bp
        } else {
            bp + self.spacing.0 * (n as f64 + 1.0)
        }
    }

    /// Fixed layout applies: `table-layout: fixed` with a width that is
    /// not automatic (css-tables-3 §3.9.4).
    fn fixed_layout(&self) -> bool {
        kw(self.style, "table-layout") == "fixed"
            && matches!(values::size(self.style, "width"), Size::Lp(_))
    }

    /// The border-box width `name` (`width`, `min-width`, `max-width`)
    /// asks for.
    fn specified(&self, name: &str, cbw: Option<f64>) -> Option<f64> {
        let Size::Lp(l) = values::size(self.style, name) else {
            return None;
        };
        let w = l.resolve_opt(cbw)?;
        let bp = self.border().horizontal() + self.padding(cbw.unwrap_or(0.0)).horizontal();
        Some(outer(self.style, w, bp))
    }

    /// The measures of cell `i`.
    fn cell_measure(&self, ctx: &mut Ctx, i: usize) -> Measure {
        let c = &self.cells[i];
        let s = &c.b.style;
        let bp = self.cell_border(i).horizontal() + values::padding(s, 0.0).horizontal();
        let (cmin, cmax) = block::intrinsic(ctx, c.b);
        let mut m = Measure {
            min: cmin + bp,
            max: cmax + bp,
            pct: None,
            fixed: false,
        };
        match values::size(s, "width") {
            Size::Lp(Lp::Pct(p)) => m.pct = Some(p.max(0.0)),
            Size::Lp(l) if !l.has_percent() => {
                let w = outer(s, l.resolve(0.0).max(0.0), bp);
                m.min = m.min.max(w);
                m.max = m.min;
                m.fixed = true;
            }
            _ => {}
        }
        if let Some(w) = fixed_length(s, "min-width") {
            m.min = m.min.max(outer(s, w, bp));
        }
        m.max = m.max.max(m.min);
        m
    }

    /// The measures of the columns in automatic layout (css-tables-3
    /// §3.9.2): their own widths, the cells that span one column, then
    /// the cells that span several, fewest columns first.
    fn measures(&self, ctx: &mut Ctx) -> Vec<Measure> {
        let mut ms = vec![Measure::default(); self.cols.len()];
        for (m, col) in ms.iter_mut().zip(&self.cols) {
            let Some(b) = col.col.or(col.group) else {
                continue;
            };
            match values::size(&b.style, "width") {
                Size::Lp(Lp::Pct(p)) => m.pct = Some(p.max(0.0)),
                Size::Lp(l) if !l.has_percent() => {
                    m.max = m.max.max(l.resolve(0.0).max(0.0));
                    m.fixed = true;
                }
                _ => {}
            }
        }
        let cm: Vec<Measure> = (0..self.cells.len())
            .map(|i| self.cell_measure(ctx, i))
            .collect();
        for (c, m) in self.cells.iter().zip(&cm) {
            if c.colspan != 1 {
                continue;
            }
            let col = &mut ms[c.col];
            col.min = col.min.max(m.min);
            col.max = col.max.max(m.max);
            col.fixed |= m.fixed;
            if let Some(p) = m.pct {
                col.pct = Some(col.pct.map_or(p, |q| q.max(p)));
            }
        }
        for m in &mut ms {
            m.max = m.max.max(m.min);
        }
        let mut spanning: Vec<usize> = (0..self.cells.len())
            .filter(|&i| self.cells[i].colspan > 1)
            .collect();
        spanning.sort_by_key(|&i| self.cells[i].colspan);
        for i in spanning {
            let c = &self.cells[i];
            let range = c.col..c.col + c.colspan;
            let gaps = self.spacing.0 * (c.colspan - 1) as f64;
            if let Some(p) = cm[i].pct {
                let have: f64 = range.clone().filter_map(|k| ms[k].pct).sum();
                let free: Vec<usize> = range.clone().filter(|&k| ms[k].pct.is_none()).collect();
                if p > have + EPS && !free.is_empty() {
                    let total: f64 = free.iter().map(|&k| ms[k].max).sum();
                    let n = free.len() as f64;
                    for &k in &free {
                        let share = if total > EPS {
                            ms[k].max / total
                        } else {
                            1.0 / n
                        };
                        ms[k].pct = Some((p - have) * share);
                    }
                }
            }
            grow(&mut ms, range.clone(), cm[i].min - gaps, true);
            grow(&mut ms, range.clone(), cm[i].max - gaps, false);
            for k in range {
                ms[k].max = ms[k].max.max(ms[k].min);
            }
        }
        // Percentages past 100% in total are cut, in column order.
        let mut total = 0.0;
        for m in &mut ms {
            if let Some(p) = m.pct {
                let p = p.min(100.0 - total).max(0.0);
                m.pct = Some(p);
                total += p;
            }
        }
        ms
    }

    /// The widths fixed layout gives to the columns from their own widths
    /// and then from the cells of the first row (a cell that spans
    /// several columns divides its width among them); `space` is the
    /// width of the columns, percentages need it.
    fn fixed_columns(&self, space: Option<f64>) -> Vec<Option<f64>> {
        let mut ws: Vec<Option<f64>> = vec![None; self.cols.len()];
        for (w, col) in ws.iter_mut().zip(&self.cols) {
            if let Some(b) = col.col.or(col.group) {
                if let Size::Lp(l) = values::size(&b.style, "width") {
                    *w = l.resolve_opt(space).map(|v| v.max(0.0));
                }
            }
        }
        for (i, c) in self.cells.iter().enumerate() {
            if c.row != 0 {
                continue;
            }
            let Size::Lp(l) = values::size(&c.b.style, "width") else {
                continue;
            };
            let Some(w) = l.resolve_opt(space) else {
                continue;
            };
            let w = if l.has_percent() {
                w
            } else {
                let bp = self.cell_border(i).horizontal()
                    + values::padding(&c.b.style, 0.0).horizontal();
                outer(&c.b.style, w.max(0.0), bp)
            };
            let each = (w - self.spacing.0 * (c.colspan - 1) as f64).max(0.0) / c.colspan as f64;
            for slot in &mut ws[c.col..c.col + c.colspan] {
                if slot.is_none() {
                    *slot = Some(each);
                }
            }
        }
        ws
    }

    /// The min-content and max-content border-box widths of the table
    /// grid box; `ms` are the column measures (`None` in fixed layout).
    fn widths(&self, ms: Option<&[Measure]>, cbw: Option<f64>) -> (f64, f64) {
        let extra = self.extra_width(cbw.unwrap_or(0.0));
        match ms {
            None => {
                let sum: f64 = self.fixed_columns(None).iter().flatten().sum();
                (sum + extra, sum + extra)
            }
            Some(ms) => {
                let min: f64 = ms.iter().map(|m| m.min).sum();
                (min + extra, max_columns(ms).max(min) + extra)
            }
        }
    }

    /// The used widths of the columns of a table grid box `w` wide.
    fn column_widths(&self, ms: Option<&[Measure]>, w: f64, cbw: f64) -> Vec<f64> {
        let space = (w - self.extra_width(cbw)).max(0.0);
        let Some(ms) = ms else {
            let ws = self.fixed_columns(Some(space));
            let sum: f64 = ws.iter().flatten().sum();
            let autos = ws.iter().filter(|w| w.is_none()).count();
            let rest = space - sum;
            if autos > 0 {
                let each = (rest / autos as f64).max(0.0);
                return ws.into_iter().map(|w| w.unwrap_or(each)).collect();
            }
            let n = ws.len() as f64;
            return ws
                .into_iter()
                .flatten()
                .map(|w| {
                    if rest <= EPS {
                        w
                    } else if sum > EPS {
                        w + rest * w / sum
                    } else {
                        rest / n
                    }
                })
                .collect();
        };
        distribute(ms, space)
    }

    /// Lays out the table grid box `w` wide (its border box) with its
    /// top at `y0`.
    fn layout(&self, ctx: &mut Ctx, w: f64, cbh: Option<f64>, y0: f64) -> Fragment {
        let s = self.style;
        let border = self.border();
        let padding = self.padding(w);
        let (sh, sv) = self.spacing;
        let (nr, nc) = (self.rows.len(), self.cols.len());
        let ms = if self.fixed_layout() {
            None
        } else {
            Some(self.measures(ctx))
        };
        let (min, _) = self.widths(ms.as_deref(), Some(w));
        let w = w.max(min);
        let widths = self.column_widths(ms.as_deref(), w, w);

        // Columns, with the collapsed ones taken out.
        let col_collapsed: Vec<bool> = self
            .cols
            .iter()
            .map(|c| {
                c.col.is_some_and(|b| is_collapsed(&b.style))
                    || c.group.is_some_and(|b| is_collapsed(&b.style))
            })
            .collect();
        let lead_h = if nc > 0 { sh } else { 0.0 };
        let mut col_x = vec![0.0; nc];
        let mut col_w = vec![0.0; nc];
        let mut x = border.left + padding.left + lead_h;
        let mut removed = 0.0;
        for k in 0..nc {
            col_x[k] = x;
            if col_collapsed[k] {
                removed += widths[k] + sh;
            } else {
                col_w[k] = widths[k];
                x += widths[k] + sh;
            }
        }
        let width = (w - removed).max(border.horizontal() + padding.horizontal());
        if s.rtl() {
            for k in 0..nc {
                col_x[k] = width - col_x[k] - col_w[k];
            }
        }
        let area_x = border.left + padding.left + lead_h;
        let area_w = (width - border.horizontal() - padding.horizontal() - 2.0 * lead_h).max(0.0);
        let span_x = |r: Range<usize>| -> (f64, f64) {
            let visible: Vec<usize> = r.clone().filter(|&k| !col_collapsed[k]).collect();
            if visible.is_empty() {
                return (col_x.get(r.start).copied().unwrap_or(area_x), 0.0);
            }
            let x0 = visible
                .iter()
                .map(|&k| col_x[k])
                .fold(f64::INFINITY, f64::min);
            let x1 = visible
                .iter()
                .map(|&k| col_x[k] + col_w[k])
                .fold(f64::NEG_INFINITY, f64::max);
            (x0, x1 - x0)
        };

        // The cells, laid out in their columns.
        let mut laid: Vec<Laid> = Vec::with_capacity(self.cells.len());
        for (i, c) in self.cells.iter().enumerate() {
            let cs = &c.b.style;
            let cb_border = self.cell_border(i);
            let cb_padding = values::padding(cs, w);
            let (cx, cw) = span_x(c.col..c.col + c.colspan);
            let bp_v = cb_border.vertical() + cb_padding.vertical();
            let content_w = (cw - cb_border.horizontal() - cb_padding.horizontal()).max(0.0);
            let spec = fixed_length(cs, "height").map(|h| outer(cs, h, bp_v));
            let (children, content) = match &c.b.kind {
                BoxKind::Container(cc) => {
                    let inner = block::layout_independent(
                        ctx,
                        c.b,
                        cc,
                        content_w,
                        spec.map(|h| (h - bp_v).max(0.0)),
                    );
                    (inner.children, inner.height)
                }
                BoxKind::Replaced(_) => (Vec::new(), 0.0),
            };
            let first = block::find_baseline(&children, false).unwrap_or(content);
            laid.push(Laid {
                baseline: cb_border.top + cb_padding.top + first,
                children,
                border: cb_border,
                padding: cb_padding,
                x: cx,
                width: cw,
                content,
                need: (content + bp_v).max(spec.unwrap_or(0.0)),
                align: valign(cs),
            });
        }

        // Row heights (css-tables-3 §3.10): the rows' own heights, the
        // cells of one row and their baselines, then the cells that span
        // several rows, fewest rows first.
        let mut row_h: Vec<f64> = self
            .rows
            .iter()
            .map(|r| fixed_length(&r.b.style, "height").unwrap_or(0.0))
            .collect();
        let mut base = vec![0.0f64; nr];
        let mut below = vec![0.0f64; nr];
        for (c, l) in self.cells.iter().zip(&laid) {
            if l.align == VAlign::Baseline {
                base[c.row] = base[c.row].max(l.baseline);
                if c.rowspan == 1 {
                    below[c.row] = below[c.row].max(l.need - l.baseline);
                }
            }
            if c.rowspan == 1 {
                row_h[c.row] = row_h[c.row].max(l.need);
            }
        }
        for r in 0..nr {
            row_h[r] = row_h[r].max(base[r] + below[r]);
        }
        let mut spanning: Vec<usize> = (0..self.cells.len())
            .filter(|&i| self.cells[i].rowspan > 1)
            .collect();
        spanning.sort_by_key(|&i| self.cells[i].rowspan);
        for i in spanning {
            let c = &self.cells[i];
            let rows = c.row..c.row + c.rowspan;
            let have: f64 = row_h[rows.clone()].iter().sum::<f64>() + sv * (c.rowspan - 1) as f64;
            spread(&mut row_h[rows], laid[i].need - have);
        }
        let row_collapsed: Vec<bool> = self
            .rows
            .iter()
            .map(|r| is_collapsed(&r.b.style) || is_collapsed(&self.groups[r.group].b.style))
            .collect();
        let lead_v = if nr > 0 { sv } else { 0.0 };
        let bp_v = border.vertical() + padding.vertical();
        let spec_h = match values::size(s, "height") {
            Size::Lp(l) => l.resolve_opt(cbh).map(|h| outer(s, h, bp_v)),
            _ => None,
        };
        if let Some(h) = spec_h {
            let have = row_h.iter().sum::<f64>() + bp_v + lead_v * (nr as f64 + 1.0);
            let open: Vec<usize> = (0..nr).filter(|&r| !row_collapsed[r]).collect();
            if h > have + EPS && !open.is_empty() {
                let mut part: Vec<f64> = open.iter().map(|&r| row_h[r]).collect();
                spread(&mut part, h - have);
                for (&r, v) in open.iter().zip(part) {
                    row_h[r] = v;
                }
            }
        }
        let mut row_y = vec![0.0; nr];
        let mut y = border.top + padding.top + lead_v;
        for r in 0..nr {
            row_y[r] = y;
            if row_collapsed[r] {
                row_h[r] = 0.0;
            } else {
                y += row_h[r] + sv;
            }
        }
        let rows_end = if nr > 0 { y - sv } else { y };
        let height = (y + padding.bottom + border.bottom).max(spec_h.unwrap_or(0.0));
        let rows_top = row_y.first().copied().unwrap_or(y);

        // The cells, aligned in their rows.
        let mut cell_frags: Vec<Vec<Fragment>> = (0..nr).map(|_| Vec::new()).collect();
        for (i, (c, l)) in self.cells.iter().zip(laid).enumerate() {
            let rows = c.row..c.row + c.rowspan;
            let open = rows.clone().filter(|&r| !row_collapsed[r]).count();
            let span_h = row_h[rows].iter().sum::<f64>() + sv * open.saturating_sub(1) as f64;
            let extent = l.content + l.border.vertical() + l.padding.vertical();
            let off = match l.align {
                VAlign::Top => 0.0,
                VAlign::Middle => (span_h - extent) / 2.0,
                VAlign::Bottom => span_h - extent,
                VAlign::Baseline => base[c.row] - l.baseline,
            };
            let cs = &c.b.style;
            let anonymous = matches!(&c.b.kind, BoxKind::Container(cc) if cc.anonymous);
            let mut f = Fragment::new(
                FragmentKind::Block {
                    anonymous,
                    bfc_root: true,
                },
                cs.clone(),
            );
            f.node = c.b.node;
            f.pseudo = c.b.pseudo;
            f.border = l.border;
            f.padding = l.padding;
            // Relative to the row, which starts at `area_x`.
            f.rect = Rect::new(l.x - area_x, 0.0, l.width, span_h);
            f.children = l.children;
            for ch in &mut f.children {
                ch.translate(
                    l.border.left + l.padding.left,
                    l.border.top + l.padding.top + off,
                );
            }
            if self.collapse {
                let [t, r, b, lt] = self.cell_edges(i);
                f.collapsed_borders = Some(Box::new([
                    widest(t.into_iter()),
                    widest(r.into_iter()),
                    widest(b.into_iter()),
                    widest(lt.into_iter()),
                ]));
            } else if kw(cs, "empty-cells") == "hide" && is_empty(c.b) {
                f.empty_hidden = true;
            }
            cell_frags[c.row].push(f);
        }

        // The table grid box with its columns, row groups and rows.
        let anonymous = matches!(&self.b.kind, BoxKind::Container(c) if c.anonymous);
        let mut grid = Fragment::new(
            FragmentKind::Block {
                anonymous,
                bfc_root: true,
            },
            s.clone(),
        );
        grid.node = self.b.node;
        grid.pseudo = self.b.pseudo;
        grid.border = border;
        grid.padding = padding;
        grid.rect = Rect::new(0.0, y0, width, height);
        if self.collapse && nr > 0 && nc > 0 {
            let [t, r, b, l] = self.outer_edges();
            grid.collapsed_borders = Some(Box::new([
                widest(t.into_iter()),
                widest(r.into_iter()),
                widest(b.into_iter()),
                widest(l.into_iter()),
            ]));
        }
        let part = |b: &BoxNode, rect: Rect| {
            let anonymous = matches!(&b.kind, BoxKind::Container(c) if c.anonymous);
            let mut f = Fragment::new(
                FragmentKind::Block {
                    anonymous,
                    bfc_root: false,
                },
                b.style.clone(),
            );
            f.node = b.node;
            f.pseudo = b.pseudo;
            f.rect = rect;
            f
        };
        let cols_rect = |r: Range<usize>| {
            let (x0, cw) = span_x(r);
            Rect::new(x0, rows_top, cw, rows_end - rows_top)
        };
        // Column groups and columns (painted below the rows, css-tables-3
        // §3.11).
        let mut k = 0;
        while k < nc {
            let g = self.cols[k].group;
            let start = k;
            while k < nc && same(self.cols[k].group, g) {
                k += 1;
            }
            let mut col_frags = Vec::new();
            let mut j = start;
            while j < k {
                let cb = self.cols[j].col;
                let s0 = j;
                while j < k && same(self.cols[j].col, cb) {
                    j += 1;
                }
                if let Some(b) = cb {
                    col_frags.push(part(b, cols_rect(s0..j)));
                }
            }
            match g {
                Some(gb) => {
                    let mut gf = part(gb, cols_rect(start..k));
                    for f in &mut col_frags {
                        f.translate(-gf.rect.x, -gf.rect.y);
                    }
                    gf.children = col_frags;
                    grid.children.push(gf);
                }
                None => grid.children.extend(col_frags),
            }
        }
        let mut cell_frags = cell_frags.into_iter();
        let mut taken = 0;
        for g in &self.groups {
            let gy = if g.start < nr {
                row_y[g.start]
            } else {
                rows_end
            };
            let gh = if g.end > g.start {
                row_y[g.end - 1] + row_h[g.end - 1] - gy
            } else {
                0.0
            };
            let mut gf = part(g.b, Rect::new(area_x, gy, area_w, gh));
            for r in g.start..g.end {
                let mut rf = part(
                    self.rows[r].b,
                    Rect::new(0.0, row_y[r] - gy, area_w, row_h[r]),
                );
                while taken <= r {
                    if let Some(cells) = cell_frags.next() {
                        if taken == r {
                            rf.children = cells;
                        }
                    }
                    taken += 1;
                }
                gf.children.push(rf);
            }
            grid.children.push(gf);
        }
        // Out-of-flow children are at the start of the content box.
        let content_w = (width - border.horizontal() - padding.horizontal()).max(0.0);
        for b in &self.oof {
            grid.children.push(block::static_position(
                b,
                border.left + padding.left,
                border.top + padding.top,
                content_w,
            ));
        }
        grid
    }
}

/// The largest width the columns `ms` need to be at their max-content
/// widths with their percentages honoured.
fn max_columns(ms: &[Measure]) -> f64 {
    let mut w: f64 = ms.iter().map(|m| m.max).sum();
    let pct: f64 = ms.iter().filter_map(|m| m.pct).sum();
    for m in ms {
        if let Some(p) = m.pct {
            if p > EPS {
                w = w.max(m.max * 100.0 / p);
            }
        }
    }
    let rest: f64 = ms.iter().filter(|m| m.pct.is_none()).map(|m| m.max).sum();
    if pct < 100.0 - EPS {
        w = w.max(rest * 100.0 / (100.0 - pct));
    }
    w
}

/// Grows the min-content (`min`) or max-content widths of the columns
/// `range` so that they add up to `need`, in proportion to their
/// max-content widths (equally when those are all 0).
fn grow(ms: &mut [Measure], range: Range<usize>, need: f64, min: bool) {
    let get = |m: &Measure| if min { m.min } else { m.max };
    let have: f64 = ms[range.clone()].iter().map(get).sum();
    if need <= have + EPS {
        return;
    }
    let excess = need - have;
    let weights: Vec<f64> = ms[range.clone()].iter().map(|m| m.max).collect();
    let total: f64 = weights.iter().sum();
    let n = weights.len() as f64;
    for (m, wt) in ms[range].iter_mut().zip(weights) {
        let add = if total > EPS {
            excess * wt / total
        } else {
            excess / n
        };
        if min {
            m.min += add;
        } else {
            m.max += add;
        }
    }
}

/// Adds `excess` to `sizes` in proportion to them (equally when they are
/// all 0); nothing when it is not positive.
fn spread(sizes: &mut [f64], excess: f64) {
    if excess <= EPS || sizes.is_empty() {
        return;
    }
    let total: f64 = sizes.iter().sum();
    let n = sizes.len() as f64;
    for s in sizes.iter_mut() {
        *s += if total > EPS {
            excess * *s / total
        } else {
            excess / n
        };
    }
}

/// Adds `excess` to the columns `idx` of `out` in proportion to
/// `weight`; false when the weights add up to 0.
fn spread_by(out: &mut [f64], idx: &[usize], weight: impl Fn(usize) -> f64, excess: f64) -> bool {
    let total: f64 = idx.iter().map(|&i| weight(i)).sum();
    if total <= EPS {
        return false;
    }
    for &i in idx {
        out[i] += excess * weight(i) / total;
    }
    true
}

/// The width distribution of automatic layout (css-tables-3 §3.9.3):
/// the used widths of columns `ms` that fill `space`.
fn distribute(ms: &[Measure], space: f64) -> Vec<f64> {
    let n = ms.len();
    if n == 0 {
        return Vec::new();
    }
    let pct_w = |m: &Measure| m.pct.map(|p| (p * space / 100.0).max(m.min));
    let guesses: [Vec<f64>; 4] = [
        ms.iter().map(|m| m.min).collect(),
        ms.iter().map(|m| pct_w(m).unwrap_or(m.min)).collect(),
        ms.iter()
            .map(|m| pct_w(m).unwrap_or(if m.fixed { m.max } else { m.min }))
            .collect(),
        ms.iter().map(|m| pct_w(m).unwrap_or(m.max)).collect(),
    ];
    let sums: Vec<f64> = guesses.iter().map(|g| g.iter().sum()).collect();
    if space <= sums[0] + EPS {
        return guesses[0].clone();
    }
    for k in 0..3 {
        if space <= sums[k + 1] + EPS {
            let d = sums[k + 1] - sums[k];
            let t = if d > EPS { (space - sums[k]) / d } else { 1.0 };
            return (0..n)
                .map(|i| guesses[k][i] + t * (guesses[k + 1][i] - guesses[k][i]))
                .collect();
        }
    }
    // Past the max-content guess: the automatic columns grow first, then
    // the ones with specified widths, then the percentage ones.
    let mut out = guesses[3].clone();
    let excess = space - sums[3];
    let auto: Vec<usize> = (0..n)
        .filter(|&i| !ms[i].fixed && ms[i].pct.is_none())
        .collect();
    let fixed: Vec<usize> = (0..n)
        .filter(|&i| ms[i].fixed && ms[i].pct.is_none())
        .collect();
    let pct: Vec<usize> = (0..n).filter(|&i| ms[i].pct.is_some()).collect();
    let all: Vec<usize> = (0..n).collect();
    let current = out.clone();
    let done = spread_by(&mut out, &auto, |i| ms[i].max, excess)
        || spread_by(&mut out, &auto, |_| 1.0, excess)
        || spread_by(&mut out, &fixed, |i| current[i], excess)
        || spread_by(&mut out, &pct, |i| ms[i].pct.unwrap_or(0.0), excess);
    if !done {
        spread_by(&mut out, &all, |_| 1.0, excess);
    }
    out
}

// ----- the table wrapper box -------------------------------------------------------------------

/// The used width of the content box of table wrapper box `b` (the
/// border box of its table, at least as wide as its captions' min-content
/// widths) for containing block width `cbw` with `avail` available
/// (css-tables-3 §3.9.2).
pub(super) fn used_width(ctx: &mut Ctx, b: &BoxNode, cbw: Option<f64>, avail: f64) -> f64 {
    let (captions, grid) = parts(b);
    let mut used = 0.0f64;
    if let Some(g) = grid {
        let t = Table::new(g);
        let ms = (!t.fixed_layout()).then(|| t.measures(ctx));
        let (min, max) = t.widths(ms.as_deref(), cbw);
        used = match t.specified("width", cbw) {
            Some(w) => w,
            None => max.min(avail.max(min)),
        };
        if let Some(m) = t.specified("max-width", cbw) {
            used = used.min(m);
        }
        if let Some(m) = t.specified("min-width", cbw) {
            used = used.max(m);
        }
        used = used.max(min);
    }
    for cap in captions {
        used = used.max(block::contribution(ctx, cap, cbw).0);
    }
    used
}

/// The min-content and max-content widths of the content box of table
/// wrapper box `b`.
pub(super) fn intrinsic(ctx: &mut Ctx, b: &BoxNode) -> (f64, f64) {
    let (captions, grid) = parts(b);
    let (mut min, mut max) = (0.0f64, 0.0f64);
    if let Some(g) = grid {
        let t = Table::new(g);
        let ms = (!t.fixed_layout()).then(|| t.measures(ctx));
        let (tmin, tmax) = t.widths(ms.as_deref(), None);
        (min, max) = match t.specified("width", None) {
            Some(w) => (w.max(tmin), w.max(tmin)),
            None => (tmin, tmax),
        };
    }
    for cap in captions {
        let (cmin, _) = block::contribution(ctx, cap, None);
        min = min.max(cmin);
        max = max.max(cmin);
    }
    (min, max)
}

/// Lays out table wrapper box `b` with content width `w`: the captions
/// whose `caption-side` is `top` above the table grid box, the others
/// below it.
pub(super) fn layout(ctx: &mut Ctx, b: &BoxNode, w: f64, cbh: Option<f64>) -> Independent {
    let (captions, grid) = parts(b);
    let mut above = Vec::new();
    let mut below = Vec::new();
    for cap in captions {
        let laid = block::layout_alone(ctx, cap, w);
        if matches!(
            kw(&cap.style, "caption-side").as_str(),
            "bottom" | "block-end"
        ) {
            below.push(laid);
        } else {
            above.push(laid);
        }
    }
    let mut y = 0.0;
    let mut children = Vec::new();
    for (mut f, h) in above {
        f.translate(0.0, y);
        y += h;
        children.push(f);
    }
    if let Some(g) = grid {
        let frag = Table::new(g).layout(ctx, w, cbh, y);
        y += frag.rect.height;
        children.push(frag);
    }
    for (mut f, h) in below {
        f.translate(0.0, y);
        y += h;
        children.push(f);
    }
    Independent {
        children,
        height: y,
    }
}

/// The baseline of a laid out table wrapper box (fragment `wrapper`),
/// from its border box: the baseline of the first row of its table
/// that has a cell with a line box (CSS 2 §17.5.3).
pub(super) fn baseline(wrapper: &Fragment) -> Option<f64> {
    let grid = wrapper
        .children
        .iter()
        .find(|c| c.node == wrapper.node && c.pseudo == wrapper.pseudo)?;
    for part in &grid.children {
        for row in &part.children {
            for cell in &row.children {
                if let Some(bl) = block::find_baseline(&cell.children, false) {
                    return Some(grid.rect.y + part.rect.y + row.rect.y + cell.rect.y + bl);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(min: f64, max: f64) -> Measure {
        Measure {
            min,
            max,
            pct: None,
            fixed: false,
        }
    }

    #[test]
    fn width_distribution_interpolates_between_guesses() {
        let ms = [m(10.0, 50.0), m(20.0, 20.0)];
        // Between min-content (30) and max-content (70).
        assert_eq!(distribute(&ms, 50.0), vec![30.0, 20.0]);
        // Below the minimum: the minimum.
        assert_eq!(distribute(&ms, 10.0), vec![10.0, 20.0]);
        // Past max-content: in proportion to max-content.
        assert_eq!(distribute(&ms, 140.0), vec![100.0, 40.0]);
        // A percentage column takes its share first.
        let pct = [
            Measure {
                pct: Some(50.0),
                ..m(0.0, 10.0)
            },
            m(0.0, 10.0),
        ];
        assert_eq!(distribute(&pct, 200.0), vec![100.0, 100.0]);
        assert_eq!(max_columns(&pct), 20.0);
    }

    #[test]
    fn spanning_cells_grow_columns_by_max_content() {
        let mut ms = vec![m(30.0, 30.0), m(10.0, 10.0)];
        grow(&mut ms, 0..2, 100.0, true);
        assert_eq!(ms[0].min, 75.0);
        assert_eq!(ms[1].min, 25.0);
    }

    #[test]
    fn border_conflicts() {
        let e = |width: f64, rank: u8, origin: u8| Edge {
            width,
            rank,
            origin,
            hidden: false,
            source: None,
            side: Side::Top,
        };
        // Wider wins, then the style, then the origin.
        assert!(e(3.0, 1, TABLE).beats(&e(2.0, 8, CELL)));
        assert!(e(2.0, 8, TABLE).beats(&e(2.0, 7, CELL)));
        assert!(e(2.0, 7, CELL).beats(&e(2.0, 7, ROW)));
        // On a full tie the first stays.
        assert!(!e(2.0, 7, CELL).beats(&e(2.0, 7, CELL)));
        // `hidden` wins over everything.
        let hidden = Edge {
            hidden: true,
            ..Edge::none()
        };
        assert!(hidden.beats(&e(10.0, 8, CELL)));
        assert!(!e(10.0, 8, CELL).beats(&hidden));
    }
}
