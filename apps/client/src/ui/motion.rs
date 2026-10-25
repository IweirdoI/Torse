//! Time-based easing, independent of presentation frame count.
use super::{Action, Mode, Page};
use std::time::Instant;

#[derive(Clone, Copy)]
pub(super) struct Frame {
    pub hover: [f32; 7],
    pub page: f32,
    pub reveal: f32,
    pub angle: f32,
}
impl Default for Frame {
    fn default() -> Self {
        Self {
            hover: [0.0; 7],
            page: 1.0,
            reveal: 1.0,
            angle: 0.0,
        }
    }
}
pub(super) struct Motion {
    previous: Option<Mode>,
    page_started: Instant,
    reveal_started: Option<Instant>,
    last: Instant,
    origin: Instant,
    hover: [f32; 7],
    active: bool,
    settled: bool,
}
impl Default for Motion {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            previous: None,
            page_started: now,
            reveal_started: None,
            last: now,
            origin: now,
            hover: [0.0; 7],
            active: true,
            settled: false,
        }
    }
}
impl Motion {
    pub fn settle(&mut self) {
        self.settled = true;
    }
    pub fn active(&self) -> bool {
        self.active
    }
    pub fn update(
        &mut self,
        mode: Mode,
        hover: Option<Action>,
        loading: bool,
        now: Instant,
    ) -> Frame {
        if self.previous != Some(mode) {
            self.page_started = now;
            if matches!(self.previous, Some(Mode::Launch(_))) && mode == Mode::Game {
                self.reveal_started = Some(now);
            }
            self.previous = Some(mode);
        }
        let dt = now
            .saturating_duration_since(self.last)
            .as_secs_f32()
            .min(0.1);
        self.last = now;
        let actions = [
            Action::Navigate(Page::Home),
            Action::Navigate(Page::Worlds),
            Action::Navigate(Page::Multiplayer),
            Action::Play,
            Action::Navigate(Page::Settings),
            Action::Navigate(Page::About),
            Action::Quit,
        ];
        let mut moving = false;
        for (i, action) in actions.into_iter().enumerate() {
            let goal =
                (hover == Some(action) || i == 3 && hover == Some(Action::Connect)) as u8 as f32;
            if self.settled {
                self.hover[i] = goal;
            } else {
                self.hover[i] += (goal - self.hover[i]) * (1.0 - (-dt / 0.065).exp());
                if (goal - self.hover[i]).abs() < 0.005 {
                    self.hover[i] = goal;
                }
            }
            moving |= self.hover[i] != goal;
        }
        let page = if self.settled {
            1.0
        } else {
            ease(
                now.saturating_duration_since(self.page_started)
                    .as_secs_f32()
                    / 0.26,
            )
        };
        let reveal = if self.settled {
            1.0
        } else {
            self.reveal_started.map_or(1.0, |started| {
                ease(now.saturating_duration_since(started).as_secs_f32() / 0.4)
            })
        };
        self.active =
            moving || loading || matches!(mode, Mode::Launch(_)) && page < 1.0 || reveal < 1.0;
        Frame {
            hover: self.hover,
            page,
            reveal,
            angle: now.saturating_duration_since(self.origin).as_secs_f32() * 4.5,
        }
    }
}
fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn hover_easing_tracks_elapsed_time_and_returns_to_rest() {
        let now = Instant::now();
        let mut fast = Motion::default();
        let mut slow = Motion::default();
        fast.last = now;
        slow.last = now;
        let mode = Mode::Launch(Page::Home);
        fast.update(mode, Some(Action::Play), false, now);
        slow.update(mode, Some(Action::Play), false, now);
        let mut frame = Frame::default();
        for i in 1..=8 {
            frame = fast.update(
                mode,
                Some(Action::Play),
                false,
                now + Duration::from_millis(i * 10),
            );
        }
        let other = slow.update(
            mode,
            Some(Action::Play),
            false,
            now + Duration::from_millis(80),
        );
        assert!((frame.hover[3] - other.hover[3]).abs() < 0.0001);
        assert!(frame.hover[3] > 0.5 && frame.hover[3] < 1.0);
        let rest = slow.update(mode, None, false, now + Duration::from_millis(160));
        assert!(rest.hover[3] > 0.0 && rest.hover[3] < other.hover[3]);
    }
    #[test]
    fn page_and_world_reveal_finish_without_restarting_each_frame() {
        let now = Instant::now();
        let mut motion = Motion::default();
        assert_eq!(
            motion
                .update(Mode::Launch(Page::Home), None, false, now)
                .page,
            0.0
        );
        assert_eq!(
            motion
                .update(
                    Mode::Launch(Page::Home),
                    None,
                    false,
                    now + Duration::from_secs(1)
                )
                .page,
            1.0
        );
        let start = now + Duration::from_secs(2);
        assert_eq!(motion.update(Mode::Game, None, false, start).reveal, 0.0);
        assert!(
            motion
                .update(Mode::Game, None, false, start + Duration::from_millis(100))
                .reveal
                < 1.0
        );
        assert_eq!(
            motion
                .update(Mode::Game, None, false, start + Duration::from_millis(450))
                .reveal,
            1.0
        );
        assert!(!motion.active());
        motion.update(
            Mode::Launch(Page::Settings),
            None,
            true,
            now + Duration::from_secs(3),
        );
        assert!(
            motion.active(),
            "loading continues to repaint after page easing finishes"
        );
    }
}
