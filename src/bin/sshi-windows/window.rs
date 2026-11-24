use std::{
    cell::RefCell,
    env,
    ffi::OsString,
    mem::size_of,
    os::windows::ffi::OsStringExt,
    path::PathBuf,
    process::Command,
    ptr::{null, null_mut},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use anyhow::{Context, Result, bail, ensure};
use iroh::EndpointId;
use iroh_ssh::{relay_url, update::UpdateInfo};
use windows_sys::Win32::{
    Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi::{
        BeginPaint, COLOR_WINDOW, CreateFontW, CreateSolidBrush, DeleteObject, EndPaint, FillRect,
        HFONT, PAINTSTRUCT, SetBkColor, SetTextColor,
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::{
            Dialogs::{
                CommDlgExtendedError, GetOpenFileNameW, GetSaveFileNameW, OFN_FILEMUSTEXIST,
                OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
            },
            EM_SETCUEBANNER, EM_SETSEL, ICC_LISTVIEW_CLASSES, INITCOMMONCONTROLSEX,
            InitCommonControlsEx, LVCF_WIDTH, LVCOLUMNW, LVIF_TEXT, LVIS_FOCUSED, LVIS_SELECTED,
            LVITEMW, LVM_DELETEALLITEMS, LVM_EDITLABELW, LVM_GETEDITCONTROL, LVM_GETNEXTITEM,
            LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMSTATE,
            LVN_BEGINLABELEDITW, LVN_ENDLABELEDITW, LVN_ITEMCHANGED, LVNI_SELECTED, LVS_EDITLABELS,
            LVS_EX_FULLROWSELECT, LVS_NOCOLUMNHEADER, LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL,
            NM_CLICK, NM_DBLCLK, NM_KILLFOCUS, NMHDR, NMITEMACTIVATE, NMLISTVIEW, NMLVDISPINFOW,
        },
        Input::KeyboardAndMouse::{SetFocus, VK_F2, VK_RETURN},
        Shell::ShellExecuteW,
        WindowsAndMessaging::*,
    },
};

use crate::{
    SESSION_ENV,
    lang::{self, tr, tr_format},
    store::{Profile, Store, crypt, last_error, wide},
    terminal::default_relay,
};

const WIDTH: i32 = 930;
const HEIGHT: i32 = 600;
const HEADER: u32 = 0x393219;
const PAPER: u32 = 0x00ff_ffff;
const DIM: u32 = 0x006e_5b46;
const APP_ICON_RESOURCE: usize = 1;
const HEADER_ICON_SIZE: i32 = 42;

const LIST: u16 = 101;
const NEW: u16 = 102;
const DELETE: u16 = 103;
const SAVE: u16 = 104;
const CONNECT: u16 = 105;
const BROWSE: u16 = 106;
const COPY: u16 = 107;
const OPEN_DATA: u16 = 108;
const ENROLL: u16 = 109;
const INSTALL_SSH_KEY: u16 = 110;
const ISSUE_INVITE: u16 = 111;
const EXPORT: u16 = 112;
const IMPORT: u16 = 113;
const RELAY_EDIT: u16 = 114;
const FORWARD: u16 = 115;
const LANG_RU: u16 = 116;
const LANG_EN: u16 = 117;
const WHATS_NEW: u16 = 118;
const EMPTY_JOIN: u16 = 119;
const MANUAL: u16 = 120;
const CHECK_UPDATES: u16 = 121;
const OPEN_UPDATE: u16 = 122;
const FILES: u16 = 123;
const UPDATE_TIMER: usize = 1;
const UPDATE_RESULT_TIMER: usize = 2;
const UPDATE_INTERVAL_MS: u32 = 6 * 60 * 60 * 1000;
const APP_MENU_RESOURCE: usize = 3;
const STYLE: u32 = WS_OVERLAPPEDWINDOW;

const INVITATION_DIALOG: usize = 2;
const INVITATION_CODE: i32 = 2001;
const COPY_INVITATION: i32 = 2002;

#[derive(Default)]
struct Controls {
    title: HWND,
    caption: HWND,
    update: HWND,
    list: HWND,
    empty_help: HWND,
    empty_join: HWND,
    empty_manual: HWND,
    name: HWND,
    user: HWND,
    endpoint: HWND,
    relay: HWND,
    token: HWND,
    ssh_key: HWND,
    my_id: HWND,
    status: HWND,
}

struct View {
    store: Store,
    id: EndpointId,
    profiles: Vec<Profile>,
    selected: Option<usize>,
    clicked: Option<usize>,
    rename_armed: bool,
    controls: Controls,
    check: Option<UpdateCheck>,
    available: Option<UpdateInfo>,
    font: HFONT,
    title_font: HFONT,
    header_brush: windows_sys::Win32::Graphics::Gdi::HBRUSH,
    header_icon: Icon,
    instance: HINSTANCE,
    layout: RefCell<Vec<ChildLayout>>,
}

struct UpdateCheck {
    result: Receiver<Result<UpdateInfo>>,
    manual: bool,
}

struct ChildLayout {
    window: HWND,
    caption: &'static str,
    rect: [i32; 4],
    horizontal: Anchor,
    vertical: Anchor,
}

#[derive(Clone, Copy)]
enum Anchor {
    Start,
    End,
    Stretch,
}

impl Anchor {
    fn apply(self, start: i32, length: i32, extra: i32) -> (i32, i32) {
        match self {
            Self::Start => (start, length),
            Self::End => (start + extra, length),
            Self::Stretch => (start, length + extra),
        }
    }
}

pub fn show(store: Store) -> Result<()> {
    let _window_lock = store.lock("window.lock", true)?;
    let id = store.identity()?.public();
    let profiles = store.profiles()?;
    let instance = unsafe { GetModuleHandleW(null()) };
    ensure!(
        !instance.is_null(),
        tr("Загрузить окно sshi", "Load the SSHI window")
    );
    let large_icon = Icon::load(instance, unsafe { GetSystemMetrics(SM_CXICON) }, unsafe {
        GetSystemMetrics(SM_CYICON)
    })?;
    let small_icon = Icon::load(instance, unsafe { GetSystemMetrics(SM_CXSMICON) }, unsafe {
        GetSystemMetrics(SM_CYSMICON)
    })?;
    let header_icon = Icon::load(instance, HEADER_ICON_SIZE, HEADER_ICON_SIZE)?;
    let list_controls = INITCOMMONCONTROLSEX {
        dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_LISTVIEW_CLASSES,
    };
    ensure!(
        unsafe { InitCommonControlsEx(&list_controls) } != 0,
        tr("Создать список подключений", "Create the connection list")
    );
    let class = wide("SshiProfiles");
    let registration = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        hIcon: large_icon.0,
        hIconSm: small_icon.0,
        hCursor: unsafe { LoadCursorW(null_mut(), IDC_ARROW) },
        hbrBackground: (COLOR_WINDOW as usize + 1) as _,
        lpszMenuName: APP_MENU_RESOURCE as *const u16,
        lpszClassName: class.as_ptr(),
        ..unsafe { std::mem::zeroed() }
    };
    if unsafe { RegisterClassExW(&registration) } == 0 {
        return Err(last_error())
            .context(tr("Зарегистрировать окно sshi", "Register the SSHI window"));
    }
    let view = RefCell::new(View {
        store,
        id,
        profiles,
        selected: None,
        clicked: None,
        rename_armed: false,
        controls: Controls::default(),
        check: None,
        available: None,
        font: font(-16, 400),
        title_font: font(-30, 700),
        header_brush: unsafe { CreateSolidBrush(HEADER) },
        header_icon,
        instance,
        layout: RefCell::default(),
    });
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: WIDTH,
        bottom: HEIGHT,
    };
    unsafe { AdjustWindowRectEx(&mut rect, STYLE, 1, 0) };
    let title = wide("sshi — подключения");
    let root = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            STYLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            rect.right - rect.left,
            rect.bottom - rect.top,
            null_mut(),
            null_mut(),
            instance,
            (&view as *const RefCell<View>).cast(),
        )
    };
    if root.is_null() {
        return Err(last_error()).context(tr(
            "Открыть окно подключений",
            "Open the connections window",
        ));
    }
    unsafe {
        SendMessageW(root, WM_SETICON, ICON_BIG as WPARAM, large_icon.0 as LPARAM);
        SendMessageW(
            root,
            WM_SETICON,
            ICON_SMALL as WPARAM,
            small_icon.0 as LPARAM,
        );
        view.borrow().apply_language(root);
        ShowWindow(root, SW_SHOW);
        SetTimer(root, UPDATE_TIMER, UPDATE_INTERVAL_MS, None);
    }
    let _ = view.borrow_mut().check_updates(root, false);
    let mut message: MSG = unsafe { std::mem::zeroed() };
    loop {
        match unsafe { GetMessageW(&mut message, null_mut(), 0, 0) } {
            0 => return Ok(()),
            -1 => {
                return Err(last_error())
                    .context(tr("Прочитать события окна", "Read window events"));
            }
            _ => {
                let list = view.borrow().controls.list;
                if message.hwnd == list && message.message == WM_KEYDOWN {
                    let item = unsafe {
                        SendMessageW(list, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as LPARAM)
                    };
                    if item >= 0 && !list.is_null() {
                        match message.wParam as u16 {
                            VK_RETURN => {
                                if message.lParam & (1 << 30) == 0 {
                                    unsafe { SendMessageW(root, WM_COMMAND, CONNECT.into(), 0) };
                                }
                                continue;
                            }
                            VK_F2 => {
                                view.borrow_mut().rename_armed = true;
                                unsafe { SendMessageW(list, LVM_EDITLABELW, item as usize, 0) };
                                continue;
                            }
                            _ => {}
                        }
                    }
                }
                if unsafe { IsDialogMessageW(root, &message) } == 0 {
                    unsafe {
                        TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }
            }
        }
    }
}

struct Icon(HICON);

impl Icon {
    fn load(instance: HINSTANCE, width: i32, height: i32) -> Result<Self> {
        let icon = unsafe {
            LoadImageW(
                instance,
                APP_ICON_RESOURCE as *const u16,
                IMAGE_ICON,
                width,
                height,
                0,
            )
        };
        if icon.is_null() {
            return Err(last_error()).context(tr("Загрузить иконку sshi", "Load the SSHI icon"));
        }
        Ok(Self(icon))
    }
}

impl Drop for Icon {
    fn drop(&mut self) {
        unsafe { DestroyIcon(self.0) };
    }
}

fn font(height: i32, weight: i32) -> HFONT {
    let name = wide("Segoe UI");
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

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let data = unsafe { &*(lparam as *const CREATESTRUCTW) };
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, data.lpCreateParams as isize) };
    }
    if message == WM_DESTROY {
        unsafe { PostQuitMessage(0) };
        return 0;
    }
    if message == WM_GETMINMAXINFO {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: WIDTH,
            bottom: HEIGHT,
        };
        unsafe {
            AdjustWindowRectEx(&mut rect, STYLE, 1, 0);
            let limits = &mut *(lparam as *mut MINMAXINFO);
            limits.ptMinTrackSize.x = rect.right - rect.left;
            limits.ptMinTrackSize.y = rect.bottom - rect.top;
        }
        return 0;
    }
    let data = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const RefCell<View>;
    if let Some(view) = unsafe { data.as_ref() } {
        match message {
            WM_CREATE => {
                if let Ok(mut view) = view.try_borrow_mut()
                    && let Err(error) = view.create_controls(hwnd)
                {
                    message_box(hwnd, &format!("{error:#}"), MB_ICONERROR);
                    return -1;
                }
                return 0;
            }
            WM_NOTIFY if wparam == usize::from(LIST) => {
                if let Ok(mut view) = view.try_borrow_mut() {
                    // List-view label, selection and click notifications carry different NMHDR payloads.
                    match unsafe { view.list_notify(lparam) } {
                        Ok(reply) => return reply,
                        Err(error) => view.status(&format!("{error:#}")),
                    }
                }
                return 0;
            }
            WM_COMMAND => {
                if let Ok(mut view) = view.try_borrow_mut()
                    && let Err(error) = view.command(hwnd, wparam as u16, (wparam >> 16) as u16)
                {
                    message_box(hwnd, &format!("{error:#}"), MB_ICONERROR);
                }
                return 0;
            }
            WM_TIMER if wparam == UPDATE_TIMER || wparam == UPDATE_RESULT_TIMER => {
                if let Ok(mut view) = view.try_borrow_mut() {
                    if wparam == UPDATE_TIMER {
                        let _ = view.check_updates(hwnd, false);
                    } else if let Err(error) = view.receive_update(hwnd) {
                        message_box(hwnd, &format!("{error:#}"), MB_ICONERROR);
                    }
                }
                return 0;
            }
            WM_SIZE if wparam != SIZE_MINIMIZED as usize => {
                if let Ok(view) = view.try_borrow() {
                    let mut rect: RECT = unsafe { std::mem::zeroed() };
                    unsafe { GetClientRect(hwnd, &mut rect) };
                    for child in view.layout.borrow().iter() {
                        let [left, top, width, height] = child.rect;
                        let (left, width) = child.horizontal.apply(left, width, rect.right - WIDTH);
                        let (top, height) = child.vertical.apply(top, height, rect.bottom - HEIGHT);
                        unsafe { MoveWindow(child.window, left, top, width, height, 1) };
                    }
                }
                return 0;
            }
            WM_CTLCOLORSTATIC => {
                if let Ok(view) = view.try_borrow() {
                    let control = lparam as HWND;
                    if control == view.controls.title || control == view.controls.caption {
                        unsafe {
                            SetTextColor(wparam as _, PAPER);
                            SetBkColor(wparam as _, HEADER);
                        }
                        return view.header_brush as LRESULT;
                    }
                    if control == view.controls.status {
                        unsafe { SetTextColor(wparam as _, DIM) };
                    }
                }
            }
            WM_PAINT => {
                if let Ok(view) = view.try_borrow() {
                    let mut paint: PAINTSTRUCT = unsafe { std::mem::zeroed() };
                    let hdc = unsafe { BeginPaint(hwnd, &mut paint) };
                    let mut header: RECT = unsafe { std::mem::zeroed() };
                    unsafe { GetClientRect(hwnd, &mut header) };
                    header.bottom = 70;
                    unsafe {
                        FillRect(hdc, &header, view.header_brush);
                        DrawIconEx(
                            hdc,
                            20,
                            14,
                            view.header_icon.0,
                            HEADER_ICON_SIZE,
                            HEADER_ICON_SIZE,
                            0,
                            null_mut(),
                            DI_NORMAL,
                        );
                        EndPaint(hwnd, &paint);
                    }
                    return 0;
                }
            }
            _ => {}
        }
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

impl View {
    fn create_controls(&mut self, hwnd: HWND) -> Result<()> {
        self.controls.title = self.label(hwnd, "sshi", [74, 16, 83, 40])?;
        unsafe { SendMessageW(self.controls.title, WM_SETFONT, self.title_font as usize, 1) };
        self.controls.caption = self.label(hwnd, "SSH через iroh", [162, 28, 240, 23])?;
        self.controls.update = self.button(hwnd, "", [486, 20, 273, 32], OPEN_UPDATE)?;
        unsafe { ShowWindow(self.controls.update, SW_HIDE) };
        self.button(hwnd, "Папка данных", [772, 20, 134, 32], OPEN_DATA)?;

        self.label(hwnd, "Подключения", [22, 92, 236, 23])?;
        self.controls.list = self.control(
            hwnd,
            "SysListView32",
            "",
            [22, 124, 236, 394],
            LIST,
            WS_TABSTOP
                | LVS_REPORT
                | LVS_NOCOLUMNHEADER
                | LVS_SINGLESEL
                | LVS_EDITLABELS
                | LVS_SHOWSELALWAYS,
            WS_EX_CLIENTEDGE,
        )?;
        let column = LVCOLUMNW {
            mask: LVCF_WIDTH,
            cx: 212,
            ..Default::default()
        };
        unsafe {
            SendMessageW(
                self.controls.list,
                LVM_INSERTCOLUMNW,
                0,
                (&column as *const LVCOLUMNW) as LPARAM,
            );
            SendMessageW(
                self.controls.list,
                LVM_SETEXTENDEDLISTVIEWSTYLE,
                0,
                LVS_EX_FULLROWSELECT as LPARAM,
            );
        }
        self.controls.empty_help = self.label(hwnd, "Чтобы подключиться, используйте приглашение от владельца сервера или введите адрес вручную.", [29, 146, 219, 78])?;
        self.controls.empty_join =
            self.button(hwnd, "По приглашению…", [29, 230, 219, 30], EMPTY_JOIN)?;
        self.controls.empty_manual =
            self.button(hwnd, "Ввести вручную…", [29, 269, 219, 30], MANUAL)?;
        self.show_empty_state();
        self.button(hwnd, "Новое", [22, 533, 110, 29], NEW)?;
        self.button(hwnd, "Удалить", [148, 533, 110, 29], DELETE)?;

        self.label(hwnd, "Название подключения", [291, 88, 526, 24])?;
        self.controls.name = self.edit(hwnd, [291, 127, 328, 30], 0)?;
        self.button(hwnd, "Терминал", [641, 126, 124, 32], CONNECT)?;
        self.button(hwnd, "Файлы", [778, 126, 128, 32], FILES)?;
        self.label(hwnd, "Пользователь", [291, 175, 128, 20])?;
        self.label(hwnd, "Endpoint ID получателя", [434, 175, 472, 20])?;
        self.controls.user = self.edit(hwnd, [291, 198, 129, 27], 0)?;
        self.controls.endpoint = self.edit(hwnd, [434, 198, 472, 27], 0)?;
        self.label(hwnd, "URL relay", [291, 235, 615, 20])?;
        self.controls.relay = self.control(
            hwnd,
            "EDIT",
            "",
            [291, 258, 615, 27],
            RELAY_EDIT,
            WS_TABSTOP | ES_AUTOHSCROLL as u32,
            WS_EX_CLIENTEDGE,
        )?;
        let hint = wide(default_relay()?);
        unsafe {
            SendMessageW(
                self.controls.relay,
                EM_SETCUEBANNER,
                0,
                hint.as_ptr() as LPARAM,
            )
        };
        self.label(hwnd, "Токен relay (если требуется)", [291, 295, 615, 20])?;
        self.controls.token = self.edit(hwnd, [291, 318, 615, 27], ES_PASSWORD as u32)?;
        self.label(hwnd, "Файл SSH-ключа (необязательно)", [291, 355, 615, 20])?;
        self.controls.ssh_key = self.edit(hwnd, [291, 378, 496, 27], 0)?;
        self.button(hwnd, "Выбрать…", [799, 378, 107, 27], BROWSE)?;
        self.label(hwnd, "Мой Endpoint ID", [291, 417, 615, 20])?;
        self.controls.my_id = self.edit(hwnd, [291, 440, 496, 27], ES_READONLY as u32)?;
        self.button(hwnd, "Копировать", [799, 440, 107, 27], COPY)?;
        self.button(hwnd, "Ввести приглашение…", [291, 480, 198, 28], ENROLL)?;
        self.button(
            hwnd,
            "Добавить SSH-ключ…",
            [499, 480, 193, 28],
            INSTALL_SSH_KEY,
        )?;
        self.button(
            hwnd,
            "Создать приглашение…",
            [703, 480, 203, 28],
            ISSUE_INVITE,
        )?;
        self.button(hwnd, "Проброс портов…", [291, 533, 199, 29], FORWARD)?;
        self.button(hwnd, "Сохранить", [761, 533, 145, 29], SAVE)?;
        self.controls.status = self.label(hwnd, "", [22, 571, 883, 20])?;
        set_text(self.controls.my_id, &self.id.to_string());
        if self.profiles.is_empty() {
            self.new_profile();
        } else {
            self.refresh_list(Some(0));
            self.select(0)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn control(
        &self,
        parent: HWND,
        class: &str,
        text: &'static str,
        rect: [i32; 4],
        id: u16,
        style: u32,
        ex_style: u32,
    ) -> Result<HWND> {
        let is_edit = class == "EDIT";
        let class = wide(class);
        let caption = text;
        let text = wide(text);
        let result = unsafe {
            CreateWindowExW(
                ex_style,
                class.as_ptr(),
                text.as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                rect[0],
                rect[1],
                rect[2],
                rect[3],
                parent,
                id as usize as _,
                self.instance,
                null(),
            )
        };
        if result.is_null() {
            return Err(last_error())
                .context(tr("Создать элемент окна sshi", "Create an SSHI control"));
        }
        unsafe { SendMessageW(result, WM_SETFONT, self.font as usize, 1) };
        let horizontal = match id {
            OPEN_DATA | BROWSE | COPY | ENROLL | INSTALL_SSH_KEY | ISSUE_INVITE | SAVE
            | CONNECT | FILES | FORWARD | OPEN_UPDATE => Anchor::End,
            _ if is_edit && rect[2] >= 400 => Anchor::Stretch,
            _ => Anchor::Start,
        };
        let vertical = match id {
            LIST => Anchor::Stretch,
            NEW | DELETE | ENROLL | INSTALL_SSH_KEY | ISSUE_INVITE | SAVE | FORWARD => Anchor::End,
            _ if rect[1] >= 528 => Anchor::End,
            _ => Anchor::Start,
        };
        self.layout.borrow_mut().push(ChildLayout {
            window: result,
            caption,
            rect,
            horizontal,
            vertical,
        });
        Ok(result)
    }

    fn label(&self, parent: HWND, text: &'static str, rect: [i32; 4]) -> Result<HWND> {
        self.control(parent, "STATIC", text, rect, 0, 0, 0)
    }

    fn edit(&self, parent: HWND, rect: [i32; 4], style: u32) -> Result<HWND> {
        self.control(
            parent,
            "EDIT",
            "",
            rect,
            0,
            WS_TABSTOP | ES_AUTOHSCROLL as u32 | style,
            WS_EX_CLIENTEDGE,
        )
    }

    fn button(&self, parent: HWND, text: &'static str, rect: [i32; 4], id: u16) -> Result<HWND> {
        self.control(parent, "BUTTON", text, rect, id, WS_TABSTOP, 0)
    }

    fn apply_language(&self, hwnd: HWND) {
        set_text(hwnd, tr("sshi — подключения", "sshi — connections"));
        for child in self.layout.borrow().iter() {
            if !child.caption.is_empty() {
                set_text(child.window, lang::caption(child.caption));
            }
        }
        let release = format!(
            "{} {}",
            tr("Что нового в", "What’s new in"),
            env!("CARGO_PKG_VERSION")
        );
        unsafe {
            let menu = GetMenu(hwnd);
            let profiles = GetSubMenu(menu, 0);
            let settings = GetSubMenu(menu, 1);
            let languages = GetSubMenu(settings, 0);
            for (parent, item, by_position, name) in [
                (menu, 0, 1, tr("&Подключения", "&Connections")),
                (menu, 1, 1, tr("&Настройки", "&Settings")),
                (settings, 0, 1, tr("&Язык", "&Language")),
                (
                    profiles,
                    NEW,
                    0,
                    tr("&Новое подключение", "&New connection"),
                ),
                (
                    profiles,
                    EXPORT,
                    0,
                    tr("Сохранить резервную &копию…", "&Back up connections…"),
                ),
                (
                    profiles,
                    IMPORT,
                    0,
                    tr("&Восстановить из копии…", "&Restore from backup…"),
                ),
                (settings, WHATS_NEW, 0, release.as_str()),
                (
                    settings,
                    CHECK_UPDATES,
                    0,
                    tr("Проверить обновления…", "Check for updates…"),
                ),
            ] {
                let mut title = wide(name);
                let info = MENUITEMINFOW {
                    cbSize: size_of::<MENUITEMINFOW>() as u32,
                    fMask: MIIM_STRING,
                    dwTypeData: title.as_mut_ptr(),
                    ..Default::default()
                };
                SetMenuItemInfoW(parent, u32::from(item), by_position, &info);
            }
            let selected = if lang::english() { LANG_EN } else { LANG_RU };
            CheckMenuRadioItem(
                languages,
                LANG_RU.into(),
                LANG_EN.into(),
                selected.into(),
                MF_BYCOMMAND,
            );
            DrawMenuBar(hwnd);
        }
        self.refresh_update_button();
    }

    fn check_updates(&mut self, hwnd: HWND, manual: bool) -> Result<()> {
        if let Some(check) = &mut self.check {
            check.manual |= manual;
        } else {
            let (sender, result) = mpsc::channel();
            ensure!(
                unsafe { SetTimer(hwnd, UPDATE_RESULT_TIMER, 250, None) } != 0,
                tr(
                    "Не удалось начать проверку обновлений",
                    "Could not start the update check"
                )
            );
            if let Err(error) =
                thread::Builder::new()
                    .name("sshi-updates".into())
                    .spawn(move || {
                        let _ = sender.send(iroh_ssh::run_async(iroh_ssh::update::check()));
                    })
            {
                unsafe { KillTimer(hwnd, UPDATE_RESULT_TIMER) };
                return Err(error.into());
            }
            self.check = Some(UpdateCheck { result, manual });
        }
        if manual {
            self.status(tr(
                "Проверяем обновления на GitHub…",
                "Checking GitHub for updates…",
            ));
        }
        Ok(())
    }

    fn receive_update(&mut self, hwnd: HWND) -> Result<()> {
        let Some(check) = &self.check else {
            return Ok(());
        };
        let result = match check.result.try_recv() {
            Err(TryRecvError::Empty) => return Ok(()),
            Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!(tr(
                "Проверка обновлений прервалась",
                "The update check was interrupted"
            ))),
            Ok(result) => result,
        };
        let manual = check.manual;
        self.check = None;
        unsafe { KillTimer(hwnd, UPDATE_RESULT_TIMER) };
        if manual {
            self.status("");
        }
        match result {
            Ok(info) => {
                self.available = info.available.then_some(info);
                self.refresh_update_button();
                if manual {
                    if self.available.is_some() {
                        self.offer_update(hwnd)?;
                    } else {
                        let text = tr_format!(
                            "У вас актуальная версия SSHI {}.",
                            "SSHI {} is up to date.",
                            env!("CARGO_PKG_VERSION")
                        );
                        message_box(hwnd, &text, MB_ICONINFORMATION);
                    }
                }
            }
            Err(error) if manual => {
                let text = tr_format!(
                    "Не удалось проверить обновления SSHI {} на GitHub.\n\n{}\n\nПовторить попытку?",
                    "SSHI {} could not check GitHub for updates.\n\n{}\n\nTry again?",
                    env!("CARGO_PKG_VERSION"),
                    format!("{error:#}")
                );
                if message_box(hwnd, &text, MB_RETRYCANCEL | MB_ICONWARNING) == IDRETRY {
                    self.check_updates(hwnd, true)?;
                }
            }
            Err(_) => {}
        }
        Ok(())
    }

    fn refresh_update_button(&self) {
        let button = self.controls.update;
        if let Some(info) = &self.available {
            set_text(
                button,
                &tr_format!("Обновить до {}…", "Update to {}…", info.latest),
            );
            unsafe { ShowWindow(button, SW_SHOWNA) };
        } else {
            unsafe { ShowWindow(button, SW_HIDE) };
        }
    }

    fn offer_update(&self, hwnd: HWND) -> Result<()> {
        let Some(info) = &self.available else {
            return Ok(());
        };
        let (url, action) = match &info.download_url {
            Some(url) => (
                url,
                tr("Скачать EXE с GitHub?", "Download the EXE from GitHub?"),
            ),
            None => (
                &info.release_url,
                tr(
                    "Открыть страницу релиза на GitHub?",
                    "Open the release page on GitHub?",
                ),
            ),
        };
        let text = tr_format!(
            "Доступна версия SSHI {}. Сейчас установлена {}.\n\nПосле загрузки закройте свои окна SSHI и замените старый EXE новым. Подключения хранятся отдельно.\n\n{}",
            "SSHI {} is available. You have {}.\n\nAfter downloading, close your SSHI windows and replace the old EXE with the new one. Your saved connections are stored separately.\n\n{}",
            info.latest,
            info.current,
            action
        );
        if message_box(hwnd, &text, MB_YESNO | MB_ICONINFORMATION) == IDYES {
            open_web_page(hwnd, url)?;
        }
        Ok(())
    }

    fn command(&mut self, hwnd: HWND, id: u16, event: u16) -> Result<()> {
        match id {
            LANG_RU | LANG_EN => {
                self.store.set_english(id == LANG_EN)?;
                self.apply_language(hwnd);
                self.status("");
            }
            CHECK_UPDATES => self.check_updates(hwnd, true)?,
            OPEN_UPDATE => self.offer_update(hwnd)?,
            WHATS_NEW => {
                open_web_page(
                    hwnd,
                    concat!(
                        "https://github.com/gadmin2151/sshi/releases/tag/v",
                        env!("CARGO_PKG_VERSION")
                    ),
                )?;
            }
            NEW | EMPTY_JOIN | ENROLL => {
                let previous = if id == ENROLL {
                    self.selected.and_then(|i| self.profiles.get(i))
                } else {
                    None
                };
                match crate::join_dialog::show(hwnd, &self.store, self.id, previous)? {
                    crate::join_dialog::Outcome::Manual => self.new_profile(),
                    crate::join_dialog::Outcome::Saved { name, open } => {
                        self.profiles = self.store.profiles()?;
                        let i = self
                            .profiles
                            .iter()
                            .position(|p| p.name == name)
                            .context(tr("Подключение отсутствует", "Connection not found"))?;
                        self.refresh_list(Some(i));
                        self.select(i)?;
                        self.status(tr(
                            "Доступ разрешён. Подключение сохранено.",
                            "Access granted. Connection saved.",
                        ));
                        if open {
                            self.connect()?;
                        }
                    }
                    crate::join_dialog::Outcome::Closed => {}
                }
            }
            MANUAL => self.new_profile(),
            DELETE => self.delete(hwnd)?,
            SAVE => self.save()?,
            CONNECT => self.connect()?,
            FILES => self.spawn_terminal("--files")?,
            INSTALL_SSH_KEY => self.spawn_terminal("--copy-ssh-key")?,
            ISSUE_INVITE => {
                if text(self.controls.endpoint).trim().is_empty() {
                    self.status(tr("Приглашение создаёт получатель или уже авторизованный клиент. Отправьте владельцу получателя свой Endpoint ID.", "The receiver or an authorized client issues invitations. Send your endpoint ID to the receiver’s owner."));
                } else {
                    let profile = self.form()?;
                    if let Some((guest, account)) = crate::join_dialog::issue(hwnd, &profile)? {
                        self.launch_terminal(
                            profile,
                            "--issue-invite",
                            &[&guest.to_string(), &account],
                        )?;
                    }
                }
            }
            FORWARD => {
                let profile = self.form()?;
                if let Some(spec) = crate::forward_dialog::show(hwnd, &profile.name)? {
                    self.launch_terminal(profile, "--forward", &spec.ssh_args())?;
                }
            }
            EXPORT => {
                if let Some(path) = choose_file(hwnd, FileAction::Backup)? {
                    self.store.backup(&path)?;
                    self.status(tr(
                        "Резервная копия подключений сохранена",
                        "Connection backup saved",
                    ));
                }
            }
            IMPORT => {
                if let Some(path) = choose_file(hwnd, FileAction::Restore)? {
                    let count = self.store.restore(&path)?;
                    self.profiles = self.store.profiles()?;
                    let selected = self
                        .selected
                        .or_else(|| (!self.profiles.is_empty()).then_some(0));
                    self.refresh_list(selected);
                    if let Some(index) = selected {
                        self.select(index)?;
                    }
                    self.status(&tr_format!(
                        "Добавлено подключений: {count}",
                        "Connections added: {count}"
                    ));
                }
            }
            RELAY_EDIT if event == EN_CHANGE as u16 => {
                // A token belongs to the relay where it was entered.
                set_text(self.controls.token, "");
            }
            BROWSE => {
                if let Some(path) = choose_file(hwnd, FileAction::Key)? {
                    set_text(self.controls.ssh_key, &path.to_string_lossy());
                }
            }
            COPY => {
                unsafe {
                    SendMessageW(self.controls.my_id, EM_SETSEL, 0, -1);
                    SendMessageW(self.controls.my_id, WM_COPY, 0, 0);
                }
                self.status(tr("Endpoint ID скопирован", "Endpoint ID copied"));
            }
            OPEN_DATA => {
                let path = wide(self.store.path());
                let verb = wide("open");
                let result = unsafe {
                    ShellExecuteW(hwnd, verb.as_ptr(), path.as_ptr(), null(), null(), SW_SHOW)
                };
                ensure!(
                    result as isize > 32,
                    tr(
                        "Не удалось открыть папку данных",
                        "Could not open the data folder"
                    )
                );
            }
            _ => {}
        }
        Ok(())
    }

    unsafe fn list_notify(&mut self, param: LPARAM) -> Result<LRESULT> {
        let list = self.controls.list;
        match unsafe { (*(param as *const NMHDR)).code } {
            LVN_ITEMCHANGED => {
                let change = unsafe { &*(param as *const NMLISTVIEW) };
                if (change.uNewState ^ change.uOldState) & LVIS_SELECTED != 0 {
                    self.clicked = None;
                    self.rename_armed = false;
                    if change.uNewState & LVIS_SELECTED != 0 {
                        self.select(usize::try_from(change.iItem)?)?;
                    }
                }
            }
            NM_CLICK | NM_DBLCLK => {
                let click = unsafe { &*(param as *const NMITEMACTIVATE) };
                let row = usize::try_from(click.iItem).ok();
                if click.hdr.code == NM_DBLCLK {
                    let activate = row.is_some() && row == self.clicked && row == self.selected;
                    self.clicked = None;
                    self.rename_armed = false;
                    if activate {
                        self.connect()?;
                    }
                } else {
                    self.rename_armed =
                        row.is_some() && row == self.clicked && row == self.selected;
                    self.clicked = row;
                }
            }
            NM_KILLFOCUS => {
                self.clicked = None;
                self.rename_armed = false;
            }
            LVN_BEGINLABELEDITW => {
                // The native control handles double-click timing. A row selected at startup
                // or with the keyboard still needs a first mouse click before renaming.
                return Ok((!self.rename_armed) as LRESULT);
            }
            LVN_ENDLABELEDITW => {
                self.clicked = None;
                self.rename_armed = false;
                let item = unsafe { &(*(param as *const NMLVDISPINFOW)).item };
                if !item.pszText.is_null() {
                    let index = usize::try_from(item.iItem)?;
                    let editor = unsafe { SendMessageW(list, LVM_GETEDITCONTROL, 0, 0) } as HWND;
                    ensure!(
                        !editor.is_null(),
                        tr("Поле названия недоступно", "The name editor is unavailable")
                    );
                    let mut profile = self
                        .profiles
                        .get(index)
                        .context(tr("Подключение отсутствует", "Connection not found"))?
                        .clone();
                    profile.name = text(editor);
                    self.persist_profile(profile, Some(index))?;
                    if Some(index) == self.selected {
                        set_text(self.controls.name, &self.profiles[index].name);
                    }
                    self.status(tr("Название сохранено", "Name saved"));
                    return Ok(1);
                }
            }
            _ => {}
        }
        Ok(0)
    }

    fn select_list_row(&self, row: Option<usize>) {
        let selected = LVIS_SELECTED | LVIS_FOCUSED;
        let item = LVITEMW {
            stateMask: selected,
            state: if row.is_some() { selected } else { 0 },
            ..Default::default()
        };
        unsafe {
            SendMessageW(
                self.controls.list,
                LVM_SETITEMSTATE,
                row.unwrap_or(usize::MAX),
                (&item as *const LVITEMW) as LPARAM,
            );
        }
    }

    fn show_empty_state(&self) {
        let empty = self.profiles.is_empty();
        unsafe {
            ShowWindow(self.controls.list, if empty { SW_HIDE } else { SW_SHOW });
            for control in [
                self.controls.empty_help,
                self.controls.empty_join,
                self.controls.empty_manual,
            ] {
                ShowWindow(control, if empty { SW_SHOW } else { SW_HIDE });
            }
        }
    }

    fn refresh_list(&mut self, selected: Option<usize>) {
        self.show_empty_state();
        self.clicked = None;
        self.rename_armed = false;
        unsafe { SendMessageW(self.controls.list, LVM_DELETEALLITEMS, 0, 0) };
        for (index, profile) in self.profiles.iter().enumerate() {
            let mut name = wide(&profile.name);
            let item = LVITEMW {
                mask: LVIF_TEXT,
                iItem: index as i32,
                pszText: name.as_mut_ptr(),
                ..Default::default()
            };
            unsafe {
                SendMessageW(
                    self.controls.list,
                    LVM_INSERTITEMW,
                    0,
                    (&item as *const LVITEMW) as LPARAM,
                )
            };
        }
        self.select_list_row(selected);
    }

    fn new_profile(&mut self) {
        self.selected = None;
        self.clicked = None;
        self.rename_armed = false;
        self.select_list_row(None);
        set_text(self.controls.name, "");
        set_text(self.controls.user, "root");
        set_text(self.controls.endpoint, "");
        set_text(self.controls.relay, "");
        set_text(self.controls.token, "");
        set_text(self.controls.ssh_key, "");
        self.status(tr(
            "Добавьте Endpoint ID получателя, чтобы подключиться",
            "Enter the receiver endpoint ID to connect",
        ));
        unsafe { SetFocus(self.controls.name) };
    }

    fn select(&mut self, index: usize) -> Result<()> {
        let profile = self
            .profiles
            .get(index)
            .context(tr("Подключение отсутствует", "Connection not found"))?;
        let token = String::from_utf8(crypt(&profile.relay_token, false)?)
            .context(tr("Прочитать токен relay", "Read the relay token"))?;
        self.selected = Some(index);
        set_text(self.controls.name, &profile.name);
        set_text(self.controls.user, &profile.user);
        set_text(self.controls.endpoint, &profile.endpoint.to_string());
        set_text(self.controls.relay, &profile.relay);
        set_text(self.controls.token, &token);
        set_text(self.controls.ssh_key, &profile.ssh_key);
        self.status("");
        Ok(())
    }

    fn form(&self) -> Result<Profile> {
        let name = text(self.controls.name).trim().to_owned();
        let user = text(self.controls.user).trim().to_owned();
        ensure!(
            iroh_ssh::profiles::valid_user(&user),
            tr("Укажите имя пользователя SSH", "Enter an SSH username")
        );
        let endpoint = text(self.controls.endpoint).trim().parse().context(tr(
            "Проверьте Endpoint ID получателя",
            "Check the receiver endpoint ID",
        ))?;
        let token = text(self.controls.token);
        let relay_text = text(self.controls.relay);
        let inherit = relay_text.trim().is_empty();
        let relay = relay_url(
            if inherit {
                default_relay()?
            } else {
                relay_text.trim().to_owned()
            }
            .as_str(),
        )
        .context(tr("Проверьте URL relay", "Check the relay URL"))?;
        ensure!(
            token.trim().is_empty() || relay.scheme() == "https",
            tr(
                "Для relay с токеном требуется HTTPS",
                "A relay with a token requires HTTPS"
            )
        );
        Ok(Profile {
            name,
            user,
            endpoint,
            relay: if inherit && token.trim().is_empty() {
                String::new()
            } else {
                relay.to_string()
            },
            ssh_key: text(self.controls.ssh_key).trim().to_owned(),
            relay_token: crypt(token.trim().as_bytes(), true)?,
        })
    }

    fn persist_profile(&mut self, profile: Profile, index: Option<usize>) -> Result<usize> {
        profile.portable().validate()?;
        let mut updated = self.profiles.clone();
        if updated.iter().enumerate().any(|(row, old)| {
            Some(row) != index && old.name.to_lowercase() == profile.name.to_lowercase()
        }) {
            bail!(tr(
                "Подключение с таким названием уже есть",
                "A connection with that name already exists"
            ));
        }
        let selected = if let Some(index) = index {
            *updated
                .get_mut(index)
                .context(tr("Подключение отсутствует", "Connection not found"))? = profile;
            index
        } else {
            updated.push(profile);
            updated.len() - 1
        };
        self.store.save_profiles(&updated)?;
        self.profiles = updated;
        Ok(selected)
    }

    fn save(&mut self) -> Result<()> {
        let selected = self.persist_profile(self.form()?, self.selected)?;
        self.selected = Some(selected);
        self.refresh_list(Some(selected));
        self.select(selected)?;
        self.status(tr("Подключение сохранено", "Connection saved"));
        Ok(())
    }

    fn delete(&mut self, hwnd: HWND) -> Result<()> {
        let index = self.selected.context(tr(
            "Выберите подключение для удаления",
            "Select a connection to delete",
        ))?;
        let profile = self
            .profiles
            .get(index)
            .context(tr("Подключение отсутствует", "Connection not found"))?;
        if message_box(
            hwnd,
            &tr_format!(
                "Удалить подключение «{}»?",
                "Delete the connection “{}”?",
                profile.name
            ),
            MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
        ) != IDYES
        {
            return Ok(());
        }
        let mut updated = self.profiles.clone();
        updated.remove(index);
        self.store.save_profiles(&updated)?;
        self.profiles = updated;
        self.refresh_list(None);
        self.new_profile();
        self.status(tr("Подключение удалено", "Connection deleted"));
        Ok(())
    }

    fn connect(&self) -> Result<()> {
        self.spawn_terminal("--terminal")
    }

    fn spawn_terminal(&self, mode: &str) -> Result<()> {
        self.launch_terminal(self.form()?, mode, &[])
    }

    fn launch_terminal(&self, mut profile: Profile, mode: &str, args: &[&str]) -> Result<()> {
        if profile.name.is_empty() {
            profile.name = format!("{}@{}", profile.user, profile.endpoint.fmt_short());
        }
        Command::new(env::current_exe()?)
            .arg(mode)
            .args(args)
            .env(SESSION_ENV, toml::to_string(&profile)?)
            .spawn()
            .context(tr("Запустить терминал sshi", "Start the SSHI terminal"))?;
        self.status(if mode == "--files" {
            tr(
                "Открыто окно передачи файлов",
                "File transfer window opened",
            )
        } else {
            tr("Терминал открыт", "Terminal opened")
        });
        Ok(())
    }

    fn status(&self, message: &str) {
        set_text(self.controls.status, message);
    }
}

impl Drop for View {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.font);
            DeleteObject(self.title_font);
            DeleteObject(self.header_brush);
        }
    }
}

pub(super) fn set_text(handle: HWND, value: &str) {
    let value = wide(value);
    unsafe { SetWindowTextW(handle, value.as_ptr()) };
}

pub(super) fn text(handle: HWND) -> String {
    let mut buffer = vec![0; unsafe { GetWindowTextLengthW(handle) }.max(0) as usize + 1];
    let length = unsafe { GetWindowTextW(handle, buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..length.max(0) as usize])
}

enum FileAction {
    Key,
    Backup,
    Restore,
}

fn choose_file(owner: HWND, action: FileAction) -> Result<Option<PathBuf>> {
    let save = matches!(action, FileAction::Backup);
    let (title, filter) = match action {
        FileAction::Key => (
            tr("Выберите приватный ключ SSH", "Choose an SSH private key"),
            tr("Все файлы\0*.*\0", "All files\0*.*\0"),
        ),
        FileAction::Backup => (
            tr(
                "Сохранить резервную копию подключений",
                "Back up connections",
            ),
            tr(
                "Резервная копия SSHI (*.json)\0*.json\0",
                "SSHI backup (*.json)\0*.json\0",
            ),
        ),
        FileAction::Restore => (
            tr("Восстановить подключения", "Restore connections"),
            tr(
                "Резервная копия SSHI (*.json)\0*.json\0Все файлы\0*.*\0",
                "SSHI backup (*.json)\0*.json\0All files\0*.*\0",
            ),
        ),
    };
    let title = wide(title);
    let filter = wide(filter);
    let extension = wide("json");
    let mut filename = vec![0u16; 32768];
    if save {
        let default = wide("sshi-connections.json");
        filename[..default.len()].copy_from_slice(&default);
    }
    let mut dialog = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: filter.as_ptr(),
        lpstrFile: filename.as_mut_ptr(),
        nMaxFile: filename.len() as u32,
        lpstrTitle: title.as_ptr(),
        lpstrDefExt: if save { extension.as_ptr() } else { null() },
        Flags: OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR | if save { 0 } else { OFN_FILEMUSTEXIST },
        ..unsafe { std::mem::zeroed() }
    };
    let chosen = unsafe {
        if save {
            GetSaveFileNameW(&mut dialog)
        } else {
            GetOpenFileNameW(&mut dialog)
        }
    };
    if chosen == 0 {
        let error = unsafe { CommDlgExtendedError() };
        ensure!(
            error == 0,
            "{}",
            tr_format!(
                "Не удалось открыть диалог выбора файла (0x{error:08x})",
                "Could not open the file dialog (0x{error:08x})"
            )
        );
        return Ok(None);
    }
    let length = filename
        .iter()
        .position(|&ch| ch == 0)
        .unwrap_or(filename.len());
    Ok(Some(OsString::from_wide(&filename[..length]).into()))
}

fn open_web_page(owner: HWND, url: &str) -> Result<()> {
    let result = unsafe {
        ShellExecuteW(
            owner,
            wide("open").as_ptr(),
            wide(url).as_ptr(),
            null(),
            null(),
            SW_SHOW,
        )
    };
    ensure!(
        result as isize > 32,
        "{}",
        tr(
            "Не удалось открыть ссылку в браузере",
            "Could not open the link in your browser"
        )
    );
    Ok(())
}

pub fn message_box(owner: HWND, message: &str, style: u32) -> i32 {
    let title = wide("sshi");
    let message = wide(message);
    unsafe { MessageBoxW(owner, message.as_ptr(), title.as_ptr(), style) }
}

pub fn show_invitation(owner: HWND, code: &str) -> Result<()> {
    let code = wide(code);
    let instance = unsafe { GetModuleHandleW(null()) };
    let result = unsafe {
        DialogBoxParamW(
            instance,
            INVITATION_DIALOG as *const u16,
            owner,
            Some(invitation_dialog),
            code.as_ptr() as LPARAM,
        )
    };
    if result == -1 {
        return Err(last_error()).context(tr(
            "Открыть окно с кодом приглашения",
            "Open the invitation window",
        ));
    }
    Ok(())
}

unsafe extern "system" fn invitation_dialog(
    dialog: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    match message {
        WM_INITDIALOG => {
            set_text(dialog, tr("Создано приглашение", "Invitation created"));
            for (id, name) in [
                (
                    2003,
                    tr(
                        "Отправьте эту ссылку приглашённому компьютеру. Ссылка действует 10 минут.",
                        "Send this link to the invited computer. It is valid for 10 minutes.",
                    ),
                ),
                (COPY_INVITATION, tr("Копировать ссылку", "Copy link")),
                (IDOK, tr("Закрыть", "Close")),
            ] {
                set_text(unsafe { GetDlgItem(dialog, id) }, name);
            }
            unsafe { SetDlgItemTextW(dialog, INVITATION_CODE, lparam as *const u16) };
            1
        }
        WM_COMMAND => match i32::from(wparam as u16) {
            COPY_INVITATION => {
                let code = unsafe { GetDlgItem(dialog, INVITATION_CODE) };
                let copied = wide(tr("Ссылка скопирована", "Link copied"));
                unsafe {
                    SendMessageW(code, EM_SETSEL, 0, -1);
                    SendMessageW(code, WM_COPY, 0, 0);
                    SetDlgItemTextW(dialog, COPY_INVITATION, copied.as_ptr());
                }
                1
            }
            IDOK | IDCANCEL => {
                unsafe { EndDialog(dialog, 1) };
                1
            }
            _ => 0,
        },
        WM_CLOSE => {
            unsafe { EndDialog(dialog, 1) };
            1
        }
        _ => 0,
    }
}
