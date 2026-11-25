//! The font style matching algorithm of CSS Fonts 4 §5.2: among the faces
//! of one family, narrow by `font-stretch`, then `font-style`, then
//! `font-weight`. Face descriptors are ranges, as `@font-face` allows
//! (system faces have single values).

/// `font-style` of a request.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Style {
    Normal,
    Italic,
    /// An oblique angle in degrees (`oblique` alone is 14).
    Oblique(f32),
}

/// `font-style` descriptor of a face.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StyleRange {
    Normal,
    Italic,
    Oblique(f32, f32),
}

impl StyleRange {
    /// The face as an angle range: normal is 0°, italic is treated as
    /// 14° oblique only for ordering against oblique requests.
    fn kind(&self) -> u8 {
        match self {
            StyleRange::Normal => 0,
            StyleRange::Italic => 1,
            StyleRange::Oblique(..) => 2,
        }
    }
}

/// What a face offers.
#[derive(Clone, Debug, PartialEq)]
pub struct Descriptor {
    pub weight: (f32, f32),
    /// Percentages.
    pub stretch: (f32, f32),
    pub style: StyleRange,
}

/// What text asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Request {
    pub weight: f32,
    pub stretch: f32,
    pub style: Style,
}

impl Default for Request {
    fn default() -> Request {
        Request {
            weight: 400.0,
            stretch: 100.0,
            style: Style::Normal,
        }
    }
}

/// The ordering key of a range for a desired value: 0 if the range holds
/// it, otherwise the rank given by `prefer_lower` and the distance.
fn range_key(want: f32, (lo, hi): (f32, f32), prefer_lower: bool) -> (u8, f32) {
    if want >= lo && want <= hi {
        return (0, 0.0);
    }
    let below = hi < want;
    let dist = if below { want - hi } else { lo - want };
    if below == prefer_lower {
        (1, dist)
    } else {
        (2, dist)
    }
}

fn stretch_key(want: f32, d: &Descriptor) -> (u8, f32) {
    // §5.2 step 4.a: at or under 100% narrower widths first, else wider.
    range_key(want, d.stretch, want <= 100.0)
}

fn weight_key(want: f32, d: &Descriptor) -> (u8, f32) {
    let (lo, hi) = d.weight;
    if want >= lo && want <= hi {
        return (0, 0.0);
    }
    if (400.0..=500.0).contains(&want) {
        // Heavier up to 500 first, then lighter descending, then heavier.
        if lo > want && lo <= 500.0 {
            return (1, lo - want);
        }
        if hi < want {
            return (2, want - hi);
        }
        return (3, lo - want);
    }
    range_key(want, d.weight, want < 400.0)
}

fn style_key(want: Style, d: &Descriptor) -> (u8, f32) {
    let kind = d.style.kind();
    match want {
        Style::Italic => match d.style {
            StyleRange::Italic => (0, 0.0),
            StyleRange::Oblique(lo, hi) => (1, range_key(14.0, (lo, hi), false).1),
            StyleRange::Normal => (3, 0.0),
        },
        Style::Oblique(a) => match d.style {
            StyleRange::Oblique(lo, hi) => {
                let (rank, dist) = range_key(a, (lo, hi), a < 0.0);
                (rank, dist)
            }
            StyleRange::Italic => (3, 0.0),
            StyleRange::Normal => (4, 0.0),
        },
        Style::Normal => match kind {
            0 => (0, 0.0),
            2 => match d.style {
                StyleRange::Oblique(lo, hi) => (1, range_key(0.0, (lo, hi), false).1),
                _ => (1, 0.0),
            },
            _ => (3, 0.0),
        },
    }
}

fn better(a: (u8, f32), b: (u8, f32)) -> bool {
    a.0 < b.0 || (a.0 == b.0 && a.1 < b.1)
}

/// Narrows `faces` to the best ones for `req`; returns their indices (all
/// tie on every descriptor, so any of them, or a composite of them by
/// `unicode-range`, serves).
pub fn best_faces(faces: &[Descriptor], req: &Request) -> Vec<usize> {
    let mut set: Vec<usize> = (0..faces.len()).collect();
    let steps: [&dyn Fn(&Descriptor) -> (u8, f32); 3] = [
        &|d| stretch_key(req.stretch, d),
        &|d| style_key(req.style, d),
        &|d| weight_key(req.weight, d),
    ];
    for key in steps {
        let Some(best) = set
            .iter()
            .map(|&i| key(&faces[i]))
            .reduce(|a, b| if better(b, a) { b } else { a })
        else {
            return set;
        };
        set.retain(|&i| key(&faces[i]) == best);
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(w: f32, style: StyleRange) -> Descriptor {
        Descriptor {
            weight: (w, w),
            stretch: (100.0, 100.0),
            style,
        }
    }

    #[test]
    fn weight_order_follows_css_fonts_4() {
        let faces = [
            face(300.0, StyleRange::Normal),
            face(500.0, StyleRange::Normal),
            face(700.0, StyleRange::Normal),
        ];
        let pick = |w| {
            best_faces(
                &faces,
                &Request {
                    weight: w,
                    ..Request::default()
                },
            )
        };
        assert_eq!(pick(400.0), vec![1]);
        assert_eq!(pick(450.0), vec![1]);
        assert_eq!(pick(600.0), vec![2]);
        assert_eq!(pick(200.0), vec![0]);
        assert_eq!(pick(900.0), vec![2]);
    }

    #[test]
    fn italic_prefers_italic_then_oblique() {
        let faces = [
            face(400.0, StyleRange::Normal),
            face(400.0, StyleRange::Oblique(10.0, 20.0)),
        ];
        let r = Request {
            style: Style::Italic,
            ..Request::default()
        };
        assert_eq!(best_faces(&faces, &r), vec![1]);
        let normal = best_faces(&faces, &Request::default());
        assert_eq!(normal, vec![0]);
    }

    #[test]
    fn stretch_narrows_before_weight() {
        let mut condensed = face(400.0, StyleRange::Normal);
        condensed.stretch = (75.0, 75.0);
        let faces = [condensed, face(700.0, StyleRange::Normal)];
        let r = Request {
            stretch: 75.0,
            weight: 700.0,
            ..Request::default()
        };
        assert_eq!(best_faces(&faces, &r), vec![0]);
    }
}
