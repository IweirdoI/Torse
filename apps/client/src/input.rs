//! Keyboard and mouse state for the windowed client.
//!
//! The window feeds raw winit events in; the frame loop reads one
//! [`MoveInput`] snapshot per frame. Look deltas accumulate between frames and
//! are consumed by [`InputState::take_input`], so event bursts within a frame
//! still rotate the camera once by their sum.

use std::collections::BTreeSet;

use crate::camera::MoveInput;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, MouseScrollDelta};
use winit::keyboard::{KeyCode, PhysicalKey};

/// Radians of yaw per pixel of horizontal drag.
const LOOK_PER_PIXEL: f32 = 0.005;
/// Multiplier applied to the fly speed per wheel notch.
const SPEED_STEP: f32 = 1.25;
/// Fastest and slowest fly speed in blocks per second.
const SPEED_RANGE: (f32, f32) = (1.0, 300.0);

/// Aggregated input state between frames.
#[derive(Debug, Default)]
pub struct InputState {
    held: BTreeSet<KeyCode>,
    look_delta: PhysicalPosition<f64>,
    dragging: bool,
    speed_factor: f32,
    screenshot: bool,
}

impl InputState {
    /// Start with the default speed factor of one.
    pub fn new() -> Self {
        Self {
            speed_factor: 1.0,
            ..Self::default()
        }
    }

    /// Track key presses; screenshot is an edge, movement is level-triggered.
    pub fn key(&mut self, key: PhysicalKey, state: ElementState) {
        let PhysicalKey::Code(code) = key else {
            return;
        };
        match state {
            ElementState::Pressed => {
                self.held.insert(code);
                if code == KeyCode::F12 {
                    self.screenshot = true;
                }
            }
            ElementState::Released => {
                self.held.remove(&code);
            }
        }
    }

    /// Dragging with any button rotates the view; the button itself is unused.
    pub fn mouse(&mut self, button: MouseButton, state: ElementState) {
        let pressed = state == ElementState::Pressed;
        if button == MouseButton::Left || button == MouseButton::Right {
            self.dragging = pressed;
        }
    }

    /// Accumulate cursor motion; only meaningful while dragging.
    pub fn cursor(&mut self, position: PhysicalPosition<f64>) {
        if self.dragging {
            self.look_delta.x += position.x;
            self.look_delta.y += position.y;
        }
    }

    /// Adjust the speed factor by wheel notches, clamped to the speed range.
    pub fn wheel(&mut self, delta: MouseScrollDelta) {
        let notches = match delta {
            MouseScrollDelta::LineDelta(_, lines) => lines,
            MouseScrollDelta::PixelDelta(position) => position.y as f32 / 48.0,
        };
        self.speed_factor =
            (self.speed_factor * SPEED_STEP.powf(notches)).clamp(SPEED_RANGE.0, SPEED_RANGE.1);
    }

    /// Consume one frame of movement intent and the pending look delta.
    ///
    /// `base_speed` is scaled by the wheel factor; no keys held yields a
    /// default `MoveInput` that leaves the camera in place.
    pub fn take_input(&mut self, base_speed: f32) -> (MoveInput, f32) {
        let axis = |positive: KeyCode, negative: KeyCode| {
            (self.held.contains(&positive) as i32 - self.held.contains(&negative) as i32) as f32
        };
        let yaw_delta = -(self.look_delta.x as f32) * LOOK_PER_PIXEL;
        let pitch_delta = -(self.look_delta.y as f32) * LOOK_PER_PIXEL;
        self.look_delta = PhysicalPosition::new(0.0, 0.0);
        let input = MoveInput {
            forward: axis(KeyCode::KeyW, KeyCode::KeyS),
            strafe: axis(KeyCode::KeyD, KeyCode::KeyA),
            vertical: axis(KeyCode::Space, KeyCode::ShiftLeft),
            yaw_delta,
            pitch_delta,
        };
        (input, base_speed * self.speed_factor)
    }

    /// Consume the screenshot edge.
    pub fn take_screenshot(&mut self) -> bool {
        std::mem::take(&mut self.screenshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(state: &mut InputState, code: KeyCode) {
        state.key(PhysicalKey::Code(code), ElementState::Pressed);
    }

    fn release(state: &mut InputState, code: KeyCode) {
        state.key(PhysicalKey::Code(code), ElementState::Released);
    }

    #[test]
    fn held_keys_drive_movement_axes() {
        let mut state = InputState::new();
        press(&mut state, KeyCode::KeyW);
        press(&mut state, KeyCode::KeyD);
        let (input, speed) = state.take_input(12.0);
        assert_eq!(input.forward, 1.0);
        assert_eq!(input.strafe, 1.0);
        assert_eq!(input.vertical, 0.0);
        assert_eq!(input.yaw_delta, 0.0);
        assert_eq!(speed, 12.0);
        release(&mut state, KeyCode::KeyW);
        press(&mut state, KeyCode::KeyS);
        let (input, _) = state.take_input(12.0);
        assert_eq!(input.forward, -1.0);
    }

    #[test]
    fn drag_deltas_accumulate_and_clear() {
        let mut state = InputState::new();
        state.mouse(MouseButton::Left, ElementState::Pressed);
        state.cursor(PhysicalPosition::new(10.0, 4.0));
        state.cursor(PhysicalPosition::new(-4.0, 6.0));
        let (input, _) = state.take_input(12.0);
        assert!((input.yaw_delta - (-6.0 * LOOK_PER_PIXEL)).abs() < 1e-6);
        assert!((input.pitch_delta - (-10.0 * LOOK_PER_PIXEL)).abs() < 1e-6);
        let (input, _) = state.take_input(12.0);
        assert_eq!(input.yaw_delta, 0.0);
        // Without a held button the cursor does not rotate the view.
        state.mouse(MouseButton::Left, ElementState::Released);
        state.cursor(PhysicalPosition::new(100.0, 100.0));
        let (input, _) = state.take_input(12.0);
        assert_eq!(input.yaw_delta, 0.0);
    }

    #[test]
    fn wheel_scales_speed_within_bounds() {
        let mut state = InputState::new();
        state.wheel(MouseScrollDelta::LineDelta(0.0, 2.0));
        let (_, speed) = state.take_input(12.0);
        assert!((speed - 12.0 * SPEED_STEP * SPEED_STEP).abs() < 1e-4);
        for _ in 0..40 {
            state.wheel(MouseScrollDelta::LineDelta(0.0, -1.0));
        }
        let (_, speed) = state.take_input(12.0);
        assert!(
            (speed - 12.0 * SPEED_RANGE.0).abs() < 1e-4 || speed >= SPEED_RANGE.0 * 12.0 * 0.99
        );
        assert!(speed < SPEED_RANGE.1 * 12.0);
    }

    #[test]
    fn screenshot_fires_once_per_press() {
        let mut state = InputState::new();
        assert!(!state.take_screenshot());
        press(&mut state, KeyCode::F12);
        assert!(state.take_screenshot());
        assert!(!state.take_screenshot());
        release(&mut state, KeyCode::F12);
        press(&mut state, KeyCode::F12);
        assert!(state.take_screenshot());
    }
}
