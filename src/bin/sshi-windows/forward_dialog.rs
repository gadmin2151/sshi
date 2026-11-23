use std::{
    fmt::Write,
    ptr::{null, null_mut},
};

use anyhow::{Context, Result};
use iroh_ssh::forwarding::{ForwardKind, ForwardSpec};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    Graphics::Gdi::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{Controls::DRAWITEMSTRUCT, Input::KeyboardAndMouse::EnableWindow, WindowsAndMessaging::*},
};

use crate::{
    lang::tr,
    store::{last_error, wide},
    window::{set_text, text},
};

const DIALOG: usize = 4;
const KIND: i32 = 2101;
const DIAGRAM: i32 = 2102;
const BIND_HOST: i32 = 2103;
const BIND_PORT: i32 = 2104;
const TARGET_HOST: i32 = 2105;
const TARGET_PORT: i32 = 2106;
const PREVIEW: i32 = 2107;
const KIND_LABEL: i32 = 2110;
const LOCAL_NODE: i32 = 2111;
const REMOTE_NODE: i32 = 2112;
const BIND_LABEL: i32 = 2113;
const BIND_PORT_LABEL: i32 = 2114;
const TARGET_LABEL: i32 = 2115;
const TARGET_PORT_LABEL: i32 = 2116;

pub fn show(owner: HWND, server: &str) -> Result<Option<ForwardSpec>> {
    let mut state = State {
        server,
        result: None,
    };
    let result = unsafe {
        DialogBoxParamW(
            GetModuleHandleW(null()),
            DIALOG as *const u16,
            owner,
            Some(dialog_proc),
            (&mut state as *mut State<'_>) as LPARAM,
        )
    };
    if result == -1 {
        return Err(last_error()).context(tr(
            "Открыть окно проброса портов",
            "Open the port forwarding window",
        ));
    }
    Ok(state.result)
}

struct State<'a> {
    server: &'a str,
    result: Option<ForwardSpec>,
}

fn field(dialog: HWND, id: i32) -> HWND {
    unsafe { GetDlgItem(dialog, id) }
}

fn kind(dialog: HWND) -> ForwardKind {
    match unsafe { SendMessageW(field(dialog, KIND), CB_GETCURSEL, 0, 0) } {
        1 => ForwardKind::Remote,
        2 => ForwardKind::Dynamic,
        _ => ForwardKind::Local,
    }
}

fn spec(dialog: HWND) -> Result<ForwardSpec> {
    ForwardSpec::new(
        kind(dialog),
        &text(field(dialog, BIND_HOST)),
        &text(field(dialog, BIND_PORT)),
        &text(field(dialog, TARGET_HOST)),
        &text(field(dialog, TARGET_PORT)),
    )
}

fn init(dialog: HWND, state: &State<'_>) {
    set_text(dialog, tr("Проброс портов", "Port forwarding"));
    for (id, caption) in [
        (KIND_LABEL, tr("Направление", "Direction")),
        (LOCAL_NODE, tr("Этот компьютер", "This computer")),
        (
            REMOTE_NODE,
            if state.server.is_empty() {
                tr("Сервер", "Server")
            } else {
                state.server
            },
        ),
        (BIND_PORT_LABEL, tr("Порт", "Port")),
        (TARGET_PORT_LABEL, tr("Порт", "Port")),
        (IDOK, tr("Запустить туннель", "Start tunnel")),
        (IDCANCEL, tr("Отмена", "Cancel")),
    ] {
        set_text(field(dialog, id), caption);
    }
    let combo = field(dialog, KIND);
    for caption in [
        tr(
            "Локальный — открыть удалённый сервис здесь",
            "Local — access a remote service here",
        ),
        tr(
            "Обратный — открыть локальный сервис на сервере",
            "Remote — share a local service on the server",
        ),
        tr(
            "SOCKS5 — направить трафик приложения через сервер",
            "SOCKS5 — route application traffic through the server",
        ),
    ] {
        unsafe { SendMessageW(combo, CB_ADDSTRING, 0, wide(caption).as_ptr() as LPARAM) };
    }
    unsafe { SendMessageW(combo, CB_SETCURSEL, 0, 0) };
    for (id, value) in [
        (BIND_HOST, "127.0.0.1"),
        (BIND_PORT, "8080"),
        (TARGET_HOST, "127.0.0.1"),
        (TARGET_PORT, "80"),
    ] {
        set_text(field(dialog, id), value);
    }
    refresh(dialog);
}

fn address(host: &str, port: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn refresh(dialog: HWND) {
    let direction = kind(dialog);
    let remote = matches!(direction, ForwardKind::Remote);
    let socks = matches!(direction, ForwardKind::Dynamic);
    let bind_label = if remote {
        tr("Слушать на сервере", "Listen on the server")
    } else {
        tr("Слушать на этом компьютере", "Listen on this computer")
    };
    let target_label = if remote {
        tr(
            "Адрес сервиса со стороны этого компьютера",
            "Service address as seen from this computer",
        )
    } else {
        tr(
            "Адрес сервиса со стороны сервера",
            "Service address as seen from the server",
        )
    };
    set_text(field(dialog, BIND_LABEL), bind_label);
    set_text(field(dialog, TARGET_LABEL), target_label);
    for id in [TARGET_LABEL, TARGET_PORT_LABEL, TARGET_HOST, TARGET_PORT] {
        unsafe { ShowWindow(field(dialog, id), if socks { SW_HIDE } else { SW_SHOW }) };
    }
    let bind = address(
        &text(field(dialog, BIND_HOST)),
        &text(field(dialog, BIND_PORT)),
    );
    let target = address(
        &text(field(dialog, TARGET_HOST)),
        &text(field(dialog, TARGET_PORT)),
    );
    let result = spec(dialog);
    let mut preview = String::new();
    if result.is_err() {
        preview.push_str(tr(
            "Укажите имя узла или IP-адрес и порт от 1 до 65535 для каждого адреса.",
            "Enter a hostname or IP address and a port from 1 to 65535 for each address.",
        ));
    } else if socks {
        let _ = write!(
            preview,
            "{} {bind}. {}",
            tr(
                "Укажите в приложении на этом ПК SOCKS5-прокси",
                "Set this computer’s application to use SOCKS5 proxy"
            ),
            tr(
                "Адрес назначения выбирает приложение.",
                "Your application chooses the destination."
            )
        );
    } else {
        let usage = if remote {
            tr("На сервере подключайтесь к", "On the server, connect to")
        } else {
            tr(
                "На этом компьютере подключайтесь к",
                "On this computer, connect to",
            )
        };
        let _ = write!(preview, "{usage} {bind} → {target}.");
    }
    set_text(field(dialog, PREVIEW), &preview);
    unsafe {
        EnableWindow(field(dialog, IDOK), result.is_ok() as i32);
        InvalidateRect(field(dialog, DIAGRAM), null(), 1);
    }
}

fn draw(item: &DRAWITEMSTRUCT, remote: bool) {
    let dc = item.hDC;
    let r = item.rcItem;
    let h = r.bottom - r.top;
    let w = r.right - r.left;
    let y = r.top + h / 2;
    let left = r.left + w * 16 / 100;
    let right = r.right - w * 16 / 100;
    unsafe {
        let saved = SaveDC(dc);
        FillRect(dc, &r, GetSysColorBrush(COLOR_WINDOW));
        let accent = GetSysColor(COLOR_HIGHLIGHT);
        SelectObject(dc, GetStockObject(DC_PEN));
        SelectObject(dc, GetStockObject(DC_BRUSH));
        SetDCPenColor(dc, accent);
        SetDCBrushColor(dc, GetSysColor(COLOR_WINDOW));
        let half = h / 3;
        for x in [left, right] {
            RoundRect(dc, x - half, y - h / 4, x + half, y + h / 5, 4, 4);
            MoveToEx(dc, x, y + h / 5, null_mut());
            LineTo(dc, x, y + h / 3);
            MoveToEx(dc, x - half / 2, y + h / 3, null_mut());
            LineTo(dc, x + half / 2, y + h / 3);
        }
        let start = left + half + 8;
        let end = right - half - 8;
        let (tail, tip, sign) = if remote {
            (end, start, 1)
        } else {
            (start, end, -1)
        };
        MoveToEx(dc, tail, y, null_mut());
        LineTo(dc, tip, y);
        MoveToEx(dc, tip + sign * h / 7, y - h / 8, null_mut());
        LineTo(dc, tip, y);
        LineTo(dc, tip + sign * h / 7, y + h / 8);
        RestoreDC(dc, saved);
    }
}

unsafe extern "system" fn dialog_proc(
    dialog: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    match message {
        WM_INITDIALOG => {
            unsafe { SetWindowLongPtrW(dialog, GWLP_USERDATA, lparam) };
            init(dialog, unsafe { &*(lparam as *const State<'_>) });
            1
        }
        WM_DRAWITEM if wparam == DIAGRAM as usize => {
            draw(
                unsafe { &*(lparam as *const DRAWITEMSTRUCT) },
                matches!(kind(dialog), ForwardKind::Remote),
            );
            1
        }
        WM_COMMAND => {
            let id = i32::from(wparam as u16);
            let event = (wparam >> 16) as u16;
            match id {
                IDOK => {
                    if let Ok(result) = spec(dialog) {
                        let data =
                            unsafe { GetWindowLongPtrW(dialog, GWLP_USERDATA) } as *mut State<'_>;
                        unsafe {
                            (*data).result = Some(result);
                            EndDialog(dialog, 1)
                        };
                    }
                }
                IDCANCEL => {
                    unsafe { EndDialog(dialog, 0) };
                }
                KIND if event == CBN_SELCHANGE as u16 => {
                    let port = field(dialog, BIND_PORT);
                    let value = text(port);
                    match kind(dialog) {
                        ForwardKind::Dynamic if value == "8080" => set_text(port, "1080"),
                        ForwardKind::Local | ForwardKind::Remote if value == "1080" => {
                            set_text(port, "8080")
                        }
                        _ => {}
                    }
                    refresh(dialog);
                }
                BIND_HOST | BIND_PORT | TARGET_HOST | TARGET_PORT if event == EN_CHANGE as u16 => {
                    refresh(dialog)
                }
                _ => return 0,
            }
            1
        }
        WM_CLOSE => {
            unsafe { EndDialog(dialog, 0) };
            1
        }
        _ => 0,
    }
}
