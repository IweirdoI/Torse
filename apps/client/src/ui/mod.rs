//! Client UI foundation: a flat dark theme drawn on the canvas, HUD, modal
//! focus, and a clearly labeled machine preview. No UI control has access to
//! authoritative mutation.
mod canvas;
mod gpu;
mod style;

use canvas::{Canvas, Rect, Sprite, Text};
use gpu::Overlay;
use std::collections::BTreeMap;
use style::{
    BORDER, CHIP_BG, CHIP_BORDER, FIELD_BG, GROUP_BG, MUTED, SEPARATOR, TEXT, TITLE_TEXT, WINDOW_BG,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Game,
    Pause,
    MachinePreview,
}
impl Mode {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "hud" => Ok(Self::Game),
            "pause" => Ok(Self::Pause),
            "machine" => Ok(Self::MachinePreview),
            _ => Err("UI preview must be hud, pause or machine".into()),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Resume,
    Preview,
    Quit,
    SensDown,
    SensUp,
}

#[derive(Default)]
pub struct State {
    pub mode: Mode,
    pub cursor: [f64; 2],
    armed: Option<Action>,
}
impl State {
    pub fn modal(&self) -> bool {
        self.mode != Mode::Game
    }
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.armed = None;
    }
    pub fn pointer(&mut self, down: bool, size: [u32; 2]) -> Option<Action> {
        let hit = Layout::new(size).hit(self.mode, self.cursor);
        if down {
            self.armed = hit;
            None
        } else {
            self.armed.take().filter(|a| Some(*a) == hit)
        }
    }
}

struct Layout {
    scale: u32,
    width: u32,
    height: u32,
}
impl Layout {
    fn new(size: [u32; 2]) -> Self {
        let scale = (size[0] / 480).min(size[1] / 270).clamp(1, 4);
        Self {
            scale,
            width: size[0].max(1).div_ceil(scale),
            height: size[1].max(1).div_ceil(scale),
        }
    }
    fn panel(&self, mode: Mode) -> Rect {
        let (w, h) = if mode == Mode::Pause {
            (236, 172)
        } else {
            (324, 140)
        };
        let w = w.min(self.width.saturating_sub(8));
        let h = h.min(self.height.saturating_sub(8));
        Rect::new(
            (self.width - w) as i32 / 2,
            (self.height - h) as i32 / 2,
            w,
            h,
        )
    }
    fn buttons(&self, mode: Mode) -> Vec<(Action, Rect, &'static str)> {
        let p = self.panel(mode);
        match mode {
            Mode::Game | Mode::MachinePreview => Vec::new(),
            Mode::Pause => vec![
                (
                    Action::Resume,
                    Rect::new(p.x + 14, p.y + 34, p.w.saturating_sub(28), 22),
                    "menu_resume",
                ),
                (
                    Action::Preview,
                    Rect::new(p.x + 14, p.y + 60, p.w.saturating_sub(28), 22),
                    "menu_preview",
                ),
                (
                    Action::Quit,
                    Rect::new(p.x + 14, p.y + 86, p.w.saturating_sub(28), 22),
                    "menu_quit",
                ),
                (
                    Action::SensDown,
                    Rect::new(p.x + p.w as i32 - 110, p.y + 130, 20, 20),
                    "sens_down",
                ),
                (
                    Action::SensUp,
                    Rect::new(p.x + p.w as i32 - 34, p.y + 130, 20, 20),
                    "sens_up",
                ),
            ],
        }
    }
    fn sensitivity_field(&self) -> Rect {
        let p = self.panel(Mode::Pause);
        Rect::new(p.x + p.w as i32 - 86, p.y + 130, 48, 20)
    }
    fn close(&self, mode: Mode) -> Rect {
        let p = self.panel(mode);
        Rect::new(p.x + p.w as i32 - 22, p.y + 3, 18, 16)
    }
    fn hit(&self, mode: Mode, cursor: [f64; 2]) -> Option<Action> {
        if mode == Mode::Game {
            return None;
        }
        let p = [
            (cursor[0] / f64::from(self.scale)) as i32,
            (cursor[1] / f64::from(self.scale)) as i32,
        ];
        if self.close(mode).contains(p) {
            return Some(Action::Resume);
        }
        self.buttons(mode)
            .into_iter()
            .find(|(_, rect, _)| rect.contains(p))
            .map(|(a, _, _)| a)
    }
}

/// Content icons only; every piece of window chrome is drawn by `style`.
struct Skin {
    icons: Vec<Sprite>,
}
impl Skin {
    fn load() -> Result<Self, String> {
        macro_rules! asset {
            ($name:literal) => {
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../games/base/assets/",
                    $name
                )) as &[u8]
            };
        }
        let sources = [
            (asset!("icons/items/stone.png"), (16, 16)),
            (asset!("icons/items/glass_slab.png"), (16, 16)),
            (asset!("icons/items/plant.png"), (16, 16)),
        ];
        Ok(Self {
            icons: sources
                .into_iter()
                .map(|(bytes, size)| Sprite::decode(bytes, size))
                .collect::<Result<_, _>>()?,
        })
    }
}

#[derive(PartialEq, Eq)]
struct View {
    size: [u32; 2],
    mode: Mode,
    material: usize,
    network: bool,
    ready: bool,
    result: String,
    /// Hotbar labels supplied by the scene; empty keeps the packaged icons.
    names: Vec<String>,
    sensitivity: u16,
    hover: Option<Action>,
    pressed: Option<Action>,
    close_hot: bool,
}

pub struct Layer {
    overlay: Overlay,
    skin: Skin,
    text: Text,
    labels: BTreeMap<&'static str, &'static str>,
    last: Option<View>,
}
pub struct Hud<'a> {
    pub material: usize,
    pub network: bool,
    pub ready: bool,
    pub result: &'a str,
    /// Hotbar labels; empty uses the packaged icon names.
    pub names: Vec<String>,
    /// Captured-look sensitivity, shown and tuned in the pause menu.
    pub sensitivity: f64,
}
impl Layer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Result<Self, String> {
        let text = Text::load()?;
        let resource = if text.chinese {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../games/base/assets/locales/zh-Hans.txt"
            ))
        } else {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../games/base/assets/locales/en.txt"
            ))
        };
        Ok(Self {
            overlay: Overlay::new(device, format),
            skin: Skin::load()?,
            text,
            labels: resource
                .lines()
                .filter_map(|line| line.split_once('='))
                .collect(),
            last: None,
        })
    }
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        size: [u32; 2],
        state: &State,
        hud: Hud<'_>,
    ) {
        let layout = Layout::new(size);
        let view = View {
            size,
            mode: state.mode,
            material: hud.material.min(2),
            network: hud.network,
            ready: hud.ready,
            result: hud.result.chars().take(120).collect(),
            names: hud.names.into_iter().take(3).collect(),
            sensitivity: (hud.sensitivity * 100.0).round() as u16,
            hover: layout.hit(state.mode, state.cursor),
            pressed: state.armed,
            close_hot: {
                let p = [
                    (state.cursor[0] / f64::from(layout.scale)) as i32,
                    (state.cursor[1] / f64::from(layout.scale)) as i32,
                ];
                state.mode != Mode::Game && layout.close(state.mode).contains(p)
            },
        };

        let mut canvas = Canvas::new(layout.width, layout.height, layout.scale);
        self.paint(&mut canvas, &layout, &view);
        // The canvas already matches the surface one to one; the shader
        // must not divide again.
        self.overlay.upload(device, queue, &canvas, 1);
        self.last = Some(view);
    }
    pub fn draw(&self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        self.overlay.draw(encoder, target);
    }
    fn label(&mut self, canvas: &mut Canvas, key: &str, rect: Rect, size: u16, color: [u8; 4]) {
        let value = self.labels.get(key).copied().unwrap_or("?");
        self.text.draw(canvas, value, rect, size, color);
    }
    /// One locale line centred horizontally inside a region.
    fn centre(&mut self, canvas: &mut Canvas, key: &str, region: Rect, size: u16, color: [u8; 4]) {
        let value = self.labels.get(key).copied().unwrap_or("?");
        let tw = self.text.measure(value, size).min(region.w);
        self.text.draw(
            canvas,
            value,
            Rect::new(
                region.x + (region.w - tw) as i32 / 2,
                region.y,
                tw,
                u32::from(size) + 2,
            ),
            size,
            color,
        );
    }
    fn shadow_label(&mut self, canvas: &mut Canvas, key: &str, rect: Rect, size: u16) {
        let value = self.labels.get(key).copied().unwrap_or("?");
        self.text.draw(
            canvas,
            value,
            Rect::new(rect.x + 1, rect.y + 1, rect.w, rect.h),
            size,
            style::SHADOW,
        );
        self.text.draw(canvas, value, rect, size, TEXT);
    }
    fn paint(&mut self, c: &mut Canvas, l: &Layout, v: &View) {
        self.text.set_scale(u16::try_from(c.scale).unwrap_or(1));
        let w = l.width as i32;
        let h = l.height as i32;
        self.paint_hud(c, l, v, w, h);
        if v.mode == Mode::Game {
            style::crosshair(c, w / 2, h / 2);
            return;
        }
        c.fill(Rect::new(0, 0, l.width, l.height), [8, 9, 11, 130]);
        let p = l.panel(v.mode);
        style::frame(c, p, WINDOW_BG, BORDER);
        let title = if v.mode == Mode::Pause {
            "menu_title"
        } else {
            "machine_title"
        };
        self.label(
            c,
            title,
            Rect::new(p.x + 12, p.y + 4, p.w.saturating_sub(40), 16),
            12,
            TITLE_TEXT,
        );
        style::close_glyph(c, l.close(v.mode), v.close_hot);
        style::hline(c, p.x, p.y + 21, p.w, SEPARATOR);
        if v.mode == Mode::Pause {
            self.paint_pause(c, l, v, p);
        } else {
            self.paint_machine(c, p);
        }
        for (action, rect, key) in l.buttons(v.mode) {
            style::button(
                c,
                rect,
                v.hover == Some(action),
                v.pressed == Some(action) && v.hover == Some(action),
            );
            let label = self.labels.get(key).copied().unwrap_or("?");
            let tw = self.text.measure(label, 12).min(rect.w.saturating_sub(12));
            let color = if v.hover == Some(action) {
                TITLE_TEXT
            } else {
                TEXT
            };
            self.text.draw(
                c,
                label,
                Rect::new(
                    rect.x + (rect.w - tw) as i32 / 2,
                    rect.y + (rect.h as i32 - 12) / 2,
                    tw,
                    14,
                ),
                12,
                color,
            );
        }
    }
    /// HUD is shared by all modes: hotbar, status chip, result toast, help.
    fn paint_hud(&mut self, c: &mut Canvas, l: &Layout, v: &View, w: i32, h: i32) {
        let slot_x = w / 2 - 38;
        let slot_y = h - 36;
        let packaged = v.names.is_empty();
        for i in 0..3 {
            let x = slot_x + i as i32 * 26;
            style::slot(c, Rect::new(x, slot_y, 24, 24), i == v.material);
            if packaged {
                c.sprite(&self.skin.icons[i], Rect::new(x + 4, slot_y + 4, 16, 16), 0);
            }
            self.text.draw(
                c,
                &(i + 1).to_string(),
                Rect::new(x + 3, slot_y + 2, 8, 8),
                8,
                MUTED,
            );
        }
        // The selected item name would collide with the modal panels.
        if v.mode == Mode::Game {
            match v.names.get(v.material) {
                Some(name) => {
                    let name = name.clone();
                    let width = self.text.measure(&name, 12);
                    self.text.draw(
                        c,
                        &name,
                        Rect::new(w / 2 - width as i32 / 2, slot_y - 18, width, 16),
                        12,
                        TEXT,
                    );
                }
                None => {
                    let selected = ["item_stone", "item_glass", "item_plant"][v.material];
                    let width = self.text.measure(self.labels[selected], 12);
                    self.shadow_label(
                        c,
                        selected,
                        Rect::new(w / 2 - width as i32 / 2, slot_y - 18, width, 16),
                        12,
                    );
                }
            }
        }
        let info = if !v.network {
            "hud_local"
        } else if v.ready {
            "hud_online"
        } else {
            "hud_connecting"
        };
        let info_w = self.text.measure(self.labels[info], 11) + 12;
        style::frame(c, Rect::new(8, 8, info_w, 20), CHIP_BG, CHIP_BORDER);
        self.label(c, info, Rect::new(14, 11, info_w, 14), 11, TEXT);
        self.shadow_label(
            c,
            "hud_help",
            Rect::new(10, h - 14, l.width.saturating_sub(20), 13),
            9,
        );
        if !v.result.is_empty() {
            let rw = l.width.saturating_sub(16);
            style::frame(c, Rect::new(8, 32, rw, 20), CHIP_BG, CHIP_BORDER);
            self.text.draw(
                c,
                &v.result,
                Rect::new(14, 35, rw.saturating_sub(12), 15),
                10,
                TEXT,
            );
        }
    }
    fn paint_pause(&mut self, c: &mut Canvas, l: &Layout, v: &View, p: Rect) {
        style::hline(c, p.x, p.y + 118, p.w, SEPARATOR);
        self.label(
            c,
            "menu_sensitivity",
            Rect::new(p.x + 14, p.y + 134, 120, 14),
            11,
            TEXT,
        );
        let field = l.sensitivity_field();
        style::frame(c, field, FIELD_BG, BORDER);
        let value = format!("{:.2}", f64::from(v.sensitivity) / 100.0);
        let tw = self.text.measure(&value, 11);
        self.text.draw(
            c,
            &value,
            Rect::new(field.x + (field.w - tw) as i32 / 2, field.y + 4, tw, 14),
            11,
            TEXT,
        );
        self.label(
            c,
            "menu_note",
            Rect::new(p.x + 14, p.y + 156, p.w.saturating_sub(28), 12),
            9,
            MUTED,
        );
    }
    fn paint_machine(&mut self, c: &mut Canvas, p: Rect) {
        self.label(
            c,
            "machine_notice",
            Rect::new(p.x + 14, p.y + 24, p.w.saturating_sub(28), 12),
            9,
            MUTED,
        );
        // Input group with the sample item; output group stays empty. The
        // middle column keeps an eight-pixel gutter to both group frames.
        let groups: [(i32, &str, Option<usize>, &str); 2] = [
            (p.x + 14, "machine_input", Some(0), "item_stone"),
            (p.x + p.w as i32 - 110, "machine_output", None, "empty"),
        ];
        for (gx, label, icon, caption) in groups {
            style::frame(c, Rect::new(gx, p.y + 42, 96, 58), GROUP_BG, SEPARATOR);
            self.centre(c, label, Rect::new(gx, p.y + 46, 96, 12), 10, TEXT);
            style::slot(c, Rect::new(gx + 36, p.y + 58, 24, 24), false);
            if let Some(index) = icon {
                c.sprite(
                    &self.skin.icons[index],
                    Rect::new(gx + 40, p.y + 62, 16, 16),
                    0,
                );
                self.text
                    .draw(c, "1", Rect::new(gx + 37, p.y + 60, 8, 9), 8, MUTED);
            }
            self.centre(c, caption, Rect::new(gx, p.y + 86, 96, 12), 9, MUTED);
        }
        let mid = Rect::new(p.x + 118, p.y + 44, 88, 44);
        self.centre(
            c,
            "machine_progress",
            Rect::new(mid.x, p.y + 46, mid.w, 12),
            10,
            TEXT,
        );
        style::progress(c, Rect::new(mid.x, p.y + 62, mid.w, 8), 0.75);
        let ratio = "3 / 4";
        let tw = self.text.measure(ratio, 10) + 4;
        self.text.draw(
            c,
            ratio,
            Rect::new(mid.x + (mid.w - tw) as i32 / 2, p.y + 74, tw, 12),
            10,
            TEXT,
        );
        self.label(
            c,
            "machine_status",
            Rect::new(p.x + 14, p.y + 118, p.w.saturating_sub(28), 13),
            11,
            TEXT,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn content_icons_decode_at_the_declared_dimensions() {
        let skin = Skin::load().unwrap();
        assert_eq!(skin.icons.len(), 3);
        for icon in &skin.icons {
            assert_eq!((icon.width, icon.height), (16, 16));
        }
    }
    #[test]
    fn modal_buttons_require_press_and_release_on_the_same_target() {
        let mut state = State::default();
        state.set_mode(Mode::Pause);
        let l = Layout::new([960, 540]);
        let r = l.buttons(Mode::Pause)[0].1;
        state.cursor = [f64::from((r.x + 5) * 2), f64::from((r.y + 5) * 2)];
        assert_eq!(state.pointer(true, [960, 540]), None);
        state.cursor = [0.0, 0.0];
        assert_eq!(state.pointer(false, [960, 540]), None);
        state.cursor = [f64::from((r.x + 5) * 2), f64::from((r.y + 5) * 2)];
        state.pointer(true, [960, 540]);
        assert_eq!(state.pointer(false, [960, 540]), Some(Action::Resume));
    }
    #[test]
    fn scaled_hit_testing_agrees_with_small_and_large_layouts() {
        for size in [[640, 360], [960, 540], [1920, 1080]] {
            let l = Layout::new(size);
            let r = l.close(Mode::MachinePreview);
            let cursor = [
                f64::from((r.x + 10) * l.scale as i32),
                f64::from((r.y + 10) * l.scale as i32),
            ];
            assert_eq!(l.hit(Mode::MachinePreview, cursor), Some(Action::Resume));
            assert_eq!(l.hit(Mode::Game, cursor), None);
        }
    }
}
