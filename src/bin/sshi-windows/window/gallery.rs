use std::ptr::null_mut;

use windows_sys::Win32::{
    Foundation::{HWND, RECT},
    Graphics::Gdi::{
        CreateFontW, CreatePen, CreateSolidBrush, DT_CENTER, DT_END_ELLIPSIS, DT_NOPREFIX,
        DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawFocusRect, DrawTextW, Ellipse, FillRect,
        HBRUSH, HDC, HFONT, LineTo, MoveToEx, PS_SOLID, RoundRect, SelectObject, SetBkMode,
        SetTextColor, TRANSPARENT,
    },
    UI::{
        Controls::{ODS_DISABLED, ODS_FOCUS, ODS_SELECTED},
        WindowsAndMessaging::{DI_NORMAL, DrawIconEx, GetClientRect, HICON},
    },
};

use super::{ACTIONS, CONNECT, COPY_ENDPOINT, FILES, NEW, wide};
use crate::{lang::tr, store::Profile};

pub(super) const MARGIN: i32 = 26;
pub(super) const TOP: i32 = 54;
pub(super) const CARD_HEIGHT: i32 = 120;
pub(super) const BACKGROUND: u32 = color(0x0e1627);
pub(super) const SURFACE: u32 = color(0x151f33);
pub(super) const TEXT: u32 = color(0xf0f4ff);
pub(super) const MUTED: u32 = color(0xa9b9d4);
pub(super) const BORDER: u32 = color(0x354964);
const SELECTED: u32 = color(0x182a49);
const FOCUS: u32 = color(0x5592ff);
const BLUE: u32 = color(0x225fdf);
const BADGES: [u32; 6] = [
    color(0x225eb1),
    color(0x9a5707),
    color(0xbd2755),
    color(0x087d77),
    color(0x6943bc),
    color(0x326f9e),
];

const fn color(rgb: u32) -> u32 {
    (rgb & 0x00ff00) | ((rgb >> 16) & 0xff) | ((rgb & 0xff) << 16)
}

pub(super) fn rect(x: i32, y: i32, width: i32, height: i32) -> RECT {
    RECT {
        left: x,
        top: y,
        right: x + width,
        bottom: y + height,
    }
}

pub(super) struct Layout {
    pub width: i32,
    pub height: i32,
    pub search: RECT,
    pub gallery: RECT,
    pub dock: RECT,
    pub actions: [RECT; 3],
    pub copy: RECT,
    details_right: i32,
    endpoint: i32,
}

impl Layout {
    pub fn read(window: HWND, has_selection: bool) -> Self {
        let mut client = RECT::default();
        unsafe { GetClientRect(window, &mut client) };
        let (width, height) = (client.right, client.bottom);
        let stacked = width < 1030;
        let dock_height = if stacked { 114 } else { 70 };
        let dock = rect(
            MARGIN,
            height - dock_height - 36,
            width - 2 * MARGIN,
            dock_height,
        );
        let action_y = if stacked {
            dock.bottom - 48
        } else {
            dock.top + 15
        };
        let connect_x = dock.right - 12 - 175;
        let files_x = connect_x - 8 - 83;
        let actions_x = files_x - 8 - 103;
        let details_right = if stacked {
            dock.right - 12
        } else {
            actions_x - 13
        };
        let endpoint = dock.left + (details_right - dock.left) * 31 / 100;
        let bottom = if has_selection {
            dock.top - 15
        } else {
            height - 36
        };
        Self {
            width,
            height,
            search: rect(MARGIN, 128, width - 2 * MARGIN, 39),
            gallery: rect(MARGIN, 184, width - 2 * MARGIN, (bottom - 184).max(1)),
            dock,
            actions: [
                rect(actions_x, action_y, 103, 39),
                rect(files_x, action_y, 83, 39),
                rect(connect_x, action_y, 175, 39),
            ],
            copy: rect(endpoint + 132, dock.top + 16, 31, 33),
            details_right,
            endpoint,
        }
    }
}

pub(super) struct Style {
    pub font: HFONT,
    pub heading: HFONT,
    pub name: HFONT,
    mono: HFONT,
    small: HFONT,
    pub background: HBRUSH,
    pub surface: HBRUSH,
}

impl Style {
    pub fn new() -> Self {
        Self {
            font: font(-16, 400, "Segoe UI"),
            heading: font(-31, 700, "Segoe UI"),
            name: font(-18, 600, "Segoe UI"),
            mono: font(-13, 400, "Consolas"),
            small: font(-13, 400, "Segoe UI"),
            background: unsafe { CreateSolidBrush(BACKGROUND) },
            surface: unsafe { CreateSolidBrush(SURFACE) },
        }
    }

    pub fn workspace(&self, dc: HDC, layout: &Layout, icon: HICON, profile: Option<&Profile>) {
        unsafe {
            FillRect(
                dc,
                &rect(0, 0, layout.width, layout.height),
                self.background,
            );
            FillRect(dc, &rect(0, 0, layout.width, TOP), self.surface);
            DrawIconEx(dc, MARGIN, 12, icon, 30, 30, 0, null_mut(), DI_NORMAL);
        }
        line(dc, 0, TOP, layout.width, TOP, BORDER, 1);
        rounded(dc, layout.search, 8, SURFACE, BORDER, 1);
        let x = layout.search.left + 19;
        let y = layout.search.top + 17;
        unsafe {
            let pen = CreatePen(PS_SOLID, 2, MUTED);
            let old_pen = SelectObject(dc, pen);
            let old_brush = SelectObject(dc, self.surface);
            Ellipse(dc, x - 5, y - 5, x + 6, y + 6);
            MoveToEx(dc, x + 4, y + 4, null_mut());
            LineTo(dc, x + 9, y + 9);
            SelectObject(dc, old_brush);
            SelectObject(dc, old_pen);
            DeleteObject(pen);
        }

        if let Some(profile) = profile {
            self.dock(dc, layout, profile);
        }
    }

    fn dock(&self, dc: HDC, layout: &Layout, profile: &Profile) {
        let area = layout.dock;
        rounded(dc, area, 11, SURFACE, BORDER, 1);
        badge(dc, area.left + 13, area.top + 14, 38, profile);
        let text_x = area.left + 61;
        draw_text(
            dc,
            rect(text_x, area.top + 12, layout.endpoint - text_x - 8, 41),
            &profile.name,
            self.name,
            TEXT,
            false,
        );
        let relay_x = layout.copy.right + 12;
        for x in [layout.endpoint - 8, relay_x - 6] {
            line(dc, x, area.top + 15, x, area.top + 53, BORDER, 1);
        }
        let endpoint = profile.endpoint.to_string();
        let shortened = format!("{}…{}", &endpoint[..8], &endpoint[endpoint.len() - 7..]);
        let relay = if profile.relay.is_empty() {
            tr("Из настроек SSHI", "From SSHI settings")
        } else {
            &profile.relay
        };
        for (x, width, title, value) in [
            (layout.endpoint, 130, "Endpoint ID", shortened.as_str()),
            (relay_x, layout.details_right - relay_x, "Relay", relay),
        ] {
            draw_text(
                dc,
                rect(x, area.top + 12, width, 17),
                title,
                self.small,
                MUTED,
                false,
            );
            draw_text(
                dc,
                rect(x, area.top + 31, width, 19),
                value,
                self.mono,
                TEXT,
                false,
            );
        }
    }

    pub fn card(&self, dc: HDC, mut area: RECT, profile: &Profile, selected: bool) {
        area.right -= 12;
        area.bottom -= 12;
        rounded(
            dc,
            area,
            12,
            if selected { SELECTED } else { SURFACE },
            if selected { FOCUS } else { BORDER },
            if selected { 2 } else { 1 },
        );
        let y = (area.top + area.bottom) / 2;
        badge(dc, area.left + 14, y - 23, 46, profile);
        let text_x = area.left + 72;
        let width = area.right - text_x - 12;
        draw_text(
            dc,
            rect(text_x, y - 22, width, 24),
            &profile.name,
            self.name,
            TEXT,
            false,
        );
        let address = format!("{} · {}…", profile.user, &profile.endpoint.to_string()[..8]);
        draw_text(
            dc,
            rect(text_x, y + 5, width, 18),
            &address,
            self.mono,
            MUTED,
            false,
        );
    }

    pub fn button(&self, dc: HDC, area: RECT, title: &str, state: u32, id: u16) {
        let primary = id == NEW || id == CONNECT;
        unsafe { FillRect(dc, &area, self.surface) };
        let background = if state & ODS_SELECTED != 0 {
            SELECTED
        } else if primary {
            BLUE
        } else {
            SURFACE
        };
        rounded(
            dc,
            area,
            8,
            background,
            if primary { FOCUS } else { BORDER },
            1,
        );
        let ink = if state & ODS_DISABLED != 0 {
            MUTED
        } else {
            TEXT
        };
        let mut caption = area;
        let y = (area.top + area.bottom) / 2;
        match id {
            COPY_ENDPOINT => {
                let x = (area.left + area.right) / 2;
                rounded(dc, rect(x - 7, y - 7, 11, 11), 2, background, ink, 1);
                rounded(dc, rect(x - 3, y - 3, 11, 11), 2, background, ink, 1);
            }
            CONNECT => {
                let x = area.left + 13;
                stroke(
                    dc,
                    &[
                        (x + 8, y - 7),
                        (x, y - 7),
                        (x, y + 7),
                        (x + 14, y + 7),
                        (x + 14, y),
                    ],
                    ink,
                    1,
                );
                stroke(
                    dc,
                    &[(x + 6, y + 1), (x + 14, y - 7), (x + 9, y - 7)],
                    ink,
                    1,
                );
                line(dc, x + 14, y - 7, x + 14, y - 2, ink, 1);
                caption.left += 24;
            }
            FILES => {
                let x = area.left + 11;
                stroke(
                    dc,
                    &[
                        (x, y + 6),
                        (x, y - 6),
                        (x + 5, y - 6),
                        (x + 8, y - 3),
                        (x + 15, y - 3),
                        (x + 15, y + 6),
                        (x, y + 6),
                    ],
                    ink,
                    1,
                );
                caption.left += 21;
            }
            ACTIONS => {
                let x = area.right - 17;
                stroke(dc, &[(x - 4, y - 2), (x, y + 2), (x + 4, y - 2)], ink, 1);
                caption.right -= 20;
            }
            _ => {}
        }
        if id != COPY_ENDPOINT {
            draw_text(dc, caption, title, self.font, ink, true);
        }
        if state & ODS_FOCUS != 0 {
            let focus = rect(
                area.left + 4,
                area.top + 4,
                area.right - area.left - 8,
                area.bottom - area.top - 8,
            );
            unsafe { DrawFocusRect(dc, &focus) };
        }
    }
}

impl Drop for Style {
    fn drop(&mut self) {
        unsafe {
            for font in [self.font, self.heading, self.name, self.mono, self.small] {
                DeleteObject(font);
            }
            DeleteObject(self.background);
            DeleteObject(self.surface);
        }
    }
}

fn font(height: i32, weight: i32, family: &str) -> HFONT {
    let name = wide(family);
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            1,
            0,
            0,
            5,
            0,
            name.as_ptr(),
        )
    }
}

fn draw_text(dc: HDC, mut area: RECT, value: &str, font: HFONT, color: u32, centered: bool) {
    let value = wide(value);
    unsafe {
        let previous_font = SelectObject(dc, font);
        let previous_mode = SetBkMode(dc, TRANSPARENT as i32);
        let previous_color = SetTextColor(dc, color);
        let alignment = if centered { DT_CENTER } else { 0 };
        DrawTextW(
            dc,
            value.as_ptr(),
            -1,
            &mut area,
            alignment | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
        );
        SetTextColor(dc, previous_color);
        SetBkMode(dc, previous_mode);
        SelectObject(dc, previous_font);
    }
}

fn rounded(dc: HDC, area: RECT, radius: i32, fill: u32, border: u32, weight: i32) {
    unsafe {
        let brush = CreateSolidBrush(fill);
        let pen = CreatePen(PS_SOLID, weight, border);
        let previous_brush = SelectObject(dc, brush);
        let previous_pen = SelectObject(dc, pen);
        RoundRect(
            dc,
            area.left,
            area.top,
            area.right,
            area.bottom,
            radius * 2,
            radius * 2,
        );
        SelectObject(dc, previous_pen);
        SelectObject(dc, previous_brush);
        DeleteObject(pen);
        DeleteObject(brush);
    }
}

fn line(dc: HDC, x: i32, y: i32, to_x: i32, to_y: i32, color: u32, weight: i32) {
    stroke(dc, &[(x, y), (to_x, to_y)], color, weight);
}

fn stroke(dc: HDC, points: &[(i32, i32)], color: u32, weight: i32) {
    let Some((&(start_x, start_y), rest)) = points.split_first() else {
        return;
    };
    unsafe {
        let pen = CreatePen(PS_SOLID, weight, color);
        let previous = SelectObject(dc, pen);
        MoveToEx(dc, start_x, start_y, null_mut());
        for &(x, y) in rest {
            LineTo(dc, x, y);
        }
        SelectObject(dc, previous);
        DeleteObject(pen);
    }
}

fn badge(dc: HDC, x: i32, y: i32, size: i32, profile: &Profile) {
    // The accent is derived from the connection name on all three platforms.
    let index = profile
        .name
        .bytes()
        .enumerate()
        .fold(0, |sum, (offset, byte)| {
            (sum + (offset + 1) * usize::from(byte)) % BADGES.len()
        });
    let area = rect(x, y, size, size);
    let tint = BADGES[index];
    rounded(dc, area, 9, tint, tint, 1);
    let x = x + size / 2;
    let y = y + size / 2;
    match index {
        0 | 3 => {
            rounded(dc, rect(x - 10, y - 9, 21, 15), 3, tint, TEXT, 2);
            line(dc, x, y + 6, x, y + 11, TEXT, 2);
            line(dc, x - 5, y + 10, x + 6, y + 10, TEXT, 2);
        }
        1 | 5 => {
            for top in [y - 10, y + 1] {
                rounded(dc, rect(x - 10, top, 21, 9), 3, tint, TEXT, 2);
                line(dc, x + 3, top + 4, x + 7, top + 4, TEXT, 2);
            }
        }
        2 => {
            rounded(dc, rect(x - 7, y - 7, 15, 15), 2, tint, TEXT, 2);
            for offset in [-4, 0, 4] {
                line(dc, x - 11, y + offset, x - 7, y + offset, TEXT, 1);
                line(dc, x + 7, y + offset, x + 12, y + offset, TEXT, 1);
                line(dc, x + offset, y - 11, x + offset, y - 7, TEXT, 1);
                line(dc, x + offset, y + 7, x + offset, y + 12, TEXT, 1);
            }
        }
        _ => {
            stroke(dc, &[(x - 8, y - 5), (x - 2, y), (x - 8, y + 5)], TEXT, 2);
            line(dc, x + 1, y + 5, x + 10, y + 5, TEXT, 2);
        }
    }
}
