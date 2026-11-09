use wayland_protocols::xdg::shell::server::xdg_positioner::{Anchor, ConstraintAdjustment, Gravity};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= f64::from(self.x)
            && y >= f64::from(self.y)
            && x < f64::from(self.x) + f64::from(self.width)
            && y < f64::from(self.y) + f64::from(self.height)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Start,
    Center,
    End,
}

impl Side {
    fn flipped(self) -> Self {
        match self {
            Side::Start => Side::End,
            Side::Center => Side::Center,
            Side::End => Side::Start,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sides {
    x: Side,
    y: Side,
}

fn anchor_sides(anchor: Anchor) -> Sides {
    let (x, y) = match anchor {
        Anchor::Top => (Side::Center, Side::Start),
        Anchor::Bottom => (Side::Center, Side::End),
        Anchor::Left => (Side::Start, Side::Center),
        Anchor::Right => (Side::End, Side::Center),
        Anchor::TopLeft => (Side::Start, Side::Start),
        Anchor::BottomLeft => (Side::Start, Side::End),
        Anchor::TopRight => (Side::End, Side::Start),
        Anchor::BottomRight => (Side::End, Side::End),
        _ => (Side::Center, Side::Center),
    };
    Sides { x, y }
}

fn gravity_sides(gravity: Gravity) -> Sides {
    let (x, y) = match gravity {
        Gravity::Top => (Side::Center, Side::Start),
        Gravity::Bottom => (Side::Center, Side::End),
        Gravity::Left => (Side::Start, Side::Center),
        Gravity::Right => (Side::End, Side::Center),
        Gravity::TopLeft => (Side::Start, Side::Start),
        Gravity::BottomLeft => (Side::Start, Side::End),
        Gravity::TopRight => (Side::End, Side::Start),
        Gravity::BottomRight => (Side::End, Side::End),
        _ => (Side::Center, Side::Center),
    };
    Sides { x, y }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Positioner {
    pub size: Option<(i32, i32)>,
    pub anchor_rect: Option<Rect>,
    pub anchor: Anchor,
    pub gravity: Gravity,
    pub constraint_adjustment: ConstraintAdjustment,
    pub offset: (i32, i32),
}

impl Default for Positioner {
    fn default() -> Self {
        Positioner {
            size: None,
            anchor_rect: None,
            anchor: Anchor::None,
            gravity: Gravity::None,
            constraint_adjustment: ConstraintAdjustment::empty(),
            offset: (0, 0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Complete {
    size: (i32, i32),
    anchor_rect: Rect,
    anchor: Sides,
    gravity: Sides,
    adjustment: ConstraintAdjustment,
    offset: (i32, i32),
}

impl Positioner {
    pub fn complete(&self) -> Option<Complete> {
        Some(Complete {
            size: self.size?,
            anchor_rect: self.anchor_rect?,
            anchor: anchor_sides(self.anchor),
            gravity: gravity_sides(self.gravity),
            adjustment: self.constraint_adjustment,
            offset: self.offset,
        })
    }
}

fn anchor_point(start: i32, length: i32, side: Side) -> i32 {
    match side {
        Side::Start => start,
        Side::Center => start + length / 2,
        Side::End => start + length,
    }
}

fn place_along(point: i32, length: i32, gravity: Side) -> i32 {
    match gravity {
        Side::Start => point - length,
        Side::Center => point - length / 2,
        Side::End => point,
    }
}

struct Axis {
    anchor_start: i32,
    anchor_length: i32,
    anchor: Side,
    gravity: Side,
    offset: i32,
    length: i32,
    bounds_start: i32,
    bounds_length: i32,
    flip: bool,
    slide: bool,
    resize: bool,
}

impl Axis {
    fn position(&self, anchor: Side, gravity: Side, offset: i32) -> i32 {
        place_along(anchor_point(self.anchor_start, self.anchor_length, anchor), self.length, gravity) + offset
    }

    fn fits(&self, start: i32, length: i32) -> bool {
        start >= self.bounds_start && start + length <= self.bounds_start + self.bounds_length
    }

    fn solve(&self) -> (i32, i32) {
        let mut start = self.position(self.anchor, self.gravity, self.offset);
        let mut length = self.length;
        if self.fits(start, length) {
            return (start, length);
        }
        if self.flip {
            let flipped = self.position(self.anchor.flipped(), self.gravity.flipped(), -self.offset);
            if self.fits(flipped, length) {
                return (flipped, length);
            }
        }
        if self.slide {
            let end = self.bounds_start + self.bounds_length;
            if start + length > end {
                start = end - length;
            }
            if start < self.bounds_start {
                start = self.bounds_start;
            }
            if self.fits(start, length) {
                return (start, length);
            }
        }
        if self.resize {
            let clipped_start = start.max(self.bounds_start);
            let clipped_end = (start + length).min(self.bounds_start + self.bounds_length);
            if clipped_end > clipped_start {
                start = clipped_start;
                length = clipped_end - clipped_start;
            }
        }
        (start, length)
    }
}

impl Complete {
    pub fn place(&self, bounds: Rect) -> Rect {
        let adjust = self.adjustment;
        let x = Axis {
            anchor_start: self.anchor_rect.x,
            anchor_length: self.anchor_rect.width,
            anchor: self.anchor.x,
            gravity: self.gravity.x,
            offset: self.offset.0,
            length: self.size.0,
            bounds_start: bounds.x,
            bounds_length: bounds.width,
            flip: adjust.contains(ConstraintAdjustment::FlipX),
            slide: adjust.contains(ConstraintAdjustment::SlideX),
            resize: adjust.contains(ConstraintAdjustment::ResizeX),
        }
        .solve();
        let y = Axis {
            anchor_start: self.anchor_rect.y,
            anchor_length: self.anchor_rect.height,
            anchor: self.anchor.y,
            gravity: self.gravity.y,
            offset: self.offset.1,
            length: self.size.1,
            bounds_start: bounds.y,
            bounds_length: bounds.height,
            flip: adjust.contains(ConstraintAdjustment::FlipY),
            slide: adjust.contains(ConstraintAdjustment::SlideY),
            resize: adjust.contains(ConstraintAdjustment::ResizeY),
        }
        .solve();
        Rect { x: x.0, y: y.0, width: x.1, height: y.1 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTPUT: Rect = Rect { x: 0, y: 0, width: 1280, height: 800 };

    fn select_menu(anchor_y: i32, height: i32, adjustment: ConstraintAdjustment) -> Complete {
        Positioner {
            size: Some((320, height)),
            anchor_rect: Some(Rect { x: 40, y: anchor_y, width: 320, height: 36 }),
            anchor: Anchor::BottomLeft,
            gravity: Gravity::BottomRight,
            constraint_adjustment: adjustment,
            offset: (0, 0),
        }
        .complete()
        .unwrap()
    }

    #[test]
    fn incomplete_positioner_is_rejected() {
        let mut positioner = Positioner { size: Some((10, 10)), ..Positioner::default() };
        assert!(positioner.complete().is_none());
        positioner.size = None;
        positioner.anchor_rect = Some(Rect { x: 0, y: 0, width: 1, height: 1 });
        assert!(positioner.complete().is_none());
    }

    #[test]
    fn unconstrained_popup_hangs_from_its_anchor_edge() {
        let placed = select_menu(88, 120, ConstraintAdjustment::empty()).place(OUTPUT);
        assert_eq!(placed, Rect { x: 40, y: 124, width: 320, height: 120 });
    }

    #[test]
    fn centered_anchor_and_gravity_center_the_popup_on_the_rect() {
        let placed = Positioner {
            size: Some((100, 50)),
            anchor_rect: Some(Rect { x: 200, y: 200, width: 40, height: 20 }),
            ..Positioner::default()
        }
        .complete()
        .unwrap()
        .place(OUTPUT);
        assert_eq!(placed, Rect { x: 170, y: 185, width: 100, height: 50 });
    }

    #[test]
    fn offset_shifts_the_unconstrained_position() {
        let mut positioner = Positioner {
            size: Some((50, 50)),
            anchor_rect: Some(Rect { x: 10, y: 10, width: 10, height: 10 }),
            anchor: Anchor::BottomRight,
            gravity: Gravity::BottomRight,
            ..Positioner::default()
        };
        positioner.offset = (3, -4);
        assert_eq!(positioner.complete().unwrap().place(OUTPUT), Rect { x: 23, y: 16, width: 50, height: 50 });
    }

    #[test]
    fn flip_moves_a_menu_above_its_anchor_when_below_overflows() {
        let adjust = ConstraintAdjustment::FlipY | ConstraintAdjustment::SlideY;
        let placed = select_menu(700, 200, adjust).place(OUTPUT);
        assert_eq!(placed, Rect { x: 40, y: 500, width: 320, height: 200 });
    }

    #[test]
    fn flip_is_reverted_when_the_flipped_position_also_overflows_and_slide_takes_over() {
        let adjust = ConstraintAdjustment::FlipY | ConstraintAdjustment::SlideY;
        let placed = select_menu(300, 600, adjust).place(OUTPUT);
        assert_eq!(placed, Rect { x: 40, y: 200, width: 320, height: 600 });
    }

    #[test]
    fn slide_aligns_to_the_start_edge_when_larger_than_the_output() {
        let placed = select_menu(10, 1000, ConstraintAdjustment::SlideY).place(OUTPUT);
        assert_eq!(placed.y, 0);
        assert_eq!(placed.height, 1000);
    }

    #[test]
    fn resize_clips_to_the_output_when_nothing_else_is_allowed() {
        let placed = select_menu(700, 200, ConstraintAdjustment::ResizeY).place(OUTPUT);
        assert_eq!(placed, Rect { x: 40, y: 736, width: 320, height: 64 });
    }

    #[test]
    fn no_adjustment_leaves_the_popup_constrained() {
        let placed = select_menu(700, 200, ConstraintAdjustment::empty()).place(OUTPUT);
        assert_eq!(placed.y, 736);
        assert_eq!(placed.height, 200);
    }

    #[test]
    fn flip_x_mirrors_a_submenu_to_the_left_of_its_parent_item() {
        let placed = Positioner {
            size: Some((200, 100)),
            anchor_rect: Some(Rect { x: 1100, y: 100, width: 150, height: 24 }),
            anchor: Anchor::TopRight,
            gravity: Gravity::BottomRight,
            constraint_adjustment: ConstraintAdjustment::FlipX,
            offset: (0, 0),
        }
        .complete()
        .unwrap()
        .place(OUTPUT);
        assert_eq!(placed, Rect { x: 900, y: 100, width: 200, height: 100 });
    }

    #[test]
    fn bounds_are_relative_to_the_parent_geometry() {
        let bounds = Rect { x: -500, y: -300, width: 1280, height: 800 };
        let adjust = ConstraintAdjustment::FlipY;
        let placed = select_menu(400, 200, adjust).place(bounds);
        assert_eq!(placed.y, 200);
    }
}
