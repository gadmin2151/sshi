use std::{cell::RefCell, ptr::null_mut, sync::mpsc};

use iroh_ssh::file_transfer::{Event, Request};
use tokio::sync::mpsc::UnboundedSender;
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, RECT, WPARAM},
    Graphics::Gdi::{CreateFontW, DeleteObject, HFONT},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::{EM_LIMITTEXT, EM_SETREADONLY},
        Input::KeyboardAndMouse::{EnableWindow, GetKeyState, VK_CONTROL},
        Shell::{DefSubclassProc, SetWindowSubclass},
        WindowsAndMessaging::*,
    },
};

use crate::{
    lang::tr,
    store::wide,
    window::{message_box, set_text, text},
};

use super::{FileAddress, call, item, set_caption};

const PATH: i32 = 3401;
const BODY: i32 = 3402;
const STATUS: i32 = 3403;

struct Editor<'a> {
    file: FileAddress,
    original: String,
    revision: String,
    pending: Option<String>,
    saved: Option<String>,
    commands: &'a UnboundedSender<Request>,
    events: &'a mpsc::Receiver<Event>,
    font: HFONT,
}

impl Drop for Editor<'_> {
    fn drop(&mut self) {
        if !self.font.is_null() {
            unsafe { DeleteObject(self.font) };
        }
    }
}

pub(super) fn show(
    hwnd: HWND,
    file: FileAddress,
    original: String,
    revision: String,
    commands: &UnboundedSender<Request>,
    events: &mpsc::Receiver<Event>,
) -> Option<String> {
    let editor = RefCell::new(Editor {
        file,
        original,
        revision,
        pending: None,
        saved: None,
        commands,
        events,
        font: null_mut(),
    });
    let result = unsafe {
        DialogBoxParamW(
            GetModuleHandleW(null_mut()),
            9usize as *const u16,
            hwnd,
            Some(dialog),
            (&editor as *const RefCell<Editor<'_>>) as LPARAM,
        )
    };
    if result == -1 {
        message_box(
            hwnd,
            tr("Не удалось открыть редактор.", "Could not open the editor."),
            MB_OK | MB_ICONERROR,
        );
    }
    editor.into_inner().saved.take()
}

impl Editor<'_> {
    fn value(hwnd: HWND) -> String {
        text(item(hwnd, BODY)).replace("\r\n", "\n")
    }

    fn controls(&self, hwnd: HWND) {
        let idle = self.pending.is_none();
        unsafe {
            EnableWindow(
                item(hwnd, IDOK),
                (idle && Self::value(hwnd) != self.original) as i32,
            );
            EnableWindow(item(hwnd, IDCANCEL), idle as i32);
        }
        call(item(hwnd, BODY), EM_SETREADONLY, (!idle) as WPARAM, 0);
    }

    fn init(&mut self, hwnd: HWND) {
        set_text(hwnd, &self.file.name);
        set_caption(hwnd, PATH, &self.file.display());
        set_caption(hwnd, IDOK, tr("Сохранить", "Save"));
        set_caption(hwnd, IDCANCEL, tr("Закрыть", "Close"));
        let face = wide("Consolas");
        self.font =
            unsafe { CreateFontW(-17, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr()) };
        let body = item(hwnd, BODY);
        if !self.font.is_null() {
            call(body, WM_SETFONT, self.font as usize, 1);
        }
        call(body, EM_LIMITTEXT, 4 * 1024 * 1024, 0);
        set_text(body, &self.original.replace('\n', "\r\n"));
        unsafe {
            SetTimer(hwnd, 1, 45, None);
            SetWindowSubclass(body, Some(save_shortcut), 1, hwnd as usize);
        }
        self.controls(hwnd);
    }

    fn save(&mut self, hwnd: HWND) {
        let current = Self::value(hwnd);
        if current == self.original || self.pending.is_some() {
            return;
        }
        let request = Request::SaveText {
            side: self.file.side,
            path: self.file.path.clone(),
            name: self.file.name.clone(),
            text: current.clone(),
            revision: self.revision.clone(),
        };
        if self.commands.send(request).is_err() {
            set_caption(
                hwnd,
                STATUS,
                tr("Файловый сеанс завершён.", "The file session ended."),
            );
            return;
        }
        self.pending = Some(current);
        set_caption(hwnd, STATUS, tr("Сохраняем…", "Saving…"));
        self.controls(hwnd);
    }

    fn poll(&mut self, hwnd: HWND) {
        if self.pending.is_none() {
            return;
        }
        match self.events.try_recv() {
            Ok(Event::Saved { revision, warning }) => {
                self.revision = revision;
                if let Some(value) = self.pending.take() {
                    self.original = value;
                }
                let status = warning.unwrap_or_else(|| tr("Сохранено.", "Saved.").into());
                set_caption(hwnd, STATUS, &status);
                self.saved = Some(status);
            }
            Ok(Event::Error { message }) => {
                self.pending = None;
                set_caption(hwnd, STATUS, &message);
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.pending = None;
                set_caption(
                    hwnd,
                    STATUS,
                    tr("Файловый сеанс завершён.", "The file session ended."),
                );
            }
            _ => return,
        }
        self.controls(hwnd);
    }

    fn close(&self, hwnd: HWND) {
        if self.pending.is_some() {
            return;
        }
        if Self::value(hwnd) == self.original
            || message_box(
                hwnd,
                tr("Закрыть без сохранения?", "Close without saving?"),
                MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
            ) == IDYES
        {
            unsafe { EndDialog(hwnd, 0) };
        }
    }
}

fn arrange(hwnd: HWND) {
    let mut bounds = RECT::default();
    let mut unit = RECT {
        left: 0,
        top: 0,
        right: 4,
        bottom: 8,
    };
    unsafe {
        GetClientRect(hwnd, &mut bounds);
        MapDialogRect(hwnd, &mut unit);
    }
    let (x, y) = (unit.right, unit.bottom);
    let (w, h) = (bounds.right, bounds.bottom);
    if w < 30 * x || h < 15 * y {
        return;
    }
    let (pad, button) = (3 * x, 15 * x);
    for (id, left, top, width, height) in [
        (PATH, pad, y, w - 2 * pad, 2 * y),
        (BODY, pad, 3 * y, w - 2 * pad, h - 10 * y),
        (STATUS, pad, h - 6 * y, w - 2 * pad, 2 * y),
        (IDOK, w - 2 * pad - 2 * button, h - 3 * y, button, 2 * y),
        (IDCANCEL, w - pad - button, h - 3 * y, button, 2 * y),
    ] {
        unsafe { MoveWindow(item(hwnd, id), left, top, width, height, 1) };
    }
}

unsafe extern "system" fn dialog(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    if msg == WM_INITDIALOG {
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp) };
        let state = unsafe { &*(lp as *const RefCell<Editor<'_>>) };
        state.borrow_mut().init(hwnd);
        return 1;
    }
    let state = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const RefCell<Editor<'_>>;
    let Some(state) = (unsafe { state.as_ref() }) else {
        return 0;
    };
    let Ok(mut editor) = state.try_borrow_mut() else {
        return 0;
    };
    match msg {
        WM_SIZE => arrange(hwnd),
        WM_TIMER => editor.poll(hwnd),
        WM_CLOSE => editor.close(hwnd),
        WM_COMMAND => match (wp & 0xffff) as i32 {
            IDOK => editor.save(hwnd),
            IDCANCEL => editor.close(hwnd),
            BODY if ((wp >> 16) & 0xffff) as u32 == EN_CHANGE => editor.controls(hwnd),
            _ => return 0,
        },
        _ => return 0,
    }
    1
}

unsafe extern "system" fn save_shortcut(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    parent: usize,
) -> isize {
    if msg == WM_KEYDOWN && wp == b'S' as usize && unsafe { GetKeyState(VK_CONTROL as i32) } < 0 {
        call(parent as HWND, WM_COMMAND, IDOK as WPARAM, 0);
        return 0;
    }
    if msg == WM_CHAR && wp == 19 {
        return 0;
    }
    unsafe { DefSubclassProc(hwnd, msg, wp, lp) }
}
