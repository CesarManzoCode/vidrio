//! Grid layout (css-grid-2) with the box alignment of css-align-3:
//! explicit tracks from `grid-template-columns` and `-rows` (`repeat()`
//! with a count, `auto-fill` and `auto-fit`, `minmax()`, `fit-content()`,
//! `fr` and line names), named areas (`grid-template-areas`) with their
//! implicit line names, implicit tracks from `grid-auto-columns` and
//! `-rows` on both sides of the explicit grid, placement by lines, names
//! and spans (§8.3) and the automatic placement algorithm in both flow
//! directions, sparse and dense (§8.5), the track sizing algorithm (§12)
//! with the intrinsic contributions of items (the automatic minimum size
//! included), items that span several tracks and items that cross
//! flexible tracks, `auto-fit` tracks that collapse when empty, `gap`,
//! `justify-content` and `align-content` over the tracks, `justify-self`
//! and `align-self` (`justify-items` and `align-items`, `auto` margins,
//! `safe`, first and last baseline sharing groups in rows), absolutely
//! positioned children with their grid area as containing block, subgrids
//! in either or both axes (their items take part in the parent's track
//! sizing with the subgrid's margins, borders and paddings at its edges),
//! the intrinsic sizes of the container and the resolved values of
//! `grid-template-columns` and `-rows` (§7.2.6).
//!
//! Track positions are physical offsets from the content box of the
//! container (columns are mirrored in `rtl`); the fragments come relative
//! to the content box, as `block::layout_independent` returns them.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use crate::css::style::ComputedStyle;
use crate::css::values::{Value, number};

use super::Ctx;
use super::block::{self, Cb, Independent, Model};
use super::boxtree::{BoxKind, BoxNode, Container, Contents};
use super::flex::{Pos, parse_align};
use super::fragment::{Fragment, FragmentKind, Rect, Sides};
use super::values::{self, Display, Inner, Lp, Margins, Size, kw, value_lp};

const EPS: f64 = 1e-7;
const INF: f64 = f64::INFINITY;
const COL: usize = 0;
const ROW: usize = 1;
/// A bound on the tracks of one axis, so that `repeat()` with a huge
/// count or tiny tracks stays finite (a design choice, not a measured
/// browser limit).
const MAX_TRACKS: usize = 10_000;

/// The box is a grid container.
pub(super) fn is_grid(s: &ComputedStyle) -> bool {
    matches!(
        values::display(s),
        Display::Box {
            inner: Inner::Grid,
            ..
        }
    )
}

fn blocks(c: &Container) -> &[BoxNode] {
    match &c.contents {
        Contents::Blocks(v) => v,
        Contents::Inline(_) => &[],
    }
}

fn order(s: &ComputedStyle) -> i64 {
    match s.get("order").and_then(|v| v.values.as_deref()) {
        Some([Value::Integer(n)]) | Some([Value::Number(n)]) => *n as i64,
        _ => 0,
    }
}

/// The key of a box in [`Ctx::subgrids`].
fn key(b: &BoxNode) -> usize {
    b as *const BoxNode as usize
}

// ----- track sizing functions ------------------------------------------------------------

/// A minimum or maximum track sizing function.
#[derive(Clone, Debug)]
enum Breadth {
    Fixed(Lp),
    Fr(f64),
    MinContent,
    MaxContent,
    Auto,
}

/// A `<track-size>`: `fit-content(L)` is `auto` as minimum and
/// max-content limited by `L` as maximum.
#[derive(Clone, Debug)]
struct TrackSize {
    min: Breadth,
    max: Breadth,
    fit: Option<Lp>,
}

impl TrackSize {
    fn auto() -> TrackSize {
        TrackSize {
            min: Breadth::Auto,
            max: Breadth::Auto,
            fit: None,
        }
    }
}

fn breadth(v: &Value) -> Option<Breadth> {
    match v {
        Value::Keyword(k) => match k.as_str() {
            "auto" => Some(Breadth::Auto),
            "min-content" => Some(Breadth::MinContent),
            "max-content" => Some(Breadth::MaxContent),
            _ => None,
        },
        Value::Dimension(n, u) if u == "fr" => Some(Breadth::Fr(n.max(0.0))),
        v => value_lp(v).map(Breadth::Fixed),
    }
}

fn track_size(v: &Value) -> Option<TrackSize> {
    match v {
        Value::Function(f, args) if f == "minmax" => {
            let parts: Vec<&Value> = args.iter().filter(|a| **a != Value::Comma).collect();
            let min = match breadth(parts.first()?)? {
                // A flexible minimum is not valid; it is `auto`.
                Breadth::Fr(_) => Breadth::Auto,
                b => b,
            };
            let max = breadth(parts.get(1)?)?;
            Some(TrackSize {
                min,
                max,
                fit: None,
            })
        }
        Value::Function(f, args) if f == "fit-content" => Some(TrackSize {
            min: Breadth::Auto,
            max: Breadth::MaxContent,
            fit: Some(args.first().and_then(value_lp)?),
        }),
        v => Some(match breadth(v)? {
            Breadth::Fr(n) => TrackSize {
                min: Breadth::Auto,
                max: Breadth::Fr(n),
                fit: None,
            },
            b => TrackSize {
                min: b.clone(),
                max: b,
                fit: None,
            },
        }),
    }
}

// ----- templates -------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Repeat {
    Count(usize),
    Fill,
    Fit,
}

/// One piece of a track list or of a subgrid's line name list.
#[derive(Clone, Debug)]
enum Entry {
    Names(Vec<String>),
    Track(TrackSize),
    Repeat(Repeat, Vec<Entry>),
}

enum Template {
    None,
    Tracks(Vec<Entry>),
    Subgrid(Vec<Entry>),
}

fn names_of(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .filter_map(|v| match v {
            Value::Ident(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

fn entries(values: &[Value]) -> Vec<Entry> {
    let mut out = Vec::new();
    for v in values {
        match v {
            Value::Block(items) => out.push(Entry::Names(names_of(items))),
            Value::Function(f, args) if f == "repeat" => {
                let Some(i) = args.iter().position(|a| *a == Value::Comma) else {
                    continue;
                };
                let count = match &args[..i] {
                    [Value::Keyword(k)] if k == "auto-fill" => Repeat::Fill,
                    [Value::Keyword(k)] if k == "auto-fit" => Repeat::Fit,
                    [Value::Integer(n)] | [Value::Number(n)] => {
                        Repeat::Count(n.clamp(1.0, MAX_TRACKS as f64) as usize)
                    }
                    _ => continue,
                };
                out.push(Entry::Repeat(count, entries(&args[i + 1..])));
            }
            v => {
                if let Some(t) = track_size(v) {
                    out.push(Entry::Track(t));
                }
            }
        }
    }
    out
}

fn template(s: &ComputedStyle, name: &str) -> Template {
    let Some(values) = s.get(name).and_then(|v| v.values.as_deref()) else {
        return Template::None;
    };
    match values {
        [Value::Keyword(k)] if k == "none" => Template::None,
        [Value::Keyword(k), rest @ ..] if k == "subgrid" => Template::Subgrid(entries(rest)),
        v => Template::Tracks(entries(v)),
    }
}

/// The explicit tracks of one axis from its template.
#[derive(Clone, Debug)]
struct Explicit {
    tracks: Vec<TrackSize>,
    /// The names of each explicit line (one more than the tracks).
    names: Vec<Vec<String>>,
    /// The tracks a `repeat(auto-fit, …)` made.
    auto_fit: Option<(usize, usize)>,
}

impl Explicit {
    fn empty() -> Explicit {
        Explicit {
            tracks: Vec::new(),
            names: vec![Vec::new()],
            auto_fit: None,
        }
    }

    fn push_names(&mut self, n: &[String]) {
        if let Some(last) = self.names.last_mut() {
            last.extend(n.iter().cloned());
        }
    }

    fn push_track(&mut self, t: &TrackSize) {
        if self.tracks.len() < MAX_TRACKS {
            self.tracks.push(t.clone());
            self.names.push(Vec::new());
        }
    }
}

/// Expands a track list with `reps` repetitions for its automatic
/// `repeat()`; adjacent line names merge.
fn expand(list: &[Entry], reps: usize) -> Explicit {
    let mut e = Explicit::empty();
    for entry in list {
        match entry {
            Entry::Names(n) => e.push_names(n),
            Entry::Track(t) => e.push_track(t),
            Entry::Repeat(r, inner) => {
                let count = match r {
                    Repeat::Count(n) => *n,
                    _ => reps,
                };
                let start = e.tracks.len();
                for _ in 0..count {
                    for x in inner {
                        match x {
                            Entry::Names(n) => e.push_names(n),
                            Entry::Track(t) => e.push_track(t),
                            Entry::Repeat(..) => {}
                        }
                    }
                }
                if *r == Repeat::Fit {
                    e.auto_fit = Some((start, e.tracks.len()));
                }
            }
        }
    }
    e
}

/// The number of repetitions of an automatic `repeat()` (§7.2.3.2): the
/// most that fit in `space` (the definite or maximum size of the content
/// box), else the fewest that fill `min`, and at least one. Each track
/// counts as its maximum sizing function when that is definite, else its
/// minimum.
fn auto_count(list: &[Entry], space: Option<f64>, min: Option<f64>, gap: f64) -> usize {
    let Some(inner) = list.iter().find_map(|e| match e {
        Entry::Repeat(Repeat::Fill | Repeat::Fit, inner) => Some(inner),
        _ => None,
    }) else {
        return 1;
    };
    let basis = space.or(min);
    let fixed = |t: &TrackSize| -> f64 {
        let def = |b: &Breadth| match b {
            Breadth::Fixed(l) => l.resolve_opt(basis),
            _ => None,
        };
        match (def(&t.min), def(&t.max)) {
            (Some(a), Some(b)) => b.max(a),
            (None, Some(b)) => b,
            (Some(a), None) => a,
            (None, None) => 0.0,
        }
        .max(0.0)
    };
    let sizes = |es: &[Entry], skip_auto: bool| -> Vec<f64> {
        let mut v = Vec::new();
        for e in es {
            match e {
                Entry::Track(t) => v.push(fixed(t)),
                Entry::Repeat(Repeat::Count(n), inner) => {
                    for _ in 0..*n {
                        for x in inner {
                            if let Entry::Track(t) = x {
                                v.push(fixed(t));
                            }
                        }
                    }
                }
                Entry::Repeat(_, inner) if !skip_auto => {
                    for x in inner {
                        if let Entry::Track(t) = x {
                            v.push(fixed(t));
                        }
                    }
                }
                _ => {}
            }
        }
        v
    };
    let outside = sizes(list, true);
    let inside = sizes(inner, false);
    if inside.is_empty() {
        return 1;
    }
    let (out_sum, in_sum) = (outside.iter().sum::<f64>(), inside.iter().sum::<f64>());
    if in_sum + gap * inside.len() as f64 <= EPS {
        return 1;
    }
    let cap = (MAX_TRACKS / inside.len()).max(1);
    let total = |n: usize| {
        let count = outside.len() + n * inside.len();
        out_sum + n as f64 * in_sum + gap * count.saturating_sub(1) as f64
    };
    let mut n = 1;
    match (space, min) {
        (Some(s), _) => {
            while n < cap && total(n + 1) <= s + EPS {
                n += 1;
            }
        }
        (None, Some(m)) => {
            while n < cap && total(n) < m - EPS {
                n += 1;
            }
        }
        _ => {}
    }
    n
}

/// The line names of a subgrid's `<line-name-list>` for its `lines`
/// lines (an `auto-fill` repetition fills the lines the others leave).
fn subgrid_names(list: &[Entry], lines: usize) -> Vec<Vec<String>> {
    let inner_names = |inner: &[Entry]| -> Vec<Vec<String>> {
        inner
            .iter()
            .filter_map(|x| match x {
                Entry::Names(n) => Some(n.clone()),
                _ => None,
            })
            .collect()
    };
    let fixed: usize = list
        .iter()
        .map(|e| match e {
            Entry::Names(_) => 1,
            Entry::Repeat(Repeat::Count(n), inner) => n * inner_names(inner).len(),
            _ => 0,
        })
        .sum();
    let mut out = Vec::new();
    for e in list {
        match e {
            Entry::Names(n) => out.push(n.clone()),
            Entry::Repeat(Repeat::Count(k), inner) => {
                for _ in 0..*k {
                    out.extend(inner_names(inner));
                }
            }
            Entry::Repeat(_, inner) => {
                let names = inner_names(inner);
                if !names.is_empty() {
                    for _ in 0..lines.saturating_sub(fixed) / names.len() {
                        out.extend(names.iter().cloned());
                    }
                }
            }
            Entry::Track(_) => {}
        }
        if out.len() > lines {
            break;
        }
    }
    out.truncate(lines);
    out.resize(lines, Vec::new());
    out
}

/// The span a subgrid takes when its placement does not give one: one
/// less than the line names it lists, at least one.
fn subgrid_span(list: &[Entry]) -> usize {
    let names: usize = list
        .iter()
        .map(|e| match e {
            Entry::Names(_) => 1,
            Entry::Repeat(Repeat::Count(n), inner) => {
                n * inner
                    .iter()
                    .filter(|x| matches!(x, Entry::Names(_)))
                    .count()
            }
            _ => 0,
        })
        .sum();
    names.saturating_sub(1).max(1)
}

/// The axes in which `b` is a subgrid (a grid container whose template
/// is `subgrid`; only meaningful for a grid item).
fn subgridded(b: &BoxNode) -> [Option<Vec<Entry>>; 2] {
    if !is_grid(&b.style) || !matches!(b.kind, BoxKind::Container(_)) {
        return [None, None];
    }
    ["grid-template-columns", "grid-template-rows"].map(|n| match template(&b.style, n) {
        Template::Subgrid(e) => Some(e),
        _ => None,
    })
}

// ----- named areas -----------------------------------------------------------------------

/// `grid-template-areas`: its size and its areas by lines, per axis.
struct Areas {
    rows: usize,
    cols: usize,
    named: Vec<(String, [(usize, usize); 2])>,
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii()
}

/// The cells of one string (`None` for a null cell token); `None` when
/// it has a trash token.
fn area_cells(s: &str) -> Option<Vec<Option<String>>> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{C}') {
            i += 1;
        } else if c == '.' {
            while i < chars.len() && chars[i] == '.' {
                i += 1;
            }
            out.push(None);
        } else if is_name_char(c) {
            let start = i;
            while i < chars.len() && is_name_char(chars[i]) {
                i += 1;
            }
            out.push(Some(chars[start..i].iter().collect()));
        } else {
            return None;
        }
    }
    Some(out)
}

/// The named areas, if the strings make a grid of filled rectangles.
fn areas(s: &ComputedStyle) -> Option<Areas> {
    let values = s.get("grid-template-areas")?.values.as_deref()?;
    let rows: Vec<Vec<Option<String>>> = values
        .iter()
        .filter_map(|v| match v {
            Value::String(s) => Some(area_cells(s)),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let cols = rows.first()?.len();
    if cols == 0 || rows.iter().any(|r| r.len() != cols) {
        return None;
    }
    let mut named: Vec<(String, [(usize, usize); 2])> = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            let Some(n) = cell else {
                continue;
            };
            match named.iter_mut().find(|(x, _)| x == n) {
                Some((_, a)) => {
                    a[COL] = (a[COL].0.min(c), a[COL].1.max(c + 1));
                    a[ROW] = (a[ROW].0.min(r), a[ROW].1.max(r + 1));
                }
                None => named.push((n.clone(), [(c, c + 1), (r, r + 1)])),
            }
        }
    }
    for (n, a) in &named {
        for row in &rows[a[ROW].0..a[ROW].1] {
            for cell in &row[a[COL].0..a[COL].1] {
                if cell.as_deref() != Some(n.as_str()) {
                    return None;
                }
            }
        }
    }
    Some(Areas {
        rows: rows.len(),
        cols,
        named,
    })
}

// ----- line resolution -------------------------------------------------------------------

/// The lines of one axis of the explicit grid, numbered from 0, with
/// their names; lines outside the explicit grid are implicit.
struct Lines {
    count: usize,
    names: Vec<Vec<String>>,
    /// The named areas: name, start line and end line.
    areas: Vec<(String, usize, usize)>,
}

impl Lines {
    fn named(&self, line: usize, name: &str) -> bool {
        self.names
            .get(line)
            .is_some_and(|n| n.iter().any(|x| x == name))
            || self.areas.iter().any(|(a, s, e)| {
                (*s == line && name.strip_suffix("-start") == Some(a.as_str()))
                    || (*e == line && name.strip_suffix("-end") == Some(a.as_str()))
            })
    }

    fn any(&self, name: &str) -> bool {
        (0..=self.count).any(|l| self.named(l, name))
    }

    /// Whether line `l` counts as named `name` while searching: every
    /// implicit line does.
    fn counts(&self, l: i32, name: &str) -> bool {
        l < 0 || l > self.count as i32 || self.named(l as usize, name)
    }

    /// The `n`th line named `name` (negative: from the end).
    fn nth(&self, n: i32, name: &str) -> i32 {
        let last = self.count as i32;
        if n > 0 {
            let mut k = 0;
            for l in 0..=last {
                if self.named(l as usize, name) {
                    k += 1;
                    if k == n {
                        return l;
                    }
                }
            }
            last + (n - k)
        } else {
            let n = -n;
            let mut k = 0;
            for l in (0..=last).rev() {
                if self.named(l as usize, name) {
                    k += 1;
                    if k == n {
                        return l;
                    }
                }
            }
            -(n - k)
        }
    }

    /// The line a span of `n` (lines named `name`) reaches from `from`.
    fn forward(&self, from: i32, n: i32, name: Option<&str>) -> i32 {
        let Some(name) = name else {
            return from + n;
        };
        let (mut l, mut k) = (from, 0);
        loop {
            l += 1;
            if self.counts(l, name) {
                k += 1;
                if k >= n {
                    return l;
                }
            }
        }
    }

    fn backward(&self, from: i32, n: i32, name: Option<&str>) -> i32 {
        let Some(name) = name else {
            return from - n;
        };
        let (mut l, mut k) = (from, 0);
        loop {
            l -= 1;
            if self.counts(l, name) {
                k += 1;
                if k >= n {
                    return l;
                }
            }
        }
    }
}

/// A computed `grid-row-start` and the like.
#[derive(Clone, Debug)]
enum Raw {
    Auto,
    Line(i32, Option<String>),
    Ident(String),
    Span(i32, Option<String>),
}

fn raw(s: &ComputedStyle, name: &str) -> Raw {
    let Some(values) = s.get(name).and_then(|v| v.values.as_deref()) else {
        return Raw::Auto;
    };
    let (mut span, mut int, mut ident) = (false, None, None);
    for v in values {
        match v {
            Value::Keyword(k) if k == "span" => span = true,
            Value::Integer(n) | Value::Number(n) => int = Some(n.clamp(-1e4, 1e4) as i32),
            Value::Ident(i) => ident = Some(i.clone()),
            _ => {}
        }
    }
    if span {
        Raw::Span(int.unwrap_or(1).max(1), ident)
    } else if let Some(n) = int {
        if n == 0 {
            Raw::Auto
        } else {
            Raw::Line(n, ident)
        }
    } else {
        match ident {
            Some(i) => Raw::Ident(i),
            None => Raw::Auto,
        }
    }
}

/// The line a start (or end) placement names.
fn line_of(lines: &Lines, r: &Raw, start: bool) -> Option<i32> {
    match r {
        Raw::Line(n, None) => Some(if *n > 0 {
            n - 1
        } else {
            lines.count as i32 + 1 + n
        }),
        Raw::Line(n, Some(name)) => Some(lines.nth(*n, name)),
        Raw::Ident(name) => {
            let edge = format!("{name}-{}", if start { "start" } else { "end" });
            Some(if lines.any(&edge) {
                lines.nth(1, &edge)
            } else {
                lines.nth(1, name)
            })
        }
        _ => None,
    }
}

/// A resolved placement in one axis, in explicit line numbers.
#[derive(Clone, Copy, Debug)]
enum Place {
    Definite(i32, i32),
    /// Automatic, with a span.
    Auto(usize),
}

/// Resolves a pair of placement properties (§8.3.1).
fn resolve(lines: &Lines, start: &Raw, end: &Raw) -> Place {
    let span_of = |r: &Raw| match r {
        Raw::Span(n, None) => Some(*n as usize),
        // A span of a named line alone is a span of 1.
        Raw::Span(_, Some(_)) => Some(1),
        _ => None,
    };
    match (line_of(lines, start, true), line_of(lines, end, false)) {
        (Some(s), Some(e)) => {
            if e == s {
                Place::Definite(s, s + 1)
            } else if e < s {
                Place::Definite(e, s)
            } else {
                Place::Definite(s, e)
            }
        }
        (Some(s), None) => match end {
            Raw::Span(n, name) => Place::Definite(s, lines.forward(s, *n, name.as_deref())),
            _ => Place::Definite(s, s + 1),
        },
        (None, Some(e)) => match start {
            Raw::Span(n, name) => Place::Definite(lines.backward(e, *n, name.as_deref()), e),
            _ => Place::Definite(e - 1, e),
        },
        (None, None) => Place::Auto(span_of(start).or_else(|| span_of(end)).unwrap_or(1)),
    }
}

/// The lines of an absolutely positioned child (§9.1): `None` is the
/// padding edge.
fn resolve_abs(lines: &Lines, start: &Raw, end: &Raw) -> (Option<i32>, Option<i32>) {
    let mut s = line_of(lines, start, true);
    let mut e = line_of(lines, end, false);
    if let (None, Some(to), Raw::Span(n, name)) = (s, e, start) {
        s = Some(lines.backward(to, *n, name.as_deref()));
    }
    if let (Some(from), None, Raw::Span(n, name)) = (s, e, end) {
        e = Some(lines.forward(from, *n, name.as_deref()));
    }
    if let (Some(a), Some(b)) = (s, e) {
        if b == a {
            e = Some(a + 1);
        } else if b < a {
            (s, e) = (Some(b), Some(a));
        }
    }
    (s, e)
}

// ----- automatic placement -----------------------------------------------------------------

/// What placement gives: the tracks of every item per axis, the leading
/// implicit tracks and the track count of each axis.
struct Placed {
    areas: Vec<[(usize, usize); 2]>,
    lead: [usize; 2],
    count: [usize; 2],
}

/// The grid item placement algorithm (§8.5). `fixed` axes (subgridded)
/// have exactly their explicit tracks: items are clamped to them.
fn auto_place(
    places: &[[Place; 2]],
    explicit: [usize; 2],
    fixed: [bool; 2],
    column_flow: bool,
    dense: bool,
) -> Placed {
    let mut lead = [0usize; 2];
    for p in places {
        for ax in [COL, ROW] {
            if let (false, Place::Definite(s, _)) = (fixed[ax], p[ax]) {
                lead[ax] = lead[ax].max((-s).clamp(0, MAX_TRACKS as i32) as usize);
            }
        }
    }
    let conv = |ax: usize, s: i32, e: i32| -> (usize, usize) {
        let (s, e) = (s + lead[ax] as i32, e + lead[ax] as i32);
        if fixed[ax] {
            let n = explicit[ax].max(1) as i32;
            let s = s.clamp(0, n - 1);
            (s as usize, e.clamp(s + 1, n) as usize)
        } else {
            let s = s.clamp(0, MAX_TRACKS as i32);
            (s as usize, e.clamp(s + 1, MAX_TRACKS as i32 + 1) as usize)
        }
    };
    let span = |p: &[Place; 2], ax: usize| match p[ax] {
        Place::Auto(n) if fixed[ax] => n.clamp(1, explicit[ax].max(1)),
        Place::Auto(n) => n.clamp(1, MAX_TRACKS),
        Place::Definite(s, e) => (e - s).max(1) as usize,
    };
    let mut out: Vec<[Option<(usize, usize)>; 2]> = places
        .iter()
        .map(|p| {
            [COL, ROW].map(|ax| match p[ax] {
                Place::Definite(s, e) => Some(conv(ax, s, e)),
                Place::Auto(_) => None,
            })
        })
        .collect();
    let a = if column_flow { ROW } else { COL };
    let b = 1 - a;
    let mut a_count = if fixed[a] {
        explicit[a]
    } else {
        let mut n = lead[a] + explicit[a];
        for (i, p) in places.iter().enumerate() {
            n = n.max(out[i][a].map_or(span(p, a), |r| r.1));
        }
        n
    };
    let b_fixed = fixed[b].then_some(explicit[b]);
    let mut occ: HashSet<(usize, usize)> = HashSet::new();
    let free = |occ: &HashSet<(usize, usize)>, ar: (usize, usize), br: (usize, usize)| {
        (ar.0..ar.1).all(|x| (br.0..br.1).all(|y| !occ.contains(&(x, y))))
    };
    let mark = |occ: &mut HashSet<(usize, usize)>, ar: (usize, usize), br: (usize, usize)| {
        for x in ar.0..ar.1 {
            for y in br.0..br.1 {
                occ.insert((x, y));
            }
        }
    };
    // 1. Items with a definite position in both axes.
    for o in &out {
        if let [Some(c), Some(r)] = *o {
            let (ar, br) = if a == COL { (c, r) } else { (r, c) };
            mark(&mut occ, ar, br);
        }
    }
    // 2. Items locked to a row (column in column flow).
    let mut cursors: HashMap<usize, usize> = HashMap::new();
    for i in 0..out.len() {
        let (Some(br), None) = (out[i][b], out[i][a]) else {
            continue;
        };
        let sa = span(&places[i], a);
        let mut x = if dense {
            0
        } else {
            cursors.get(&br.0).copied().unwrap_or(0)
        };
        loop {
            if fixed[a] && x + sa > a_count {
                x = a_count.saturating_sub(sa);
                break;
            }
            if free(&occ, (x, x + sa), br) {
                break;
            }
            x += 1;
        }
        a_count = a_count.max(x + sa);
        out[i][a] = Some((x, x + sa));
        mark(&mut occ, (x, x + sa), br);
        cursors.insert(br.0, x + sa);
    }
    // 4. Everything else, with the auto-placement cursor.
    let (mut ca, mut cb) = (0usize, 0usize);
    for i in 0..out.len() {
        if out[i][a].is_some() && out[i][b].is_some() {
            continue;
        }
        let sb = span(&places[i], b);
        if let Some(ar) = out[i][a] {
            let mut y = if dense {
                0
            } else {
                if ar.0 < ca {
                    cb += 1;
                }
                ca = ar.0;
                cb
            };
            loop {
                if let Some(n) = b_fixed {
                    if y + sb > n {
                        y = n.saturating_sub(sb);
                        break;
                    }
                }
                if free(&occ, ar, (y, y + sb)) {
                    break;
                }
                y += 1;
            }
            if !dense {
                cb = y;
            }
            out[i][b] = Some((y, y + sb));
            mark(&mut occ, ar, (y, y + sb));
        } else {
            let sa = span(&places[i], a).min(a_count.max(1));
            if dense {
                (ca, cb) = (0, 0);
            }
            loop {
                if ca + sa > a_count.max(sa) {
                    ca = 0;
                    cb += 1;
                    continue;
                }
                if let Some(n) = b_fixed {
                    if cb + sb > n {
                        cb = n.saturating_sub(sb);
                        break;
                    }
                }
                if free(&occ, (ca, ca + sa), (cb, cb + sb)) {
                    break;
                }
                ca += 1;
            }
            a_count = a_count.max(ca + sa);
            out[i][a] = Some((ca, ca + sa));
            out[i][b] = Some((cb, cb + sb));
            mark(&mut occ, (ca, ca + sa), (cb, cb + sb));
        }
    }
    let areas: Vec<[(usize, usize); 2]> = out
        .into_iter()
        .map(|o| [o[COL].unwrap_or((0, 1)), o[ROW].unwrap_or((0, 1))])
        .collect();
    let mut count = [0usize; 2];
    for ax in [COL, ROW] {
        count[ax] = if fixed[ax] {
            explicit[ax]
        } else {
            let ends = areas.iter().map(|r| r[ax].1).max().unwrap_or(0);
            (lead[ax] + explicit[ax]).max(ends)
        };
    }
    count[a] = count[a].max(if fixed[a] { 0 } else { a_count });
    Placed { areas, lead, count }
}

// ----- tracks ------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum MinK {
    Fixed,
    MinContent,
    MaxContent,
    Auto,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum MaxK {
    Fixed,
    MinContent,
    MaxContent,
    Auto,
    /// `fit-content()` with its argument (infinite when it does not
    /// resolve).
    Fit(f64),
    Flex(f64),
}

impl MaxK {
    fn intrinsic(self) -> bool {
        matches!(
            self,
            MaxK::MinContent | MaxK::MaxContent | MaxK::Auto | MaxK::Fit(_)
        )
    }
}

#[derive(Clone, Debug)]
struct Track {
    size: TrackSize,
    mn: MinK,
    mx: MaxK,
    /// The value of a fixed maximum.
    fmax: f64,
    base: f64,
    limit: f64,
    growable: bool,
    /// An empty `auto-fit` track.
    collapsed: bool,
}

impl Track {
    fn new(size: TrackSize) -> Track {
        Track {
            size,
            mn: MinK::Auto,
            mx: MaxK::Auto,
            fmax: 0.0,
            base: 0.0,
            limit: INF,
            growable: false,
            collapsed: false,
        }
    }

    /// Initializes the base size and growth limit (§12.4); percentages
    /// of an indefinite `basis` behave as `auto`.
    fn init(&mut self, basis: Option<f64>) {
        self.growable = false;
        if self.collapsed {
            (self.mn, self.mx) = (MinK::Fixed, MaxK::Fixed);
            (self.fmax, self.base, self.limit) = (0.0, 0.0, 0.0);
            return;
        }
        let fixed = |b: &Breadth| match b {
            Breadth::Fixed(l) => l.resolve_opt(basis).map(|v| v.max(0.0)),
            _ => None,
        };
        let min = fixed(&self.size.min);
        self.mn = match (&self.size.min, min) {
            (Breadth::Fixed(_), Some(_)) => MinK::Fixed,
            (Breadth::MinContent, _) => MinK::MinContent,
            (Breadth::MaxContent, _) => MinK::MaxContent,
            _ => MinK::Auto,
        };
        self.base = min.unwrap_or(0.0);
        self.mx = match (&self.size.max, fixed(&self.size.max)) {
            (Breadth::Fixed(_), Some(v)) => {
                self.fmax = v;
                MaxK::Fixed
            }
            (Breadth::Fr(f), _) => MaxK::Flex(*f),
            (Breadth::MinContent, _) => MaxK::MinContent,
            (Breadth::MaxContent, _) => MaxK::MaxContent,
            _ => MaxK::Auto,
        };
        if let Some(l) = &self.size.fit {
            self.mx = MaxK::Fit(l.resolve_opt(basis).unwrap_or(INF).max(0.0));
        }
        self.limit = if self.mx == MaxK::Fixed {
            self.fmax
        } else {
            INF
        };
        if self.limit < self.base {
            self.limit = self.base;
        }
    }

    fn flex(&self) -> Option<f64> {
        match self.mx {
            MaxK::Flex(f) => Some(f),
            _ => None,
        }
    }
}

/// The sum of the base sizes of `tracks` and the gaps between the ones
/// that are not collapsed.
fn used(tracks: &[Track], gap: f64) -> f64 {
    let live = tracks.iter().filter(|t| !t.collapsed).count();
    tracks.iter().map(|t| t.base).sum::<f64>() + gap * live.saturating_sub(1) as f64
}

/// The available space of a track sizing run.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Avail {
    Definite(f64),
    /// An automatic size in the block axis (rows of an auto-height grid).
    Indefinite,
    MinContent,
    MaxContent,
}

/// An item's part in sizing the tracks of one axis: its tracks and its
/// contributions (outer sizes, with the extra margins of the subgrids it
/// is in).
#[derive(Clone, Debug)]
struct Part {
    start: usize,
    end: usize,
    minc: f64,
    maxc: f64,
    /// The outer content-based minimum when the automatic minimum size
    /// applies (`None`: the minimum size is not `auto`, or it scrolls).
    auto_min: Option<f64>,
    /// The outer size of the used minimum size otherwise.
    fixed_min: f64,
    /// The preferred size is definite: the minimum contribution is the
    /// min-content contribution.
    pref_definite: bool,
    /// Rows: the baseline sharing group (`true` for last baselines) and
    /// the distance from the shared edge to the baseline.
    baseline: Option<(bool, f64)>,
}

impl Part {
    fn add(&mut self, v: f64) {
        self.minc += v;
        self.maxc += v;
        self.fixed_min += v;
        if let Some(a) = &mut self.auto_min {
            *a += v;
        }
    }

    fn range(&self) -> Range<usize> {
        self.start..self.end
    }
}

/// The minimum contribution of a part (§6.6).
fn minimum(p: &Part, tracks: &[Track], gap: f64) -> f64 {
    if p.pref_definite {
        return p.minc;
    }
    let spans_auto = p.range().any(|i| tracks[i].mn == MinK::Auto);
    match p.auto_min {
        Some(a) if spans_auto => {
            if p.range().all(|i| tracks[i].mx == MaxK::Fixed) {
                let sum = p.range().map(|i| tracks[i].fmax).sum::<f64>()
                    + gap * (p.end - p.start - 1) as f64;
                a.min(sum.max(p.fixed_min))
            } else {
                a
            }
        }
        _ => p.fixed_min,
    }
}

/// A contribution limited by the fixed maximum sizing functions it
/// spans (or a `fit-content()` argument) and floored by the minimum
/// contribution.
fn limited(p: &Part, c: f64, tracks: &[Track], gap: f64) -> f64 {
    let cap = if p.range().all(|i| tracks[i].mx == MaxK::Fixed) {
        Some(p.range().map(|i| tracks[i].fmax).sum::<f64>() + gap * (p.end - p.start - 1) as f64)
    } else if let (1, MaxK::Fit(a)) = (p.end - p.start, tracks[p.start].mx) {
        Some(a)
    } else {
        None
    };
    cap.map_or(c, |cap| c.min(cap)).max(minimum(p, tracks, gap))
}

#[derive(Clone, Copy, PartialEq)]
enum Target {
    Base,
    Limit,
}

#[derive(Clone, Copy, PartialEq)]
enum Beyond {
    Intrinsic,
    MaxContent,
    All,
}

/// Distributes extra space across spanned tracks (§12.5.1) for every
/// part of `group`; `by_flex` distributes in proportion to flex factors.
#[allow(clippy::too_many_arguments)]
fn distribute(
    tracks: &mut [Track],
    group: &[&Part],
    gap: f64,
    target: Target,
    affected: &dyn Fn(&Track) -> bool,
    contrib: &dyn Fn(&Part, &[Track]) -> f64,
    beyond: Beyond,
    by_flex: bool,
) {
    let n = tracks.len();
    let mut planned = vec![0.0f64; n];
    let mut touched = vec![false; n];
    let size = |t: &Track| match target {
        Target::Base => t.base,
        Target::Limit if t.limit.is_finite() => t.limit,
        Target::Limit => t.base,
    };
    let cap = |t: &Track| match target {
        Target::Base => match t.mx {
            MaxK::Fit(a) => t.limit.min(a).max(t.base),
            _ => t.limit,
        },
        Target::Limit if t.growable || t.limit.is_infinite() => INF,
        Target::Limit => t.limit,
    };
    for p in group {
        let aff: Vec<usize> = p.range().filter(|&i| affected(&tracks[i])).collect();
        if aff.is_empty() {
            continue;
        }
        for &i in &aff {
            touched[i] = true;
        }
        let spanned =
            p.range().map(|i| size(&tracks[i])).sum::<f64>() + gap * (p.end - p.start - 1) as f64;
        let mut space = contrib(p, tracks) - spanned;
        if space <= EPS {
            continue;
        }
        let weight = |k: usize| {
            if by_flex {
                tracks[aff[k]].flex().unwrap_or(0.0)
            } else {
                1.0
            }
        };
        let mut inc = vec![0.0f64; aff.len()];
        // Up to the limits, freezing tracks as they reach them.
        let mut open: Vec<usize> = (0..aff.len()).collect();
        while space > EPS && !open.is_empty() {
            let ws: f64 = open.iter().map(|&k| weight(k)).sum();
            let count = open.len() as f64;
            let part = |k: usize| {
                if ws > 0.0 {
                    weight(k) / ws
                } else {
                    1.0 / count
                }
            };
            let room =
                |k: usize, inc: &[f64]| cap(&tracks[aff[k]]) - size(&tracks[aff[k]]) - inc[k];
            let frozen: Vec<usize> = open
                .iter()
                .copied()
                .filter(|&k| room(k, &inc) < space * part(k) - EPS)
                .collect();
            if frozen.is_empty() {
                for &k in &open {
                    inc[k] += space * part(k);
                }
                space = 0.0;
                break;
            }
            for &k in &frozen {
                let r = room(k, &inc).max(0.0);
                inc[k] += r;
                space -= r;
            }
            open.retain(|k| !frozen.contains(k));
        }
        // Beyond the limits.
        if space > EPS {
            let reached_fit = |k: usize, inc: &[f64]| {
                let t = &tracks[aff[k]];
                matches!(t.mx, MaxK::Fit(a) if size(t) + inc[k] >= a - EPS)
            };
            let mut pick: Vec<usize> = (0..aff.len())
                .filter(|&k| {
                    let t = &tracks[aff[k]];
                    match beyond {
                        Beyond::Intrinsic => t.mx.intrinsic() && !reached_fit(k, &inc),
                        Beyond::MaxContent => {
                            matches!(t.mx, MaxK::MaxContent | MaxK::Auto)
                                || (matches!(t.mx, MaxK::Fit(_)) && !reached_fit(k, &inc))
                        }
                        Beyond::All => true,
                    }
                })
                .collect();
            if pick.is_empty() {
                pick = (0..aff.len()).collect();
            }
            let ws: f64 = pick.iter().map(|&k| weight(k)).sum();
            let count = pick.len() as f64;
            for &k in &pick {
                inc[k] += if ws > 0.0 {
                    space * weight(k) / ws
                } else {
                    space / count
                };
            }
        }
        for (k, &i) in aff.iter().enumerate() {
            planned[i] = planned[i].max(inc[k]);
        }
    }
    for (i, t) in tracks.iter_mut().enumerate() {
        if !touched[i] {
            continue;
        }
        match target {
            Target::Base => t.base += planned[i],
            Target::Limit if t.limit.is_infinite() => {
                t.limit = t.base + planned[i];
                t.growable = true;
            }
            Target::Limit => t.limit += planned[i],
        }
    }
}

fn floor_limits(tracks: &mut [Track]) {
    for t in tracks.iter_mut() {
        if t.limit < t.base {
            t.limit = t.base;
        }
    }
}

/// Resolves the intrinsic track sizes (§12.5).
fn resolve_intrinsic(tracks: &mut [Track], parts: &[Part], gap: f64, avail: Avail) {
    let crosses_flex = |p: &Part, tracks: &[Track]| p.range().any(|i| tracks[i].flex().is_some());
    let min_contrib = |p: &Part, tracks: &[Track]| match avail {
        Avail::MinContent => limited(p, p.minc, tracks, gap),
        Avail::MaxContent => limited(p, p.maxc, tracks, gap),
        _ => minimum(p, tracks, gap),
    };
    // 2. Items that span one track.
    for i in 0..tracks.len() {
        let ps: Vec<&Part> = parts
            .iter()
            .filter(|p| p.start == i && p.end == i + 1 && !crosses_flex(p, tracks))
            .collect();
        if ps.is_empty() {
            continue;
        }
        let max_of = |f: &dyn Fn(&Part) -> f64| ps.iter().map(|p| f(p)).fold(0.0f64, f64::max);
        let t = &tracks[i];
        let base = match t.mn {
            MinK::MinContent => max_of(&|p| p.minc),
            MinK::MaxContent => max_of(&|p| p.maxc),
            MinK::Auto => max_of(&|p| min_contrib(p, tracks)),
            MinK::Fixed => t.base,
        };
        let limit = match t.mx {
            MaxK::MinContent => Some(max_of(&|p| p.minc)),
            MaxK::MaxContent | MaxK::Auto => Some(max_of(&|p| p.maxc)),
            MaxK::Fit(a) => Some(max_of(&|p| p.maxc).min(a)),
            _ => None,
        };
        let t = &mut tracks[i];
        t.base = t.base.max(base);
        if let Some(l) = limit {
            t.limit = if t.limit.is_finite() {
                t.limit.max(l)
            } else {
                l
            };
        }
        if t.limit < t.base {
            t.limit = t.base;
        }
    }
    // 3. Items that span several tracks, none flexible, by span.
    let max_span = parts.iter().map(|p| p.end - p.start).max().unwrap_or(0);
    for span in 2..=max_span {
        let group: Vec<&Part> = parts
            .iter()
            .filter(|p| p.end - p.start == span && !crosses_flex(p, tracks))
            .collect();
        if group.is_empty() {
            continue;
        }
        distribute(
            tracks,
            &group,
            gap,
            Target::Base,
            &|t| t.mn != MinK::Fixed,
            &min_contrib,
            Beyond::Intrinsic,
            false,
        );
        distribute(
            tracks,
            &group,
            gap,
            Target::Base,
            &|t| matches!(t.mn, MinK::MinContent | MinK::MaxContent),
            &|p, _| p.minc,
            Beyond::Intrinsic,
            false,
        );
        distribute(
            tracks,
            &group,
            gap,
            Target::Base,
            &|t| t.mn == MinK::MaxContent || (t.mn == MinK::Auto && avail == Avail::MaxContent),
            &|p, _| p.maxc,
            Beyond::MaxContent,
            false,
        );
        floor_limits(tracks);
        distribute(
            tracks,
            &group,
            gap,
            Target::Limit,
            &|t| t.mx.intrinsic(),
            &|p, _| p.minc,
            Beyond::All,
            false,
        );
        distribute(
            tracks,
            &group,
            gap,
            Target::Limit,
            &|t| matches!(t.mx, MaxK::MaxContent | MaxK::Auto | MaxK::Fit(_)),
            &|p, _| p.maxc,
            Beyond::All,
            false,
        );
        floor_limits(tracks);
    }
    // 4. Items that cross flexible tracks: only flexible tracks grow, in
    // proportion to their flex factors.
    let group: Vec<&Part> = parts.iter().filter(|p| crosses_flex(p, tracks)).collect();
    if !group.is_empty() {
        distribute(
            tracks,
            &group,
            gap,
            Target::Base,
            &|t| t.flex().is_some() && t.mn != MinK::Fixed,
            &min_contrib,
            Beyond::All,
            true,
        );
        distribute(
            tracks,
            &group,
            gap,
            Target::Base,
            &|t| t.flex().is_some() && matches!(t.mn, MinK::MinContent | MinK::MaxContent),
            &|p, _| p.minc,
            Beyond::All,
            true,
        );
        distribute(
            tracks,
            &group,
            gap,
            Target::Base,
            &|t| {
                t.flex().is_some()
                    && (t.mn == MinK::MaxContent
                        || (t.mn == MinK::Auto && avail == Avail::MaxContent))
            },
            &|p, _| p.maxc,
            Beyond::All,
            true,
        );
        floor_limits(tracks);
    }
    // 5. Growth limits still infinite become the base sizes.
    for t in tracks.iter_mut() {
        if t.limit.is_infinite() {
            t.limit = t.base;
        }
    }
}

/// Maximizes the tracks (§12.6); `cap` is the container's maximum size
/// when the space is not definite.
fn maximize(tracks: &mut [Track], gap: f64, avail: Avail, cap: Option<f64>) {
    let free = match (avail, cap) {
        (Avail::Definite(a), _) => a - used(tracks, gap),
        (Avail::MinContent, _) => return,
        (_, Some(c)) => c - used(tracks, gap),
        _ => INF,
    };
    if free <= EPS {
        return;
    }
    if free.is_infinite() {
        for t in tracks.iter_mut() {
            t.base = t.base.max(t.limit);
        }
        return;
    }
    let mut space = free;
    let mut open: Vec<usize> = (0..tracks.len())
        .filter(|&i| tracks[i].limit > tracks[i].base + EPS)
        .collect();
    while space > EPS && !open.is_empty() {
        let share = space / open.len() as f64;
        let frozen: Vec<usize> = open
            .iter()
            .copied()
            .filter(|&i| tracks[i].limit - tracks[i].base < share - EPS)
            .collect();
        if frozen.is_empty() {
            for &i in &open {
                tracks[i].base += share;
            }
            break;
        }
        for &i in &frozen {
            space -= tracks[i].limit - tracks[i].base;
            tracks[i].base = tracks[i].limit;
        }
        open.retain(|i| !frozen.contains(i));
    }
}

/// Finds the size of an fr (§12.7.1) for `range` and `space`.
fn fr_size(tracks: &[Track], range: Range<usize>, space: f64, gap: f64) -> f64 {
    let live = range.clone().filter(|&i| !tracks[i].collapsed).count();
    let gaps = gap * live.saturating_sub(1) as f64;
    let mut inflexible = vec![false; tracks.len()];
    loop {
        let mut leftover = space - gaps;
        let mut sum = 0.0;
        for i in range.clone() {
            match tracks[i].flex() {
                Some(f) if !inflexible[i] => sum += f,
                _ => leftover -= tracks[i].base,
            }
        }
        let hyp = leftover / if sum < 1.0 { 1.0 } else { sum };
        let mut again = false;
        for i in range.clone() {
            if let Some(f) = tracks[i].flex() {
                if !inflexible[i] && hyp * f < tracks[i].base - EPS {
                    inflexible[i] = true;
                    again = true;
                }
            }
        }
        if !again {
            return hyp.max(0.0);
        }
    }
}

/// Expands the flexible tracks (§12.7); `limits` are the container's
/// minimum and maximum sizes.
fn expand_flex(
    tracks: &mut [Track],
    parts: &[Part],
    gap: f64,
    avail: Avail,
    limits: (Option<f64>, Option<f64>),
) {
    if !tracks.iter().any(|t| t.flex().is_some()) {
        return;
    }
    let n = tracks.len();
    let f = match avail {
        Avail::MinContent => return,
        Avail::Definite(a) => {
            if a - used(tracks, gap) <= EPS {
                return;
            }
            fr_size(tracks, 0..n, a, gap)
        }
        _ => {
            let mut f = 0.0f64;
            for t in tracks.iter() {
                if let Some(x) = t.flex() {
                    f = f.max(if x > 1.0 { t.base / x } else { t.base });
                }
            }
            for p in parts {
                if p.range().any(|i| tracks[i].flex().is_some()) {
                    f = f.max(fr_size(tracks, p.range(), p.maxc, gap));
                }
            }
            let total = |f: f64| {
                tracks
                    .iter()
                    .map(|t| t.flex().map_or(t.base, |x| t.base.max(f * x)))
                    .sum::<f64>()
                    + gap
                        * tracks
                            .iter()
                            .filter(|t| !t.collapsed)
                            .count()
                            .saturating_sub(1) as f64
            };
            if let Some(mn) = limits.0 {
                if total(f) < mn - EPS {
                    f = fr_size(tracks, 0..n, mn, gap);
                }
            }
            if let Some(mx) = limits.1 {
                if total(f) > mx + EPS {
                    f = fr_size(tracks, 0..n, mx, gap);
                }
            }
            f
        }
    };
    for t in tracks.iter_mut() {
        if let Some(x) = t.flex() {
            if f * x > t.base {
                t.base = f * x;
                t.limit = t.limit.max(t.base);
            }
        }
    }
}

/// Stretches the `auto` tracks over the free space (§12.8).
fn stretch_auto(tracks: &mut [Track], gap: f64, avail: Avail, min: Option<f64>) {
    let free = match avail {
        Avail::Definite(a) => a - used(tracks, gap),
        Avail::Indefinite => match min {
            Some(m) => m - used(tracks, gap),
            None => return,
        },
        _ => return,
    };
    let autos: Vec<usize> = (0..tracks.len())
        .filter(|&i| tracks[i].mx == MaxK::Auto && !tracks[i].collapsed)
        .collect();
    if free <= EPS || autos.is_empty() {
        return;
    }
    let share = free / autos.len() as f64;
    for i in autos {
        tracks[i].base += share;
    }
}

// ----- alignment ---------------------------------------------------------------------------

/// `justify-self` (`auto` is the container's `justify-items`, where
/// `legacy` alone is `normal`).
fn justify_self(s: &ComputedStyle, container: &ComputedStyle) -> (Pos, bool) {
    let own = kw(s, "justify-self");
    if own.is_empty() || own == "auto" {
        let items = kw(container, "justify-items").replace("legacy", "");
        parse_align(items.trim())
    } else {
        parse_align(&own)
    }
}

fn align_self(s: &ComputedStyle, container: &ComputedStyle) -> (Pos, bool) {
    let own = kw(s, "align-self");
    if own.is_empty() || own == "auto" {
        parse_align(&kw(container, "align-items"))
    } else {
        parse_align(&own)
    }
}

/// `normal` stretches boxes that are not replaced and have no
/// preferred aspect ratio (css-align-3 §6.1).
fn stretches(p: Pos, b: &BoxNode) -> bool {
    match p {
        Pos::Stretch => true,
        Pos::Normal => {
            matches!(b.kind, BoxKind::Container(_)) && values::aspect_ratio(&b.style).1.is_none()
        }
        _ => false,
    }
}

/// The offset from the start edge of a positional alignment with `free`
/// space; `flip` = the start edge is the physical right one (for `left`
/// and `right`), `item_flip` = the item's direction is the other one
/// (for `self-start` and `self-end`).
fn offset_of(p: Pos, safe: bool, free: f64, flip: bool, item_flip: bool) -> f64 {
    let o = match p {
        Pos::Center => free / 2.0,
        Pos::End | Pos::FlexEnd | Pos::LastBaseline => free,
        Pos::SelfStart => {
            if item_flip {
                free
            } else {
                0.0
            }
        }
        Pos::SelfEnd => {
            if item_flip {
                0.0
            } else {
                free
            }
        }
        Pos::Left => {
            if flip {
                free
            } else {
                0.0
            }
        }
        Pos::Right => {
            if flip {
                0.0
            } else {
                free
            }
        }
        _ => 0.0,
    };
    if safe && free < 0.0 { 0.0 } else { o }
}

// ----- the grid ----------------------------------------------------------------------------

/// What a parent grid tells one subgridded axis while building it: the
/// tracks the subgrid spans, the parent's names of those lines and gap.
#[derive(Clone, Debug)]
struct SubAxis {
    tracks: usize,
    names: Vec<Vec<String>>,
    gap: f64,
}

/// One subgridded axis as the parent grid passes it before laying the
/// subgrid out: the parent's line names, the tracks (start and size,
/// relative to the subgrid's content box) and the parent's gap.
#[derive(Clone, Debug)]
pub(crate) struct InheritAxis {
    names: Vec<Vec<String>>,
    pos: Vec<(f64, f64)>,
    gap: f64,
}

/// The tracks a grid gives a subgrid (`Ctx::subgrids`).
#[derive(Clone, Debug, Default)]
pub(crate) struct Inherit {
    axes: [Option<InheritAxis>; 2],
}

struct Axis {
    explicit: Explicit,
    /// The explicit tracks (template and areas).
    count: usize,
    lead: usize,
    tracks: Vec<Track>,
    gap: f64,
    gap_normal: bool,
    /// The start and size of every track once placed.
    pos: Vec<(f64, f64)>,
    /// The axis is subgridded: its tracks come from the parent.
    sub: bool,
    /// A subgrid's own line names (its resolved value).
    own_names: Vec<Vec<String>>,
}

impl Axis {
    /// The explicit names of line `l` (counted from the first track).
    fn names(&self, l: usize) -> &[String] {
        match l.checked_sub(self.lead) {
            Some(e) => self.explicit.names.get(e).map_or(&[], |v| v.as_slice()),
            None => &[],
        }
    }

    /// The extent of tracks `s..e`: its lowest and highest coordinates.
    fn span(&self, (s, e): (usize, usize)) -> (f64, f64) {
        let mut lo = INF;
        let mut hi = -INF;
        for &(p, z) in self.pos.get(s..e.min(self.pos.len())).unwrap_or(&[]) {
            lo = lo.min(p);
            hi = hi.max(p + z);
        }
        if lo > hi { (0.0, 0.0) } else { (lo, hi) }
    }

    fn span_size(&self, r: (usize, usize)) -> f64 {
        let (lo, hi) = self.span(r);
        hi - lo
    }

    fn total(&self) -> f64 {
        let live = self.tracks.iter().filter(|t| !t.collapsed).count();
        self.tracks.iter().map(|t| t.base).sum::<f64>() + self.gap * live.saturating_sub(1) as f64
    }

    /// Takes the tracks of a parent grid, narrowed by half the difference
    /// of the gaps at the inner edges when the subgrid has its own gap.
    fn adopt(&mut self, inh: &InheritAxis) {
        let d = if self.gap_normal {
            0.0
        } else {
            (self.gap - inh.gap) / 2.0
        };
        if self.gap_normal {
            self.gap = inh.gap;
        }
        let lo = inh.pos.iter().map(|p| p.0).fold(INF, f64::min);
        let hi = inh.pos.iter().map(|p| p.0 + p.1).fold(-INF, f64::max);
        self.pos = inh
            .pos
            .iter()
            .map(|&(p, z)| {
                let left = if p > lo + EPS { d } else { 0.0 };
                let right = if p + z < hi - EPS { d } else { 0.0 };
                (p + left, (z - left - right).max(0.0))
            })
            .collect();
        for (t, p) in self.tracks.iter_mut().zip(&self.pos) {
            t.base = p.1;
        }
    }

    /// The resolved value (§7.2.6): every track in pixels with the
    /// explicit line names, or `subgrid` with the subgrid's names.
    fn resolved(&self) -> String {
        if self.sub {
            let mut s = "subgrid".to_string();
            for n in &self.own_names {
                s.push_str(&format!(" [{}]", n.join(" ")));
            }
            return s;
        }
        if self.tracks.is_empty() {
            return "none".to_string();
        }
        let mut out: Vec<String> = Vec::new();
        for (i, t) in self.tracks.iter().enumerate() {
            let n = self.names(i);
            if !n.is_empty() {
                out.push(format!("[{}]", n.join(" ")));
            }
            let size = if t.collapsed { 0.0 } else { t.base };
            out.push(format!("{}px", number(size)));
        }
        let n = self.names(self.tracks.len());
        if !n.is_empty() {
            out.push(format!("[{}]", n.join(" ")));
        }
        out.join(" ")
    }
}

/// A grid item: its box, its tracks per axis and, for a subgrid, its
/// own grid.
struct GItem<'a> {
    b: &'a BoxNode,
    area: [(usize, usize); 2],
    sub: Option<Box<Grid<'a>>>,
}

impl GItem<'_> {
    fn subgridded(&self, ax: usize) -> bool {
        self.sub.as_ref().is_some_and(|g| g.axes[ax].sub)
    }
}

struct Grid<'a> {
    style: &'a ComputedStyle,
    items: Vec<GItem<'a>>,
    abs: Vec<&'a BoxNode>,
    axes: [Axis; 2],
    lines: [Lines; 2],
    rtl: bool,
}

/// The margin, border and padding of `b` at the start and end of an
/// axis (percentages of an unknown basis are 0).
fn edges(b: &BoxNode, ax: usize, rtl: bool) -> (f64, f64) {
    let m = Model::of(&b.style, 0.0);
    let mm = m.margin.or_zero();
    let o = m.bp().add(&mm);
    if ax == COL {
        if rtl {
            (o.right, o.left)
        } else {
            (o.left, o.right)
        }
    } else {
        (o.top, o.bottom)
    }
}

/// A size limit of the content box from `min-*` or `max-*`.
fn content_limit(s: &ComputedStyle, name: &str, basis: Option<f64>, bp: f64) -> Option<f64> {
    match values::size(s, name) {
        Size::Lp(l) => l.resolve_opt(basis).map(|v| {
            if values::border_box_sizing(s) {
                (v - bp).max(0.0)
            } else {
                v
            }
        }),
        _ => None,
    }
}

/// Lays out an item with content width `w` and content height `h`
/// (`None`: automatic); `children_cbh` is the height percentages inside
/// it resolve against. Returns the fragment (border box at 0, 0), its
/// used content height and its content height.
fn lay(
    ctx: &mut Ctx,
    b: &BoxNode,
    m: &Model,
    w: f64,
    h: Option<f64>,
    children_cbh: Option<f64>,
    cb: Cb,
) -> (Fragment, f64, f64) {
    let s = &b.style;
    let bp = m.bp();
    let (mut frag, h, content) = match &b.kind {
        BoxKind::Replaced(r) => {
            let (rw, rh) = block::replaced_size(ctx, b, r, Some(cb.width), cb.height);
            let (auto, given) = values::aspect_ratio(s);
            let ratio = if auto {
                r.natural_ratio().or(given)
            } else {
                given
            };
            let natural = match ratio {
                Some(q) if q > 0.0 && (rw - w).abs() > EPS && h.is_none() => w / q,
                _ => rh,
            };
            (
                Fragment::new(FragmentKind::Replaced(r.kind), s.clone()),
                h.unwrap_or(natural),
                natural,
            )
        }
        BoxKind::Container(c) => {
            let inner = block::layout_independent(ctx, b, c, w, children_cbh);
            let content = match block::ratio_height(s, w, &bp) {
                Some(rh) => block::ratio_floor(s, rh, true, inner.height),
                None => inner.height,
            };
            let h = h.unwrap_or_else(|| {
                block::clamp_height(s, content, cb.height, bp.vertical(), inner.height)
            });
            let mut f = Fragment::new(
                FragmentKind::Block {
                    anonymous: c.anonymous,
                    bfc_root: true,
                },
                s.clone(),
            );
            f.children = inner.children;
            for ch in &mut f.children {
                ch.translate(bp.left, bp.top);
            }
            (f, h, content)
        }
    };
    frag.node = b.node;
    frag.pseudo = b.pseudo;
    frag.border = m.border;
    frag.padding = m.padding;
    frag.rect.width = w + bp.horizontal();
    frag.rect.height = h + bp.vertical();
    match &b.kind {
        BoxKind::Replaced(r) => block::set_object(&mut frag, b, r),
        BoxKind::Container(c) => {
            if let Some(marker) = &c.marker {
                block::place_marker(ctx, marker, &mut frag, cb);
            }
        }
    }
    (frag, h, content)
}

impl<'a> Grid<'a> {
    /// Builds the grid of container `style` with `children`: its explicit
    /// tracks, the placement of its items and its implicit tracks.
    /// `size` is the definite content size per axis, `limits` the minimum
    /// and maximum sizes that count `auto-fill` repetitions, and `sub`
    /// the subgridded axes.
    fn build(
        style: &'a ComputedStyle,
        children: &'a [BoxNode],
        size: [Option<f64>; 2],
        limits: [(Option<f64>, Option<f64>); 2],
        sub: [Option<SubAxis>; 2],
    ) -> Grid<'a> {
        let areas = areas(style);
        let names = [
            ("grid-template-columns", "column-gap", "grid-auto-columns"),
            ("grid-template-rows", "row-gap", "grid-auto-rows"),
        ];
        let mut explicit: Vec<Explicit> = Vec::new();
        let mut info: Vec<(usize, f64, bool, Vec<Vec<String>>)> = Vec::new();
        for ax in [COL, ROW] {
            let (tname, gname, _) = names[ax];
            let gap_normal = kw(style, gname) == "normal";
            let own_gap = values::lp(style, gname)
                .and_then(|l| l.resolve_opt(size[ax]))
                .unwrap_or(0.0)
                .max(0.0);
            let mut own_names = Vec::new();
            let (e, gap) = match (&sub[ax], template(style, tname)) {
                (Some(sa), t) => {
                    let own = match t {
                        Template::Subgrid(list) => subgrid_names(&list, sa.tracks + 1),
                        _ => vec![Vec::new(); sa.tracks + 1],
                    };
                    let mut names = own.clone();
                    for (l, n) in names.iter_mut().enumerate() {
                        if let Some(p) = sa.names.get(l) {
                            n.extend(p.iter().cloned());
                        }
                    }
                    own_names = own;
                    let e = Explicit {
                        tracks: vec![TrackSize::auto(); sa.tracks],
                        names,
                        auto_fit: None,
                    };
                    (e, if gap_normal { sa.gap } else { own_gap })
                }
                (None, Template::Tracks(list)) => {
                    let space = size[ax].or(limits[ax].1);
                    let reps = auto_count(&list, space, limits[ax].0, own_gap);
                    (expand(&list, reps), own_gap)
                }
                (None, _) => (Explicit::empty(), own_gap),
            };
            let area_count = areas
                .as_ref()
                .map_or(0, |a| if ax == COL { a.cols } else { a.rows });
            let count = if sub[ax].is_some() {
                e.tracks.len()
            } else {
                e.tracks.len().max(area_count)
            };
            explicit.push(e);
            info.push((count, gap, gap_normal, own_names));
        }
        let lines: [Lines; 2] = [COL, ROW].map(|ax| Lines {
            count: info[ax].0,
            names: explicit[ax].names.clone(),
            areas: areas.as_ref().map_or(Vec::new(), |a| {
                a.named
                    .iter()
                    .map(|(n, r)| (n.clone(), r[ax].0, r[ax].1))
                    .collect()
            }),
        });

        let mut ordered: Vec<&'a BoxNode> = children
            .iter()
            .filter(|ch| !values::out_of_flow(&ch.style))
            .collect();
        ordered.sort_by_key(|ch| order(&ch.style));
        let abs: Vec<&'a BoxNode> = children
            .iter()
            .filter(|ch| values::out_of_flow(&ch.style))
            .collect();
        let props = [
            ("grid-column-start", "grid-column-end"),
            ("grid-row-start", "grid-row-end"),
        ];
        let places: Vec<[Place; 2]> = ordered
            .iter()
            .map(|ch| {
                let subs = subgridded(ch);
                [COL, ROW].map(|ax| {
                    let (s, e) = (raw(&ch.style, props[ax].0), raw(&ch.style, props[ax].1));
                    let spans = matches!(s, Raw::Span(..)) || matches!(e, Raw::Span(..));
                    match (resolve(&lines[ax], &s, &e), &subs[ax]) {
                        (Place::Auto(_), Some(list)) if !spans => Place::Auto(subgrid_span(list)),
                        (p, _) => p,
                    }
                })
            })
            .collect();
        let flow = kw(style, "grid-auto-flow");
        let placed = auto_place(
            &places,
            [info[COL].0, info[ROW].0],
            [sub[COL].is_some(), sub[ROW].is_some()],
            flow.contains("column"),
            flow.contains("dense"),
        );

        let mut axes: Vec<Axis> = Vec::new();
        for (ax, e) in explicit.into_iter().enumerate() {
            let (count, gap, gap_normal, own_names) = std::mem::take(&mut info[ax]);
            let lead = placed.lead[ax];
            let auto_sizes: Vec<TrackSize> =
                match style.get(names[ax].2).and_then(|v| v.values.as_deref()) {
                    Some(v) => v.iter().filter_map(track_size).collect(),
                    None => Vec::new(),
                };
            let auto_sizes = if auto_sizes.is_empty() {
                vec![TrackSize::auto()]
            } else {
                auto_sizes
            };
            let len = auto_sizes.len();
            let mut tracks = Vec::with_capacity(placed.count[ax]);
            for i in 0..placed.count[ax] {
                let size = if i < lead {
                    let j = lead - i;
                    auto_sizes[(len - 1) - ((j - 1) % len)].clone()
                } else if i - lead < e.tracks.len() {
                    e.tracks[i - lead].clone()
                } else {
                    auto_sizes[(i - lead - e.tracks.len()) % len].clone()
                };
                tracks.push(Track::new(size));
            }
            if let Some((s, end)) = e.auto_fit {
                for t in s..end {
                    let i = t + lead;
                    let empty = !placed.areas.iter().any(|a| a[ax].0 <= i && i < a[ax].1);
                    if empty && i < tracks.len() {
                        tracks[i].collapsed = true;
                    }
                }
            }
            axes.push(Axis {
                explicit: e,
                count,
                lead,
                tracks,
                gap,
                gap_normal,
                pos: Vec::new(),
                sub: sub[ax].is_some(),
                own_names,
            });
        }
        let rows = axes.pop().expect("two axes");
        let cols = axes.pop().expect("two axes");
        let axes = [cols, rows];

        let items = ordered
            .into_iter()
            .zip(placed.areas)
            .map(|(ch, area)| {
                let subs = subgridded(ch);
                let sub = match (&ch.kind, subs[COL].is_some() || subs[ROW].is_some()) {
                    (BoxKind::Container(c), true) => {
                        let sa = [COL, ROW].map(|ax| {
                            subs[ax].as_ref().map(|_| SubAxis {
                                tracks: area[ax].1 - area[ax].0,
                                names: (area[ax].0..=area[ax].1)
                                    .map(|l| axes[ax].names(l).to_vec())
                                    .collect(),
                                gap: axes[ax].gap,
                            })
                        });
                        Some(Box::new(Grid::build(
                            &ch.style,
                            blocks(c),
                            [None, None],
                            [(None, None); 2],
                            sa,
                        )))
                    }
                    _ => None,
                };
                GItem { b: ch, area, sub }
            })
            .collect();
        Grid {
            style,
            items,
            abs,
            axes,
            lines,
            rtl: style.rtl(),
        }
    }

    /// The tracks of `it` in the axes it subgrids, for its layout.
    fn inherit(&self, it: &GItem, axes: [bool; 2]) -> Inherit {
        let m = Model::of(&it.b.style, self.axes[COL].span_size(it.area[COL]));
        let mut out = Inherit::default();
        for ax in [COL, ROW] {
            if !axes[ax] {
                continue;
            }
            let (s, e) = it.area[ax];
            let axis = &self.axes[ax];
            let (lo, _) = axis.span(it.area[ax]);
            let origin = if ax == COL {
                lo + m.margin.left.unwrap_or(0.0) + m.border.left + m.padding.left
            } else {
                lo + m.margin.top.unwrap_or(0.0) + m.border.top + m.padding.top
            };
            let pos = axis.pos[s..e.min(axis.pos.len())]
                .iter()
                .map(|&(p, z)| (p - origin, z))
                .collect();
            let names = (s..=e).map(|l| axis.names(l).to_vec()).collect();
            out.axes[ax] = Some(InheritAxis {
                names,
                pos,
                gap: axis.gap,
            });
        }
        out
    }

    /// The content width of item `b` in an area `aw` wide (`forced`:
    /// a subgrid stretches over the columns it subgrids).
    fn item_width(&self, ctx: &mut Ctx, b: &BoxNode, aw: f64, forced: bool) -> f64 {
        let s = &b.style;
        let m = Model::of(s, aw);
        let bp_h = m.bp().horizontal();
        let Margins { left, right, .. } = m.margin;
        let avail = (aw - left.unwrap_or(0.0) - right.unwrap_or(0.0) - bp_h).max(0.0);
        let (p, _) = justify_self(s, self.style);
        let auto_margins = left.is_none() || right.is_none();
        match &b.kind {
            BoxKind::Replaced(r) => {
                if p == Pos::Stretch && !auto_margins && values::size(s, "width").is_auto() {
                    block::clamp_width(ctx, b, avail, Some(aw), bp_h)
                } else {
                    block::replaced_size(ctx, b, r, Some(aw), None).0
                }
            }
            BoxKind::Container(_) => {
                let w = match block::specified_width(ctx, b, Some(aw), bp_h, avail) {
                    Some(w) if !forced => w,
                    _ if forced || (stretches(p, b) && !auto_margins) => avail,
                    _ => block::shrink_to_fit(ctx, b, avail),
                };
                block::clamp_width(ctx, b, w, Some(aw), bp_h)
            }
        }
    }

    /// The column part of an item.
    fn width_part(&self, ctx: &mut Ctx, b: &BoxNode) -> Part {
        let s = &b.style;
        let m = Model::of(s, 0.0);
        let bp_h = m.bp().horizontal();
        let outer = bp_h + m.margin.or_zero().horizontal();
        let (minc, maxc) = block::contribution(ctx, b, None);
        let pref = values::size(s, "width");
        let pref_definite = match &pref {
            Size::Lp(l) => !l.has_percent(),
            Size::Auto | Size::Stretch | Size::None => false,
            _ => true,
        };
        let (ox, _) = values::overflow(s);
        let auto_min = (values::size(s, "min-width").is_auto() && !ox.scrolls()).then(|| {
            let content = match &b.kind {
                BoxKind::Replaced(r) => block::replaced_size(ctx, b, r, None, None).0,
                BoxKind::Container(_) => block::intrinsic(ctx, b).0,
            };
            let mut c = block::clamp_width(ctx, b, content, None, bp_h);
            if let Some(w) = block::specified_width(ctx, b, None, bp_h, 0.0) {
                c = c.min(w);
            }
            c + outer
        });
        let fixed_min = block::clamp_width(ctx, b, 0.0, None, bp_h) + outer;
        Part {
            start: 0,
            end: 0,
            minc,
            maxc,
            auto_min,
            fixed_min,
            pref_definite,
            baseline: None,
        }
    }

    /// The row part of an item, laid out at the width of its columns.
    fn height_part(&self, ctx: &mut Ctx, it: &GItem) -> Part {
        let b = it.b;
        let s = &b.style;
        let aw = self.axes[COL].span_size(it.area[COL]);
        let m = Model::of(s, aw);
        let bp = m.bp();
        let w = self.item_width(ctx, b, aw, it.subgridded(COL));
        let cb = Cb {
            width: aw,
            height: None,
            rtl: self.rtl,
        };
        let spec = block::specified_height(s, cb, bp.vertical());
        if it.sub.is_some() {
            ctx.subgrids
                .insert(key(b), self.inherit(it, [it.subgridded(COL), false]));
        }
        let (frag, h, content) = lay(ctx, b, &m, w, spec, spec, cb);
        ctx.subgrids.remove(&key(b));
        let h = block::clamp_height(s, h, None, bp.vertical(), content);
        let (mt, mb) = (m.margin.top, m.margin.bottom);
        let outer = bp.vertical() + mt.unwrap_or(0.0) + mb.unwrap_or(0.0);
        let (_, oy) = values::overflow(s);
        let auto_min = (values::size(s, "min-height").is_auto() && !oy.scrolls()).then(|| {
            let c = block::clamp_height(s, content, None, bp.vertical(), content);
            spec.map_or(c, |sp| c.min(sp)) + outer
        });
        let fixed_min = block::clamp_height(s, 0.0, None, bp.vertical(), content) + outer;
        let (p, _) = align_self(s, self.style);
        let baseline = match p {
            Pos::Baseline | Pos::LastBaseline if mt.is_some() && mb.is_some() => {
                let bh = frag.rect.height;
                if p == Pos::Baseline {
                    let first = block::find_baseline(&frag.children, false).unwrap_or(bh);
                    Some((false, mt.unwrap_or(0.0) + first))
                } else {
                    let last = block::find_baseline(&frag.children, true).unwrap_or(bh);
                    Some((true, bh - last + mb.unwrap_or(0.0)))
                }
            }
            _ => None,
        };
        Part {
            start: 0,
            end: 0,
            minc: h + outer,
            maxc: h + outer,
            auto_min,
            fixed_min,
            pref_definite: spec.is_some(),
            baseline,
        }
    }

    /// The parts of every item in axis `ax` of this grid, with the items
    /// of subgrids in that axis in their place (`offset` = the first
    /// track of this grid in the outermost one; `extra` = the margins
    /// added at this grid's edges; `inner` = the margin added at its inner
    /// lines for a different gap).
    fn gather(
        &self,
        ctx: &mut Ctx,
        ax: usize,
        offset: usize,
        extra: (f64, f64),
        inner: f64,
        out: &mut Vec<Part>,
    ) {
        let n = self.axes[ax].tracks.len();
        for it in &self.items {
            let (s, e) = it.area[ax];
            let es = if s == 0 { extra.0 } else { inner };
            let ee = if e == n { extra.1 } else { inner };
            if let Some(sub) = &it.sub {
                if sub.axes[ax].sub {
                    let (a, z) = edges(it.b, ax, self.rtl);
                    let d = if sub.axes[ax].gap_normal {
                        0.0
                    } else {
                        (sub.axes[ax].gap - self.axes[ax].gap) / 2.0
                    };
                    sub.gather(ctx, ax, offset + s, (es + a, ee + z), d, out);
                    continue;
                }
            }
            let mut p = if ax == COL {
                self.width_part(ctx, it.b)
            } else {
                self.height_part(ctx, it)
            };
            p.start = offset + s;
            p.end = offset + e;
            p.add(es + ee);
            out.push(p);
        }
    }

    /// Runs the track sizing algorithm in axis `ax` (§12.3) with the
    /// container's minimum and maximum sizes `limits`.
    fn size(&mut self, ctx: &mut Ctx, ax: usize, avail: Avail, limits: (Option<f64>, Option<f64>)) {
        let basis = match avail {
            Avail::Definite(a) => Some(a),
            _ => None,
        };
        for t in &mut self.axes[ax].tracks {
            t.init(basis);
        }
        let mut parts = Vec::new();
        self.gather(ctx, ax, 0, (0.0, 0.0), 0.0, &mut parts);
        if ax == ROW {
            shim_baselines(&mut parts);
        }
        let content = kw(
            self.style,
            if ax == COL {
                "justify-content"
            } else {
                "align-content"
            },
        );
        let (distribution, _) = parse_align(&content);
        let axis = &mut self.axes[ax];
        let gap = axis.gap;
        let tracks = &mut axis.tracks;
        resolve_intrinsic(tracks, &parts, gap, avail);
        maximize(tracks, gap, avail, limits.1);
        expand_flex(tracks, &parts, gap, avail, limits);
        if matches!(distribution, Pos::Normal | Pos::Stretch) {
            stretch_auto(tracks, gap, avail, limits.0);
        }
    }

    /// Places the sized tracks of axis `ax` in a content box `size` long
    /// with `justify-content` or `align-content` (§11.1).
    fn place_tracks(&mut self, ax: usize, size: f64) {
        let flip = ax == COL && self.rtl;
        let prop = if ax == COL {
            "justify-content"
        } else {
            "align-content"
        };
        let (p, safe) = parse_align(&kw(self.style, prop));
        let axis = &mut self.axes[ax];
        if axis.sub {
            return;
        }
        let live = axis.tracks.iter().filter(|t| !t.collapsed).count();
        let free = size - axis.total();
        let k = live as f64;
        let (start, between) = match p {
            Pos::SpaceBetween if live > 1 && free > 0.0 => (0.0, free / (k - 1.0)),
            Pos::SpaceAround if live > 0 && free > 0.0 => (free / k / 2.0, free / k),
            Pos::SpaceEvenly if live > 0 && free > 0.0 => (free / (k + 1.0), free / (k + 1.0)),
            Pos::SpaceBetween
            | Pos::SpaceAround
            | Pos::SpaceEvenly
            | Pos::Normal
            | Pos::Stretch
            | Pos::Baseline
            | Pos::LastBaseline => (0.0, 0.0),
            p => (offset_of(p, safe, free, flip, false), 0.0),
        };
        let mut cursor = start;
        let mut seen = false;
        axis.pos = axis
            .tracks
            .iter()
            .map(|t| {
                if t.collapsed {
                    (cursor, 0.0)
                } else {
                    if seen {
                        cursor += axis.gap + between;
                    }
                    seen = true;
                    let p = (cursor, t.base);
                    cursor += t.base;
                    p
                }
            })
            .collect();
        if flip {
            for p in &mut axis.pos {
                p.0 = size - p.0 - p.1;
            }
        }
    }

    /// Sizes and places the columns of the subgrids inside this grid
    /// (their own columns, or the ones they take from here), so that the
    /// items of the subgrids that take part in rows know their widths.
    fn nested_columns(&mut self, ctx: &mut Ctx) {
        for i in 0..self.items.len() {
            let Some(mut sub) = self.items[i].sub.take() else {
                continue;
            };
            let it = &self.items[i];
            if sub.axes[COL].sub {
                let inh = self.inherit(it, [true, false]);
                if let Some(a) = &inh.axes[COL] {
                    sub.axes[COL].adopt(a);
                }
            } else {
                let aw = self.axes[COL].span_size(it.area[COL]);
                let w = self.item_width(ctx, it.b, aw, false);
                sub.size(ctx, COL, Avail::Definite(w), (None, None));
                sub.place_tracks(COL, w);
            }
            sub.nested_columns(ctx);
            self.items[i].sub = Some(sub);
        }
    }

    /// The line `l` of axis `ax` at the start (or end) edge of an area.
    fn line_coord(&self, ax: usize, l: usize, start: bool) -> f64 {
        let axis = &self.axes[ax];
        let flip = ax == COL && self.rtl;
        let n = axis.pos.len();
        let edge = |i: usize, front: bool| {
            let (p, z) = axis.pos[i];
            if front != flip { p } else { p + z }
        };
        if n == 0 {
            0.0
        } else if start && l < n {
            edge(l, true)
        } else if l > 0 {
            edge((l - 1).min(n - 1), false)
        } else {
            edge(0, true)
        }
    }

    /// The grid area of an absolutely positioned child (§9.1), relative to
    /// the content box `size`; `auto` lines are the padding edges.
    fn abs_area(&self, ch: &BoxNode, size: (f64, f64), pad: &Sides) -> Rect {
        let props = [
            ("grid-column-start", "grid-column-end"),
            ("grid-row-start", "grid-row-end"),
        ];
        let mut out = [(0.0, 0.0); 2];
        for ax in [COL, ROW] {
            let axis = &self.axes[ax];
            let (s, e) = resolve_abs(
                &self.lines[ax],
                &raw(&ch.style, props[ax].0),
                &raw(&ch.style, props[ax].1),
            );
            let n = axis.tracks.len() as i32;
            let internal = |l: Option<i32>| {
                l.map(|l| l + axis.lead as i32)
                    .filter(|l| (0..=n).contains(l))
                    .map(|l| l as usize)
            };
            let (pad_lo, pad_hi, len) = if ax == COL {
                (pad.left, pad.right, size.0)
            } else {
                (pad.top, pad.bottom, size.1)
            };
            let flip = ax == COL && self.rtl;
            let (auto_start, auto_end) = if flip {
                (len + pad_hi, -pad_lo)
            } else {
                (-pad_lo, len + pad_hi)
            };
            let a = internal(s).map_or(auto_start, |l| self.line_coord(ax, l, true));
            let b = internal(e).map_or(auto_end, |l| self.line_coord(ax, l, false));
            out[ax] = (a.min(b), a.max(b));
        }
        Rect::new(
            out[COL].0,
            out[ROW].0,
            out[COL].1 - out[COL].0,
            out[ROW].1 - out[ROW].0,
        )
    }
}

/// Baseline sharing groups in rows (§12.5 step 1): items aligned by
/// their first baseline that start in the same row (or by their last
/// one that end in the same row) grow by the shim that aligns them.
fn shim_baselines(parts: &mut [Part]) {
    let mut first: HashMap<usize, f64> = HashMap::new();
    let mut last: HashMap<usize, f64> = HashMap::new();
    for p in parts.iter() {
        match p.baseline {
            Some((false, v)) => {
                let e = first.entry(p.start).or_insert(0.0);
                *e = e.max(v);
            }
            Some((true, v)) => {
                let e = last.entry(p.end).or_insert(0.0);
                *e = e.max(v);
            }
            None => {}
        }
    }
    for p in parts.iter_mut() {
        let shim = match p.baseline {
            Some((false, v)) => first.get(&p.start).map_or(0.0, |m| m - v),
            Some((true, v)) => last.get(&p.end).map_or(0.0, |m| m - v),
            None => 0.0,
        };
        if shim > 0.0 {
            p.add(shim);
        }
    }
}

// ----- the algorithm -----------------------------------------------------------------------

/// An item laid out, before alignment.
struct Laid {
    frag: Fragment,
    area: Rect,
    margin: Margins,
    rows: (usize, usize),
    baseline: Option<(bool, f64)>,
}

/// Lays out the content of grid container `b` with content width `w`
/// and, when definite, content height `cbh`.
pub(super) fn layout(
    ctx: &mut Ctx,
    b: &BoxNode,
    c: &Container,
    w: f64,
    cbh: Option<f64>,
) -> Independent {
    let inherit = ctx.subgrids.remove(&key(b)).unwrap_or_default();
    let s = &b.style;
    let model = Model::of(s, w);
    let bp = model.bp();
    let min_h = content_limit(s, "min-height", None, bp.vertical());
    let max_h = content_limit(s, "max-height", None, bp.vertical());
    let sub = [COL, ROW].map(|ax| {
        inherit.axes[ax].as_ref().map(|a| SubAxis {
            tracks: a.pos.len(),
            names: a.names.clone(),
            gap: a.gap,
        })
    });
    let mut g = Grid::build(
        s,
        blocks(c),
        [Some(w), cbh],
        [(None, None), (min_h, max_h)],
        sub,
    );

    // Columns.
    if let Some(a) = &inherit.axes[COL] {
        g.axes[COL].adopt(a);
    } else {
        g.size(ctx, COL, Avail::Definite(w), (None, None));
        g.place_tracks(COL, w);
    }
    g.nested_columns(ctx);

    // Rows.
    let height = if let Some(a) = &inherit.axes[ROW] {
        g.axes[ROW].adopt(a);
        cbh.unwrap_or_else(|| g.axes[ROW].span_size((0, g.axes[ROW].pos.len())))
    } else {
        let avail = cbh.map_or(Avail::Indefinite, Avail::Definite);
        g.size(ctx, ROW, avail, (min_h, max_h));
        let total = g.axes[ROW].total();
        let content_h =
            cbh.unwrap_or_else(|| block::clamp_height(s, total, None, bp.vertical(), total));
        g.place_tracks(ROW, content_h);
        cbh.unwrap_or(total)
    };
    if let (Some(n), None) = (b.node, b.pseudo) {
        ctx.grid_tracks
            .insert(n, [g.axes[COL].resolved(), g.axes[ROW].resolved()]);
    }

    // The items, laid out in their areas.
    let mut laid: Vec<Laid> = Vec::with_capacity(g.items.len());
    for it in &g.items {
        let ib = it.b;
        let is = &ib.style;
        let (x0, x1) = g.axes[COL].span(it.area[COL]);
        let (y0, y1) = g.axes[ROW].span(it.area[ROW]);
        let area = Rect::new(x0, y0, x1 - x0, y1 - y0);
        let m = Model::of(is, area.width);
        let ibp = m.bp();
        let iw = g.item_width(ctx, ib, area.width, it.subgridded(COL));
        let cb = Cb {
            width: area.width,
            height: Some(area.height),
            rtl: g.rtl,
        };
        let (ap, _) = align_self(is, s);
        let spec_h = block::specified_height(is, cb, ibp.vertical());
        let auto_margins = m.margin.top.is_none() || m.margin.bottom.is_none();
        let stretch = it.subgridded(ROW) || (stretches(ap, ib) && !auto_margins);
        let h = match spec_h {
            Some(h) if !it.subgridded(ROW) => Some(h),
            _ if stretch => {
                Some((area.height - m.margin.or_zero().vertical() - ibp.vertical()).max(0.0))
            }
            _ => None,
        }
        .map(|h| block::clamp_height(is, h, Some(area.height), ibp.vertical(), h));
        if it.sub.is_some() {
            ctx.subgrids.insert(
                key(ib),
                g.inherit(it, [it.subgridded(COL), it.subgridded(ROW)]),
            );
        }
        let (frag, _, _) = lay(ctx, ib, &m, iw, h, h, cb);
        ctx.subgrids.remove(&key(ib));
        let (mt, mb) = (m.margin.top, m.margin.bottom);
        let baseline = match ap {
            Pos::Baseline | Pos::LastBaseline if !auto_margins && !it.subgridded(ROW) => {
                let bh = frag.rect.height;
                if ap == Pos::Baseline {
                    let first = block::find_baseline(&frag.children, false).unwrap_or(bh);
                    Some((false, mt.unwrap_or(0.0) + first))
                } else {
                    let last = block::find_baseline(&frag.children, true).unwrap_or(bh);
                    Some((true, bh - last + mb.unwrap_or(0.0)))
                }
            }
            _ => None,
        };
        laid.push(Laid {
            frag,
            area,
            margin: m.margin,
            rows: it.area[ROW],
            baseline,
        });
    }

    // Baseline sharing groups of the rows.
    let mut first: HashMap<usize, f64> = HashMap::new();
    let mut last: HashMap<usize, f64> = HashMap::new();
    for l in &laid {
        match l.baseline {
            Some((false, v)) => {
                let e = first.entry(l.rows.0).or_insert(0.0);
                *e = e.max(v);
            }
            Some((true, v)) => {
                let e = last.entry(l.rows.1).or_insert(0.0);
                *e = e.max(v);
            }
            None => {}
        }
    }

    // Alignment in the areas (§10).
    let rtl = g.rtl;
    let mut out: Vec<Fragment> = Vec::with_capacity(laid.len() + g.abs.len());
    for (l, it) in laid.into_iter().zip(&g.items) {
        let is = &it.b.style;
        let Laid {
            mut frag,
            area,
            margin,
            ..
        } = l;
        let (jp, jsafe) = justify_self(is, s);
        let (ap, asafe) = align_self(is, s);
        let item_flip = is.rtl() != rtl;
        // Horizontal.
        let bw = frag.rect.width;
        let (ml, mr) = (margin.left, margin.right);
        let free = area.width - bw - ml.unwrap_or(0.0) - mr.unwrap_or(0.0);
        let (uml, umr, x) = if (ml.is_none() || mr.is_none()) && free > 0.0 {
            let (a, z) = match (ml, mr) {
                (None, None) => (free / 2.0, free / 2.0),
                (None, Some(r)) => (free, r),
                (Some(l), _) => (l, free),
            };
            (a, z, area.x + a)
        } else {
            let (a, z) = (ml.unwrap_or(0.0), mr.unwrap_or(0.0));
            let off = if it.subgridded(COL) {
                0.0
            } else {
                offset_of(jp, jsafe, free, rtl, item_flip)
            };
            let x = if rtl {
                area.right() - off - (bw + a + z) + a
            } else {
                area.x + off + a
            };
            (a, z, x)
        };
        // Vertical.
        let bh = frag.rect.height;
        let (mt, mb) = (margin.top, margin.bottom);
        let free = area.height - bh - mt.unwrap_or(0.0) - mb.unwrap_or(0.0);
        let (umt, umb, y) = if (mt.is_none() || mb.is_none()) && free > 0.0 {
            let (a, z) = match (mt, mb) {
                (None, None) => (free / 2.0, free / 2.0),
                (None, Some(v)) => (free, v),
                (Some(v), _) => (v, free),
            };
            (a, z, area.y + a)
        } else {
            let (a, z) = (mt.unwrap_or(0.0), mb.unwrap_or(0.0));
            let off = match l.baseline {
                _ if it.subgridded(ROW) => 0.0,
                Some((false, v)) => first.get(&l.rows.0).map_or(0.0, |m| m - v),
                Some((true, v)) => free - last.get(&l.rows.1).map_or(0.0, |m| m - v),
                None => offset_of(ap, asafe, free, false, false),
            };
            (a, z, area.y + off + a)
        };
        frag.rect.x = x;
        frag.rect.y = y;
        frag.margin = Sides::new(umt, umr, umb, uml);
        out.push(frag);
    }

    // Absolutely positioned children: their static position is the
    // padding box's start corner, their containing block (when the
    // container is it) their grid area.
    let pad = model.padding;
    for ch in &g.abs {
        let area = g.abs_area(ch, (w, height), &pad);
        let x = if rtl { w + pad.right } else { -pad.left };
        let mut frag = block::static_position(ch, x.min(-pad.left), -pad.top, w + pad.horizontal());
        frag.grid_area = Some(Rect::new(
            area.x - frag.rect.x,
            area.y - frag.rect.y,
            area.width,
            area.height,
        ));
        out.push(frag);
    }

    Independent {
        children: out,
        height,
    }
}

/// The min-content and max-content widths of the content box of grid
/// container `b` (§12.3 under min-content and max-content constraints).
pub(super) fn intrinsic(ctx: &mut Ctx, b: &BoxNode, c: &Container) -> (f64, f64) {
    let s = &b.style;
    let bp_h = Model::of(s, 0.0).bp().horizontal();
    let limits = (
        content_limit(s, "min-width", None, bp_h),
        content_limit(s, "max-width", None, bp_h),
    );
    let mut g = Grid::build(
        s,
        blocks(c),
        [None, None],
        [limits, (None, None)],
        [None, None],
    );
    g.size(ctx, COL, Avail::MinContent, limits);
    let min = g.axes[COL].total();
    g.size(ctx, COL, Avail::MaxContent, limits);
    let max = g.axes[COL].total();
    (min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(px: f64) -> TrackSize {
        TrackSize {
            min: Breadth::Fixed(Lp::Px(px)),
            max: Breadth::Fixed(Lp::Px(px)),
            fit: None,
        }
    }

    #[test]
    fn auto_repeat_counts() {
        let list = vec![Entry::Repeat(
            Repeat::Fill,
            vec![Entry::Track(fixed(100.0))],
        )];
        assert_eq!(auto_count(&list, Some(400.0), None, 0.0), 4);
        assert_eq!(auto_count(&list, Some(400.0), None, 20.0), 3);
        assert_eq!(auto_count(&list, Some(50.0), None, 0.0), 1);
        assert_eq!(auto_count(&list, None, Some(250.0), 0.0), 3);
        assert_eq!(auto_count(&list, None, None, 0.0), 1);
    }

    #[test]
    fn line_names_merge_and_areas_parse() {
        let list = vec![
            Entry::Names(vec!["a".into()]),
            Entry::Repeat(
                Repeat::Count(2),
                vec![
                    Entry::Names(vec!["b".into()]),
                    Entry::Track(fixed(20.0)),
                    Entry::Names(vec!["c".into()]),
                ],
            ),
        ];
        let e = expand(&list, 1);
        assert_eq!(e.tracks.len(), 2);
        assert_eq!(e.names[0], vec!["a".to_string(), "b".to_string()]);
        assert_eq!(e.names[1], vec!["c".to_string(), "b".to_string()]);
        assert_eq!(
            area_cells("a  b ..."),
            Some(vec![Some("a".into()), Some("b".into()), None])
        );
        assert_eq!(area_cells("a #"), None);
    }

    #[test]
    fn named_lines_and_spans_resolve() {
        let lines = Lines {
            count: 3,
            names: vec![vec!["x".into()], vec![], vec!["x".into()], vec![]],
            areas: vec![("main".into(), 1, 3)],
        };
        let d = |p: Place| match p {
            Place::Definite(s, e) => (s, e),
            Place::Auto(n) => (-100, n as i32),
        };
        assert_eq!(
            d(resolve(
                &lines,
                &Raw::Ident("main".into()),
                &Raw::Ident("main".into())
            )),
            (1, 3)
        );
        assert_eq!(
            d(resolve(&lines, &Raw::Line(2, Some("x".into())), &Raw::Auto)),
            (2, 3)
        );
        // Not enough lines named `x`: implicit lines count.
        assert_eq!(
            d(resolve(&lines, &Raw::Line(3, Some("x".into())), &Raw::Auto)),
            (4, 5)
        );
        assert_eq!(
            d(resolve(
                &lines,
                &Raw::Line(1, None),
                &Raw::Span(2, Some("x".into()))
            )),
            (0, 4)
        );
        assert_eq!(
            d(resolve(&lines, &Raw::Line(-1, None), &Raw::Line(1, None))),
            (0, 3)
        );
        assert_eq!(
            d(resolve(&lines, &Raw::Span(3, None), &Raw::Auto)),
            (-100, 3)
        );
    }

    #[test]
    fn flexible_tracks_respect_their_base_sizes() {
        let mut tracks: Vec<Track> = (0..3)
            .map(|_| {
                Track::new(TrackSize {
                    min: Breadth::Auto,
                    max: Breadth::Fr(1.0),
                    fit: None,
                })
            })
            .collect();
        for t in &mut tracks {
            t.init(Some(300.0));
        }
        tracks[0].base = 150.0;
        assert_eq!(fr_size(&tracks, 0..3, 300.0, 0.0), 75.0);
        assert_eq!(fr_size(&tracks, 0..3, 600.0, 0.0), 200.0);
    }
}
