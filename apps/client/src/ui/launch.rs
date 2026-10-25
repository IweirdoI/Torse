//! Startup pages share one visual language and return typed UI actions.
use super::{Action, Canvas, Layer, Layout, Rect, View};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Home,
    Worlds,
    Multiplayer,
    Settings,
    About,
}
impl Page {
    pub const ALL: [Self; 5] = [
        Self::Home,
        Self::Worlds,
        Self::Multiplayer,
        Self::Settings,
        Self::About,
    ];
    pub fn next(self, delta: i32) -> Self {
        let i = Self::ALL.iter().position(|p| *p == self).unwrap() as i32;
        Self::ALL[(i + delta).rem_euclid(5) as usize]
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchState {
    pub address: String,
    pub address_focus: bool,
    pub select_address: bool,
    pub radius: u32,
    pub fullscreen: bool,
    pub in_session: bool,
    pub connecting: bool,
    pub status: Option<&'static str>,
}
impl Default for LaunchState {
    fn default() -> Self {
        Self {
            address: "127.0.0.1:7777".into(),
            address_focus: false,
            select_address: false,
            radius: 4,
            fullscreen: false,
            in_session: false,
            connecting: false,
            status: None,
        }
    }
}
impl LaunchState {
    pub fn type_text(&mut self, text: &str) {
        if !self.address_focus || self.connecting {
            return;
        }
        let filtered: String = text
            .chars()
            .take(1024)
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-' | '[' | ']'))
            .collect();
        if filtered.is_empty() {
            return;
        }
        if self.select_address {
            self.address.clear();
            self.select_address = false;
        }
        for c in filtered.chars() {
            if self.address.len() < 255 {
                self.address.push(c);
            }
        }
        self.status = None;
    }
    pub fn backspace(&mut self) {
        if self.address_focus && !self.connecting {
            if self.select_address {
                self.address.clear();
                self.select_address = false;
            } else {
                self.address.pop();
            }
            self.status = None;
        }
    }
}

const ACCENT: [u8; 4] = [178, 211, 170, 255];
const WHITE: [u8; 4] = [245, 246, 234, 255];
const INK: [u8; 4] = [18, 40, 28, 255];
const SECONDARY: [u8; 4] = [188, 203, 183, 255];
fn main_rect(l: &Layout) -> Rect {
    let width = l.width.saturating_sub(64).min(716);
    let height = if l.height < 460 { 168 } else { 272 };
    Rect::new(
        (l.width - width) as i32 / 2,
        ((l.height as i32 - 150 - height as i32) / 2).max(60),
        width,
        height,
    )
}
fn primary(l: &Layout) -> Rect {
    Rect::new(l.width as i32 / 2 - 56, l.height as i32 - 84, 112, 48)
}
pub(super) fn buttons(l: &Layout, page: Page) -> Vec<(Action, Rect, &'static str)> {
    let y = l.height as i32 - 82;
    let cx = l.width as i32 / 2;
    let mut out = vec![
        (
            Action::Navigate(Page::Home),
            Rect::new(cx - 244, y, 44, 44),
            "launch_home",
        ),
        (
            Action::Navigate(Page::Worlds),
            Rect::new(cx - 180, y, 44, 44),
            "launch_worlds",
        ),
        (
            Action::Navigate(Page::Multiplayer),
            Rect::new(cx - 116, y, 44, 44),
            "launch_multi",
        ),
        (
            if page == Page::Multiplayer {
                Action::Connect
            } else {
                Action::Play
            },
            primary(l),
            if page == Page::Multiplayer {
                "launch_connect_short"
            } else {
                "launch_play"
            },
        ),
        (
            Action::Navigate(Page::Settings),
            Rect::new(cx + 72, y, 44, 44),
            "launch_settings",
        ),
        (
            Action::Navigate(Page::About),
            Rect::new(cx + 136, y, 44, 44),
            "launch_about",
        ),
        (Action::Quit, Rect::new(cx + 200, y, 44, 44), "launch_quit"),
    ];
    let r = main_rect(l);
    match page {
        Page::Multiplayer => out.push((
            Action::Address,
            Rect::new(r.x + 24, r.y + 76, r.w - 48, 42),
            "",
        )),
        Page::Settings => {
            let right = r.x + r.w as i32 - 24;
            out.extend([
                (
                    Action::RadiusDown,
                    Rect::new(right - 130, r.y + 54, 34, 34),
                    "launch_minus",
                ),
                (
                    Action::RadiusUp,
                    Rect::new(right - 34, r.y + 54, 34, 34),
                    "launch_plus",
                ),
                (
                    Action::Fullscreen,
                    Rect::new(right - 130, r.y + 112, 130, 34),
                    "launch_toggle",
                ),
            ]);
        }
        _ => {}
    }
    out
}
fn line(c: &mut Canvas, a: [f32; 2], b: [f32; 2], color: [u8; 4]) {
    c.stroke(a, b, 1.5, color);
}
fn icon(c: &mut Canvas, p: Page, x: f32, y: f32, size: f32, color: [u8; 4]) {
    let paths: &[&[[i32; 2]]] = match p {
        Page::Home => &[
            &[[0, 8], [8, 1], [16, 8]],
            &[[3, 8], [3, 17], [13, 17], [13, 8]],
        ],
        Page::Worlds => &[
            &[
                [0, 4],
                [8, 0],
                [16, 4],
                [8, 8],
                [0, 4],
                [0, 13],
                [8, 17],
                [16, 13],
                [16, 4],
            ],
            &[[8, 8], [8, 17]],
        ],
        Page::Multiplayer => &[
            &[[1, 2], [15, 2], [15, 8], [1, 8], [1, 2]],
            &[[8, 8], [8, 14]],
            &[[0, 17], [0, 14], [16, 14], [16, 17]],
        ],
        Page::Settings => &[
            &[[1, 4], [16, 4]],
            &[[1, 12], [16, 12]],
            &[[5, 1], [5, 7]],
            &[[12, 9], [12, 15]],
        ],
        Page::About => &[
            &[
                [2, 0],
                [14, 0],
                [17, 4],
                [17, 14],
                [14, 17],
                [2, 17],
                [0, 14],
                [0, 4],
                [2, 0],
            ],
            &[[8, 7], [8, 13]],
            &[[8, 3], [8, 4]],
        ],
    };
    for path in paths {
        for pair in path.windows(2) {
            line(
                c,
                [
                    x + pair[0][0] as f32 * size / 18.0,
                    y + pair[0][1] as f32 * size / 18.0,
                ],
                [
                    x + pair[1][0] as f32 * size / 18.0,
                    y + pair[1][1] as f32 * size / 18.0,
                ],
                color,
            );
        }
    }
}

impl Layer {
    pub(super) fn paint_launch(&mut self, c: &mut Canvas, l: &Layout, v: &View, page: Page) {
        if self.background.as_ref().is_none_or(|bg| {
            bg.pixel_width != c.pixel_width
                || bg.pixel_height != c.pixel_height
                || bg.density != c.density
        }) {
            let mut bg = c.empty_like();
            bg.cover(&self.forest);
            bg.fill(Rect::new(0, 0, l.width, l.height), [5, 16, 10, 42]);
            for y in 0..l.height {
                let edge = ((y as f32 / l.height as f32 - 0.36).abs() * 95.0) as u8;
                bg.fill(Rect::new(0, y as i32, l.width, 1), [3, 14, 7, edge]);
            }
            self.background = Some(bg);
        }
        c.pixels
            .copy_from_slice(&self.background.as_ref().unwrap().pixels);
        self.label(
            c,
            "launch_version",
            Rect::new(28, 22, 230, 22),
            12,
            [224, 234, 214, 210],
        );
        let player = self.labels.get("launch_player").copied().unwrap_or("");
        let player_width = self.text.measure(player, 12);
        self.text.draw(
            c,
            player,
            Rect::new(
                l.width as i32 - player_width as i32 - 28,
                22,
                player_width,
                22,
            ),
            12,
            [224, 234, 214, 210],
        );
        let mut content = c.empty_like();
        self.paint_launch_content(&mut content, l, v, page);
        let progress = self.frame_motion.page;
        c.composite(&content, (1.0 - progress) * 16.0, progress);
        self.paint_dock(c, l, v, page);
        if v.loading.is_some() || v.launch.connecting {
            c.fill(Rect::new(0, 0, l.width, l.height), [3, 13, 8, 185]);
            let cx = l.width as i32 / 2;
            let cy = l.height as i32 / 2 - 18;
            for i in 0..24 {
                let angle = self.frame_motion.angle + i as f32 * std::f32::consts::TAU / 24.0;
                let alpha = (40 + i * 8) as u8;
                let a = [
                    cx as f32 + angle.cos() * 16.0,
                    cy as f32 + angle.sin() * 16.0,
                ];
                let b = [
                    cx as f32 + angle.cos() * 21.0,
                    cy as f32 + angle.sin() * 21.0,
                ];
                line(c, a, b, [206, 226, 192, alpha]);
            }
            let label = if v.launch.connecting {
                "launch_connecting"
            } else if matches!(v.loading, Some(Action::Navigate(_))) {
                "launch_opening_page"
            } else {
                "launch_entering_world"
            };
            self.center_label(c, label, Rect::new(0, cy + 42, l.width, 30), 18, WHITE);
        }
    }
    fn paint_dock(&mut self, c: &mut Canvas, l: &Layout, v: &View, page: Page) {
        let dock = Rect::new(l.width as i32 / 2 - 260, l.height as i32 - 98, 520, 78);
        c.rounded(
            Rect::new(dock.x, dock.y + 3, dock.w, dock.h),
            15,
            [0, 0, 0, 45],
        );
        c.rounded(dock, 15, [11, 28, 18, 220]);
        c.fill(
            Rect::new(dock.x + 20, dock.y, dock.w - 40, 1),
            [180, 208, 167, 70],
        );
        let controls = buttons(l, page);
        for (i, (action, base, key)) in controls.into_iter().take(7).enumerate() {
            let hover = self.frame_motion.hover[i];
            let grow = hover * if i == 3 { 8.0 } else { 10.0 };
            let lift = hover * 7.0;
            let rect = [
                base.x as f32 - grow / 2.0,
                base.y as f32 - grow / 2.0 - lift,
                base.w as f32 + grow,
                base.h as f32 + grow,
            ];
            let selected = matches!(action, Action::Navigate(p) if p == page);
            let pressed = v.pressed == Some(action) && v.hover == Some(action);
            let color = if i == 3 {
                if pressed {
                    [139, 174, 130, 255]
                } else {
                    [192, 215, 177, 255]
                }
            } else if selected {
                [69, 95, 59, 235]
            } else {
                [42, 66, 44, (100.0 + 120.0 * hover) as u8]
            };
            c.rounded_at(rect, if i == 3 { 8.0 } else { rect[2] / 2.0 }, color);
            if i == 3 {
                let label = if action == Action::Connect {
                    "launch_connect_short"
                } else if v.launch.in_session && page != Page::Worlds {
                    "launch_resume"
                } else {
                    "launch_play"
                };
                self.center_label_at(c, label, rect, 15.0 + hover * 1.5, INK);
            } else {
                let size = 19.0 + hover * 4.0;
                let x = rect[0] + (rect[2] - size) / 2.0;
                let y = rect[1] + (rect[3] - size) / 2.0;
                if let Action::Navigate(item) = action {
                    icon(c, item, x, y, size, WHITE);
                } else {
                    line(c, [x + 4.0, y + 1.0], [x + 4.0, y + size - 1.0], WHITE);
                    line(
                        c,
                        [x + 4.0, y + size - 1.0],
                        [x + size - 1.0, y + size - 1.0],
                        WHITE,
                    );
                    line(
                        c,
                        [x + 9.0, y + size / 2.0],
                        [x + size, y + size / 2.0],
                        WHITE,
                    );
                    line(
                        c,
                        [x + size - 4.0, y + size / 2.0 - 4.0],
                        [x + size, y + size / 2.0],
                        WHITE,
                    );
                    line(
                        c,
                        [x + size - 4.0, y + size / 2.0 + 4.0],
                        [x + size, y + size / 2.0],
                        WHITE,
                    );
                }
                if selected {
                    c.rounded(Rect::new(base.x + 19, dock.y + 66, 6, 3), 1, ACCENT);
                }
            }
            if hover > 0.12 {
                let label = if i == 3
                    && v.launch.in_session
                    && action == Action::Play
                    && page != Page::Worlds
                {
                    "launch_resume"
                } else {
                    key
                };
                let text = self.labels.get(label).copied().unwrap_or("?");
                let width = self.text.measure(text, 13) + 24;
                let tip = [
                    base.x as f32 + (base.w as f32 - width as f32) / 2.0,
                    base.y as f32 - 47.0 - lift,
                    width as f32,
                    29.0,
                ];
                c.rounded_at(tip, 5.0, [9, 24, 14, (230.0 * hover) as u8]);
                self.center_label_at(c, label, tip, 13.0, [245, 246, 234, (255.0 * hover) as u8]);
            }
        }
    }
    fn paint_launch_content(&mut self, c: &mut Canvas, l: &Layout, v: &View, page: Page) {
        let compact = l.height < 460;
        if page == Page::Home {
            let y = if compact {
                72
            } else {
                (l.height as i32 * 23 / 100).max(110)
            };
            let font = if compact { 52 } else { 80 };
            self.center_label(
                c,
                "launch_brand",
                Rect::new(0, y + 2, l.width, 100),
                font,
                [0, 0, 0, 90],
            );
            self.center_label(
                c,
                "launch_brand",
                Rect::new(0, y, l.width, 100),
                font,
                WHITE,
            );
            let subtitle = y + if compact { 94 } else { 112 };
            c.fill(
                Rect::new(l.width as i32 / 2 - 24, subtitle - 11, 48, 1),
                [216, 231, 203, 170],
            );
            self.center_label(
                c,
                "launch_tagline",
                Rect::new(0, subtitle, l.width, 30),
                16,
                [229, 237, 216, 255],
            );
            if !compact && v.launch.in_session {
                self.center_label(
                    c,
                    if v.network {
                        "launch_online_sub"
                    } else {
                        "launch_world_name"
                    },
                    Rect::new(0, subtitle + 42, l.width, 25),
                    12,
                    SECONDARY,
                );
            }
            return;
        }
        let r = main_rect(l);
        c.rounded(Rect::new(r.x, r.y + 5, r.w, r.h), 10, [0, 0, 0, 40]);
        c.rounded(r, 10, [15, 35, 23, 230]);
        c.fill(Rect::new(r.x + 24, r.y, r.w - 48, 1), [177, 204, 158, 95]);
        let title = match page {
            Page::Worlds => "launch_worlds",
            Page::Multiplayer => "launch_multi",
            Page::Settings => "launch_settings",
            _ => "launch_about",
        };
        self.label(
            c,
            title,
            Rect::new(r.x + 24, r.y + 16, r.w - 48, 30),
            22,
            WHITE,
        );
        match page {
            Page::Worlds => {
                c.rounded(Rect::new(r.x + 24, r.y + 62, 62, 62), 7, [62, 88, 52, 255]);
                icon(
                    c,
                    Page::Worlds,
                    (r.x + 44) as f32,
                    (r.y + 82) as f32,
                    22.0,
                    ACCENT,
                );
                self.label(
                    c,
                    "launch_world_name",
                    Rect::new(r.x + 106, r.y + 64, r.w - 130, 28),
                    20,
                    WHITE,
                );
                self.label(
                    c,
                    "launch_world_detail",
                    Rect::new(r.x + 106, r.y + 99, r.w - 130, 24),
                    13,
                    SECONDARY,
                );
                self.label(
                    c,
                    "launch_world_notice",
                    Rect::new(
                        r.x + 24,
                        r.y + if compact { 134 } else { 155 },
                        r.w - 48,
                        25,
                    ),
                    13,
                    SECONDARY,
                );
                if !compact {
                    self.label(
                        c,
                        "launch_world_controls",
                        Rect::new(r.x + 24, r.y + 208, r.w - 48, 24),
                        13,
                        SECONDARY,
                    );
                }
            }
            Page::Multiplayer => {
                self.label(
                    c,
                    "launch_address_label",
                    Rect::new(r.x + 24, r.y + 48, r.w - 48, 22),
                    12,
                    SECONDARY,
                );
                let field = buttons(l, page)
                    .into_iter()
                    .find(|b| b.0 == Action::Address)
                    .unwrap()
                    .1;
                c.rounded(
                    field,
                    5,
                    if v.launch.select_address {
                        [68, 100, 61, 255]
                    } else {
                        [29, 52, 35, 255]
                    },
                );
                c.fill(
                    Rect::new(field.x + 4, field.y + field.h as i32 - 2, field.w - 8, 1),
                    if v.launch.address_focus {
                        ACCENT
                    } else {
                        [111, 136, 100, 255]
                    },
                );
                let mut address = v.launch.address.as_str();
                while self.text.measure(address, 16) > field.w.saturating_sub(32)
                    && !address.is_empty()
                {
                    address = &address[1..];
                }
                self.text.draw(
                    c,
                    address,
                    Rect::new(field.x + 12, field.y + 10, field.w - 24, 26),
                    16,
                    WHITE,
                );
                if v.launch.address_focus && !v.launch.select_address {
                    let end = self.text.measure(address, 16) as i32;
                    c.fill(Rect::new(field.x + 14 + end, field.y + 9, 1, 24), ACCENT);
                }
                if !compact {
                    self.label(
                        c,
                        "launch_connect_note",
                        Rect::new(r.x + 24, r.y + 140, r.w - 48, 28),
                        13,
                        SECONDARY,
                    );
                }
                if let Some(status) = v.launch.status {
                    self.label(
                        c,
                        status,
                        Rect::new(
                            r.x + 24,
                            r.y + if compact { 136 } else { 203 },
                            r.w - 48,
                            24,
                        ),
                        13,
                        [244, 190, 157, 255],
                    );
                }
            }
            Page::Settings => {
                let right = r.x + r.w as i32 - 24;
                for (offset, label, note) in [
                    (54, "launch_distance", "launch_distance_note"),
                    (112, "launch_display", "launch_display_note"),
                ] {
                    self.label(
                        c,
                        label,
                        Rect::new(r.x + 24, r.y + offset, r.w - 210, 25),
                        16,
                        WHITE,
                    );
                    if !compact {
                        self.label(
                            c,
                            note,
                            Rect::new(r.x + 24, r.y + offset + 26, r.w - 210, 24),
                            12,
                            SECONDARY,
                        );
                    }
                }
                self.text.draw(
                    c,
                    &v.launch.radius.to_string(),
                    Rect::new(right - 80, r.y + 61, 30, 25),
                    17,
                    WHITE,
                );
                for (action, rect, key) in buttons(l, page).into_iter().skip(7) {
                    c.rounded(
                        rect,
                        4,
                        if v.hover == Some(action) {
                            [83, 112, 70, 255]
                        } else {
                            [46, 72, 45, 255]
                        },
                    );
                    let key = if action == Action::Fullscreen {
                        if v.launch.fullscreen {
                            "launch_fullscreen"
                        } else {
                            "launch_windowed"
                        }
                    } else {
                        key
                    };
                    self.center_label(c, key, rect, 14, ACCENT);
                }
                if !compact {
                    self.label(
                        c,
                        "launch_settings_note",
                        Rect::new(r.x + 24, r.y + 220, r.w - 48, 24),
                        12,
                        SECONDARY,
                    );
                }
            }
            Page::About => {
                self.label(
                    c,
                    "launch_about_text",
                    Rect::new(r.x + 24, r.y + 65, r.w - 48, 28),
                    16,
                    WHITE,
                );
                self.label(
                    c,
                    "launch_about_license",
                    Rect::new(r.x + 24, r.y + 111, r.w - 48, 24),
                    13,
                    SECONDARY,
                );
                if !compact {
                    self.label(
                        c,
                        "launch_about_keys",
                        Rect::new(r.x + 24, r.y + 198, r.w - 48, 30),
                        13,
                        SECONDARY,
                    );
                }
            }
            _ => {}
        }
    }
    fn center_label(&mut self, c: &mut Canvas, key: &str, rect: Rect, size: u16, color: [u8; 4]) {
        self.center_label_at(
            c,
            key,
            [rect.x as f32, rect.y as f32, rect.w as f32, rect.h as f32],
            size as f32,
            color,
        );
    }
    fn center_label_at(
        &mut self,
        c: &mut Canvas,
        key: &str,
        rect: [f32; 4],
        size: f32,
        color: [u8; 4],
    ) {
        let value = self.labels.get(key).copied().unwrap_or("?");
        let width = self
            .text
            .measure_at(value, size)
            .min((rect[2] - 12.0).max(0.0));
        self.text.draw_at(
            c,
            value,
            [
                rect[0] + (rect[2] - width) / 2.0,
                rect[1] + (rect[3] - size) / 2.0 - 1.0,
                width + 0.5,
                size + 7.0,
            ],
            size,
            color,
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn address_editing_replaces_selection_and_bounds_pasted_text() {
        let mut state = LaunchState {
            address_focus: true,
            select_address: true,
            ..Default::default()
        };
        state.type_text(" [::1]:7777\n");
        assert_eq!(state.address, "[::1]:7777");
        state.backspace();
        assert_eq!(state.address, "[::1]:777");
        state.select_address = true;
        state.backspace();
        assert!(state.address.is_empty());
        state.type_text(&"x".repeat(2000));
        assert_eq!(state.address.len(), 255);
        state.connecting = true;
        state.backspace();
        state.type_text("y");
        assert_eq!(
            state.address.len(),
            255,
            "active connection freezes its address"
        );
    }
    #[test]
    fn physical_pointer_tracks_controls_across_display_scales() {
        for dpi in [1.0, 1.25, 1.5, 2.0] {
            for size in [[1120, 680], [1920, 1080], [2560, 1440]] {
                for page in Page::ALL {
                    let mode = super::super::Mode::Launch(page);
                    let layout = Layout::for_display(size, mode, dpi);
                    for (action, rect, _) in buttons(&layout, page) {
                        let physical = [
                            (rect.x as f64 + rect.w as f64 / 2.0) * layout.density as f64,
                            (rect.y as f64 + rect.h as f64 / 2.0) * layout.density as f64,
                        ];
                        assert_eq!(layout.hit(mode, physical), Some(action), "{dpi} {size:?}");
                    }
                }
            }
        }
    }
    #[test]
    fn startup_controls_remain_separate_and_on_screen_at_supported_sizes() {
        for size in [
            [640, 360],
            [960, 540],
            [1280, 720],
            [1920, 1080],
            [2560, 1440],
        ] {
            for page in Page::ALL {
                let mode = super::super::Mode::Launch(page);
                let layout = Layout::for_mode(size, mode);
                let controls = buttons(&layout, page);
                for (index, (action, r, _)) in controls.iter().enumerate() {
                    assert!(
                        r.x >= 0
                            && r.y >= 0
                            && r.x + r.w as i32 <= layout.width as i32
                            && r.y + r.h as i32 <= layout.height as i32,
                        "{size:?} {page:?} {action:?}"
                    );
                    assert_eq!(
                        layout.hit(
                            mode,
                            [
                                (f64::from(r.x) + 5.0) * f64::from(layout.density),
                                (f64::from(r.y) + 5.0) * f64::from(layout.density)
                            ]
                        ),
                        Some(*action)
                    );
                    for (_, other, _) in &controls[index + 1..] {
                        assert!(
                            r.x + r.w as i32 <= other.x
                                || other.x + other.w as i32 <= r.x
                                || r.y + r.h as i32 <= other.y
                                || other.y + other.h as i32 <= r.y
                        );
                    }
                }
            }
        }
    }
}
