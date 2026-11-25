//! OpenType layout: the common tables (script, feature and lookup lists,
//! coverage, class definitions), `GDEF`, glyph substitution (`GSUB`
//! types 1–8) and glyph positioning (`GPOS` types 1–9).
//!
//! Lookups run over a [`Buffer`] of glyphs; positions are in font units.

use super::read::{Reader, Tag, i16_at, u16_at, u32_at};

/// Nesting limit of contextual lookups.
const MAX_NESTING: u32 = 8;
/// Bound on the buffer growth that multiple substitutions may cause.
const MAX_LEN_FACTOR: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct GlyphInfo {
    pub glyph: u16,
    /// Index of the first character of the cluster in the shaped text.
    pub cluster: u32,
    /// GDEF glyph class: 0 unknown, 1 base, 2 ligature, 3 mark, 4 component.
    pub class: u8,
    /// GDEF mark attachment class.
    pub mark_class: u16,
    /// Ligature id of the ligature this glyph belongs to (0 = none) and the
    /// component index a mark attaches to.
    pub lig_id: u16,
    pub lig_comp: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct GlyphPos {
    pub x_advance: i32,
    pub y_advance: i32,
    pub x_offset: i32,
    pub y_offset: i32,
    /// Index of the glyph this one is attached to (marks, cursive), if any.
    pub attach_to: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct Buffer {
    pub info: Vec<GlyphInfo>,
    pub pos: Vec<GlyphPos>,
    next_lig_id: u16,
}

impl Buffer {
    pub fn new(info: Vec<GlyphInfo>) -> Buffer {
        let pos = vec![GlyphPos::default(); info.len()];
        Buffer {
            info,
            pos,
            next_lig_id: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.info.len()
    }

    pub fn is_empty(&self) -> bool {
        self.info.is_empty()
    }
}

pub fn coverage(d: &[u8], off: usize, glyph: u16) -> Option<u16> {
    let mut r = Reader::at(d, off);
    match r.u16()? {
        1 => {
            let n = r.u16()? as usize;
            let (mut lo, mut hi) = (0usize, n);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let g = u16_at(d, off + 4 + mid * 2)?;
                if g == glyph {
                    return Some(mid as u16);
                }
                if g < glyph {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            None
        }
        2 => {
            let n = r.u16()? as usize;
            let (mut lo, mut hi) = (0usize, n);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let rec = off + 4 + mid * 6;
                let start = u16_at(d, rec)?;
                let end = u16_at(d, rec + 2)?;
                if glyph < start {
                    hi = mid;
                } else if glyph > end {
                    lo = mid + 1;
                } else {
                    let base = u16_at(d, rec + 4)?;
                    return Some(base.wrapping_add(glyph - start));
                }
            }
            None
        }
        _ => None,
    }
}

pub fn class_def(d: &[u8], off: usize, glyph: u16) -> u16 {
    (|| {
        let mut r = Reader::at(d, off);
        match r.u16()? {
            1 => {
                let start = r.u16()?;
                let n = r.u16()?;
                if glyph >= start && glyph - start < n {
                    u16_at(d, off + 6 + 2 * (glyph - start) as usize)
                } else {
                    Some(0)
                }
            }
            2 => {
                let n = r.u16()? as usize;
                let (mut lo, mut hi) = (0usize, n);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    let rec = off + 4 + mid * 6;
                    let start = u16_at(d, rec)?;
                    let end = u16_at(d, rec + 2)?;
                    if glyph < start {
                        hi = mid;
                    } else if glyph > end {
                        lo = mid + 1;
                    } else {
                        return u16_at(d, rec + 4);
                    }
                }
                Some(0)
            }
            _ => Some(0),
        }
    })()
    .unwrap_or(0)
}

/// The glyph definition table.
#[derive(Clone, Copy, Debug, Default)]
pub struct Gdef<'a> {
    data: &'a [u8],
    glyph_class: usize,
    mark_attach: usize,
    mark_sets: usize,
}

impl<'a> Gdef<'a> {
    pub fn parse(d: &'a [u8]) -> Gdef<'a> {
        let minor = u16_at(d, 2).unwrap_or(0);
        Gdef {
            data: d,
            glyph_class: u16_at(d, 4).unwrap_or(0) as usize,
            mark_attach: u16_at(d, 10).unwrap_or(0) as usize,
            mark_sets: if minor >= 2 {
                u16_at(d, 12).unwrap_or(0) as usize
            } else {
                0
            },
        }
    }

    pub fn has_classes(&self) -> bool {
        self.glyph_class != 0
    }

    pub fn glyph_class(&self, g: u16) -> u8 {
        if self.glyph_class == 0 {
            return 0;
        }
        class_def(self.data, self.glyph_class, g) as u8
    }

    pub fn mark_class(&self, g: u16) -> u16 {
        if self.mark_attach == 0 {
            return 0;
        }
        class_def(self.data, self.mark_attach, g)
    }

    fn in_mark_set(&self, set: u16, g: u16) -> bool {
        if self.mark_sets == 0 {
            return false;
        }
        let d = self.data;
        let base = self.mark_sets;
        let count = u16_at(d, base + 2).unwrap_or(0);
        if set >= count {
            return false;
        }
        let Some(off) = u32_at(d, base + 4 + 4 * set as usize) else {
            return false;
        };
        coverage(d, base + off as usize, g).is_some()
    }

    pub fn classify(&self, info: &mut GlyphInfo) {
        info.class = self.glyph_class(info.glyph);
        info.mark_class = self.mark_class(info.glyph);
    }
}

/// Selects the lookups of `features` (with their values) for a script and
/// language; the result is sorted by lookup index, as lookups apply in
/// lookup list order.
pub fn select_lookups(
    d: &[u8],
    script: Tag,
    lang: Option<Tag>,
    features: &[(Tag, u32)],
) -> Vec<(u16, u32)> {
    (|| {
        let scripts = u16_at(d, 4)? as usize;
        let feats = u16_at(d, 6)? as usize;
        let nscripts = u16_at(d, scripts)? as usize;
        let mut script_off = None;
        for want in [
            script,
            Tag::new(b"DFLT"),
            Tag::new(b"dflt"),
            Tag::new(b"latn"),
        ] {
            for i in 0..nscripts {
                let rec = scripts + 2 + 6 * i;
                if u32_at(d, rec)? == want.0 {
                    script_off = Some(scripts + u16_at(d, rec + 4)? as usize);
                    break;
                }
            }
            if script_off.is_some() {
                break;
            }
        }
        let s = script_off?;
        let mut langsys = u16_at(d, s)? as usize;
        if let Some(lang) = lang {
            let n = u16_at(d, s + 2)? as usize;
            for i in 0..n {
                let rec = s + 4 + 6 * i;
                if u32_at(d, rec)? == lang.0 {
                    langsys = u16_at(d, rec + 4)? as usize;
                }
            }
        }
        if langsys == 0 {
            return Some(Vec::new());
        }
        let ls = s + langsys;
        let required = u16_at(d, ls + 2)?;
        let n = u16_at(d, ls + 4)? as usize;
        let nfeats = u16_at(d, feats)? as usize;
        let mut out: Vec<(u16, u32)> = Vec::new();
        let add_feature = |fi: u16, value: u32, out: &mut Vec<(u16, u32)>| -> Option<()> {
            if fi as usize >= nfeats {
                return Some(());
            }
            let rec = feats + 2 + 6 * fi as usize;
            let f = feats + u16_at(d, rec + 4)? as usize;
            let nl = u16_at(d, f + 2)? as usize;
            for k in 0..nl {
                let li = u16_at(d, f + 4 + 2 * k)?;
                if let Some(e) = out.iter_mut().find(|e| e.0 == li) {
                    e.1 = e.1.max(value);
                } else {
                    out.push((li, value));
                }
            }
            Some(())
        };
        if required != 0xffff {
            add_feature(required, 1, &mut out)?;
        }
        for k in 0..n {
            let fi = u16_at(d, ls + 6 + 2 * k)?;
            let tag = u32_at(d, feats + 2 + 6 * fi as usize)?;
            if let Some(&(_, value)) = features.iter().rev().find(|(t, _)| t.0 == tag) {
                if value != 0 {
                    add_feature(fi, value, &mut out)?;
                }
            }
        }
        out.sort_by_key(|e| e.0);
        Some(out)
    })()
    .unwrap_or_default()
}

/// Lists the feature tags a table offers (for `check()`-like queries).
pub fn feature_tags(d: &[u8]) -> Vec<Tag> {
    let mut out = Vec::new();
    if let Some(feats) = u16_at(d, 6) {
        let n = u16_at(d, feats as usize).unwrap_or(0) as usize;
        for i in 0..n {
            if let Some(t) = u32_at(d, feats as usize + 2 + 6 * i) {
                if !out.contains(&Tag(t)) {
                    out.push(Tag(t));
                }
            }
        }
    }
    out
}

const IGNORE_BASE: u16 = 2;
const IGNORE_LIGATURES: u16 = 4;
const IGNORE_MARKS: u16 = 8;
const USE_MARK_FILTERING_SET: u16 = 0x10;

#[derive(Clone, Copy)]
struct Lookup {
    kind: u16,
    flag: u16,
    mark_set: u16,
    /// Position of the lookup table and its subtable count.
    pos: usize,
    count: usize,
}

/// Runs lookups of one table (GSUB or GPOS) over a buffer.
pub struct Applier<'a, 'b> {
    d: &'a [u8],
    gdef: Gdef<'a>,
    is_gsub: bool,
    lookups: usize,
    pub buf: &'b mut Buffer,
    /// Value of the feature that selected the current lookup.
    value: u32,
    /// Upper bound on the buffer length (multiple substitutions).
    budget: usize,
}

impl<'a, 'b> Applier<'a, 'b> {
    pub fn new(d: &'a [u8], gdef: Gdef<'a>, is_gsub: bool, buf: &'b mut Buffer) -> Applier<'a, 'b> {
        let lookups = u16_at(d, 8).unwrap_or(0) as usize;
        let budget = buf.len().max(1) * MAX_LEN_FACTOR + 64;
        Applier {
            d,
            gdef,
            is_gsub,
            lookups,
            buf,
            value: 1,
            budget,
        }
    }

    fn lookup(&self, index: u16) -> Option<Lookup> {
        let d = self.d;
        if self.lookups == 0 {
            return None;
        }
        let n = u16_at(d, self.lookups)?;
        if index >= n {
            return None;
        }
        let pos = self.lookups + u16_at(d, self.lookups + 2 + 2 * index as usize)? as usize;
        let kind = u16_at(d, pos)?;
        let flag = u16_at(d, pos + 2)?;
        let count = u16_at(d, pos + 4)? as usize;
        let mark_set = if flag & USE_MARK_FILTERING_SET != 0 {
            u16_at(d, pos + 6 + 2 * count)?
        } else {
            0
        };
        Some(Lookup {
            kind,
            flag,
            mark_set,
            pos,
            count,
        })
    }

    /// Resolves an extension subtable to (type, position).
    fn subtable(&self, l: &Lookup, i: usize) -> Option<(u16, usize)> {
        let d = self.d;
        let st = l.pos + u16_at(d, l.pos + 6 + 2 * i)? as usize;
        let ext = if self.is_gsub { 7 } else { 9 };
        if l.kind == ext {
            let kind = u16_at(d, st + 2)?;
            let off = u32_at(d, st + 4)? as usize;
            return Some((kind, st + off));
        }
        Some((l.kind, st))
    }

    /// Applies a lookup over the whole buffer.
    pub fn apply_lookup(&mut self, index: u16, value: u32) {
        let Some(l) = self.lookup(index) else {
            return;
        };
        self.value = value;
        let reverse = self.is_gsub && self.subtable(&l, 0).map(|s| s.0) == Some(8);
        if reverse {
            let mut i = self.buf.len();
            while i > 0 {
                i -= 1;
                if !self.skip(&l, i) {
                    self.apply_at(&l, i, 0);
                }
            }
            return;
        }
        let mut i = 0;
        while i < self.buf.len() {
            if self.skip(&l, i) {
                i += 1;
                continue;
            }
            match self.apply_at(&l, i, 0) {
                Some(next) => i = next.max(i + 1),
                None => i += 1,
            }
        }
    }

    fn skip(&self, l: &Lookup, i: usize) -> bool {
        let info = &self.buf.info[i];
        match info.class {
            1 => l.flag & IGNORE_BASE != 0,
            2 => l.flag & IGNORE_LIGATURES != 0,
            3 => {
                if l.flag & IGNORE_MARKS != 0 {
                    return true;
                }
                if l.flag & USE_MARK_FILTERING_SET != 0 {
                    return !self.gdef.in_mark_set(l.mark_set, info.glyph);
                }
                let attach = l.flag >> 8;
                attach != 0 && info.mark_class != attach
            }
            _ => false,
        }
    }

    fn next_index(&self, l: &Lookup, mut i: usize) -> Option<usize> {
        loop {
            i += 1;
            if i >= self.buf.len() {
                return None;
            }
            if !self.skip(l, i) {
                return Some(i);
            }
        }
    }

    fn prev_index(&self, l: &Lookup, mut i: usize) -> Option<usize> {
        loop {
            if i == 0 {
                return None;
            }
            i -= 1;
            if !self.skip(l, i) {
                return Some(i);
            }
        }
    }

    /// Tries each subtable at glyph `i`; returns the index after the
    /// glyphs consumed when one applies.
    fn apply_at(&mut self, l: &Lookup, i: usize, depth: u32) -> Option<usize> {
        for s in 0..l.count {
            let Some((kind, st)) = self.subtable(l, s) else {
                continue;
            };
            let r = if self.is_gsub {
                self.gsub(l, kind, st, i, depth)
            } else {
                self.gpos(l, kind, st, i, depth)
            };
            if r.is_some() {
                return r;
            }
        }
        None
    }

    fn set_glyph(&mut self, i: usize, g: u16) {
        let info = &mut self.buf.info[i];
        info.glyph = g;
        let class = self.gdef.glyph_class(g);
        if self.gdef.has_classes() {
            info.class = class;
        }
        info.mark_class = self.gdef.mark_class(g);
    }

    fn gsub(&mut self, l: &Lookup, kind: u16, st: usize, i: usize, depth: u32) -> Option<usize> {
        let d = self.d;
        let g = self.buf.info[i].glyph;
        let format = u16_at(d, st)?;
        match kind {
            1 => {
                let cov = coverage(d, st + u16_at(d, st + 2)? as usize, g)?;
                let new = if format == 1 {
                    g.wrapping_add(u16_at(d, st + 4)?)
                } else {
                    let n = u16_at(d, st + 4)?;
                    if cov >= n {
                        return None;
                    }
                    u16_at(d, st + 6 + 2 * cov as usize)?
                };
                self.set_glyph(i, new);
                Some(i + 1)
            }
            2 => {
                let cov = coverage(d, st + u16_at(d, st + 2)? as usize, g)?;
                let n = u16_at(d, st + 4)?;
                if cov >= n {
                    return None;
                }
                let seq = st + u16_at(d, st + 6 + 2 * cov as usize)? as usize;
                let count = u16_at(d, seq)? as usize;
                if count == 0 {
                    self.buf.info.remove(i);
                    self.buf.pos.remove(i);
                    return Some(i);
                }
                if self.buf.len() + count > self.budget {
                    return None;
                }
                let template = self.buf.info[i];
                let first = u16_at(d, seq + 2)?;
                self.set_glyph(i, first);
                for k in 1..count {
                    let mut info = template;
                    info.glyph = u16_at(d, seq + 2 + 2 * k)?;
                    self.gdef.classify(&mut info);
                    if !self.gdef.has_classes() {
                        info.class = template.class;
                    }
                    self.buf.info.insert(i + k, info);
                    self.buf.pos.insert(i + k, GlyphPos::default());
                }
                Some(i + count)
            }
            3 => {
                let cov = coverage(d, st + u16_at(d, st + 2)? as usize, g)?;
                let n = u16_at(d, st + 4)?;
                if cov >= n {
                    return None;
                }
                let set = st + u16_at(d, st + 6 + 2 * cov as usize)? as usize;
                let count = u16_at(d, set)? as u32;
                let pick = self.value.max(1) - 1;
                if pick >= count {
                    return None;
                }
                let new = u16_at(d, set + 2 + 2 * pick as usize)?;
                self.set_glyph(i, new);
                Some(i + 1)
            }
            4 => {
                let cov = coverage(d, st + u16_at(d, st + 2)? as usize, g)?;
                let n = u16_at(d, st + 4)?;
                if cov >= n {
                    return None;
                }
                let set = st + u16_at(d, st + 6 + 2 * cov as usize)? as usize;
                let nlig = u16_at(d, set)? as usize;
                'lig: for k in 0..nlig {
                    let lig = set + u16_at(d, set + 2 + 2 * k)? as usize;
                    let lig_glyph = u16_at(d, lig)?;
                    let comps = u16_at(d, lig + 2)? as usize;
                    if comps == 0 {
                        continue;
                    }
                    let mut positions = vec![i];
                    let mut j = i;
                    for c in 1..comps {
                        let Some(nj) = self.next_index(l, j) else {
                            continue 'lig;
                        };
                        if self.buf.info[nj].glyph != u16_at(d, lig + 4 + 2 * (c - 1))? {
                            continue 'lig;
                        }
                        positions.push(nj);
                        j = nj;
                    }
                    self.ligate(&positions, lig_glyph);
                    return Some(i + 1);
                }
                None
            }
            5 | 6 => {
                let (positions, records) = self.match_context(l, kind == 6, format, st, i)?;
                Some(self.apply_records(&positions, &records, depth))
            }
            8 => {
                let cov = coverage(d, st + u16_at(d, st + 2)? as usize, g)?;
                let mut p = st + 4;
                let nb = u16_at(d, p)? as usize;
                let mut j = i;
                for k in 0..nb {
                    j = self.prev_index(l, j)?;
                    let c = st + u16_at(d, p + 2 + 2 * k)? as usize;
                    coverage(d, c, self.buf.info[j].glyph)?;
                }
                p += 2 + 2 * nb;
                let nl = u16_at(d, p)? as usize;
                let mut j = i;
                for k in 0..nl {
                    j = self.next_index(l, j)?;
                    let c = st + u16_at(d, p + 2 + 2 * k)? as usize;
                    coverage(d, c, self.buf.info[j].glyph)?;
                }
                p += 2 + 2 * nl;
                let n = u16_at(d, p)?;
                if cov >= n {
                    return None;
                }
                let new = u16_at(d, p + 2 + 2 * cov as usize)?;
                self.set_glyph(i, new);
                Some(i + 1)
            }
            _ => None,
        }
    }

    /// Replaces the glyphs at `positions` with one ligature glyph at the
    /// first; skipped glyphs in between (marks) stay and learn which
    /// component they follow.
    fn ligate(&mut self, positions: &[usize], lig_glyph: u16) {
        let first = positions[0];
        self.buf.next_lig_id = self.buf.next_lig_id.wrapping_add(1).max(1);
        let id = self.buf.next_lig_id;
        let cluster = positions
            .iter()
            .map(|&p| self.buf.info[p].cluster)
            .min()
            .unwrap_or(0);
        self.set_glyph(first, lig_glyph);
        if !self.gdef.has_classes() {
            self.buf.info[first].class = 2;
        }
        self.buf.info[first].cluster = cluster;
        self.buf.info[first].lig_id = id;
        let last = *positions.last().unwrap_or(&first);
        let mut comp = 1u16;
        for p in first + 1..=last {
            if positions.contains(&p) {
                comp += 1;
            } else {
                self.buf.info[p].lig_id = id;
                self.buf.info[p].lig_comp = comp;
                self.buf.info[p].cluster = cluster;
            }
        }
        for &p in positions[1..].iter().rev() {
            self.buf.info.remove(p);
            self.buf.pos.remove(p);
        }
    }

    /// Applies nested lookups of a matched context; returns the index after
    /// the matched input.
    fn apply_records(&mut self, positions: &[usize], records: &[(u16, u16)], depth: u32) -> usize {
        let mut positions = positions.to_vec();
        let mut end = *positions.last().unwrap_or(&0) + 1;
        if depth < MAX_NESTING {
            for &(seq, li) in records {
                let Some(&p) = positions.get(seq as usize) else {
                    continue;
                };
                let Some(nl) = self.lookup(li) else {
                    continue;
                };
                if p >= self.buf.len() || self.skip(&nl, p) {
                    continue;
                }
                let before = self.buf.len();
                let saved = self.value;
                self.apply_at(&nl, p, depth + 1);
                self.value = saved;
                let delta = self.buf.len() as isize - before as isize;
                if delta != 0 {
                    for q in positions.iter_mut() {
                        if *q > p {
                            *q = (*q as isize + delta).max(p as isize) as usize;
                        }
                    }
                    end = (end as isize + delta).max(p as isize + 1) as usize;
                }
            }
        }
        end.min(self.buf.len())
    }

    /// Matches a context (GSUB 5/6, GPOS 7/8) subtable at `i`; returns the
    /// input positions and the sequence lookup records.
    fn match_context(
        &self,
        l: &Lookup,
        chain: bool,
        format: u16,
        st: usize,
        i: usize,
    ) -> Option<(Vec<usize>, Vec<(u16, u16)>)> {
        let d = self.d;
        let g = self.buf.info[i].glyph;
        match format {
            1 | 2 => {
                let cov = coverage(d, st + u16_at(d, st + 2)? as usize, g)?;
                let (set_index, classes, sets_at) = if format == 1 {
                    (cov, None, st + 4)
                } else if chain {
                    let b = u16_at(d, st + 4)? as usize;
                    let ic = u16_at(d, st + 6)? as usize;
                    let la = u16_at(d, st + 8)? as usize;
                    let c = class_def(d, st + ic, g);
                    (c, Some((b, ic, la)), st + 10)
                } else {
                    let ic = u16_at(d, st + 4)? as usize;
                    let c = class_def(d, st + ic, g);
                    (c, Some((0, ic, 0)), st + 6)
                };
                let nsets = u16_at(d, sets_at)?;
                if set_index >= nsets {
                    return None;
                }
                let set_off = u16_at(d, sets_at + 2 + 2 * set_index as usize)? as usize;
                if set_off == 0 {
                    return None;
                }
                let set = st + set_off;
                let nrules = u16_at(d, set)? as usize;
                for k in 0..nrules {
                    let rule = set + u16_at(d, set + 2 + 2 * k)? as usize;
                    let test = |which: u8, value: u16, glyph: u16| -> bool {
                        match classes {
                            None => glyph == value,
                            Some((b, ic, la)) => {
                                let cd = match which {
                                    0 => b,
                                    1 => ic,
                                    _ => la,
                                };
                                cd != 0 && class_def(d, st + cd, glyph) == value
                            }
                        }
                    };
                    let mut r = Reader::at(d, rule);
                    let r2 = if chain {
                        (|| {
                            let nb = r.u16()? as usize;
                            let back: Vec<u16> = (0..nb).map(|_| r.u16()).collect::<Option<_>>()?;
                            let ni = r.u16()? as usize;
                            let input: Vec<u16> =
                                (1..ni).map(|_| r.u16()).collect::<Option<_>>()?;
                            let nl = r.u16()? as usize;
                            let ahead: Vec<u16> =
                                (0..nl).map(|_| r.u16()).collect::<Option<_>>()?;
                            let nr = r.u16()? as usize;
                            let recs: Vec<(u16, u16)> = (0..nr)
                                .map(|_| Some((r.u16()?, r.u16()?)))
                                .collect::<Option<_>>()?;
                            Some((back, input, ahead, recs))
                        })()
                    } else {
                        (|| {
                            let ni = r.u16()? as usize;
                            let nr = r.u16()? as usize;
                            let input: Vec<u16> =
                                (1..ni).map(|_| r.u16()).collect::<Option<_>>()?;
                            let recs: Vec<(u16, u16)> = (0..nr)
                                .map(|_| Some((r.u16()?, r.u16()?)))
                                .collect::<Option<_>>()?;
                            Some((Vec::new(), input, Vec::new(), recs))
                        })()
                    };
                    let Some((back, input, ahead, recs)) = r2 else {
                        continue;
                    };
                    if let Some(pos) =
                        self.match_sequence(l, i, &back, &input, &ahead, &|w, v, gl| test(w, v, gl))
                    {
                        return Some((pos, recs));
                    }
                }
                None
            }
            3 => {
                let mut r = Reader::at(d, st + 2);
                let (back, input, ahead, recs);
                if chain {
                    let nb = r.u16()? as usize;
                    back = (0..nb).map(|_| r.u16()).collect::<Option<Vec<u16>>>()?;
                    let ni = r.u16()? as usize;
                    input = (0..ni).map(|_| r.u16()).collect::<Option<Vec<u16>>>()?;
                    let nl = r.u16()? as usize;
                    ahead = (0..nl).map(|_| r.u16()).collect::<Option<Vec<u16>>>()?;
                    let nr = r.u16()? as usize;
                    recs = (0..nr)
                        .map(|_| Some((r.u16()?, r.u16()?)))
                        .collect::<Option<Vec<_>>>()?;
                } else {
                    let ni = r.u16()? as usize;
                    let nr = r.u16()? as usize;
                    input = (0..ni).map(|_| r.u16()).collect::<Option<Vec<u16>>>()?;
                    recs = (0..nr)
                        .map(|_| Some((r.u16()?, r.u16()?)))
                        .collect::<Option<Vec<_>>>()?;
                    back = Vec::new();
                    ahead = Vec::new();
                }
                let first = *input.first()?;
                coverage(d, st + first as usize, g)?;
                let test = |_: u8, value: u16, glyph: u16| {
                    coverage(d, st + value as usize, glyph).is_some()
                };
                let pos = self.match_sequence(l, i, &back, &input[1..], &ahead, &test)?;
                Some((pos, recs))
            }
            _ => None,
        }
    }

    /// Matches backtrack (closest first), the rest of the input and the
    /// lookahead around `i`, skipping glyphs the lookup ignores.
    fn match_sequence(
        &self,
        l: &Lookup,
        i: usize,
        back: &[u16],
        input: &[u16],
        ahead: &[u16],
        test: &dyn Fn(u8, u16, u16) -> bool,
    ) -> Option<Vec<usize>> {
        let mut positions = vec![i];
        let mut j = i;
        for &v in input {
            j = self.next_index(l, j)?;
            if !test(1, v, self.buf.info[j].glyph) {
                return None;
            }
            positions.push(j);
        }
        let mut k = j;
        for &v in ahead {
            k = self.next_index(l, k)?;
            if !test(2, v, self.buf.info[k].glyph) {
                return None;
            }
        }
        let mut b = i;
        for &v in back {
            b = self.prev_index(l, b)?;
            if !test(0, v, self.buf.info[b].glyph) {
                return None;
            }
        }
        Some(positions)
    }

    fn gpos(&mut self, l: &Lookup, kind: u16, st: usize, i: usize, depth: u32) -> Option<usize> {
        let d = self.d;
        let g = self.buf.info[i].glyph;
        let format = u16_at(d, st)?;
        match kind {
            1 => {
                let cov = coverage(d, st + u16_at(d, st + 2)? as usize, g)?;
                let vf = u16_at(d, st + 4)?;
                let at = if format == 1 {
                    st + 6
                } else {
                    let n = u16_at(d, st + 6)?;
                    if cov >= n {
                        return None;
                    }
                    st + 8 + cov as usize * value_size(vf)
                };
                self.add_value(i, vf, at);
                Some(i + 1)
            }
            2 => {
                let cov = coverage(d, st + u16_at(d, st + 2)? as usize, g)?;
                let vf1 = u16_at(d, st + 4)?;
                let vf2 = u16_at(d, st + 6)?;
                let j = self.next_index(l, i)?;
                let g2 = self.buf.info[j].glyph;
                let (s1, s2) = (value_size(vf1), value_size(vf2));
                let rec_at = if format == 1 {
                    let n = u16_at(d, st + 8)?;
                    if cov >= n {
                        return None;
                    }
                    let set = st + u16_at(d, st + 10 + 2 * cov as usize)? as usize;
                    let count = u16_at(d, set)? as usize;
                    let rec = 2 + s1 + s2;
                    let (mut lo, mut hi) = (0usize, count);
                    let mut found = None;
                    while lo < hi {
                        let mid = (lo + hi) / 2;
                        let at = set + 2 + mid * rec;
                        let second = u16_at(d, at)?;
                        if second == g2 {
                            found = Some(at + 2);
                            break;
                        }
                        if second < g2 {
                            lo = mid + 1;
                        } else {
                            hi = mid;
                        }
                    }
                    found?
                } else {
                    let cd1 = st + u16_at(d, st + 8)? as usize;
                    let cd2 = st + u16_at(d, st + 10)? as usize;
                    let n1 = u16_at(d, st + 12)?;
                    let n2 = u16_at(d, st + 14)?;
                    let c1 = class_def(d, cd1, g);
                    let c2 = class_def(d, cd2, g2);
                    if c1 >= n1 || c2 >= n2 {
                        return None;
                    }
                    st + 16 + (c1 as usize * n2 as usize + c2 as usize) * (s1 + s2)
                };
                self.add_value(i, vf1, rec_at);
                self.add_value(j, vf2, rec_at + s1);
                Some(if vf2 != 0 { j + 1 } else { j })
            }
            3 => {
                let cov = coverage(d, st + u16_at(d, st + 2)? as usize, g)?;
                let j = self.next_index(l, i)?;
                let cov2 = coverage(d, st + u16_at(d, st + 2)? as usize, self.buf.info[j].glyph)?;
                let n = u16_at(d, st + 4)?;
                if cov >= n || cov2 >= n {
                    return None;
                }
                let exit = u16_at(d, st + 6 + 4 * cov as usize + 2)? as usize;
                let entry = u16_at(d, st + 6 + 4 * cov2 as usize)? as usize;
                if exit == 0 || entry == 0 {
                    return None;
                }
                let (ex, ey) = anchor(d, st + exit)?;
                let (nx, ny) = anchor(d, st + entry)?;
                let first = self.buf.pos[i];
                self.buf.pos[i].x_advance = ex + first.x_offset;
                let p = &mut self.buf.pos[j];
                p.x_advance -= nx;
                p.x_offset -= nx;
                p.y_offset = first.y_offset + ey - ny;
                p.attach_to = Some(i as u32);
                Some(j)
            }
            4..=6 => {
                if self.buf.info[i].class != 3 && self.gdef.has_classes() {
                    return None;
                }
                let mark_cov = coverage(d, st + u16_at(d, st + 2)? as usize, g)?;
                // Find the glyph to attach to.
                let target = if kind == 6 {
                    let j = self.prev_index(l, i)?;
                    if self.buf.info[j].class != 3 && self.gdef.has_classes() {
                        return None;
                    }
                    j
                } else {
                    let mut j = i;
                    loop {
                        if j == 0 {
                            return None;
                        }
                        j -= 1;
                        if self.buf.info[j].class != 3 {
                            break;
                        }
                    }
                    j
                };
                let tg = self.buf.info[target].glyph;
                let base_cov = coverage(d, st + u16_at(d, st + 4)? as usize, tg)?;
                let nclass = u16_at(d, st + 6)? as usize;
                let marks = st + u16_at(d, st + 8)? as usize;
                let bases = st + u16_at(d, st + 10)? as usize;
                let nmarks = u16_at(d, marks)?;
                if mark_cov >= nmarks {
                    return None;
                }
                let mrec = marks + 2 + 4 * mark_cov as usize;
                let class = u16_at(d, mrec)? as usize;
                if class >= nclass {
                    return None;
                }
                let (mx, my) = anchor(d, marks + u16_at(d, mrec + 2)? as usize)?;
                let anchor_at = if kind == 5 {
                    let nlig = u16_at(d, bases)?;
                    if base_cov >= nlig {
                        return None;
                    }
                    let attach = bases + u16_at(d, bases + 2 + 2 * base_cov as usize)? as usize;
                    let ncomp = u16_at(d, attach)? as usize;
                    if ncomp == 0 {
                        return None;
                    }
                    let info = self.buf.info[i];
                    let comp = if info.lig_id != 0
                        && info.lig_id == self.buf.info[target].lig_id
                        && info.lig_comp > 0
                    {
                        (info.lig_comp as usize).min(ncomp) - 1
                    } else {
                        ncomp - 1
                    };
                    let off = u16_at(d, attach + 2 + 2 * (comp * nclass + class))? as usize;
                    if off == 0 {
                        return None;
                    }
                    attach + off
                } else {
                    let nbase = u16_at(d, bases)?;
                    if base_cov >= nbase {
                        return None;
                    }
                    let off =
                        u16_at(d, bases + 2 + 2 * (base_cov as usize * nclass + class))? as usize;
                    if off == 0 {
                        return None;
                    }
                    bases + off
                };
                let (bx, by) = anchor(d, anchor_at)?;
                let base = self.buf.pos[target];
                let between: i32 = (target..i).map(|k| self.buf.pos[k].x_advance).sum();
                let p = &mut self.buf.pos[i];
                p.x_offset = base.x_offset + bx - mx - between;
                p.y_offset = base.y_offset + by - my;
                p.attach_to = Some(target as u32);
                Some(i + 1)
            }
            7 | 8 => {
                let (positions, records) = self.match_context(l, kind == 8, format, st, i)?;
                Some(self.apply_records(&positions, &records, depth))
            }
            _ => None,
        }
    }

    fn add_value(&mut self, i: usize, vf: u16, at: usize) {
        let d = self.d;
        let mut p = at;
        let mut field = |bit: u16| -> i32 {
            if vf & bit != 0 {
                let v = i16_at(d, p).unwrap_or(0) as i32;
                p += 2;
                v
            } else {
                0
            }
        };
        let xp = field(1);
        let yp = field(2);
        let xa = field(4);
        let ya = field(8);
        let pos = &mut self.buf.pos[i];
        pos.x_offset += xp;
        pos.y_offset += yp;
        pos.x_advance += xa;
        pos.y_advance += ya;
    }
}

fn value_size(vf: u16) -> usize {
    2 * (vf & 0xff).count_ones() as usize
}

fn anchor(d: &[u8], at: usize) -> Option<(i32, i32)> {
    Some((i16_at(d, at + 2)? as i32, i16_at(d, at + 4)? as i32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(v: &mut Vec<u8>, x: u16) {
        v.extend_from_slice(&x.to_be_bytes());
    }

    #[test]
    fn coverage_formats() {
        let mut c1 = Vec::new();
        for x in [1u16, 3, 5, 9, 12] {
            w(&mut c1, x);
        }
        assert_eq!(coverage(&c1, 0, 9), Some(1));
        assert_eq!(coverage(&c1, 0, 10), None);
        let mut c2 = Vec::new();
        for x in [2u16, 1, 20, 29, 4] {
            w(&mut c2, x);
        }
        assert_eq!(coverage(&c2, 0, 25), Some(9));
        assert_eq!(coverage(&c2, 0, 30), None);
    }

    #[test]
    fn class_def_formats() {
        let mut c = Vec::new();
        for x in [1u16, 10, 3, 7, 8, 9] {
            w(&mut c, x);
        }
        assert_eq!(class_def(&c, 0, 11), 8);
        assert_eq!(class_def(&c, 0, 13), 0);
    }

    /// A GSUB with one `liga` feature whose lookup ligates glyphs 1 2 → 9.
    fn liga_gsub() -> Vec<u8> {
        let mut t = Vec::new();
        // Header: version 1.0, ScriptList at 10, FeatureList, LookupList.
        w(&mut t, 1);
        w(&mut t, 0);
        let fix = t.len();
        w(&mut t, 0);
        w(&mut t, 0);
        w(&mut t, 0);
        // ScriptList: one script 'DFLT' with a default LangSys.
        let sl = t.len();
        w(&mut t, 1);
        t.extend_from_slice(b"DFLT");
        w(&mut t, 8);
        // Script table at sl+8: default LangSys at +4, no lang records.
        w(&mut t, 4);
        w(&mut t, 0);
        // LangSys: lookupOrder, required none, one feature index 0.
        w(&mut t, 0);
        w(&mut t, 0xffff);
        w(&mut t, 1);
        w(&mut t, 0);
        let fl = t.len();
        w(&mut t, 1);
        t.extend_from_slice(b"liga");
        w(&mut t, 8);
        // Feature: params 0, one lookup index 0.
        w(&mut t, 0);
        w(&mut t, 1);
        w(&mut t, 0);
        let ll = t.len();
        w(&mut t, 1);
        w(&mut t, 4);
        // Lookup: type 4, flag 0, one subtable at +8.
        w(&mut t, 4);
        w(&mut t, 0);
        w(&mut t, 1);
        w(&mut t, 8);
        // Ligature subst: format 1, coverage at +8, one set at +14.
        w(&mut t, 1);
        w(&mut t, 8);
        w(&mut t, 1);
        w(&mut t, 14);
        // Coverage format 1: glyph 1.
        w(&mut t, 1);
        w(&mut t, 1);
        w(&mut t, 1);
        // LigatureSet: one ligature at +4.
        w(&mut t, 1);
        w(&mut t, 4);
        // Ligature: glyph 9, 2 components, second is 2.
        w(&mut t, 9);
        w(&mut t, 2);
        w(&mut t, 2);
        t[fix..fix + 2].copy_from_slice(&(sl as u16).to_be_bytes());
        t[fix + 2..fix + 4].copy_from_slice(&(fl as u16).to_be_bytes());
        t[fix + 4..fix + 6].copy_from_slice(&(ll as u16).to_be_bytes());
        t
    }

    #[test]
    fn ligature_substitution() {
        let t = liga_gsub();
        let lookups = select_lookups(&t, Tag::new(b"latn"), None, &[(Tag::new(b"liga"), 1)]);
        assert_eq!(lookups, vec![(0, 1)]);
        let info = [1u16, 2, 3]
            .iter()
            .enumerate()
            .map(|(i, &g)| GlyphInfo {
                glyph: g,
                cluster: i as u32,
                ..GlyphInfo::default()
            })
            .collect();
        let mut buf = Buffer::new(info);
        let mut a = Applier::new(&t, Gdef::default(), true, &mut buf);
        a.apply_lookup(0, 1);
        let glyphs: Vec<u16> = buf.info.iter().map(|g| g.glyph).collect();
        assert_eq!(glyphs, vec![9, 3]);
        assert_eq!(buf.info[1].cluster, 2);
        assert!(select_lookups(&t, Tag::new(b"latn"), None, &[(Tag::new(b"liga"), 0)]).is_empty());
    }
}
