//! Integer device-pixel geometry.

/// A point in device pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const ORIGIN: Point = Point { x: 0, y: 0 };

    pub const fn new(x: i32, y: i32) -> Point {
        Point { x, y }
    }
}

/// A size in device pixels. Width and height are non-negative by construction.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

impl Size {
    pub const fn new(width: u32, height: u32) -> Size {
        Size { width, height }
    }

    /// Number of pixels (`width * height`) as a `usize`.
    pub const fn area(self) -> usize {
        self.width as usize * self.height as usize
    }

    /// Whether either dimension is zero.
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// An axis-aligned rectangle in device pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    pub origin: Point,
    pub size: Size,
}

impl Rect {
    pub const fn new(origin: Point, size: Size) -> Rect {
        Rect { origin, size }
    }

    pub const fn from_size(size: Size) -> Rect {
        Rect {
            origin: Point::ORIGIN,
            size,
        }
    }

    pub const fn left(self) -> i32 {
        self.origin.x
    }
    pub const fn top(self) -> i32 {
        self.origin.y
    }
    pub const fn right(self) -> i32 {
        self.origin.x + self.size.width as i32
    }
    pub const fn bottom(self) -> i32 {
        self.origin.y + self.size.height as i32
    }

    /// Whether `p` lies inside this rect (right/bottom edges exclusive).
    pub fn contains(self, p: Point) -> bool {
        p.x >= self.left() && p.x < self.right() && p.y >= self.top() && p.y < self.bottom()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn area_and_emptiness() {
        assert_eq!(Size::new(4, 3).area(), 12);
        assert!(Size::new(0, 5).is_empty());
        assert!(!Size::new(1, 1).is_empty());
    }

    #[test]
    fn rect_edges_and_contains() {
        let r = Rect::new(Point::new(10, 20), Size::new(100, 50));
        assert_eq!(
            (r.left(), r.top(), r.right(), r.bottom()),
            (10, 20, 110, 70)
        );
        assert!(r.contains(Point::new(10, 20)));
        assert!(r.contains(Point::new(109, 69)));
        assert!(!r.contains(Point::new(110, 20))); // right edge exclusive
        assert!(!r.contains(Point::new(9, 20)));
    }
}
