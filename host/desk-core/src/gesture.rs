//! Turns polled touch samples into taps, long presses and swipes.
//!
//! The GT911 is polled, so input arrives as "finger at (x, y)" or "no finger"
//! every few milliseconds. A gesture is decided on release, except a long
//! press, which fires while the finger is still down so the user feels it
//! take effect without lifting.

use crate::protocol::{Dir, Gesture};

pub const LONG_MS: u64 = 600;
/// Movement allowed before a touch stops being a tap.
const SLOP: i32 = 24;
/// Horizontal travel that makes a swipe.
const SWIPE: i32 = 80;

/// A gesture plus when its finger first touched down — the prompt modal uses
/// that to refuse a touch that began before it appeared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub gesture: Gesture,
    pub down_at: u64,
}

#[derive(Default)]
pub struct Recognizer {
    touch: Option<Touch>,
}

struct Touch {
    start: (i32, i32),
    last: (i32, i32),
    down_at: u64,
    moved: bool,
    long_fired: bool,
}

impl Recognizer {
    /// Feed one poll result. `now` is milliseconds on any monotonic clock.
    pub fn sample(&mut self, now: u64, point: Option<(u16, u16)>) -> Option<Event> {
        match (point, &mut self.touch) {
            (Some((x, y)), None) => {
                let p = (x as i32, y as i32);
                self.touch = Some(Touch { start: p, last: p, down_at: now, moved: false, long_fired: false });
                None
            }
            (Some((x, y)), Some(t)) => {
                t.last = (x as i32, y as i32);
                let (dx, dy) = (t.last.0 - t.start.0, t.last.1 - t.start.1);
                if dx.abs() > SLOP || dy.abs() > SLOP {
                    t.moved = true;
                }
                if !t.moved && !t.long_fired && now - t.down_at >= LONG_MS {
                    t.long_fired = true;
                    let (x, y) = (t.start.0 as u16, t.start.1 as u16);
                    return Some(Event { gesture: Gesture::Long { x, y }, down_at: t.down_at });
                }
                None
            }
            (None, Some(_)) => {
                let t = self.touch.take()?;
                if t.long_fired {
                    return None;
                }
                let (dx, dy) = (t.last.0 - t.start.0, t.last.1 - t.start.1);
                let gesture = if dx.abs() >= SWIPE && dx.abs() > 2 * dy.abs() {
                    Gesture::Swipe { dir: if dx < 0 { Dir::Left } else { Dir::Right } }
                } else if dy.abs() >= SWIPE && dy.abs() > 2 * dx.abs() {
                    // Up means the finger moved up: content scrolls forward.
                    Gesture::Swipe { dir: if dy < 0 { Dir::Up } else { Dir::Down } }
                } else if !t.moved {
                    Gesture::Tap { x: t.start.0 as u16, y: t.start.1 as u16 }
                } else {
                    // A drag that is neither a tap nor a swipe. Doing nothing
                    // is the safe reading of an ambiguous touch.
                    return None;
                };
                Some(Event { gesture, down_at: t.down_at })
            }
            (None, None) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(samples: &[(u64, Option<(u16, u16)>)]) -> alloc::vec::Vec<Event> {
        let mut r = Recognizer::default();
        samples.iter().filter_map(|&(t, p)| r.sample(t, p)).collect()
    }

    #[test]
    fn short_still_touch_is_a_tap_at_the_down_point() {
        let ev = run(&[(0, Some((100, 200))), (50, Some((105, 198))), (90, None)]);
        assert_eq!(ev, [Event { gesture: Gesture::Tap { x: 100, y: 200 }, down_at: 0 }]);
    }

    #[test]
    fn held_touch_fires_long_once_and_nothing_on_release() {
        let ev = run(&[(0, Some((10, 10))), (300, Some((10, 10))), (650, Some((11, 10))), (900, Some((11, 10))), (950, None)]);
        assert_eq!(ev, [Event { gesture: Gesture::Long { x: 10, y: 10 }, down_at: 0 }]);
    }

    #[test]
    fn horizontal_drag_is_a_swipe() {
        let ev = run(&[(0, Some((300, 240))), (60, Some((250, 245))), (120, Some((180, 250))), (130, None)]);
        assert_eq!(ev[0].gesture, Gesture::Swipe { dir: Dir::Left });
        let ev = run(&[(0, Some((100, 240))), (120, Some((200, 240))), (130, None)]);
        assert_eq!(ev[0].gesture, Gesture::Swipe { dir: Dir::Right });
    }

    #[test]
    fn vertical_drag_is_an_up_or_down_swipe() {
        let ev = run(&[(0, Some((240, 400))), (120, Some((245, 250))), (130, None)]);
        assert_eq!(ev[0].gesture, Gesture::Swipe { dir: Dir::Up });
        let ev = run(&[(0, Some((240, 100))), (120, Some((240, 220))), (130, None)]);
        assert_eq!(ev[0].gesture, Gesture::Swipe { dir: Dir::Down });
    }

    #[test]
    fn diagonal_or_short_drag_is_nothing() {
        assert!(run(&[(0, Some((100, 100))), (100, Some((190, 170))), (110, None)]).is_empty());
        assert!(run(&[(0, Some((100, 100))), (100, Some((150, 100))), (110, None)]).is_empty());
    }

    #[test]
    fn slow_drag_never_becomes_long() {
        let ev = run(&[(0, Some((100, 100))), (200, Some((140, 100))), (700, Some((141, 100))), (710, None)]);
        assert!(ev.is_empty());
    }
}
