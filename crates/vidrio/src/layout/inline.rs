//! Inline formatting contexts (CSS 2 §9.4.2, §10.8; css-text-3;
//! css-inline-3): white space processing, `text-transform`, shaping,
//! break opportunities (UAX #14 tailored by `word-break`, `line-break`,
//! `overflow-wrap`, `hyphens` and `white-space`), line boxes shortened by
//! floats, tabs and `tab-size`, `text-indent`, `letter-spacing` and
//! `word-spacing`, `text-align` and `text-align-last` with justification,
//! bidi reordering (UAX #9) with `direction` and `unicode-bidi`,
//! `vertical-align` against the strut, `::first-line`, `text-overflow`,
//! and the min-content and max-content widths of inline content.

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use vgfx::font::Face;

use crate::css::style::ComputedStyle;
use crate::css::values::Value;

use super::Ctx;
use super::bidi;
use super::block::{self, Cb};
use super::boxtree::{InlineContent, InlineItem, is_collapsible_space};
use super::float::FloatContext;
use super::fragment::{Fragment, FragmentKind, PlacedGlyph, Rect, Sides, TextFragment};
use super::linebreak::{self, Break, Strictness, WordBreak};
use super::text::{self, Metrics};
use super::values::{self, Float, kw, lp};

const EPS: f64 = 1e-7;
/// Stands for an atomic inline in the text of the context.
const OBJECT: char = '\u{FFFC}';
const SHY: char = '\u{AD}';

/// Where an inline formatting context is laid out: the left edge and top
/// of its block container's content box (formatting context
/// coordinates), its width and the containing block height.
pub(crate) struct Place {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub cb_height: Option<f64>,
    /// Lines never wrap.
    pub single_line: bool,
}

/// The laid out content: line boxes and the floats and out-of-flow boxes
/// met on the way, all in formatting context coordinates, and the total
/// height of the line boxes.
pub(crate) struct Laid {
    pub lines: Vec<Fragment>,
    pub others: Vec<Fragment>,
    pub height: f64,
}

/// The used `line-height` of `style` whose primary font has metrics `m`.
pub(crate) fn line_height(style: &ComputedStyle, m: &Metrics) -> f64 {
    if kw(style, "line-height") == "normal" {
        m.normal_line_height()
    } else {
        style.line_height
    }
}

/// The ascent and descent of an inline box's layout bounds: the font's,
/// with half the leading added to each (CSS 2 §10.8.1).
fn half_leading(style: &ComputedStyle, m: &Metrics) -> (f64, f64) {
    let lh = line_height(style, m);
    let a = m.ascent + (lh - (m.ascent + m.descent)) / 2.0;
    (a, lh - a)
}

/// `white-space-collapse`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ws {
    Collapse,
    Discard,
    Preserve,
    PreserveBreaks,
    PreserveSpaces,
    BreakSpaces,
}

/// What the text of one style needs.
struct St {
    style: Rc<ComputedStyle>,
    m: Metrics,
    ws: Ws,
    wrap: bool,
    opts: linebreak::Options,
    /// `overflow-wrap: anywhere` or `word-break: break-word`: emergency
    /// breaks that count for min-content.
    anywhere: bool,
    /// `overflow-wrap: break-word`: emergency breaks only.
    break_word: bool,
    /// `hyphens` is not `none`: soft hyphens are break opportunities.
    hyphens: bool,
    letter: f64,
    word: f64,
    tab: f64,
}

impl St {
    fn new(ctx: &mut Ctx, style: &Rc<ComputedStyle>) -> St {
        let m = ctx.fonts.metrics(style);
        let ws = match kw(style, "white-space-collapse").as_str() {
            "discard" => Ws::Discard,
            "preserve" => Ws::Preserve,
            "preserve-breaks" => Ws::PreserveBreaks,
            "preserve-spaces" => Ws::PreserveSpaces,
            "break-spaces" => Ws::BreakSpaces,
            _ => Ws::Collapse,
        };
        let wb = kw(style, "word-break");
        let ow = kw(style, "overflow-wrap");
        let opts = linebreak::Options {
            word_break: match wb.as_str() {
                "break-all" => WordBreak::BreakAll,
                "keep-all" => WordBreak::KeepAll,
                _ => WordBreak::Normal,
            },
            strictness: match kw(style, "line-break").as_str() {
                "loose" => Strictness::Loose,
                "strict" => Strictness::Strict,
                "anywhere" => Strictness::Anywhere,
                _ => Strictness::Normal,
            },
        };
        let letter = text::letter_spacing(style);
        let word = text::word_spacing(style, m.space);
        let tab = match style.get("tab-size").and_then(|v| v.values.as_deref()) {
            Some([Value::Number(n)]) | Some([Value::Integer(n)]) => n * (m.space + letter + word),
            Some([v]) => values::value_lp(v).map_or(0.0, |l| l.resolve(0.0)),
            _ => 8.0 * (m.space + letter + word),
        };
        St {
            style: style.clone(),
            m,
            ws,
            wrap: kw(style, "text-wrap-mode") != "nowrap",
            opts,
            anywhere: ow == "anywhere" || wb == "break-word",
            break_word: ow == "break-word",
            hyphens: kw(style, "hyphens") != "none",
            letter,
            word,
            tab,
        }
    }
}

/// The styles of a context, one entry per distinct computed style.
struct Styles {
    list: Vec<St>,
    ptrs: Vec<*const ComputedStyle>,
    /// The `::first-line` style that replaces the container's.
    swap: Option<(*const ComputedStyle, Rc<ComputedStyle>)>,
}

impl Styles {
    fn index(&mut self, ctx: &mut Ctx, s: &Rc<ComputedStyle>) -> usize {
        let s = match &self.swap {
            Some((from, to)) if Rc::as_ptr(s) == *from => to.clone(),
            _ => s.clone(),
        };
        let p = Rc::as_ptr(&s);
        if let Some(i) = self.ptrs.iter().position(|&q| q == p) {
            return i;
        }
        self.ptrs.push(p);
        self.list.push(St::new(ctx, &s));
        self.list.len() - 1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Text,
    /// An atomic inline (index into the atoms).
    Atomic(usize),
    /// A preserved newline or `<br>`.
    Newline,
}

#[derive(Clone, Copy, Debug)]
struct Glyph {
    id: u16,
    dx: f64,
    dy: f64,
    adv: f64,
}

/// One character of the context after white space processing.
#[derive(Clone, Debug)]
struct Ch {
    c: char,
    st: usize,
    /// The innermost inline box around it.
    owner: Option<usize>,
    kind: Kind,
    /// A collapsible space: removed at the start and end of a line.
    collapsible: bool,
    /// A preserved space that hangs at the end of a line.
    hang: bool,
    level: u8,
    face: Option<Arc<Face>>,
    glyphs: Vec<Glyph>,
    /// The advance with letter and word spacing.
    adv: f64,
}

impl Ch {
    fn new(c: char, st: usize, owner: Option<usize>, kind: Kind) -> Ch {
        Ch {
            c,
            st,
            owner,
            kind,
            collapsible: false,
            hang: false,
            level: 0,
            face: None,
            glyphs: Vec::new(),
            adv: 0.0,
        }
    }
}

/// Things between characters.
#[derive(Clone, Copy, Debug)]
enum Ev {
    Open(usize),
    Close(usize),
    /// A float (index into the items).
    Float(usize),
    /// An out-of-flow box (index into the items).
    Oof(usize),
}

/// An atomic inline: its fragment (margin box at 0, 0), min-content and
/// margin box sizes, and the distance from its margin box top to its
/// baseline.
struct Atom {
    item: usize,
    frag: Option<Fragment>,
    min: f64,
    width: f64,
    height: f64,
    baseline: f64,
}

enum Mode {
    Layout(Cb),
    Intrinsic,
}

/// The context ready for line breaking.
struct Prep {
    sts: Vec<St>,
    chars: Vec<Ch>,
    /// Sorted by position: an event at `p` is before character `p`.
    events: Vec<(usize, Ev)>,
    /// The break opportunity before each character.
    breaks: Vec<Break>,
    atoms: Vec<Atom>,
    parent: Vec<Option<usize>>,
    box_st: Vec<usize>,
    /// The start and end edges (margin, border and padding) of each
    /// inline box.
    edges: Vec<(f64, f64)>,
    open_ev: Vec<usize>,
    close_ev: Vec<usize>,
    para: Option<bidi::Paragraph>,
    /// The bidi entry of each character (and one past the end).
    entry: Vec<usize>,
    /// The paragraph embedding level.
    base: u8,
}

fn open_controls(s: &ComputedStyle) -> Vec<u8> {
    let rtl = s.rtl();
    let (embed, isolate, over) = if rtl {
        (bidi::RLE, bidi::RLI, bidi::RLO)
    } else {
        (bidi::LRE, bidi::LRI, bidi::LRO)
    };
    match kw(s, "unicode-bidi").as_str() {
        "embed" => vec![embed],
        "isolate" => vec![isolate],
        "bidi-override" => vec![over],
        "isolate-override" => vec![isolate, over],
        "plaintext" => vec![bidi::FSI],
        _ => Vec::new(),
    }
}

fn close_controls(s: &ComputedStyle) -> Vec<u8> {
    match kw(s, "unicode-bidi").as_str() {
        "embed" | "bidi-override" => vec![bidi::PDF],
        "isolate" | "plaintext" => vec![bidi::PDI],
        "isolate-override" => vec![bidi::PDF, bidi::PDI],
        _ => Vec::new(),
    }
}

/// White space processing (css-text-3 §4.1.1) of one text item into
/// `own`, with `prev_space` carried across items; collapsible spaces
/// before a preserved newline are removed, also from `chars`.
fn collapse(text: &str, ws: Ws, prev_space: &mut bool, own: &mut Vec<char>, chars: &mut Vec<Ch>) {
    for c in text.chars() {
        match ws {
            Ws::Collapse | Ws::Discard | Ws::PreserveBreaks => {
                if c == '\n' && ws == Ws::PreserveBreaks {
                    if own.last() == Some(&' ') {
                        own.pop();
                    } else if own.is_empty() && chars.last().is_some_and(|c| c.collapsible) {
                        chars.pop();
                    }
                    own.push('\n');
                    *prev_space = true;
                } else if is_collapsible_space(c) {
                    if ws != Ws::Discard && !*prev_space {
                        own.push(' ');
                        *prev_space = true;
                    }
                } else {
                    own.push(c);
                    *prev_space = false;
                }
            }
            Ws::Preserve | Ws::PreserveSpaces | Ws::BreakSpaces => {
                match c {
                    '\r' => continue,
                    '\n' if ws == Ws::PreserveSpaces => own.push(' '),
                    _ => own.push(c),
                }
                *prev_space = false;
            }
        }
    }
}

fn prepare(
    ctx: &mut Ctx,
    ic: &InlineContent,
    root: &Rc<ComputedStyle>,
    first_line: Option<&Rc<ComputedStyle>>,
    mode: &Mode,
) -> Prep {
    let mut styles = Styles {
        list: Vec::new(),
        ptrs: Vec::new(),
        swap: first_line.map(|f| (Rc::as_ptr(root), f.clone())),
    };
    let root_st = styles.index(ctx, root);
    let nb = ic.boxes.len();
    let mut parent = vec![None; nb];
    let mut box_st = vec![root_st; nb];
    let mut open_ev = vec![usize::MAX; nb];
    let mut close_ev = vec![usize::MAX; nb];
    let mut stack: Vec<usize> = Vec::new();
    let mut chars: Vec<Ch> = Vec::new();
    let mut events: Vec<(usize, Ev)> = Vec::new();
    let mut atoms: Vec<Atom> = Vec::new();
    // Collapsible spaces at the start of the context go.
    let mut prev_space = true;
    for (k, item) in ic.items.iter().enumerate() {
        let owner = stack.last().copied();
        let owner_st = owner.map_or(root_st, |b| box_st[b]);
        match item {
            InlineItem::Open(b) => {
                parent[*b] = owner;
                box_st[*b] = styles.index(ctx, &ic.boxes[*b].style);
                open_ev[*b] = events.len();
                events.push((chars.len(), Ev::Open(*b)));
                stack.push(*b);
            }
            InlineItem::Close(b) => {
                close_ev[*b] = events.len();
                events.push((chars.len(), Ev::Close(*b)));
                if let Some(at) = stack.iter().rposition(|x| x == b) {
                    stack.truncate(at);
                }
            }
            InlineItem::Float(_) => events.push((chars.len(), Ev::Float(k))),
            InlineItem::OutOfFlow(_) => events.push((chars.len(), Ev::Oof(k))),
            InlineItem::Atomic(b) => {
                let atom = match mode {
                    Mode::Layout(cb) => {
                        let (frag, w, h, baseline) = block::layout_atomic(ctx, b, *cb);
                        Atom {
                            item: k,
                            frag: Some(frag),
                            min: w,
                            width: w,
                            height: h,
                            baseline,
                        }
                    }
                    Mode::Intrinsic => {
                        let (min, max) = block::contribution(ctx, b, None);
                        Atom {
                            item: k,
                            frag: None,
                            min,
                            width: max,
                            height: 0.0,
                            baseline: 0.0,
                        }
                    }
                };
                atoms.push(atom);
                chars.push(Ch::new(
                    OBJECT,
                    owner_st,
                    owner,
                    Kind::Atomic(atoms.len() - 1),
                ));
                prev_space = false;
            }
            InlineItem::Break(_) => {
                chars.push(Ch::new('\n', owner_st, owner, Kind::Newline));
                prev_space = true;
            }
            InlineItem::WordBreak => {
                chars.push(Ch::new('\u{200B}', owner_st, owner, Kind::Text));
            }
            InlineItem::Text { style, text } => {
                let st = styles.index(ctx, style);
                let ws = styles.list[st].ws;
                let mut own: Vec<char> = Vec::new();
                collapse(text, ws, &mut prev_space, &mut own, &mut chars);
                let prev_letter = chars
                    .last()
                    .is_some_and(|c| text::is_word_letter(c.c as u32));
                let s: String = own.iter().collect();
                let s = text::transform(&s, &styles.list[st].style, prev_letter);
                let wrap = styles.list[st].wrap;
                for c in s.chars() {
                    let kind = if c == '\n' { Kind::Newline } else { Kind::Text };
                    let mut ch = Ch::new(c, st, owner, kind);
                    ch.collapsible = c == ' ' && matches!(ws, Ws::Collapse | Ws::PreserveBreaks);
                    ch.hang = wrap
                        && matches!(c, ' ' | '\t' | '\u{3000}')
                        && matches!(ws, Ws::Preserve | Ws::PreserveSpaces);
                    chars.push(ch);
                }
            }
        }
    }
    let cbw = match mode {
        Mode::Layout(cb) => cb.width,
        Mode::Intrinsic => 0.0,
    };
    let edges: Vec<(f64, f64)> = ic
        .boxes
        .iter()
        .map(|b| {
            let m = values::margins(&b.style, cbw).or_zero();
            let bp = values::border(&b.style).add(&values::padding(&b.style, cbw));
            let left = m.left + bp.left;
            let right = m.right + bp.right;
            let (start, end) = if b.style.rtl() {
                (right, left)
            } else {
                (left, right)
            };
            (
                if b.split_start { 0.0 } else { start },
                if b.split_end { 0.0 } else { end },
            )
        })
        .collect();

    // Bidi: the characters with the controls of `unicode-bidi`.
    let rtl = root.rtl();
    let ub = kw(root, "unicode-bidi");
    let mut classes: Vec<u8> = Vec::new();
    let mut cps: Vec<u32> = Vec::new();
    let mut entry = vec![0usize; chars.len() + 1];
    let mut explicit = false;
    let root_override = matches!(ub.as_str(), "bidi-override" | "isolate-override");
    if root_override {
        classes.push(if rtl { bidi::RLO } else { bidi::LRO });
        cps.push(0);
        explicit = true;
    }
    let mut ev = 0;
    for i in 0..=chars.len() {
        while ev < events.len() && events[ev].0 == i {
            let controls = match events[ev].1 {
                Ev::Open(b) => open_controls(&ic.boxes[b].style),
                Ev::Close(b) => close_controls(&ic.boxes[b].style),
                _ => Vec::new(),
            };
            for c in controls {
                classes.push(c);
                cps.push(0);
                explicit = true;
            }
            ev += 1;
        }
        entry[i] = classes.len();
        if let Some(ch) = chars.get(i) {
            classes.push(match ch.kind {
                Kind::Atomic(_) => bidi::ON,
                _ => bidi::class_of(ch.c as u32),
            });
            cps.push(ch.c as u32);
        }
    }
    if root_override {
        classes.push(bidi::PDF);
        cps.push(0);
    }
    let base = if ub == "plaintext" {
        None
    } else {
        Some(rtl as u8)
    };
    let needed = explicit
        || base != Some(0)
        || classes
            .iter()
            .any(|&c| matches!(c, bidi::R | bidi::AL | bidi::AN));
    let para = needed.then(|| bidi::resolve(&classes, &cps, base));
    if let Some(p) = &para {
        for (i, ch) in chars.iter_mut().enumerate() {
            ch.level = p.levels[entry[i]];
        }
    }
    let base_level = para.as_ref().map_or(0, |p| p.level);

    // Shaping, by runs of one style and one direction.
    let mut i = 0;
    while i < chars.len() {
        if chars[i].kind != Kind::Text {
            i += 1;
            continue;
        }
        let st = chars[i].st;
        let odd = chars[i].level % 2 == 1;
        let mut j = i + 1;
        while j < chars.len()
            && chars[j].kind == Kind::Text
            && chars[j].st == st
            && (chars[j].level % 2 == 1) == odd
        {
            j += 1;
        }
        let s: String = chars[i..j].iter().map(|c| c.c).collect();
        let starts: Vec<usize> = s.char_indices().map(|(b, _)| b).collect();
        let style = styles.list[st].style.clone();
        for chunk in ctx.fonts.shape(&s, &style, odd) {
            for g in &chunk.glyphs {
                let k = match starts.binary_search(&(g.cluster as usize)) {
                    Ok(k) => k,
                    Err(k) => k.saturating_sub(1),
                };
                let ch = &mut chars[i + k];
                ch.glyphs.push(Glyph {
                    id: g.glyph,
                    dx: g.x_offset as f64,
                    dy: g.y_offset as f64,
                    adv: g.x_advance as f64,
                });
                ch.adv += g.x_advance as f64;
                if ch.face.is_none() {
                    ch.face = chunk.face.clone();
                }
            }
        }
        for k in i + 1..j {
            if chars[k].face.is_none() {
                chars[k].face = chars[k - 1].face.clone();
            }
        }
        i = j;
    }
    for ch in &mut chars {
        let st = &styles.list[ch.st];
        match ch.kind {
            Kind::Text => {
                if matches!(ch.c, '\t' | '\n') || text::is_default_ignorable(ch.c) {
                    ch.glyphs.clear();
                    ch.adv = 0.0;
                    continue;
                }
                if !ch.glyphs.is_empty() {
                    ch.adv += st.letter;
                }
                if text::is_word_separator(ch.c) {
                    ch.adv += st.word;
                }
            }
            Kind::Atomic(a) => ch.adv = atoms[a].width,
            Kind::Newline => {
                ch.glyphs.clear();
                ch.adv = 0.0;
            }
        }
    }

    // Break opportunities.
    let cs: Vec<char> = chars.iter().map(|c| c.c).collect();
    let opts: Vec<linebreak::Options> = chars.iter().map(|c| styles.list[c.st].opts).collect();
    let mut breaks = linebreak::opportunities(&cs, &opts);
    for i in 1..chars.len() {
        if breaks[i] == Break::Mandatory {
            continue;
        }
        let (a, b) = (&chars[i - 1], &chars[i]);
        let (sa, sb) = (&styles.list[a.st], &styles.list[b.st]);
        if sa.ws == Ws::BreakSpaces && a.kind == Kind::Text && matches!(a.c, ' ' | '\t') {
            breaks[i] = Break::Allowed;
        }
        if a.c == SHY && !sa.hyphens {
            breaks[i] = Break::None;
        }
        if !sa.wrap || !sb.wrap {
            breaks[i] = Break::None;
        }
    }

    Prep {
        sts: styles.list,
        chars,
        events,
        breaks,
        atoms,
        parent,
        box_st,
        edges,
        open_ev,
        close_ev,
        para,
        entry,
        base: base_level,
    }
}

/// `text-indent`: its length (percentages of `basis`), `hanging` and
/// `each-line`.
fn text_indent(style: &ComputedStyle, basis: Option<f64>) -> (f64, bool, bool) {
    let (mut len, mut hanging, mut each) = (0.0, false, false);
    if let Some(list) = style.get("text-indent").and_then(|v| v.values.as_deref()) {
        for v in list {
            match v {
                Value::Keyword(k) if k == "hanging" => hanging = true,
                Value::Keyword(k) if k == "each-line" => each = true,
                v => {
                    if let Some(l) = values::value_lp(v) {
                        len = l.resolve_opt(basis).unwrap_or(0.0);
                    }
                }
            }
        }
    }
    (len, hanging, each)
}

/// The width of character `i` at `x` from the line's start (`offset`
/// from the content box's start edge before that, for tab stops).
fn char_width(p: &Prep, i: usize, x: f64, offset: f64) -> f64 {
    let ch = &p.chars[i];
    if ch.c != '\t' || ch.kind != Kind::Text {
        return ch.adv;
    }
    let st = &p.sts[ch.st];
    if st.tab <= EPS {
        return 0.0;
    }
    let pos = offset + x;
    let mut next = ((pos + EPS) / st.tab).floor() * st.tab + st.tab;
    if next - pos < 0.5 * st.m.space {
        next += st.tab;
    }
    next - pos
}

/// The min-content and max-content widths of inline content (CSS Sizing
/// 3 §5.1): the widest unbreakable piece, and the widest line with only
/// forced breaks.
pub(crate) fn intrinsic(
    ctx: &mut Ctx,
    ic: &InlineContent,
    style: &Rc<ComputedStyle>,
    single_line: bool,
) -> (f64, f64) {
    let p = prepare(ctx, ic, style, None, &Mode::Intrinsic);
    let (indent, _, each) = text_indent(style, None);
    let n = p.chars.len();
    let (mut min, mut max) = (0.0f64, 0.0f64);
    let (mut seg, mut seg_trail) = (indent, 0.0);
    let (mut line, mut line_trail) = (indent, 0.0);
    let mut ev = 0;
    for i in 0..=n {
        if i > 0 && i < n {
            let brk = p.breaks[i];
            let st = &p.sts[p.chars[i].st];
            let emergency = st.anywhere
                && p.chars[i].kind == Kind::Text
                && !linebreak::is_grapheme_extend(p.chars[i].c);
            if brk != Break::None || emergency {
                min = min.max(seg - seg_trail);
                seg = 0.0;
                seg_trail = 0.0;
            }
            if brk == Break::Mandatory {
                max = max.max(line - line_trail);
                line = if each { indent } else { 0.0 };
                line_trail = 0.0;
            }
        }
        while ev < p.events.len() && p.events[ev].0 == i {
            match p.events[ev].1 {
                Ev::Open(b) => {
                    seg += p.edges[b].0;
                    line += p.edges[b].0;
                }
                Ev::Close(b) => {
                    seg += p.edges[b].1;
                    line += p.edges[b].1;
                }
                Ev::Float(k) => {
                    if let InlineItem::Float(b) = &ic.items[k] {
                        let (fmin, fmax) = block::contribution(ctx, b, None);
                        min = min.max(fmin);
                        line += fmax;
                    }
                }
                Ev::Oof(_) => {}
            }
            ev += 1;
        }
        if i == n {
            break;
        }
        let ch = &p.chars[i];
        let (wmin, wmax) = match ch.kind {
            Kind::Atomic(a) => (p.atoms[a].min, p.atoms[a].width),
            Kind::Newline => (0.0, 0.0),
            Kind::Text => {
                let w = char_width(&p, i, line, 0.0);
                (if ch.c == '\t' { p.sts[ch.st].tab } else { w }, w)
            }
        };
        if ch.collapsible || ch.hang {
            seg_trail += wmin;
            line_trail += wmax;
        } else if ch.kind != Kind::Newline {
            seg_trail = 0.0;
            line_trail = 0.0;
        }
        seg += wmin;
        line += wmax;
    }
    min = min.max(seg - seg_trail);
    max = max.max(line - line_trail);
    if single_line {
        min = max;
    }
    (min, max.max(min))
}

/// A line chosen by [`fill`]: characters `start..end` and events
/// `ev_start..ev_end`, whether a forced break ends it, and the left and
/// right edges of the line box between the floats.
struct Filled {
    start: usize,
    end: usize,
    ev_start: usize,
    ev_end: usize,
    forced: bool,
    l: f64,
    r: f64,
}

enum Fill {
    Done(Filled),
    /// The first unbreakable piece needs this width: try lower, past a
    /// float.
    Lower(f64),
}

/// The floats and the containing block of a context during layout.
struct Area<'f> {
    x0: f64,
    x1: f64,
    cb: Cb,
    single_line: bool,
    floats: &'f mut FloatContext,
    placed: HashSet<usize>,
    deferred: Vec<usize>,
    others: Vec<Fragment>,
}

impl Area<'_> {
    fn available(&self, y: f64, band: f64) -> (f64, f64) {
        if self.single_line {
            (self.x0, self.x1)
        } else {
            self.floats.available(y, band, self.x0, self.x1)
        }
    }

    /// Places the float of item `k` (laid out as `frag`, margin box `mw`
    /// × `mh`) no higher than `y`.
    fn place(&mut self, ic: &InlineContent, k: usize, frag: Fragment, mw: f64, mh: f64, y: f64) {
        let InlineItem::Float(b) = &ic.items[k] else {
            return;
        };
        let mut y = y;
        let (cl, cr) = values::clear(&b.style);
        if cl || cr {
            if let Some(cy) = self.floats.clearance_y(cl, cr) {
                y = y.max(cy);
            }
        }
        let side = match values::float(&b.style) {
            Float::Right => Float::Right,
            _ => Float::Left,
        };
        let (fx, fy) = self.floats.place(side, mw, mh, y, self.x0, self.x1);
        let mut frag = frag;
        frag.translate(fx, fy);
        self.others.push(frag);
        self.placed.insert(k);
    }

    fn place_deferred(&mut self, ctx: &mut Ctx, ic: &InlineContent, y: f64) {
        for k in std::mem::take(&mut self.deferred) {
            if let InlineItem::Float(b) = &ic.items[k] {
                let (frag, mw, mh) = block::layout_float(ctx, b, self.cb);
                self.place(ic, k, frag, mw, mh, y);
            }
        }
    }
}

/// Chooses the content of the line that starts at character `start` and
/// event `ev_start` at `y`, placing the floats met on the way.
#[allow(clippy::too_many_arguments)]
fn fill(
    ctx: &mut Ctx,
    ic: &InlineContent,
    p: &Prep,
    start: usize,
    ev_start: usize,
    y: f64,
    band: f64,
    indent: f64,
    retry: bool,
    area: &mut Area,
) -> Fill {
    let n = p.chars.len();
    let (mut l, mut r) = area.available(y, band);
    let mut w = indent;
    let mut content = false;
    let mut last_break: Option<(usize, usize)> = None;
    let (mut i, mut e) = (start, ev_start);
    let done = |end: usize, ev_end: usize, forced: bool, l: f64, r: f64| {
        Fill::Done(Filled {
            start,
            end,
            ev_start,
            ev_end,
            forced,
            l,
            r,
        })
    };
    loop {
        // The end edges of boxes closing here stay on a line that ends
        // here.
        let mut after_close = e;
        while after_close < p.events.len()
            && p.events[after_close].0 == i
            && matches!(p.events[after_close].1, Ev::Close(_))
        {
            after_close += 1;
        }
        if i > start && i < n {
            match p.breaks[i] {
                Break::Mandatory => return done(i, after_close, true, l, r),
                Break::Allowed => last_break = Some((i, after_close)),
                Break::None => {}
            }
        }
        while e < p.events.len() && p.events[e].0 == i {
            match p.events[e].1 {
                Ev::Open(b) => w += p.edges[b].0,
                Ev::Close(b) => w += p.edges[b].1,
                Ev::Float(k) => {
                    if !area.placed.contains(&k) && !area.deferred.contains(&k) {
                        if let InlineItem::Float(b) = &ic.items[k] {
                            let (frag, mw, mh) = block::layout_float(ctx, b, area.cb);
                            if !content || w + mw <= r - l + EPS {
                                area.place(ic, k, frag, mw, mh, y);
                                (l, r) = area.available(y, band);
                            } else {
                                area.deferred.push(k);
                            }
                        }
                    }
                }
                Ev::Oof(_) => {}
            }
            e += 1;
        }
        if i >= n {
            let forced = n > start && p.chars[n - 1].kind == Kind::Newline;
            return done(n, e, forced, l, r);
        }
        let ch = &p.chars[i];
        let cw = if ch.collapsible && !content {
            0.0
        } else {
            char_width(p, i, w, l - area.x0)
        };
        let soft = ch.collapsible || ch.hang || ch.kind == Kind::Newline;
        if !area.single_line && !soft && w + cw > r - l + EPS {
            if let Some((bi, be)) = last_break {
                return done(bi, be, false, l, r);
            }
            if retry && (l > area.x0 + EPS || r < area.x1 - EPS) {
                let mut needed = w + cw;
                let mut k = i + 1;
                while k < n && p.breaks[k] == Break::None {
                    needed += p.chars[k].adv;
                    k += 1;
                }
                return Fill::Lower(needed);
            }
            let emergency = |k: usize| {
                let st = &p.sts[p.chars[k].st];
                st.anywhere || st.break_word
            };
            if content
                && i > start
                && (emergency(i) || emergency(i - 1))
                && !linebreak::is_grapheme_extend(ch.c)
            {
                return done(i, after_close, false, l, r);
            }
        }
        w += cw;
        if !ch.collapsible && ch.kind != Kind::Newline {
            content = true;
        }
        i += 1;
    }
}

/// What a piece of a line is.
#[derive(Clone, Debug)]
enum PieceKind {
    /// The start (`open`) or end edge of inline box `b`.
    Edge { b: usize, open: bool },
    /// Characters of one style, owner, level and face.
    Run {
        chars: Vec<usize>,
        st: usize,
        owner: Option<usize>,
    },
    /// An atomic inline (its character).
    Atomic(usize),
    /// The static position of an out-of-flow box (its item).
    Anchor(usize),
    /// A hyphen or an ellipsis.
    Generated {
        text: String,
        st: usize,
        owner: Option<usize>,
        glyphs: Vec<Glyph>,
        face: Option<Arc<Face>>,
    },
}

#[derive(Clone, Debug)]
struct Piece {
    kind: PieceKind,
    level: u8,
    x: f64,
    width: f64,
    hidden: bool,
}

impl Piece {
    fn new(kind: PieceKind, level: u8, width: f64) -> Piece {
        Piece {
            kind,
            level,
            x: 0.0,
            width,
            hidden: false,
        }
    }
}

/// `vertical-align` of a box relative to its parent.
enum Va {
    /// The baseline moves down by this much.
    Shift(f64),
    Top,
    Bottom,
}

/// `vertical-align` (CSS 2 §10.8.1) of a box with style `s` whose layout
/// bounds are `a` above and `d` below its baseline, in a parent whose
/// font has metrics `pm`.
fn valign(s: &ComputedStyle, pm: &Metrics, a: f64, d: f64, own_lh: f64) -> Va {
    match kw(s, "vertical-align").as_str() {
        "baseline" => Va::Shift(0.0),
        "sub" => Va::Shift(pm.size / 5.0),
        "super" => Va::Shift(-pm.size / 3.0),
        "text-top" => Va::Shift(a - pm.ascent),
        "text-bottom" => Va::Shift(pm.descent - d),
        "middle" => Va::Shift((a - d) / 2.0 - pm.x_height / 2.0),
        "top" => Va::Top,
        "bottom" => Va::Bottom,
        _ => Va::Shift(-lp(s, "vertical-align").map_or(0.0, |l| l.resolve(own_lh))),
    }
}

/// The boxes aligned together: the root inline box with everything
/// aligned to it, and each subtree aligned to the line box's top or
/// bottom.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Group {
    Root,
    Top(usize),
    Bottom(usize),
}

/// The fragments a string gets in a style (a hyphen, an ellipsis).
fn generated(ctx: &mut Ctx, st: &St, s: &str) -> (Vec<Glyph>, Option<Arc<Face>>, f64) {
    let mut glyphs = Vec::new();
    let mut face = None;
    let mut width = 0.0;
    for chunk in ctx.fonts.shape(s, &st.style, false) {
        if face.is_none() {
            face = chunk.face.clone();
        }
        for g in &chunk.glyphs {
            glyphs.push(Glyph {
                id: g.glyph,
                dx: g.x_offset as f64,
                dy: g.y_offset as f64,
                adv: g.x_advance as f64 + st.letter,
            });
            width += g.x_advance as f64 + st.letter;
        }
    }
    (glyphs, face, width)
}

/// The ellipsis string of `text-overflow` at the end of a left-to-right
/// line, if any.
fn overflow_string(style: &ComputedStyle) -> Option<String> {
    let list = style.get("text-overflow")?.values.clone()?;
    let end = list.last()?;
    match end {
        Value::Keyword(k) if k == "ellipsis" => Some("\u{2026}".to_string()),
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// A finished line: its fragment (`None` when the line is empty and takes
/// no room), its height and the out-of-flow boxes anchored in it.
struct Built {
    frag: Option<Fragment>,
    height: f64,
    oofs: Vec<Fragment>,
}

/// Builds the line box of `f` at `y` (CSS 2 §10.8, css-text-3 §4.1.2,
/// §6, §7, css-text-4 text-overflow).
#[allow(clippy::too_many_arguments)]
fn build_line(
    ctx: &mut Ctx,
    ic: &InlineContent,
    p: &Prep,
    f: &Filled,
    y: f64,
    indent: f64,
    container: &ComputedStyle,
    last: bool,
    area: &Area,
) -> Built {
    let (s, e) = (f.start, f.end);
    let n = e - s;
    let x0 = area.x0;
    let cb = area.cb;
    // Collapsible spaces at the start and end of the line go; preserved
    // ones at the end hang.
    let mut removed = vec![false; n];
    let mut hanging = vec![false; n];
    let mut k = s;
    while k < e && p.chars[k].collapsible {
        removed[k - s] = true;
        k += 1;
    }
    let mut k = e;
    while k > s {
        let c = &p.chars[k - 1];
        if c.kind == Kind::Newline || removed[k - 1 - s] {
            k -= 1;
        } else if c.collapsible {
            removed[k - 1 - s] = true;
            k -= 1;
        } else {
            break;
        }
    }
    let mut k = e;
    while k > s {
        let c = &p.chars[k - 1];
        if c.kind == Kind::Newline || removed[k - 1 - s] {
            k -= 1;
        } else if c.hang {
            hanging[k - 1 - s] = true;
            k -= 1;
        } else {
            break;
        }
    }

    // Widths in logical order (tab stops need the position).
    let mut cw = vec![0.0; n];
    let mut ev = f.ev_start;
    let mut pos = indent;
    let mut content_w = 0.0;
    for i in s..e {
        while ev < f.ev_end && p.events[ev].0 == i {
            let w = match p.events[ev].1 {
                Ev::Open(b) => p.edges[b].0,
                Ev::Close(b) => p.edges[b].1,
                _ => 0.0,
            };
            pos += w;
            content_w += w;
            ev += 1;
        }
        if removed[i - s] {
            continue;
        }
        cw[i - s] = char_width(p, i, pos, f.l - x0);
        pos += cw[i - s];
        if !hanging[i - s] {
            content_w += cw[i - s];
        }
    }
    while ev < f.ev_end {
        content_w += match p.events[ev].1 {
            Ev::Open(b) => p.edges[b].0,
            Ev::Close(b) => p.edges[b].1,
            _ => 0.0,
        };
        ev += 1;
    }

    // A hyphen where a soft hyphen breaks the line.
    let mut hyphen = None;
    if !f.forced && e > s && e < p.chars.len() && p.chars[e - 1].c == SHY {
        let c = &p.chars[e - 1];
        let st = &p.sts[c.st];
        let mut g = generated(ctx, st, "\u{2010}");
        if g.0.iter().all(|x| x.id == 0) {
            g = generated(ctx, st, "-");
        }
        content_w += g.2;
        hyphen = Some((c.st, c.owner, c.level, g));
    }

    let line_levels: Vec<u8> = match &p.para {
        Some(para) if e > s => {
            let a = p.entry[s];
            let b = p.entry[e - 1] + 1;
            let lv = para.line_levels(a, b);
            (s..e).map(|i| lv[p.entry[i] - a]).collect()
        }
        _ => (s..e).map(|i| p.chars[i].level).collect(),
    };

    // Alignment and justification.
    let rtl = container.rtl();
    let avail = f.r - f.l - indent;
    let free = avail - content_w;
    let ta = kw(container, "text-align");
    let tal = kw(container, "text-align-last");
    let mut mode = if ta == "justify-all" {
        "justify".to_string()
    } else if last || f.forced {
        match tal.as_str() {
            "auto" if ta == "justify" => "start".to_string(),
            "auto" => ta.clone(),
            _ => tal.clone(),
        }
    } else {
        ta.clone()
    };
    if mode == "match-parent" {
        mode = "start".to_string();
    }
    let mut justified = false;
    if mode == "justify" {
        mode = "start".to_string();
        let tj = kw(container, "text-justify");
        if free > EPS && tj != "none" {
            let per_char = tj == "inter-character" || tj == "distribute";
            let visible: Vec<usize> = (s..e)
                .filter(|&i| {
                    !removed[i - s]
                        && !hanging[i - s]
                        && p.chars[i].kind != Kind::Newline
                        && (p.chars[i].kind != Kind::Text || !p.chars[i].glyphs.is_empty())
                })
                .collect();
            let chosen: Vec<usize> = if per_char {
                visible[..visible.len().saturating_sub(1)].to_vec()
            } else {
                visible
                    .iter()
                    .copied()
                    .filter(|&i| {
                        p.chars[i].kind == Kind::Text && text::is_word_separator(p.chars[i].c)
                    })
                    .collect()
            };
            if !chosen.is_empty() {
                let extra = free / chosen.len() as f64;
                for i in chosen {
                    cw[i - s] += extra;
                }
                justified = true;
            }
        }
    }
    let offset = if justified || free < 0.0 {
        if rtl && !justified { free } else { 0.0 }
    } else {
        match mode.as_str() {
            "left" => 0.0,
            "right" => free,
            "center" => free / 2.0,
            "end" => {
                if rtl {
                    0.0
                } else {
                    free
                }
            }
            _ => {
                if rtl {
                    free
                } else {
                    0.0
                }
            }
        }
    };
    let x_start = f.l + if rtl { 0.0 } else { indent } + offset;

    // The pieces in logical order.
    let base = p.base;
    let mut pieces: Vec<Piece> = Vec::new();
    let mut ev = f.ev_start;
    for i in s..=e {
        while ev < f.ev_end && p.events[ev].0 == i {
            match p.events[ev].1 {
                Ev::Open(b) => pieces.push(Piece::new(
                    PieceKind::Edge { b, open: true },
                    base,
                    p.edges[b].0,
                )),
                Ev::Close(b) => pieces.push(Piece::new(
                    PieceKind::Edge { b, open: false },
                    base,
                    p.edges[b].1,
                )),
                Ev::Oof(k) => pieces.push(Piece::new(PieceKind::Anchor(k), base, 0.0)),
                Ev::Float(_) => {}
            }
            ev += 1;
        }
        if i == e {
            break;
        }
        if removed[i - s] {
            continue;
        }
        let ch = &p.chars[i];
        let lv = line_levels[i - s];
        match ch.kind {
            Kind::Newline => {}
            Kind::Atomic(_) => pieces.push(Piece::new(PieceKind::Atomic(i), lv, cw[i - s])),
            Kind::Text => {
                let extended = match pieces.last_mut() {
                    Some(Piece {
                        kind: PieceKind::Run { chars, st, owner },
                        level,
                        width,
                        ..
                    }) if *st == ch.st
                        && *owner == ch.owner
                        && *level == lv
                        && chars.last().is_some_and(|&l| {
                            l + 1 == i && same_face(&p.chars[l].face, &ch.face)
                        }) =>
                    {
                        chars.push(i);
                        *width += cw[i - s];
                        true
                    }
                    _ => false,
                };
                if !extended {
                    pieces.push(Piece::new(
                        PieceKind::Run {
                            chars: vec![i],
                            st: ch.st,
                            owner: ch.owner,
                        },
                        lv,
                        cw[i - s],
                    ));
                }
            }
        }
    }
    if let Some((st, owner, level, (glyphs, face, width))) = hyphen {
        pieces.push(Piece::new(
            PieceKind::Generated {
                text: "\u{2010}".to_string(),
                st,
                owner,
                glyphs,
                face,
            },
            level,
            width,
        ));
    }

    let significant = pieces.iter().any(|pc| match &pc.kind {
        PieceKind::Run { .. } | PieceKind::Atomic(_) | PieceKind::Generated { .. } => true,
        PieceKind::Edge { .. } => pc.width != 0.0,
        PieceKind::Anchor(_) => false,
    }) || (e > s && p.chars[e - 1].kind == Kind::Newline);
    if !significant {
        let mut oofs = Vec::new();
        for pc in &pieces {
            if let PieceKind::Anchor(k) = pc.kind {
                if let InlineItem::OutOfFlow(b) = &ic.items[k] {
                    oofs.push(block::static_position(b, x_start, y, 0.0));
                }
            }
        }
        return Built {
            frag: None,
            height: 0.0,
            oofs,
        };
    }

    // Visual order (UAX #9 L2) and positions.
    let levels: Vec<u8> = pieces.iter().map(|pc| pc.level).collect();
    let order: Vec<usize> = if levels.iter().any(|l| l % 2 == 1) {
        bidi::visual_order(&levels)
    } else {
        (0..pieces.len()).collect()
    };
    let mut x = x_start;
    for &k in &order {
        pieces[k].x = x;
        x += pieces[k].width;
    }

    // `text-overflow` at the end of a left-to-right line that overflows a
    // box that clips.
    if values::clips_inline(container) && base % 2 == 0 && x > f.r + EPS {
        if let Some(es) = overflow_string(container) {
            let (glyphs, face, ew) = generated(ctx, &p.sts[0], &es);
            let limit = f.r - ew;
            let mut cut = x_start;
            let mut cutting = false;
            for &k in &order {
                let pc = &mut pieces[k];
                if cutting {
                    pc.hidden = true;
                    continue;
                }
                if pc.x + pc.width <= limit + EPS {
                    cut = pc.x + pc.width;
                    continue;
                }
                cutting = true;
                match &mut pc.kind {
                    PieceKind::Run { chars, .. } if pc.level % 2 == 0 => {
                        let mut kept = 0;
                        let mut wsum = 0.0;
                        for &c in chars.iter() {
                            if pc.x + wsum + cw[c - s] > limit + EPS {
                                break;
                            }
                            wsum += cw[c - s];
                            kept += 1;
                        }
                        chars.truncate(kept);
                        pc.width = wsum;
                        pc.hidden = kept == 0;
                        cut = pc.x + wsum;
                    }
                    PieceKind::Edge { .. } | PieceKind::Anchor(_) => {}
                    _ => pc.hidden = true,
                }
            }
            let mut ell = Piece::new(
                PieceKind::Generated {
                    text: es,
                    st: 0,
                    owner: None,
                    glyphs,
                    face,
                },
                base,
                ew,
            );
            ell.x = cut;
            pieces.push(ell);
        }
    }

    // Vertical alignment against the strut.
    let nb = p.edges.len();
    let active: Vec<usize> = (0..nb)
        .filter(|&b| {
            p.open_ev[b] < f.ev_end && (p.close_ev[b] == usize::MAX || p.close_ev[b] >= f.ev_start)
        })
        .collect();
    let root = &p.sts[0];
    let (ra, rd) = half_leading(&root.style, &root.m);
    let mut off = vec![0.0; nb];
    let mut group = vec![Group::Root; nb];
    let mut extents: Vec<(Group, f64, f64)> = vec![(Group::Root, -ra, rd)];
    let mut extend =
        |extents: &mut Vec<(Group, f64, f64)>, g: Group, top: f64, bottom: f64| match extents
            .iter_mut()
            .find(|x| x.0 == g)
        {
            Some(x) => {
                x.1 = x.1.min(top);
                x.2 = x.2.max(bottom);
            }
            None => extents.push((g, top, bottom)),
        };
    for &b in &active {
        let st = &p.sts[p.box_st[b]];
        let (a, d) = half_leading(&st.style, &st.m);
        let (pm, poff, pg) = match p.parent[b] {
            Some(q) => (&p.sts[p.box_st[q]].m, off[q], group[q]),
            None => (&root.m, 0.0, Group::Root),
        };
        match valign(&st.style, pm, a, d, line_height(&st.style, &st.m)) {
            Va::Shift(o) => {
                off[b] = poff + o;
                group[b] = pg;
            }
            Va::Top => group[b] = Group::Top(b),
            Va::Bottom => group[b] = Group::Bottom(b),
        }
        extend(&mut extents, group[b], off[b] - a, off[b] + d);
    }
    // Atomic inlines: (piece, group, offset).
    let mut atomic_pos: Vec<(usize, Group, f64)> = Vec::new();
    for (k, pc) in pieces.iter().enumerate() {
        let PieceKind::Atomic(i) = pc.kind else {
            continue;
        };
        if pc.hidden {
            continue;
        }
        let Kind::Atomic(a) = p.chars[i].kind else {
            continue;
        };
        let atom = &p.atoms[a];
        let InlineItem::Atomic(bn) = &ic.items[atom.item] else {
            continue;
        };
        let (aa, ad) = (atom.baseline, atom.height - atom.baseline);
        let (pm, poff, pg) = match p.chars[i].owner {
            Some(q) => (&p.sts[p.box_st[q]].m, off[q], group[q]),
            None => (&root.m, 0.0, Group::Root),
        };
        let own_m = ctx.fonts.metrics(&bn.style);
        let (g, o) = match valign(&bn.style, pm, aa, ad, line_height(&bn.style, &own_m)) {
            Va::Shift(o) => (pg, poff + o),
            Va::Top => (Group::Top(nb + k), 0.0),
            Va::Bottom => (Group::Bottom(nb + k), 0.0),
        };
        extend(&mut extents, g, o - aa, o + ad);
        atomic_pos.push((k, g, o));
    }
    let (rt, rb) = (extents[0].1, extents[0].2);
    let mut h = rb - rt;
    for &(g, t, b) in &extents {
        if let Group::Top(_) = g {
            h = h.max(b - t);
        }
    }
    let mut shift = 0.0;
    for &(g, t, b) in &extents {
        if let Group::Bottom(_) = g {
            if b - t > h {
                shift += b - t - h;
                h = b - t;
            }
        }
    }
    let root_base = shift - rt;
    let base_of = |g: Group| -> f64 {
        match g {
            Group::Root => root_base,
            Group::Top(_) => -extents.iter().find(|x| x.0 == g).map_or(0.0, |x| x.1),
            Group::Bottom(_) => h - extents.iter().find(|x| x.0 == g).map_or(0.0, |x| x.2),
        }
    };
    let baseline_of_owner = |owner: Option<usize>| -> f64 {
        match owner {
            Some(b) => base_of(group[b]) + off[b],
            None => root_base,
        }
    };

    // Fragments, relative to the line box.
    let mut line = Fragment::new(
        FragmentKind::Line {
            baseline: root_base,
        },
        root.style.clone(),
    );
    line.rect = Rect::new(f.l, y, f.r - f.l, h);
    // Inline boxes first (their backgrounds go under their content).
    for &b in &active {
        let (mut x_min, mut x_max) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut open_x, mut close_x): (Option<f64>, Option<f64>) = (None, None);
        for pc in pieces.iter().filter(|pc| !pc.hidden) {
            let within = match &pc.kind {
                PieceKind::Edge { b: eb, open } => {
                    if *eb == b {
                        if *open {
                            open_x = Some(pc.x);
                        } else {
                            close_x = Some(pc.x);
                        }
                        true
                    } else {
                        is_inside(p, Some(*eb), b)
                    }
                }
                PieceKind::Run { owner, .. } | PieceKind::Generated { owner, .. } => {
                    is_inside(p, *owner, b)
                }
                PieceKind::Atomic(i) => is_inside(p, p.chars[*i].owner, b),
                PieceKind::Anchor(_) => false,
            };
            if within {
                x_min = x_min.min(pc.x);
                x_max = x_max.max(pc.x + pc.width);
            }
        }
        if x_min > x_max {
            continue;
        }
        let ib = &ic.boxes[b];
        let st = &p.sts[p.box_st[b]];
        let m = values::margins(&ib.style, cb.width).or_zero();
        let border = values::border(&ib.style);
        let pad = values::padding(&ib.style, cb.width);
        let left = [open_x, close_x]
            .iter()
            .any(|x| x.is_some_and(|x| (x - x_min).abs() < EPS));
        let right_edge = |x: Option<f64>, w: f64| x.is_some_and(|x| (x + w - x_max).abs() < EPS);
        let right = right_edge(open_x, p.edges[b].0) || right_edge(close_x, p.edges[b].1);
        let bx = x_min + if left { m.left } else { 0.0 };
        let bx2 = x_max - if right { m.right } else { 0.0 };
        let baseline = base_of(group[b]) + off[b];
        let side = |on: bool, v: f64| if on { v } else { 0.0 };
        let mut frag = Fragment::new(
            FragmentKind::Inline {
                start: open_x.is_some() && !ib.split_start,
                end: close_x.is_some() && !ib.split_end,
            },
            ib.style.clone(),
        );
        frag.node = ib.node;
        frag.pseudo = ib.pseudo;
        frag.margin = Sides::new(m.top, side(right, m.right), m.bottom, side(left, m.left));
        frag.border = Sides::new(
            border.top,
            side(right, border.right),
            border.bottom,
            side(left, border.left),
        );
        frag.padding = Sides::new(
            pad.top,
            side(right, pad.right),
            pad.bottom,
            side(left, pad.left),
        );
        frag.rect = Rect::new(
            bx - f.l,
            baseline - st.m.ascent - pad.top - border.top,
            (bx2 - bx).max(0.0),
            st.m.ascent + st.m.descent + pad.vertical() + border.vertical(),
        );
        line.children.push(frag);
    }
    let mut oofs = Vec::new();
    for (k, pc) in pieces.iter().enumerate() {
        if pc.hidden {
            continue;
        }
        match &pc.kind {
            PieceKind::Run { chars, st, owner } => {
                let st = &p.sts[*st];
                let baseline = baseline_of_owner(*owner);
                let order: Vec<usize> = if pc.level % 2 == 1 {
                    chars.iter().rev().copied().collect()
                } else {
                    chars.clone()
                };
                let mut glyphs = Vec::new();
                let mut gx = 0.0;
                for &c in &order {
                    let mut cx = gx;
                    for g in &p.chars[c].glyphs {
                        glyphs.push(PlacedGlyph {
                            id: g.id,
                            x: cx + g.dx,
                            y: -g.dy,
                        });
                        cx += g.adv;
                    }
                    gx += cw[c - s];
                }
                let text: String = chars.iter().map(|&c| p.chars[c].c).collect();
                let face = p.chars[chars[0]].face.clone();
                line.children
                    .push(text_fragment(st, text, face, glyphs, pc, baseline, f.l));
            }
            PieceKind::Generated {
                text,
                st,
                owner,
                glyphs,
                face,
            } => {
                let st = &p.sts[*st];
                let baseline = baseline_of_owner(*owner);
                let mut placed = Vec::new();
                let mut gx = 0.0;
                for g in glyphs {
                    placed.push(PlacedGlyph {
                        id: g.id,
                        x: gx + g.dx,
                        y: -g.dy,
                    });
                    gx += g.adv;
                }
                line.children.push(text_fragment(
                    st,
                    text.clone(),
                    face.clone(),
                    placed,
                    pc,
                    baseline,
                    f.l,
                ));
            }
            PieceKind::Atomic(i) => {
                let Kind::Atomic(a) = p.chars[*i].kind else {
                    continue;
                };
                let atom = &p.atoms[a];
                let Some((_, g, o)) = atomic_pos.iter().find(|x| x.0 == k).copied() else {
                    continue;
                };
                if let Some(frag) = &atom.frag {
                    let mut frag = frag.clone();
                    let top = base_of(g) + o - atom.baseline;
                    frag.translate(pc.x - f.l, top);
                    line.children.push(frag);
                }
            }
            PieceKind::Anchor(item) => {
                if let InlineItem::OutOfFlow(b) = &ic.items[*item] {
                    oofs.push(block::static_position(b, pc.x, y, 0.0));
                }
            }
            PieceKind::Edge { .. } => {}
        }
    }
    Built {
        frag: Some(line),
        height: h,
        oofs,
    }
}

/// True if a piece owned by `owner` is inside inline box `b`.
fn is_inside(p: &Prep, owner: Option<usize>, b: usize) -> bool {
    let mut cur = owner;
    while let Some(x) = cur {
        if x == b {
            return true;
        }
        cur = p.parent[x];
    }
    false
}

fn same_face(a: &Option<Arc<Face>>, b: &Option<Arc<Face>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a.id == b.id,
        (None, None) => true,
        _ => false,
    }
}

/// A text fragment for piece `pc` of style `st` with its baseline at
/// `baseline` (line box coordinates).
fn text_fragment(
    st: &St,
    text: String,
    face: Option<Arc<Face>>,
    glyphs: Vec<PlacedGlyph>,
    pc: &Piece,
    baseline: f64,
    line_x: f64,
) -> Fragment {
    let m = &st.m;
    let mut frag = Fragment::new(
        FragmentKind::Text(TextFragment {
            text,
            face,
            font_size: m.size,
            baseline: m.ascent,
            glyphs,
            level: pc.level,
            ascent: m.ascent,
            descent: m.descent,
        }),
        st.style.clone(),
    );
    frag.rect = Rect::new(
        pc.x - line_x,
        baseline - m.ascent,
        pc.width,
        m.ascent + m.descent,
    );
    frag
}

/// Lays out inline content into line boxes from `place.y` down, around
/// and among the floats of the formatting context.
pub(crate) fn layout(
    ctx: &mut Ctx,
    ic: &InlineContent,
    style: &Rc<ComputedStyle>,
    first_line: Option<&Rc<ComputedStyle>>,
    place: Place,
    floats: &mut FloatContext,
) -> Laid {
    let cb = Cb {
        width: place.width,
        height: place.cb_height,
        rtl: style.rtl(),
    };
    let mode = Mode::Layout(cb);
    let p = prepare(ctx, ic, style, None, &mode);
    // `::first-line` (css-pseudo-4 §2.3): the first line is broken and
    // laid out with the container's style replaced; the text must keep
    // its characters for the two to agree.
    let p_first = first_line
        .map(|fl| prepare(ctx, ic, style, Some(fl), &mode))
        .filter(|q| q.chars.len() == p.chars.len());
    let mut area = Area {
        x0: place.x,
        x1: place.x + place.width,
        cb,
        single_line: place.single_line,
        floats,
        placed: HashSet::new(),
        deferred: Vec::new(),
        others: Vec::new(),
    };
    let n = p.chars.len();
    let indent_spec = text_indent(style, Some(place.width));
    let mut lines = Vec::new();
    let mut y = place.y;
    let (mut ci, mut ei) = (0usize, 0usize);
    let mut line_no = 0usize;
    let mut after_forced = false;
    while ci < n || ei < p.events.len() {
        let q = match (&p_first, line_no) {
            (Some(pf), 0) => pf,
            _ => &p,
        };
        area.place_deferred(ctx, ic, y);
        let (sa, sd) = half_leading(&q.sts[0].style, &q.sts[0].m);
        let band = sa + sd;
        let indent = {
            let (len, hanging, each) = indent_spec;
            let mut on = line_no == 0 || (each && after_forced);
            if hanging {
                on = !on;
            }
            if on { len } else { 0.0 }
        };
        let mut ly = y;
        let mut retry = true;
        let filled = loop {
            match fill(ctx, ic, q, ci, ei, ly, band, indent, retry, &mut area) {
                Fill::Done(f) => break f,
                Fill::Lower(needed) => {
                    let (ny, _, _) = area.floats.find_space(ly, needed, band, area.x0, area.x1);
                    if ny > ly + EPS {
                        ly = ny;
                    } else {
                        retry = false;
                    }
                }
            }
        };
        let last = filled.end >= n && filled.ev_end >= p.events.len();
        let built = build_line(ctx, ic, q, &filled, ly, indent, style, last, &area);
        area.others.extend(built.oofs);
        if let Some(frag) = built.frag {
            lines.push(frag);
            y = ly + built.height;
            line_no += 1;
        }
        after_forced = filled.forced;
        if filled.end == ci && filled.ev_end == ei {
            // No progress is impossible by construction; stop rather
            // than loop.
            break;
        }
        ci = filled.end;
        ei = filled.ev_end;
    }
    area.place_deferred(ctx, ic, y);
    Laid {
        lines,
        others: area.others,
        height: y - place.y,
    }
}
