//! Glyph outlines: closed contours of lines, quadratic (TrueType) and
//! cubic (CFF) Bézier segments.

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub fn new(x: f32, y: f32) -> Point {
        Point { x, y }
    }

    pub fn lerp(self, o: Point, t: f32) -> Point {
        Point::new(self.x + (o.x - self.x) * t, self.y + (o.y - self.y) * t)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Segment {
    MoveTo(Point),
    LineTo(Point),
    QuadTo(Point, Point),
    CubicTo(Point, Point, Point),
    Close,
}

/// An affine transform `[a b c d e f]`: x' = a·x + c·y + e, y' = b·x + d·y + f.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Transform {
    pub const IDENTITY: Transform = Transform {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub fn apply(&self, p: Point) -> Point {
        Point::new(
            self.a * p.x + self.c * p.y + self.e,
            self.b * p.x + self.d * p.y + self.f,
        )
    }
}

/// A glyph outline in font units, y pointing up.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outline {
    pub segments: Vec<Segment>,
}

impl Outline {
    pub fn new() -> Outline {
        Outline::default()
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn move_to(&mut self, x: f32, y: f32) {
        self.segments.push(Segment::MoveTo(Point::new(x, y)));
    }

    pub fn line_to(&mut self, x: f32, y: f32) {
        self.segments.push(Segment::LineTo(Point::new(x, y)));
    }

    pub fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.segments
            .push(Segment::QuadTo(Point::new(x1, y1), Point::new(x, y)));
    }

    pub fn cubic_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.segments.push(Segment::CubicTo(
            Point::new(x1, y1),
            Point::new(x2, y2),
            Point::new(x, y),
        ));
    }

    pub fn close(&mut self) {
        self.segments.push(Segment::Close);
    }

    /// Number of contours (one per `MoveTo`).
    pub fn contours(&self) -> usize {
        self.segments
            .iter()
            .filter(|s| matches!(s, Segment::MoveTo(_)))
            .count()
    }

    /// Appends `other` transformed by `t`.
    pub fn append(&mut self, other: &Outline, t: &Transform) {
        for s in &other.segments {
            self.segments.push(match *s {
                Segment::MoveTo(p) => Segment::MoveTo(t.apply(p)),
                Segment::LineTo(p) => Segment::LineTo(t.apply(p)),
                Segment::QuadTo(p, q) => Segment::QuadTo(t.apply(p), t.apply(q)),
                Segment::CubicTo(p, q, r) => Segment::CubicTo(t.apply(p), t.apply(q), t.apply(r)),
                Segment::Close => Segment::Close,
            });
        }
    }

    pub fn transformed(&self, t: &Transform) -> Outline {
        let mut o = Outline::new();
        o.append(self, t);
        o
    }

    /// Bounds of all points, control points included: (xmin, ymin, xmax, ymax).
    pub fn control_bounds(&self) -> Option<(f32, f32, f32, f32)> {
        let mut b: Option<(f32, f32, f32, f32)> = None;
        let mut add = |p: Point| {
            b = Some(match b {
                None => (p.x, p.y, p.x, p.y),
                Some((x0, y0, x1, y1)) => (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y)),
            });
        };
        for s in &self.segments {
            match *s {
                Segment::MoveTo(p) | Segment::LineTo(p) => add(p),
                Segment::QuadTo(p, q) => {
                    add(p);
                    add(q);
                }
                Segment::CubicTo(p, q, r) => {
                    add(p);
                    add(q);
                    add(r);
                }
                Segment::Close => {}
            }
        }
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_and_transform() {
        let mut o = Outline::new();
        o.move_to(0.0, 0.0);
        o.line_to(10.0, 0.0);
        o.quad_to(10.0, 20.0, 0.0, 10.0);
        o.close();
        assert_eq!(o.control_bounds(), Some((0.0, 0.0, 10.0, 20.0)));
        let t = Transform {
            e: 5.0,
            ..Transform::IDENTITY
        };
        assert_eq!(
            o.transformed(&t).control_bounds(),
            Some((5.0, 0.0, 15.0, 20.0))
        );
        assert_eq!(o.contours(), 1);
    }
}
