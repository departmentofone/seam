//! Shared screen layout: where every device's monitors sit relative to each
//! other, and how a cursor moves from one device to the next.
//!
//! Each device keeps its own monitors exactly as Windows arranges them, in
//! physical pixels. A `Placement` then puts that whole block somewhere in a
//! shared "layout space", scaled so mixed DPIs line up sensibly.

use crate::protocol::{DeviceId, LayoutDoc, Monitor, Placement};

/// How far apart two facing edges may be (in layout units) and still connect.
/// The calibration screen snaps edges together, so this only absorbs rounding
/// and small hand-placed gaps.
const EDGE_GAP: f64 = 48.0;
/// How far two facing edges may overlap and still connect.
const EDGE_OVERLAP: f64 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Clone, Debug)]
pub struct DeskDevice {
    pub id: DeviceId,
    pub monitors: Vec<Monitor>,
    pub place: Placement,
}

impl DeskDevice {
    fn origin(&self) -> (f64, f64) {
        let x = self.monitors.iter().map(|m| m.rect.x).min().unwrap_or(0);
        let y = self.monitors.iter().map(|m| m.rect.y).min().unwrap_or(0);
        (x as f64, y as f64)
    }

    pub fn to_layout(&self, x: f64, y: f64) -> (f64, f64) {
        let (ox, oy) = self.origin();
        (self.place.x + (x - ox) * self.place.scale, self.place.y + (y - oy) * self.place.scale)
    }

    pub fn from_layout(&self, lx: f64, ly: f64) -> (f64, f64) {
        let (ox, oy) = self.origin();
        (ox + (lx - self.place.x) / self.place.scale, oy + (ly - self.place.y) / self.place.scale)
    }

    pub fn layout_rect(&self, m: &Monitor) -> LRect {
        let (x, y) = self.to_layout(m.rect.x as f64, m.rect.y as f64);
        LRect { x, y, w: m.rect.w as f64 * self.place.scale, h: m.rect.h as f64 * self.place.scale }
    }

    pub fn monitor_at(&self, x: i32, y: i32) -> Option<&Monitor> {
        self.monitors.iter().find(|m| m.rect.contains(x, y))
    }

    /// The monitor containing (x, y), or the closest one if the point is off
    /// every screen.
    pub fn nearest_monitor(&self, x: i32, y: i32) -> Option<&Monitor> {
        self.monitor_at(x, y).or_else(|| {
            self.monitors.iter().min_by_key(|m| {
                let dx = (m.rect.x - x).max(0).max(x - (m.rect.right() - 1));
                let dy = (m.rect.y - y).max(0).max(y - (m.rect.bottom() - 1));
                dx as i64 * dx as i64 + dy as i64 * dy as i64
            })
        })
    }

    /// Bounding box of all monitors in device pixels.
    pub fn bounds(&self) -> (i32, i32, i32, i32) {
        let l = self.monitors.iter().map(|m| m.rect.x).min().unwrap_or(0);
        let t = self.monitors.iter().map(|m| m.rect.y).min().unwrap_or(0);
        let r = self.monitors.iter().map(|m| m.rect.right()).max().unwrap_or(0);
        let b = self.monitors.iter().map(|m| m.rect.bottom()).max().unwrap_or(0);
        (l, t, r, b)
    }
}

/// Where the cursor ends up after crossing an edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub device: DeviceId,
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Step {
    Stay { x: f64, y: f64 },
    Cross(Hit),
}

#[derive(Clone, Debug, Default)]
pub struct Desk {
    pub devices: Vec<DeskDevice>,
}

impl Desk {
    pub fn device(&self, id: &DeviceId) -> Option<&DeskDevice> {
        self.devices.iter().find(|d| d.id == *id)
    }

    /// The cursor sits at (x, y) on `from` and pushes past `edge` of its
    /// monitor. Returns where it lands, if another device's screen is there.
    pub fn cross(&self, from: &DeviceId, x: i32, y: i32, edge: Edge) -> Option<Hit> {
        let dev = self.device(from)?;
        let mon = dev.nearest_monitor(x, y)?;
        let r = mon.rect;
        // A point on the edge line, in the middle of the pixel row/column.
        let (ex, ey) = match edge {
            Edge::Left => (r.x as f64, y as f64 + 0.5),
            Edge::Right => (r.right() as f64, y as f64 + 0.5),
            Edge::Top => (x as f64 + 0.5, r.y as f64),
            Edge::Bottom => (x as f64 + 0.5, r.bottom() as f64),
        };
        let (lx, ly) = dev.to_layout(ex, ey);

        let mut best: Option<(f64, Hit)> = None;
        for other in self.devices.iter().filter(|d| d.id != *from) {
            for m in &other.monitors {
                let lr = other.layout_rect(m);
                // Distance from our edge line to the facing edge of `m`,
                // positive when there is a gap, and whether we line up with it.
                let (gap, along) = match edge {
                    Edge::Right => (lr.x - lx, ly >= lr.y && ly < lr.y + lr.h),
                    Edge::Left => (lx - (lr.x + lr.w), ly >= lr.y && ly < lr.y + lr.h),
                    Edge::Bottom => (lr.y - ly, lx >= lr.x && lx < lr.x + lr.w),
                    Edge::Top => (ly - (lr.y + lr.h), lx >= lr.x && lx < lr.x + lr.w),
                };
                if !along || gap < -EDGE_OVERLAP || gap > EDGE_GAP {
                    continue;
                }
                let (px, py) = other.from_layout(lx, ly);
                let mr = m.rect;
                let (hx, hy) = match edge {
                    Edge::Right => (mr.x, clamp(py, mr.y, mr.bottom() - 1)),
                    Edge::Left => (mr.right() - 1, clamp(py, mr.y, mr.bottom() - 1)),
                    Edge::Bottom => (clamp(px, mr.x, mr.right() - 1), mr.y),
                    Edge::Top => (clamp(px, mr.x, mr.right() - 1), mr.bottom() - 1),
                };
                let hit = Hit { device: other.id, x: hx, y: hy };
                let score = gap.abs();
                if best.map_or(true, |(s, _)| score < s) {
                    best = Some((score, hit));
                }
            }
        }
        best.map(|(_, h)| h)
    }

    /// Whether pushing past `edge` at (x, y) would leave `from` for another
    /// device (and not just onto another of its own monitors).
    pub fn edge_has_target(&self, from: &DeviceId, x: i32, y: i32, edge: Edge) -> bool {
        let Some(dev) = self.device(from) else { return false };
        let (nx, ny) = match edge {
            Edge::Left => (x - 1, y),
            Edge::Right => (x + 1, y),
            Edge::Top => (x, y - 1),
            Edge::Bottom => (x, y + 1),
        };
        dev.monitor_at(nx, ny).is_none() && self.cross(from, x, y, edge).is_some()
    }

    /// Move a cursor that lives on `dev` by (dx, dy) device pixels. Stays on
    /// the device's screens unless it runs off an edge that leads elsewhere.
    pub fn step(&self, dev_id: &DeviceId, x: f64, y: f64, dx: f64, dy: f64) -> Step {
        let Some(dev) = self.device(dev_id) else { return Step::Stay { x, y } };
        let (mut x, mut y) = (x, y);

        for axis in 0..2 {
            let (nx, ny) = if axis == 0 { (x + dx, y) } else { (x, y + dy) };
            if (axis == 0 && dx == 0.0) || (axis == 1 && dy == 0.0) {
                continue;
            }
            if dev.monitor_at(nx.floor() as i32, ny.floor() as i32).is_some() {
                x = nx;
                y = ny;
                continue;
            }
            let cx = x.floor() as i32;
            let cy = y.floor() as i32;
            let edge = match (axis, if axis == 0 { dx } else { dy } > 0.0) {
                (0, true) => Edge::Right,
                (0, false) => Edge::Left,
                (_, true) => Edge::Bottom,
                (_, false) => Edge::Top,
            };
            if let Some(hit) = self.cross(dev_id, cx, cy, edge) {
                return Step::Cross(hit);
            }
            // Nothing there: slide along the edge of the current screen.
            if let Some(m) = dev.nearest_monitor(cx, cy) {
                let r = m.rect;
                x = nx.clamp(r.x as f64, (r.right() - 1) as f64);
                y = ny.clamp(r.y as f64, (r.bottom() - 1) as f64);
            }
        }
        Step::Stay { x, y }
    }
}

fn clamp(v: f64, lo: i32, hi: i32) -> i32 {
    (v.round() as i32).clamp(lo, hi.max(lo))
}

/// Default scale for a device: one layout unit per logical pixel of its
/// primary monitor.
pub fn default_scale(monitors: &[Monitor]) -> f64 {
    let s = monitors.iter().find(|m| m.primary).or(monitors.first()).map_or(1.0, |m| m.scale);
    1.0 / (s.max(0.5) as f64)
}

/// Give a placement to every device in `devices` that does not have one yet,
/// lining new ones up to the right of what is already placed. Returns true
/// if anything changed.
pub fn fill_missing(doc: &mut LayoutDoc, devices: &[(DeviceId, Vec<Monitor>)]) -> bool {
    let mut changed = false;
    for (id, mons) in devices {
        if doc.get(id).is_some() || mons.is_empty() {
            continue;
        }
        let mut right_edge = 0.0f64;
        let mut top = 0.0f64;
        for (other_id, p) in &doc.placements {
            if let Some((_, om)) = devices.iter().find(|(d, _)| d == other_id) {
                let dd = DeskDevice { id: *other_id, monitors: om.clone(), place: *p };
                let (l, t, r, _) = dd.bounds();
                let (lx, ty) = dd.to_layout(l as f64, t as f64);
                let rx = lx + (r - l) as f64 * p.scale;
                if rx > right_edge {
                    right_edge = rx;
                    top = ty;
                }
            }
        }
        doc.set(*id, Placement { x: right_edge, y: top, scale: default_scale(mons) });
        changed = true;
    }
    if changed {
        doc.revision += 1;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Rect;
    use uuid::Uuid;

    fn mon(x: i32, y: i32, w: i32, h: i32, scale: f32) -> Monitor {
        Monitor { id: format!("{x},{y}"), name: "m".into(), rect: Rect { x, y, w, h }, scale, primary: x == 0 && y == 0 }
    }

    fn two() -> (Desk, Uuid, Uuid) {
        let pc = Uuid::from_u128(1);
        let laptop = Uuid::from_u128(2);
        let desk = Desk {
            devices: vec![
                DeskDevice { id: pc, monitors: vec![mon(0, 0, 1920, 1080, 1.0)], place: Placement { x: 0.0, y: 0.0, scale: 1.0 } },
                // 2880x1800 at 200%: 1440x900 layout units, placed right of the PC.
                DeskDevice { id: laptop, monitors: vec![mon(0, 0, 2880, 1800, 2.0)], place: Placement { x: 1920.0, y: 90.0, scale: 0.5 } },
            ],
        };
        (desk, pc, laptop)
    }

    #[test]
    fn crosses_right_with_scaling() {
        let (desk, pc, laptop) = two();
        let hit = desk.cross(&pc, 1919, 540, Edge::Right).unwrap();
        assert_eq!(hit.device, laptop);
        assert_eq!(hit.x, 0);
        // Layout y 540.5 -> laptop (540.5 - 90) * 2 = 901.
        assert_eq!(hit.y, 901);
    }

    #[test]
    fn no_target_above_laptop_top() {
        let (desk, pc, _) = two();
        assert!(desk.cross(&pc, 1919, 20, Edge::Right).is_none());
        assert!(!desk.edge_has_target(&pc, 1919, 20, Edge::Right));
        assert!(desk.edge_has_target(&pc, 1919, 500, Edge::Right));
    }

    #[test]
    fn step_back_home() {
        let (desk, pc, laptop) = two();
        match desk.step(&laptop, 2.0, 900.0, -5.0, 0.0) {
            Step::Cross(h) => {
                assert_eq!(h.device, pc);
                assert_eq!(h.x, 1919);
                assert_eq!(h.y, 540);
            }
            s => panic!("expected cross, got {s:?}"),
        }
    }

    #[test]
    fn step_slides_along_edge() {
        let (desk, _, laptop) = two();
        match desk.step(&laptop, 2879.0, 10.0, 50.0, -40.0) {
            Step::Stay { x, y } => {
                assert_eq!(x, 2879.0);
                assert_eq!(y, 0.0);
            }
            s => panic!("expected stay, got {s:?}"),
        }
    }

    #[test]
    fn fill_missing_places_to_the_right() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let mut doc = LayoutDoc::default();
        let devs = vec![(a, vec![mon(0, 0, 1920, 1080, 1.0)]), (b, vec![mon(0, 0, 2880, 1800, 2.0)])];
        assert!(fill_missing(&mut doc, &devs));
        assert_eq!(doc.get(&b).unwrap().x, 1920.0);
        assert_eq!(doc.get(&b).unwrap().scale, 0.5);
        assert!(!fill_missing(&mut doc, &devs));
    }
}
