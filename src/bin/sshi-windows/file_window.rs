use std::{cell::RefCell, mem::size_of, ptr::null_mut, sync::mpsc, thread};

use crate::{
    lang::{tr, tr_format},
    store::{Profile, Store, last_error, wide},
    terminal::{self, FileConnection, LoginInput, LoginQuestion, QuestionKind},
    window::{message_box, set_text, text},
};
use anyhow::{Result, ensure};
use iroh_ssh::{
    file_transfer::{self, Entry, Event, Kind, Listing, Request, Side},
    run_async,
};
use tokio::sync::mpsc as async_mpsc;
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, POINT, RECT, WPARAM},
    Graphics::Gdi::{CreateFontW, DeleteObject, HFONT, PtInRect},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::{
            EM_LIMITTEXT, EM_SETPASSWORDCHAR, ICC_LISTVIEW_CLASSES, ICC_PROGRESS_CLASS,
            INITCOMMONCONTROLSEX, InitCommonControlsEx, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW,
            LVIF_TEXT, LVITEMW, LVM_DELETEALLITEMS, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW,
            LVM_INSERTITEMW, LVM_SETCOLUMNWIDTH, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMTEXTW,
            LVN_BEGINDRAG, LVN_ITEMCHANGED, LVNI_SELECTED, LVS_EX_DOUBLEBUFFER,
            LVS_EX_FULLROWSELECT, NM_DBLCLK, NM_RETURN, NMHDR, NMLISTVIEW, PBM_SETPOS,
            PBM_SETRANGE32,
        },
        Input::KeyboardAndMouse::{EnableWindow, ReleaseCapture, SetCapture},
        WindowsAndMessaging::*,
    },
};

const TITLE: i32 = 3001;
const UPLOAD: i32 = 3002;
const DOWNLOAD: i32 = 3003;
const STATUS: i32 = 3004;
const PROGRESS: i32 = 3005;
const HINT: i32 = 3006;
const LOCAL: i32 = 3100;
const REMOTE: i32 = 3200;
const PARENT: i32 = 1;
const ADDRESS: i32 = 2;
const ROWS: i32 = 3;
const GO: i32 = 4;
const REFRESH: i32 = 5;
const PROMPT: i32 = 3301;
const ANSWER: i32 = 3302;

struct View {
    name: String,
    commands: async_mpsc::UnboundedSender<Request>,
    events: mpsc::Receiver<Event>,
    questions: mpsc::Receiver<LoginQuestion>,
    local: Option<Listing>,
    remote: Option<Listing>,
    busy: bool,
    copying: Option<Side>,
    finished: Option<String>,
    drag: Option<(Side, Vec<String>)>,
    heading: HFONT,
}

impl Drop for View {
    fn drop(&mut self) {
        if !self.heading.is_null() {
            unsafe { DeleteObject(self.heading) };
        }
    }
}

pub fn show(store: Store, profile: Profile) -> Result<()> {
    let controls = INITCOMMONCONTROLSEX {
        dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_LISTVIEW_CLASSES | ICC_PROGRESS_CLASS,
    };
    ensure!(
        unsafe { InitCommonControlsEx(&controls) } != 0,
        "initialize file controls"
    );
    let (commands, requests) = async_mpsc::unbounded_channel();
    let (reports, events) = mpsc::channel();
    let (prompts, questions) = mpsc::channel();
    let name = profile.name.clone();
    thread::Builder::new()
        .name("sshi-files".into())
        .spawn(move || {
            let _ = run_async(work(
                store,
                profile,
                requests,
                reports,
                LoginInput::Desktop(prompts),
            ));
        })?;
    let view = RefCell::new(View {
        name,
        commands,
        events,
        questions,
        local: None,
        remote: None,
        busy: false,
        copying: None,
        finished: None,
        drag: None,
        heading: null_mut(),
    });
    let result = unsafe {
        DialogBoxParamW(
            GetModuleHandleW(null_mut()),
            7usize as *const u16,
            null_mut(),
            Some(dialog),
            (&view as *const RefCell<View>) as LPARAM,
        )
    };
    ensure!(result != -1, "open the SSHI file window: {}", last_error());
    Ok(())
}

async fn work(
    store: Store,
    profile: Profile,
    mut requests: async_mpsc::UnboundedReceiver<Request>,
    events: mpsc::Sender<Event>,
    input: LoginInput,
) -> Result<()> {
    let mut connection: Option<FileConnection> = None;
    while let Some(request) = requests.recv().await {
        let result = async {
            if request.reconnects() {
                connection = None;
            }
            if request.needs_sftp() && connection.is_none() {
                connection = Some(terminal::open_files(&store, &profile, input.clone()).await?);
            }
            file_transfer::handle(request, connection.as_ref().map(|c| &c.sftp), |event| {
                let _ = events.send(event);
            })
            .await
        }
        .await;
        if events
            .send(result.unwrap_or_else(|error| Event::Error {
                message: format!("{error:#}"),
            }))
            .is_err()
        {
            break;
        }
    }
    if let Some(connection) = connection {
        let _ = connection.sftp.close().await;
    }
    Ok(())
}

fn number(side: Side) -> i32 {
    if side == Side::Local { LOCAL } else { REMOTE }
}

fn other(side: Side) -> Side {
    if side == Side::Local {
        Side::Remote
    } else {
        Side::Local
    }
}

fn item(dialog: HWND, id: i32) -> HWND {
    unsafe { GetDlgItem(dialog, id) }
}

fn call(hwnd: HWND, message: u32, first: WPARAM, second: LPARAM) -> isize {
    unsafe { SendMessageW(hwnd, message, first, second) }
}

fn set_caption(hwnd: HWND, id: i32, value: &str) {
    set_text(item(hwnd, id), value)
}

fn listing_size(value: Option<u64>) -> String {
    let Some(bytes) = value else {
        return String::new();
    };
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let kib = bytes as f64 / 1024.0;
    if kib < 1024.0 {
        format!("{kib:.1} KiB")
    } else if kib < 1024.0 * 1024.0 {
        format!("{:.1} MiB", kib / 1024.0)
    } else {
        format!("{:.1} GiB", kib / (1024.0 * 1024.0))
    }
}

impl View {
    fn init(&mut self, hwnd: HWND) {
        set_text(
            hwnd,
            &tr_format!("SSHI — Файлы · {}", "SSHI — Files · {}", self.name),
        );
        set_caption(hwnd, TITLE, tr("Передача файлов", "File transfer"));
        set_caption(
            hwnd,
            HINT,
            tr(
                "Перетащите файлы в соседнюю панель",
                "Drag files to the other pane",
            ),
        );
        set_caption(hwnd, LOCAL, tr("Этот компьютер", "This computer"));
        set_caption(hwnd, REMOTE, &self.name);
        set_caption(hwnd, UPLOAD, tr("Загрузить →", "Upload →"));
        set_caption(hwnd, DOWNLOAD, tr("← Скачать", "← Download"));
        let face = wide("Segoe UI");
        self.heading =
            unsafe { CreateFontW(-22, 0, 0, 0, 600, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr()) };
        if !self.heading.is_null() {
            call(item(hwnd, TITLE), WM_SETFONT, self.heading as usize, 1);
        }
        for side in [LOCAL, REMOTE] {
            set_caption(hwnd, side + PARENT, tr("Вверх", "Up"));
            set_caption(hwnd, side + GO, tr("Перейти", "Go"));
            set_caption(hwnd, side + REFRESH, tr("Обнов.", "Refresh"));
            let view = item(hwnd, side + ROWS);
            call(
                view,
                LVM_SETEXTENDEDLISTVIEWSTYLE,
                0,
                (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as LPARAM,
            );
            for (idx, name, width) in [(0, tr("Имя", "Name"), 280), (1, tr("Размер", "Size"), 80)]
            {
                let mut name = wide(name);
                let column = LVCOLUMNW {
                    mask: LVCF_TEXT | LVCF_WIDTH,
                    pszText: name.as_mut_ptr(),
                    cx: width,
                    ..Default::default()
                };
                call(
                    view,
                    LVM_INSERTCOLUMNW,
                    idx,
                    (&column as *const LVCOLUMNW) as LPARAM,
                );
            }
        }
        call(item(hwnd, PROGRESS), PBM_SETRANGE32, 0, 100);
        unsafe { ShowWindow(item(hwnd, PROGRESS), SW_HIDE) };
        unsafe { SetTimer(hwnd, 1, 75, None) };
        self.arrange(hwnd);
        self.browse(hwnd, Side::Local, String::new(), false);
    }

    fn arrange(&self, hwnd: HWND) {
        let mut base = RECT {
            left: 0,
            top: 0,
            right: 4,
            bottom: 8,
        };
        let mut client = RECT::default();
        unsafe {
            MapDialogRect(hwnd, &mut base);
            GetClientRect(hwnd, &mut client);
        }
        if base.right <= 0 || base.bottom <= 0 {
            return;
        }
        let width = client.right * 4 / base.right;
        let height = client.bottom * 8 / base.bottom;
        let place = |id, x, y, w, h| {
            let mut r = RECT {
                left: x,
                top: y,
                right: x + w,
                bottom: y + h,
            };
            unsafe {
                MapDialogRect(hwnd, &mut r);
                MoveWindow(
                    item(hwnd, id),
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    1,
                );
            }
            r.right - r.left
        };
        let pane_width = (width - 29) / 2;
        if pane_width < 215 || height < 228 {
            return;
        }
        place(UPLOAD, width - 200, 9, 90, 19);
        place(DOWNLOAD, width - 102, 9, 92, 19);
        for (base, x) in [(LOCAL, 10), (REMOTE, 19 + pane_width)] {
            place(base, x, 39, pane_width, height - 86);
            place(base + PARENT, x + 8, 55, 37, 16);
            place(base + ADDRESS, x + 49, 55, pane_width - 131, 16);
            place(base + GO, x + pane_width - 78, 55, 34, 16);
            place(base + REFRESH, x + pane_width - 40, 55, 32, 16);
            let list = place(base + ROWS, x + 8, 76, pane_width - 16, height - 133);
            call(
                item(hwnd, base + ROWS),
                LVM_SETCOLUMNWIDTH,
                0,
                (list - 102).max(90) as LPARAM,
            );
        }
        place(PROGRESS, 12, height - 40, width - 24, 8);
        place(STATUS, 12, height - 28, width - 24, 24);
    }

    fn listing(&self, side: Side) -> Option<&Listing> {
        if side == Side::Local {
            self.local.as_ref()
        } else {
            self.remote.as_ref()
        }
    }

    fn controls(&self, hwnd: HWND) {
        for side in [Side::Local, Side::Remote] {
            let base = number(side);
            for id in [ADDRESS, ROWS, GO, REFRESH] {
                unsafe { EnableWindow(item(hwnd, base + id), (!self.busy) as i32) };
            }
            let parent = self.listing(side).is_some_and(|view| view.parent.is_some());
            unsafe { EnableWindow(item(hwnd, base + PARENT), (!self.busy && parent) as i32) };
            let command = if side == Side::Local {
                UPLOAD
            } else {
                DOWNLOAD
            };
            let enabled = !self.busy
                && self.listing(other(side)).is_some()
                && !self.selection(hwnd, side).is_empty();
            unsafe { EnableWindow(item(hwnd, command), enabled as i32) };
        }
    }

    fn display(&mut self, hwnd: HWND, listing: Listing) {
        let side = listing.side;
        let base = number(side);
        set_caption(hwnd, base + ADDRESS, &listing.path);
        let list = item(hwnd, base + ROWS);
        call(list, WM_SETREDRAW, 0, 0);
        call(list, LVM_DELETEALLITEMS, 0, 0);
        for (index, entry) in listing.entries.iter().enumerate() {
            let safe: String = entry.name.chars().filter(|c| !c.is_control()).collect();
            let mut name = if entry.kind == Kind::Directory {
                wide(format!("[{safe}]"))
            } else {
                wide(safe)
            };
            let row = LVITEMW {
                mask: LVIF_TEXT,
                iItem: index as i32,
                pszText: name.as_mut_ptr(),
                ..Default::default()
            };
            let position = call(list, LVM_INSERTITEMW, 0, (&row as *const LVITEMW) as LPARAM);
            if position >= 0 {
                let mut size = wide(listing_size(entry.size));
                let subitem = LVITEMW {
                    iSubItem: 1,
                    pszText: size.as_mut_ptr(),
                    ..Default::default()
                };
                call(
                    list,
                    LVM_SETITEMTEXTW,
                    position as usize,
                    (&subitem as *const LVITEMW) as LPARAM,
                );
            }
        }
        call(list, WM_SETREDRAW, 1, 0);
        unsafe { windows_sys::Win32::Graphics::Gdi::InvalidateRect(list, null_mut(), 1) };
        if side == Side::Local {
            self.local = Some(listing);
        } else {
            self.remote = Some(listing);
        }
    }

    fn selection(&self, hwnd: HWND, side: Side) -> Vec<String> {
        let Some(listing) = self.listing(side) else {
            return Vec::new();
        };
        let list = item(hwnd, number(side) + ROWS);
        let mut names = Vec::new();
        let mut index = -1_i32;
        loop {
            let next = call(
                list,
                LVM_GETNEXTITEM,
                index as usize,
                LVNI_SELECTED as LPARAM,
            ) as i32;
            if next <= index {
                break;
            }
            let Some(entry) = listing.entries.get(next as usize) else {
                break;
            };
            if entry.kind != Kind::Other {
                names.push(entry.name.clone());
            }
            index = next;
        }
        names
    }

    fn request(&mut self, hwnd: HWND, request: Request, status: &str) -> bool {
        if self.busy {
            return false;
        }
        if self.commands.send(request).is_err() {
            set_caption(
                hwnd,
                STATUS,
                tr("Файловый сеанс завершён.", "The file session ended."),
            );
            return false;
        }
        self.busy = true;
        set_caption(hwnd, STATUS, status);
        self.controls(hwnd);
        true
    }

    fn browse(&mut self, hwnd: HWND, side: Side, path: String, reconnect: bool) {
        let status = if side == Side::Remote && (reconnect || self.remote.is_none()) {
            tr(
                "Подключаемся к удалённой машине…",
                "Connecting to the remote computer…",
            )
        } else {
            tr("Открываем папку…", "Opening folder…")
        };
        self.request(
            hwnd,
            Request::List {
                side,
                path,
                reconnect,
            },
            status,
        );
    }

    fn navigate(&mut self, hwnd: HWND, side: Side, row: i32) {
        if let Some(view) = self.listing(side)
            && let Some(Entry {
                name,
                kind: Kind::Directory,
                ..
            }) = view.entries.get(row as usize)
        {
            let path = if side == Side::Local {
                std::path::Path::new(&view.path)
                    .join(name)
                    .to_string_lossy()
                    .into_owned()
            } else {
                format!("{}/{name}", view.path.trim_end_matches('/'))
            };
            self.browse(hwnd, side, path, false);
        }
    }

    fn copy(&mut self, hwnd: HWND, side: Side, names: Vec<String>) {
        if names.is_empty() {
            return;
        }
        let (Some(source), Some(target)) = (self.listing(side), self.listing(other(side))) else {
            return;
        };
        let request = Request::Copy {
            side,
            source: source.path.clone(),
            destination: target.path.clone(),
            names,
        };
        if self.request(hwnd, request, tr("Передаём файлы…", "Transferring files…"))
        {
            self.copying = Some(other(side));
            call(item(hwnd, PROGRESS), PBM_SETPOS, 0, 0);
            unsafe { ShowWindow(item(hwnd, PROGRESS), SW_SHOW) };
        }
    }

    fn receive(&mut self, hwnd: HWND) {
        if let Ok(question) = self.questions.try_recv() {
            let answer = show_question(hwnd, &question.message, question.kind);
            let _ = question.answer.send(answer);
        }
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Progress { progress } => {
                    let percent = progress.total.filter(|&n| n > 0).map_or(0, |n| {
                        ((progress.bytes.min(n) as u128 * 100) / n as u128) as usize
                    });
                    call(item(hwnd, PROGRESS), PBM_SETPOS, percent, 0);
                    set_caption(
                        hwnd,
                        STATUS,
                        &format!("{} · {}", progress.name, listing_size(Some(progress.bytes))),
                    );
                    continue;
                }
                Event::Listing { listing } => {
                    let initial = self.local.is_none() && listing.side == Side::Local;
                    self.display(hwnd, listing);
                    set_caption(
                        hwnd,
                        STATUS,
                        self.finished
                            .take()
                            .as_deref()
                            .unwrap_or(tr("Готово", "Ready")),
                    );
                    self.busy = false;
                    if initial {
                        self.browse(hwnd, Side::Remote, String::new(), false);
                    }
                }
                Event::Copied { files } => {
                    self.busy = false;
                    self.finished = Some(tr_format!(
                        "Скопировано файлов — {files}",
                        "Files copied — {files}"
                    ));
                    if let Some(side) = self.copying.take()
                        && let Some(view) = self.listing(side)
                    {
                        self.browse(hwnd, side, view.path.clone(), false);
                    }
                }
                Event::Error { message } => {
                    self.finished = None;
                    self.copying = None;
                    self.busy = false;
                    set_caption(hwnd, STATUS, &message);
                }
            }
            unsafe { ShowWindow(item(hwnd, PROGRESS), SW_HIDE) };
            self.controls(hwnd);
        }
    }

    fn command(&mut self, hwnd: HWND, id: i32) {
        if id == UPLOAD || id == DOWNLOAD {
            let side = if id == UPLOAD {
                Side::Local
            } else {
                Side::Remote
            };
            self.copy(hwnd, side, self.selection(hwnd, side));
            return;
        }
        for side in [Side::Local, Side::Remote] {
            let base = number(side);
            match id - base {
                PARENT => {
                    if let Some(path) = self.listing(side).and_then(|view| view.parent.clone()) {
                        self.browse(hwnd, side, path, false);
                    }
                }
                GO | REFRESH => self.browse(
                    hwnd,
                    side,
                    text(item(hwnd, base + ADDRESS)),
                    side == Side::Remote && id == base + REFRESH,
                ),
                _ => {}
            }
        }
    }

    fn drag_target(&self, hwnd: HWND, source: Side) -> bool {
        let mut pointer = POINT::default();
        let mut bounds = RECT::default();
        unsafe {
            GetCursorPos(&mut pointer) != 0
                && GetWindowRect(item(hwnd, number(other(source)) + ROWS), &mut bounds) != 0
                && PtInRect(&bounds, pointer) != 0
        }
    }
}

unsafe extern "system" fn dialog(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    if msg == WM_INITDIALOG {
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp) };
        let cell = unsafe { &*(lp as *const RefCell<View>) };
        cell.borrow_mut().init(hwnd);
        return 1;
    }
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const RefCell<View>;
    if pointer.is_null() {
        return 0;
    }
    let Ok(mut view) = (unsafe { &*pointer }).try_borrow_mut() else {
        return 0;
    };
    match msg {
        WM_TIMER => view.receive(hwnd),
        WM_SIZE => view.arrange(hwnd),
        WM_GETMINMAXINFO => {
            let mut size = RECT {
                left: 0,
                top: 0,
                right: 464,
                bottom: 267,
            };
            unsafe {
                MapDialogRect(hwnd, &mut size);
                let info = &mut *(lp as *mut MINMAXINFO);
                info.ptMinTrackSize.x = info.ptMinTrackSize.x.max(size.right);
                info.ptMinTrackSize.y = info.ptMinTrackSize.y.max(size.bottom);
            }
        }
        WM_CLOSE => {
            if view.copying.is_none() {
                unsafe { EndDialog(hwnd, 0) };
            }
        }
        WM_COMMAND => {
            let id = (wp & 0xffff) as i32;
            if id == IDCANCEL {
                if view.copying.is_none() {
                    unsafe {
                        EndDialog(hwnd, 0);
                    }
                }
            } else {
                view.command(hwnd, id);
            }
        }
        WM_NOTIFY => {
            let notice = unsafe { &*(lp as *const NMHDR) };
            let side = match notice.idFrom as i32 {
                i if i == LOCAL + ROWS => Side::Local,
                i if i == REMOTE + ROWS => Side::Remote,
                _ => return 0,
            };
            match notice.code {
                LVN_ITEMCHANGED => view.controls(hwnd),
                NM_DBLCLK | NM_RETURN => {
                    let row = call(
                        notice.hwndFrom,
                        LVM_GETNEXTITEM,
                        (-1isize) as usize,
                        LVNI_SELECTED as LPARAM,
                    );
                    view.navigate(hwnd, side, row as i32);
                }
                LVN_BEGINDRAG if !view.busy && view.listing(other(side)).is_some() => {
                    let drag = unsafe { &*(lp as *const NMLISTVIEW) };
                    let mut names = view.selection(hwnd, side);
                    if let Some(entry) = view
                        .listing(side)
                        .and_then(|list| list.entries.get(drag.iItem as usize))
                        && !names.contains(&entry.name)
                        && entry.kind != Kind::Other
                    {
                        names = vec![entry.name.clone()];
                    }
                    if !names.is_empty() {
                        view.drag = Some((side, names));
                        unsafe { SetCapture(hwnd) };
                        set_caption(
                            hwnd,
                            STATUS,
                            tr(
                                "Перетащите в соседнюю панель.",
                                "Drop the selection in the other pane.",
                            ),
                        );
                    }
                }
                _ => return 0,
            }
        }
        WM_MOUSEMOVE if view.drag.is_some() => {
            let side = view
                .drag
                .as_ref()
                .map(|(side, _)| *side)
                .unwrap_or(Side::Local);
            let cursor = if view.drag_target(hwnd, side) {
                IDC_SIZEWE
            } else {
                IDC_NO
            };
            unsafe { SetCursor(LoadCursorW(null_mut(), cursor)) };
        }
        WM_LBUTTONUP if view.drag.is_some() => {
            if let Some((side, names)) = view.drag.take() {
                unsafe { ReleaseCapture() };
                if view.drag_target(hwnd, side) {
                    view.copy(hwnd, side, names);
                } else {
                    set_caption(hwnd, STATUS, tr("Готово", "Ready"));
                }
            }
        }
        WM_CAPTURECHANGED if view.drag.is_some() => {
            view.drag = None;
            set_caption(hwnd, STATUS, tr("Готово", "Ready"));
        }
        _ => return 0,
    }
    1
}

struct Prompt<'a> {
    message: &'a str,
    secret: bool,
    answer: Option<String>,
}

fn show_question(hwnd: HWND, message: &str, kind: QuestionKind) -> Option<String> {
    if matches!(kind, QuestionKind::TrustHost) {
        return (message_box(hwnd, message, MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2) == IDYES)
            .then(|| "yes".into());
    }
    let mut prompt = Prompt {
        message,
        secret: matches!(kind, QuestionKind::Secret),
        answer: None,
    };
    unsafe {
        DialogBoxParamW(
            GetModuleHandleW(null_mut()),
            8usize as *const u16,
            hwnd,
            Some(input_dialog),
            (&mut prompt as *mut Prompt<'_>) as LPARAM,
        );
    }
    prompt.answer
}

unsafe extern "system" fn input_dialog(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    if msg == WM_INITDIALOG {
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp) };
        let prompt = unsafe { &*(lp as *const Prompt<'_>) };
        set_caption(hwnd, PROMPT, prompt.message);
        set_caption(hwnd, IDCANCEL, tr("Отмена", "Cancel"));
        call(item(hwnd, ANSWER), EM_LIMITTEXT, 4096, 0);
        if !prompt.secret {
            call(item(hwnd, ANSWER), EM_SETPASSWORDCHAR, 0, 0);
        }
        return 1;
    }
    if msg == WM_COMMAND {
        let id = (wp & 0xffff) as i32;
        if id == IDOK {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Prompt<'_>;
            if !ptr.is_null() {
                unsafe { (*ptr).answer = Some(text(item(hwnd, ANSWER))) };
                set_caption(hwnd, ANSWER, "");
            }
            unsafe { EndDialog(hwnd, 1) };
            return 1;
        }
        if id == IDCANCEL {
            unsafe { EndDialog(hwnd, 0) };
            return 1;
        }
    }
    0
}
