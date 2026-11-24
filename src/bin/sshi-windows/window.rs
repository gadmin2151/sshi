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
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use iroh::EndpointId;
use iroh_ssh::{relay_url, update::UpdateInfo};
use windows_sys::Win32::{
    Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::{
        Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute},
        Gdi::{
            BeginPaint, EndPaint, FillRect, InvalidateRect, PAINTSTRUCT, SetBkColor, SetTextColor,
        },
    },
    System::{
        LibraryLoader::GetModuleHandleW,
        SystemServices::{SS_CENTER, SS_ENDELLIPSIS, SS_NOPREFIX},
    },
    UI::{
        Controls::{
            CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDRF_DODEFAULT, CDRF_NOTIFYITEMDRAW,
            CDRF_SKIPDEFAULT, DRAWITEMSTRUCT,
            Dialogs::{
                CommDlgExtendedError, GetOpenFileNameW, GetSaveFileNameW, OFN_FILEMUSTEXIST,
                OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
            },
            EM_LIMITTEXT, EM_SETCUEBANNER, EM_SETSEL, HIMAGELIST, ICC_LISTVIEW_CLASSES,
            ILC_COLOR32, INITCOMMONCONTROLSEX, ImageList_Create, ImageList_Destroy,
            ImageList_SetImageCount, InitCommonControlsEx, LVA_DEFAULT, LVIF_IMAGE, LVIF_TEXT,
            LVIR_ICON, LVIS_FOCUSED, LVIS_SELECTED, LVITEMW, LVM_ARRANGE, LVM_DELETEALLITEMS,
            LVM_EDITLABELW, LVM_ENSUREVISIBLE, LVM_GETEDITCONTROL, LVM_GETITEMRECT,
            LVM_INSERTITEMW, LVM_SETBKCOLOR, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETICONSPACING,
            LVM_SETIMAGELIST, LVM_SETITEMSTATE, LVN_BEGINLABELEDITW, LVN_ENDLABELEDITW,
            LVN_ITEMCHANGED, LVS_AUTOARRANGE, LVS_EDITLABELS, LVS_EX_DOUBLEBUFFER,
            LVS_SHAREIMAGELISTS, LVS_SHOWSELALWAYS, LVS_SINGLESEL, LVSIL_NORMAL, NM_CLICK,
            NM_CUSTOMDRAW, NM_DBLCLK, NM_KILLFOCUS, NMHDR, NMITEMACTIVATE, NMLISTVIEW,
            NMLVCUSTOMDRAW, NMLVDISPINFOW,
        },
        Input::KeyboardAndMouse::{
            GetDoubleClickTime, GetKeyState, SetFocus, VK_CONTROL, VK_F2, VK_RETURN,
        },
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

mod gallery;
use gallery::{BACKGROUND, CARD_HEIGHT, Layout, MARGIN, MUTED, SURFACE, Style, TEXT, TOP, rect};

const WIDTH: i32 = 1180;
const HEIGHT: i32 = 700;
const MIN_WIDTH: i32 = 575;
const MIN_HEIGHT: i32 = 440;
const APP_ICON_RESOURCE: usize = 1;

const LIST: u16 = 101;
const NEW: u16 = 102;
const DELETE: u16 = 103;
const CONNECT: u16 = 105;
const COPY: u16 = 107;
const OPEN_DATA: u16 = 108;
const ENROLL: u16 = 109;
const INSTALL_SSH_KEY: u16 = 110;
const ISSUE_INVITE: u16 = 111;
const EXPORT: u16 = 112;
const IMPORT: u16 = 113;
const FORWARD: u16 = 115;
const LANG_RU: u16 = 116;
const LANG_EN: u16 = 117;
const WHATS_NEW: u16 = 118;
const EMPTY_JOIN: u16 = 119;
const MANUAL: u16 = 120;
const CHECK_UPDATES: u16 = 121;
const OPEN_UPDATE: u16 = 122;
const FILES: u16 = 123;
const NOTES: u16 = 124;
const SETTINGS: u16 = 125;
const ACTIONS: u16 = 126;
const COPY_ENDPOINT: u16 = 127;
const EDIT: u16 = 128;
const SEARCH: u16 = 129;
const RENAME: u16 = 130;
const REVEAL_RENAMED: u16 = 131;
const UPDATE_TIMER: usize = 1;
const UPDATE_RESULT_TIMER: usize = 2;
const RENAME_TIMER: usize = 3;
const UPDATE_INTERVAL_MS: u32 = 6 * 60 * 60 * 1000;
const WINDOW_STYLE: u32 = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;

const INVITATION_DIALOG: usize = 2;
const INVITATION_CODE: i32 = 2001;
const COPY_INVITATION: i32 = 2002;
const NOTES_DIALOG: usize = 10;
const NOTES_PROFILE: i32 = 4101;
const NOTES_TEXT: i32 = 4102;
const NOTES_ERROR: i32 = 4103;
const PROFILE_DIALOG: usize = 11;
const PROFILE_NAME: i32 = 4201;
const PROFILE_USER: i32 = 4202;
const PROFILE_ENDPOINT: i32 = 4203;
const PROFILE_RELAY: i32 = 4204;
const PROFILE_TOKEN: i32 = 4205;
const PROFILE_KEY: i32 = 4206;
const PROFILE_BROWSE: i32 = 4207;
const PROFILE_OPEN: i32 = 4208;
const PROFILE_FILES: i32 = 4209;
const PROFILE_ERROR: i32 = 4210;

#[derive(Default)]
struct Controls {
    brand: HWND,
    heading: HWND,
    settings: HWND,
    update: HWND,
    add: HWND,
    search: HWND,
    list: HWND,
    empty_help: HWND,
    empty_join: HWND,
    empty_manual: HWND,
    actions: HWND,
    files: HWND,
    connect: HWND,
    copy: HWND,
    clipboard: HWND,
    status: HWND,
}

struct View {
    store: Store,
    id: EndpointId,
    profiles: Vec<Profile>,
    visible: Vec<usize>,
    selected: Option<usize>,
    clicked: Option<(usize, Instant)>,
    rename_target: Option<usize>,
    controls: Controls,
    check: Option<UpdateCheck>,
    available: Option<UpdateInfo>,
    style: Style,
    card_images: RefCell<Option<CardImages>>,
    header_icon: Icon,
    instance: HINSTANCE,
}

struct UpdateCheck {
    result: Receiver<Result<UpdateInfo>>,
    manual: bool,
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
    let header_icon = Icon::load(instance, 30, 30)?;
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
        visible: Vec::new(),
        selected: None,
        clicked: None,
        rename_target: None,
        controls: Controls::default(),
        check: None,
        available: None,
        style: Style::new(),
        card_images: RefCell::default(),
        header_icon,
        instance,
    });
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: WIDTH,
        bottom: HEIGHT,
    };
    unsafe { AdjustWindowRectEx(&mut rect, WINDOW_STYLE, 0, 0) };
    let title = wide("SSHI");
    let root = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WINDOW_STYLE,
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
        let dark: i32 = 1;
        DwmSetWindowAttribute(
            root,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            (&dark as *const i32).cast(),
            size_of::<i32>() as u32,
        );
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
                if message.message == WM_KEYDOWN {
                    let (search, list, selected) = {
                        let view = view.borrow();
                        (
                            view.controls.search,
                            view.controls.list,
                            view.selected.is_some(),
                        )
                    };
                    let control = unsafe { GetKeyState(VK_CONTROL as i32) } < 0;
                    if control && message.wParam == b'F' as usize {
                        unsafe { SetFocus(search) };
                        continue;
                    }
                    if control && message.wParam == b'N' as usize {
                        unsafe { SendMessageW(root, WM_COMMAND, NEW.into(), 0) };
                        continue;
                    }
                    if message.hwnd == list && selected {
                        match message.wParam as u16 {
                            VK_RETURN => {
                                if message.lParam & (1 << 30) == 0 {
                                    unsafe { SendMessageW(root, WM_COMMAND, CONNECT.into(), 0) };
                                }
                                continue;
                            }
                            VK_F2 => {
                                unsafe { SendMessageW(root, WM_COMMAND, RENAME.into(), 0) };
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

struct CardImages {
    handle: HIMAGELIST,
    width: i32,
}

impl CardImages {
    fn new(width: i32) -> Option<Self> {
        // A blank native image defines the clickable card rectangle. Custom draw supplies its content.
        let handle =
            unsafe { ImageList_Create((width - 2).max(1), CARD_HEIGHT, ILC_COLOR32, 1, 1) };
        if handle == 0 {
            return None;
        }
        let images = Self { handle, width };
        (unsafe { ImageList_SetImageCount(handle, 1) } != 0).then_some(images)
    }
}

impl Drop for CardImages {
    fn drop(&mut self) {
        unsafe { ImageList_Destroy(self.handle) };
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
            right: MIN_WIDTH,
            bottom: MIN_HEIGHT,
        };
        unsafe {
            AdjustWindowRectEx(&mut rect, WINDOW_STYLE, 0, 0);
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
                if unsafe { (*(lparam as *const NMHDR)).code } == NM_CUSTOMDRAW {
                    if let Ok(view) = view.try_borrow() {
                        return view.paint_cards(unsafe { &*(lparam as *const NMLVCUSTOMDRAW) });
                    }
                    return CDRF_DODEFAULT as LRESULT;
                }
                if let Ok(mut view) = view.try_borrow_mut() {
                    match unsafe { view.list_notify(hwnd, lparam) } {
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
            WM_TIMER if wparam == RENAME_TIMER => {
                unsafe { KillTimer(hwnd, RENAME_TIMER) };
                if let Ok(mut view) = view.try_borrow_mut()
                    && view
                        .rename_target
                        .take()
                        .is_some_and(|index| Some(index) == view.selected)
                {
                    view.rename_selected(hwnd);
                }
                return 0;
            }
            WM_SIZE if wparam != SIZE_MINIMIZED as usize => {
                if let Ok(view) = view.try_borrow() {
                    view.layout(hwnd);
                }
                return 0;
            }
            WM_DRAWITEM if lparam != 0 => {
                if let Ok(view) = view.try_borrow() {
                    let item = unsafe { &*(lparam as *const DRAWITEMSTRUCT) };
                    view.style.button(
                        item.hDC,
                        item.rcItem,
                        &text(item.hwndItem),
                        item.itemState,
                        item.CtlID as u16,
                    );
                    return 1;
                }
            }
            WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT => {
                if let Ok(view) = view.try_borrow() {
                    let control = lparam as HWND;
                    let surface = control == view.controls.brand || control == view.controls.search;
                    unsafe {
                        SetTextColor(
                            wparam as _,
                            if control == view.controls.status
                                || control == view.controls.empty_help
                            {
                                MUTED
                            } else {
                                TEXT
                            },
                        );
                        SetBkColor(wparam as _, if surface { SURFACE } else { BACKGROUND });
                    }
                    return if surface {
                        view.style.surface
                    } else {
                        view.style.background
                    } as LRESULT;
                }
            }
            WM_PAINT => {
                if let Ok(view) = view.try_borrow() {
                    let mut paint: PAINTSTRUCT = unsafe { std::mem::zeroed() };
                    let hdc = unsafe { BeginPaint(hwnd, &mut paint) };
                    let layout = Layout::read(hwnd, view.current().is_some());
                    view.style
                        .workspace(hdc, &layout, view.header_icon.0, view.current());
                    unsafe { EndPaint(hwnd, &paint) };
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
        self.controls.brand = self.control(hwnd, "STATIC", 0, SS_NOPREFIX)?;
        self.controls.settings = self.button(hwnd, SETTINGS)?;
        self.controls.update = self.button(hwnd, OPEN_UPDATE)?;
        self.controls.heading = self.control(hwnd, "STATIC", 0, SS_NOPREFIX)?;
        self.controls.add = self.button(hwnd, NEW)?;
        self.controls.search =
            self.control(hwnd, "EDIT", SEARCH, WS_TABSTOP | ES_AUTOHSCROLL as u32)?;
        self.controls.list = self.control(
            hwnd,
            "SysListView32",
            LIST,
            WS_TABSTOP
                | LVS_SINGLESEL
                | LVS_EDITLABELS
                | LVS_SHOWSELALWAYS
                | LVS_AUTOARRANGE
                | LVS_SHAREIMAGELISTS,
        )?;
        self.controls.empty_help = self.control(hwnd, "STATIC", 0, SS_CENTER | SS_NOPREFIX)?;
        self.controls.empty_join = self.button(hwnd, EMPTY_JOIN)?;
        self.controls.empty_manual = self.button(hwnd, MANUAL)?;
        self.controls.copy = self.button(hwnd, COPY_ENDPOINT)?;
        self.controls.actions = self.button(hwnd, ACTIONS)?;
        self.controls.files = self.button(hwnd, FILES)?;
        self.controls.connect = self.button(hwnd, CONNECT)?;
        self.controls.status = self.control(hwnd, "STATIC", 0, SS_ENDELLIPSIS | SS_NOPREFIX)?;
        self.controls.clipboard = self.control(hwnd, "EDIT", 0, ES_READONLY as u32)?;
        unsafe {
            ShowWindow(self.controls.clipboard, SW_HIDE);
            SendMessageW(self.controls.brand, WM_SETFONT, self.style.name as usize, 1);
            SendMessageW(
                self.controls.heading,
                WM_SETFONT,
                self.style.heading as usize,
                1,
            );
            SendMessageW(self.controls.list, LVM_SETBKCOLOR, 0, BACKGROUND as LPARAM);
            SendMessageW(
                self.controls.list,
                LVM_SETEXTENDEDLISTVIEWSTYLE,
                LVS_EX_DOUBLEBUFFER as usize,
                LVS_EX_DOUBLEBUFFER as LPARAM,
            );
        }
        self.refresh_list(hwnd, Some(0));
        Ok(())
    }

    fn control(&self, parent: HWND, class: &str, id: u16, style: u32) -> Result<HWND> {
        let class = wide(class);
        let result = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                null(),
                WS_CHILD | WS_VISIBLE | style,
                0,
                0,
                0,
                0,
                parent,
                id as usize as _,
                self.instance,
                null(),
            )
        };
        ensure!(
            !result.is_null(),
            "{}: {}",
            tr("Создать элемент окна SSHI", "Create an SSHI control"),
            last_error()
        );
        unsafe { SendMessageW(result, WM_SETFONT, self.style.font as usize, 1) };
        Ok(result)
    }

    fn button(&self, parent: HWND, id: u16) -> Result<HWND> {
        self.control(parent, "BUTTON", id, WS_TABSTOP | BS_OWNERDRAW as u32)
    }

    fn layout(&self, hwnd: HWND) {
        let area = Layout::read(hwnd, self.current().is_some());
        let center = area.width / 2;
        let middle = area.gallery.top + (area.gallery.bottom - area.gallery.top - 98).max(0) / 2;
        let placement = [
            (self.controls.brand, rect(MARGIN + 40, 15, 115, 25)),
            (
                self.controls.settings,
                rect(area.width - MARGIN - 128, 10, 128, 34),
            ),
            (
                self.controls.update,
                rect(area.width - MARGIN - 350, 10, 212, 34),
            ),
            (self.controls.heading, rect(MARGIN, TOP + 20, 385, 45)),
            (
                self.controls.add,
                rect(area.width - MARGIN - 212, TOP + 19, 212, 39),
            ),
            (
                self.controls.search,
                rect(
                    area.search.left + 39,
                    area.search.top + 9,
                    area.search.right - area.search.left - 51,
                    23,
                ),
            ),
            (self.controls.list, area.gallery),
            (
                self.controls.empty_help,
                rect(MARGIN, middle, area.width - 2 * MARGIN, 42),
            ),
            (
                self.controls.empty_join,
                rect(center - 188, middle + 48, 182, 36),
            ),
            (
                self.controls.empty_manual,
                rect(center + 6, middle + 48, 182, 36),
            ),
            (self.controls.copy, area.copy),
            (self.controls.actions, area.actions[0]),
            (self.controls.files, area.actions[1]),
            (self.controls.connect, area.actions[2]),
            (
                self.controls.status,
                rect(MARGIN, area.height - 29, area.width - 2 * MARGIN, 23),
            ),
        ];
        for (window, place) in placement {
            unsafe {
                MoveWindow(
                    window,
                    place.left,
                    place.top,
                    (place.right - place.left).max(0),
                    (place.bottom - place.top).max(0),
                    1,
                );
            }
        }
        let available = (area.gallery.right
            - area.gallery.left
            - unsafe { GetSystemMetrics(SM_CXVSCROLL) }
            - 2)
        .max(1);
        let columns = ((available + 12) / 242).clamp(1, 3);
        let width = available / columns;
        let mut current = self.card_images.borrow_mut();
        if current.as_ref().is_none_or(|images| images.width != width)
            && let Some(images) = CardImages::new(width)
        {
            let list = self.controls.list;
            let spacing = (CARD_HEIGHT as LPARAM + 10) << 16 | width as LPARAM;
            unsafe {
                SendMessageW(list, LVM_SETIMAGELIST, LVSIL_NORMAL as usize, images.handle);
                SendMessageW(list, LVM_SETICONSPACING, 0, spacing);
                SendMessageW(list, LVM_ARRANGE, LVA_DEFAULT as usize, 0);
            }
            *current = Some(images);
        }
        unsafe {
            InvalidateRect(hwnd, null(), 0);
            InvalidateRect(self.controls.list, null(), 1);
        }
    }

    fn paint_cards(&self, draw: &NMLVCUSTOMDRAW) -> LRESULT {
        let item = &draw.nmcd;
        match item.dwDrawStage {
            CDDS_PREPAINT => {
                unsafe { FillRect(item.hdc, &item.rc, self.style.background) };
                CDRF_NOTIFYITEMDRAW as LRESULT
            }
            CDDS_ITEMPREPAINT => {
                if let Some(index) = self.visible.get(item.dwItemSpec) {
                    let mut bounds = RECT {
                        left: LVIR_ICON as i32,
                        ..Default::default()
                    };
                    let found = unsafe {
                        SendMessageW(
                            self.controls.list,
                            LVM_GETITEMRECT,
                            item.dwItemSpec,
                            (&mut bounds as *mut RECT) as LPARAM,
                        )
                    };
                    if found != 0 {
                        self.style.card(
                            item.hdc,
                            bounds,
                            &self.profiles[*index],
                            self.selected == Some(*index),
                        );
                        return CDRF_SKIPDEFAULT as LRESULT;
                    }
                }
                CDRF_DODEFAULT as LRESULT
            }
            _ => CDRF_DODEFAULT as LRESULT,
        }
    }

    fn apply_language(&self, hwnd: HWND) {
        for (window, title) in [
            (self.controls.brand, "SSHI"),
            (self.controls.settings, tr("Настройки", "Settings")),
            (self.controls.heading, tr("Подключения", "Connections")),
            (
                self.controls.add,
                tr("+  Новое подключение", "+  New connection"),
            ),
            (
                self.controls.empty_join,
                tr("По приглашению…", "Join by invitation…"),
            ),
            (
                self.controls.empty_manual,
                tr("Ввести вручную…", "Enter manually…"),
            ),
            (
                self.controls.copy,
                tr("Скопировать Endpoint ID", "Copy Endpoint ID"),
            ),
            (self.controls.actions, tr("Действия", "Actions")),
            (self.controls.files, tr("Файлы", "Files")),
            (
                self.controls.connect,
                tr("Открыть терминал", "Open terminal"),
            ),
        ] {
            set_text(window, title);
        }
        let hint = wide(tr("Поиск подключений", "Search connections"));
        unsafe {
            SendMessageW(
                self.controls.search,
                EM_SETCUEBANNER,
                1,
                hint.as_ptr() as LPARAM,
            )
        };
        self.refresh_update_button();
        self.update_selection(hwnd);
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
        let command = match id {
            SETTINGS => popup(
                hwnd,
                self.controls.settings,
                &[
                    (
                        COPY,
                        tr("Скопировать мой Endpoint ID", "Copy my Endpoint ID"),
                    ),
                    (OPEN_DATA, tr("Папка данных", "Data folder")),
                    (0, ""),
                    (EXPORT, tr("Экспорт подключений…", "Back up connections…")),
                    (IMPORT, tr("Импорт подключений…", "Restore from backup…")),
                    (0, ""),
                    (LANG_RU, "Русский"),
                    (LANG_EN, "English"),
                    (0, ""),
                    (WHATS_NEW, tr("Что нового", "What’s new")),
                    (
                        CHECK_UPDATES,
                        tr("Проверить обновления…", "Check for updates…"),
                    ),
                ],
            )?,
            ACTIONS if self.current().is_some() => popup(
                hwnd,
                self.controls.actions,
                &[
                    (EDIT, tr("Изменить профиль…", "Edit profile…")),
                    (RENAME, tr("Переименовать…", "Rename…")),
                    (NOTES, tr("Заметки…", "Notes…")),
                    (FORWARD, tr("Пробросить порт…", "Forward a port…")),
                    (0, ""),
                    (ENROLL, tr("Ввести приглашение…", "Redeem invitation…")),
                    (
                        ISSUE_INVITE,
                        tr("Создать приглашение…", "Create invitation…"),
                    ),
                    (
                        INSTALL_SSH_KEY,
                        tr("Установить SSH-ключ…", "Install SSH key…"),
                    ),
                    (0, ""),
                    (DELETE, tr("Удалить подключение…", "Delete connection…")),
                ],
            )?,
            _ => Some(id),
        };
        match command {
            Some(LANG_RU | LANG_EN) => {
                self.store.set_english(command == Some(LANG_EN))?;
                self.apply_language(hwnd);
                self.status("");
            }
            Some(CHECK_UPDATES) => self.check_updates(hwnd, true)?,
            Some(OPEN_UPDATE) => self.offer_update(hwnd)?,
            Some(WHATS_NEW) => open_web_page(
                hwnd,
                concat!(
                    "https://github.com/gadmin2151/sshi/releases/tag/v",
                    env!("CARGO_PKG_VERSION")
                ),
            )?,
            Some(NEW | EMPTY_JOIN | ENROLL) => {
                let previous = if command == Some(ENROLL) {
                    self.current()
                } else {
                    None
                };
                match crate::join_dialog::show(hwnd, &self.store, self.id, previous)? {
                    crate::join_dialog::Outcome::Manual => self.edit_connection(hwnd, None)?,
                    crate::join_dialog::Outcome::Saved { name, open } => {
                        let profiles = self.store.profiles()?;
                        let index = profiles
                            .iter()
                            .position(|profile| profile.name == name)
                            .context(tr("Подключение отсутствует", "Connection not found"))?;
                        self.profiles = profiles;
                        self.reveal(hwnd, index);
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
            Some(MANUAL) => self.edit_connection(hwnd, None)?,
            Some(EDIT) => {
                if let Some(index) = self.selected {
                    self.edit_connection(hwnd, Some(index))?;
                }
            }
            Some(RENAME) => {
                self.rename_selected(hwnd);
            }
            Some(REVEAL_RENAMED) => {
                if let Some(index) = self.selected {
                    self.reveal(hwnd, index);
                    unsafe { SetFocus(self.controls.list) };
                }
            }
            Some(SEARCH) if event == EN_CHANGE as u16 => {
                self.refresh_list(hwnd, self.selected);
            }
            Some(DELETE) => self.delete(hwnd)?,
            Some(CONNECT) => self.connect()?,
            Some(FILES) => self.spawn_terminal("--files")?,
            Some(NOTES) => self.edit_notes(hwnd)?,
            Some(INSTALL_SSH_KEY) => self.spawn_terminal("--copy-ssh-key")?,
            Some(ISSUE_INVITE) => {
                let profile = self.selected_profile()?;
                if let Some((guest, account)) = crate::join_dialog::issue(hwnd, &profile)? {
                    self.launch_terminal(
                        profile,
                        "--issue-invite",
                        &[&guest.to_string(), &account],
                    )?;
                }
            }
            Some(FORWARD) => {
                let profile = self.selected_profile()?;
                if let Some(spec) = crate::forward_dialog::show(hwnd, &profile.name)? {
                    self.launch_terminal(profile, "--forward", &spec.ssh_args())?;
                }
            }
            Some(EXPORT) => {
                if let Some(path) = choose_file(hwnd, FileAction::Backup)? {
                    self.store.backup(&path)?;
                    self.status(tr(
                        "Резервная копия подключений сохранена",
                        "Connection backup saved",
                    ));
                }
            }
            Some(IMPORT) => {
                if let Some(path) = choose_file(hwnd, FileAction::Restore)? {
                    let selected = self.current().map(|profile| profile.name.clone());
                    let count = self.store.restore(&path)?;
                    self.profiles = self.store.profiles()?;
                    let index = self
                        .profiles
                        .iter()
                        .position(|profile| Some(&profile.name) == selected.as_ref());
                    self.refresh_list(hwnd, index);
                    self.status(&tr_format!(
                        "Добавлено подключений: {count}",
                        "Connections added: {count}"
                    ));
                }
            }
            Some(COPY) => self.copy_id(&self.id.to_string()),
            Some(COPY_ENDPOINT) => self.copy_id(&self.selected_profile()?.endpoint.to_string()),
            Some(OPEN_DATA) => {
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

    fn copy_id(&self, id: &str) {
        set_text(self.controls.clipboard, id);
        unsafe {
            SendMessageW(self.controls.clipboard, EM_SETSEL, 0, -1);
            SendMessageW(self.controls.clipboard, WM_COPY, 0, 0);
        }
        self.status(tr("Endpoint ID скопирован", "Endpoint ID copied"));
    }

    fn current(&self) -> Option<&Profile> {
        self.selected.and_then(|index| self.profiles.get(index))
    }

    fn selected_profile(&self) -> Result<Profile> {
        self.current()
            .cloned()
            .context(tr("Выберите подключение", "Select a connection"))
    }

    fn list_index(&self, item: i32) -> Option<usize> {
        usize::try_from(item)
            .ok()
            .and_then(|item| self.visible.get(item).copied())
    }

    fn reset_clicks(&mut self, hwnd: HWND) {
        unsafe { KillTimer(hwnd, RENAME_TIMER) };
        self.rename_target = None;
        self.clicked = None;
    }

    fn rename_selected(&mut self, hwnd: HWND) {
        self.reset_clicks(hwnd);
        if let Some(tile) = self
            .visible
            .iter()
            .position(|index| Some(*index) == self.selected)
        {
            unsafe {
                SetFocus(self.controls.list);
                SendMessageW(self.controls.list, LVM_EDITLABELW, tile, 0);
            }
        }
    }

    unsafe fn list_notify(&mut self, hwnd: HWND, param: LPARAM) -> Result<LRESULT> {
        match unsafe { (*(param as *const NMHDR)).code } {
            LVN_ITEMCHANGED => {
                let change = unsafe { &*(param as *const NMLISTVIEW) };
                if (change.uNewState ^ change.uOldState) & LVIS_SELECTED != 0 {
                    self.reset_clicks(hwnd);
                    if change.uNewState & LVIS_SELECTED != 0 {
                        self.selected = self.list_index(change.iItem);
                        unsafe {
                            InvalidateRect(hwnd, null(), 0);
                            InvalidateRect(self.controls.list, null(), 0);
                        }
                        self.status("");
                    }
                }
            }
            NM_CLICK | NM_DBLCLK => {
                let click = unsafe { &*(param as *const NMITEMACTIVATE) };
                let index = self.list_index(click.iItem);
                let clicked = self.clicked.take();
                self.reset_clicks(hwnd);
                if click.hdr.code == NM_DBLCLK {
                    if index.is_some()
                        && index == clicked.map(|(index, _)| index)
                        && index == self.selected
                    {
                        self.connect()?;
                    }
                } else if let Some(index) = index {
                    let interval = unsafe { GetDoubleClickTime() };
                    if self.selected == Some(index)
                        && clicked.is_some_and(|(last, time)| {
                            last == index && time.elapsed() > Duration::from_millis(interval.into())
                        })
                    {
                        self.rename_target = Some(index);
                        unsafe { SetTimer(hwnd, RENAME_TIMER, interval, None) };
                    }
                    self.clicked = Some((index, Instant::now()));
                }
            }
            NM_KILLFOCUS => self.reset_clicks(hwnd),
            LVN_BEGINLABELEDITW => return Ok(1),
            LVN_ENDLABELEDITW => {
                self.reset_clicks(hwnd);
                let item = unsafe { &(*(param as *const NMLVDISPINFOW)).item };
                if !item.pszText.is_null()
                    && let Some(index) = self.list_index(item.iItem)
                {
                    let editor =
                        unsafe { SendMessageW(self.controls.list, LVM_GETEDITCONTROL, 0, 0) }
                            as HWND;
                    ensure!(
                        !editor.is_null(),
                        tr("Поле названия недоступно", "The name editor is unavailable")
                    );
                    let mut profile = self.profiles[index].clone();
                    profile.name = text(editor);
                    self.persist_profile(profile, Some(index))?;
                    self.status(tr("Название сохранено", "Name saved"));
                    // Let the native control finish editing before rebuilding its items.
                    unsafe { PostMessageW(hwnd, WM_COMMAND, REVEAL_RENAMED.into(), 0) };
                    return Ok(1);
                }
            }
            _ => {}
        }
        Ok(0)
    }

    fn update_selection(&self, hwnd: HWND) {
        let empty = self.visible.is_empty();
        let initial = self.profiles.is_empty();
        set_text(
            self.controls.empty_help,
            if initial {
                tr(
                    "Добавьте подключение по приглашению или укажите Endpoint ID компьютера.",
                    "Use an invitation or enter the computer’s Endpoint ID to add a connection.",
                )
            } else {
                tr(
                    "По вашему запросу ничего не найдено.",
                    "No matching connections.",
                )
            },
        );
        unsafe {
            ShowWindow(self.controls.list, if empty { SW_HIDE } else { SW_SHOWNA });
            ShowWindow(
                self.controls.empty_help,
                if empty { SW_SHOWNA } else { SW_HIDE },
            );
            for button in [self.controls.empty_join, self.controls.empty_manual] {
                ShowWindow(button, if initial { SW_SHOWNA } else { SW_HIDE });
            }
            for button in [
                self.controls.actions,
                self.controls.files,
                self.controls.connect,
                self.controls.copy,
            ] {
                ShowWindow(
                    button,
                    if self.current().is_some() {
                        SW_SHOWNA
                    } else {
                        SW_HIDE
                    },
                );
            }
        }
        self.layout(hwnd);
        unsafe { InvalidateRect(self.controls.list, null(), 0) };
    }

    fn reveal(&mut self, hwnd: HWND, index: usize) {
        set_text(self.controls.search, "");
        self.refresh_list(hwnd, Some(index));
    }

    fn refresh_list(&mut self, hwnd: HWND, selected: Option<usize>) {
        let query = text(self.controls.search).trim().to_lowercase();
        self.visible = self
            .profiles
            .iter()
            .enumerate()
            .filter_map(|(index, profile)| {
                (query.is_empty()
                    || profile.name.to_lowercase().contains(&query)
                    || format!("{}@{}", profile.user, profile.endpoint)
                        .to_lowercase()
                        .contains(&query))
                .then_some(index)
            })
            .collect();
        self.selected = selected
            .filter(|index| self.visible.contains(index))
            .or_else(|| self.visible.first().copied());
        self.reset_clicks(hwnd);
        let list = self.controls.list;
        unsafe {
            SendMessageW(list, WM_SETREDRAW, 0, 0);
            SendMessageW(list, LVM_DELETEALLITEMS, 0, 0);
        }
        for (tile, index) in self.visible.iter().copied().enumerate() {
            let mut name = wide(&self.profiles[index].name);
            let item = LVITEMW {
                mask: LVIF_TEXT | LVIF_IMAGE,
                iItem: tile as i32,
                iImage: 0,
                pszText: name.as_mut_ptr(),
                ..Default::default()
            };
            unsafe {
                SendMessageW(
                    list,
                    LVM_INSERTITEMW,
                    0,
                    (&item as *const LVITEMW) as LPARAM,
                )
            };
            if self.selected == Some(index) {
                let state = LVITEMW {
                    stateMask: LVIS_SELECTED | LVIS_FOCUSED,
                    state: LVIS_SELECTED | LVIS_FOCUSED,
                    ..Default::default()
                };
                unsafe {
                    SendMessageW(
                        list,
                        LVM_SETITEMSTATE,
                        tile,
                        (&state as *const LVITEMW) as LPARAM,
                    )
                };
            }
        }
        unsafe { SendMessageW(list, WM_SETREDRAW, 1, 0) };
        self.update_selection(hwnd);
        if let Some(tile) = self
            .visible
            .iter()
            .position(|index| Some(*index) == self.selected)
        {
            unsafe { SendMessageW(list, LVM_ENSUREVISIBLE, tile, 0) };
        }
    }

    fn persist_profile(&mut self, mut profile: Profile, index: Option<usize>) -> Result<usize> {
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
            let previous = updated
                .get_mut(index)
                .context(tr("Подключение отсутствует", "Connection not found"))?;
            profile.notes = std::mem::take(&mut previous.notes);
            *previous = profile;
            index
        } else {
            updated.push(profile);
            updated.len() - 1
        };
        self.store.save_profiles(&updated)?;
        self.profiles = updated;
        Ok(selected)
    }

    fn edit_connection(&mut self, hwnd: HWND, index: Option<usize>) -> Result<()> {
        let initial = index.and_then(|index| self.profiles.get(index)).cloned();
        let token = if let Some(profile) = &initial {
            String::from_utf8(crypt(&profile.relay_token, false)?)
                .context(tr("Прочитать токен relay", "Read the relay token"))?
        } else {
            String::new()
        };
        let instance = self.instance;
        let mut prompt = ProfilePrompt {
            view: self,
            index,
            initial,
            token,
            relay: default_relay()?,
            saved: None,
        };
        let result = unsafe {
            DialogBoxParamW(
                instance,
                PROFILE_DIALOG as *const u16,
                hwnd,
                Some(profile_dialog),
                (&mut prompt as *mut ProfilePrompt<'_>) as LPARAM,
            )
        };
        if result == -1 {
            return Err(last_error()).context(tr(
                "Открыть редактор подключения",
                "Open the connection editor",
            ));
        }
        if let Some(index) = prompt.saved {
            self.reveal(hwnd, index);
            self.status(tr("Подключение сохранено", "Connection saved"));
        }
        Ok(())
    }

    fn edit_notes(&mut self, hwnd: HWND) -> Result<()> {
        let Some(profile) = self.selected.and_then(|index| self.profiles.get_mut(index)) else {
            return Ok(());
        };
        let mut prompt = NotesPrompt {
            store: &self.store,
            name: &profile.name,
            notes: &profile.notes,
            saved: None,
        };
        let result = unsafe {
            DialogBoxParamW(
                self.instance,
                NOTES_DIALOG as *const u16,
                hwnd,
                Some(notes_dialog),
                (&mut prompt as *mut NotesPrompt<'_>) as LPARAM,
            )
        };
        ensure!(
            result != -1,
            "{}: {}",
            tr("Открыть заметки", "Open the notes editor"),
            last_error()
        );
        if let Some(notes) = prompt.saved {
            profile.notes = notes;
            self.status(tr("Заметки сохранены", "Notes saved"));
        }
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
        self.refresh_list(hwnd, Some(index.min(self.profiles.len().saturating_sub(1))));
        self.status(tr("Подключение удалено", "Connection deleted"));
        Ok(())
    }

    fn connect(&self) -> Result<()> {
        self.spawn_terminal("--terminal")
    }

    fn spawn_terminal(&self, mode: &str) -> Result<()> {
        self.launch_terminal(self.selected_profile()?, mode, &[])
    }

    fn launch_terminal(&self, mut profile: Profile, mode: &str, args: &[&str]) -> Result<()> {
        if profile.name.is_empty() {
            profile.name = format!("{}@{}", profile.user, profile.endpoint.fmt_short());
        }
        profile.notes.clear();
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

struct ProfilePrompt<'a> {
    view: &'a mut View,
    index: Option<usize>,
    initial: Option<Profile>,
    token: String,
    relay: String,
    saved: Option<usize>,
}

unsafe extern "system" fn profile_dialog(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    let field = |id| unsafe { GetDlgItem(hwnd, id) };
    if msg == WM_INITDIALOG {
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp) };
        let prompt = unsafe { &*(lp as *const ProfilePrompt<'_>) };
        set_text(
            hwnd,
            if prompt.initial.is_some() {
                tr("Настройки подключения", "Connection settings")
            } else {
                tr("Новое подключение", "New connection")
            },
        );
        let record = prompt.initial.as_ref();
        let endpoint = record
            .map(|profile| profile.endpoint.to_string())
            .unwrap_or_default();
        for (id, value) in [
            (4221, tr("Название подключения", "Connection name")),
            (4222, tr("Пользователь SSH", "SSH username")),
            (4223, tr("Endpoint ID получателя", "Receiver endpoint ID")),
            (4224, tr("URL relay", "Relay URL")),
            (
                4225,
                tr("Токен relay (если требуется)", "Relay token (if required)"),
            ),
            (
                4226,
                tr("Файл SSH-ключа (необязательно)", "SSH key file (optional)"),
            ),
            (PROFILE_NAME, record.map_or("", |profile| &profile.name)),
            (PROFILE_USER, record.map_or("root", |profile| &profile.user)),
            (PROFILE_ENDPOINT, &endpoint),
            (PROFILE_RELAY, record.map_or("", |profile| &profile.relay)),
            (PROFILE_TOKEN, &prompt.token),
            (PROFILE_KEY, record.map_or("", |profile| &profile.ssh_key)),
            (PROFILE_BROWSE, tr("Выбрать…", "Browse…")),
            (PROFILE_OPEN, tr("Открыть терминал", "Open terminal")),
            (PROFILE_FILES, tr("Файлы", "Files")),
            (IDOK, tr("Сохранить", "Save")),
            (IDCANCEL, tr("Отмена", "Cancel")),
        ] {
            set_text(field(id), value);
        }
        let relay = wide(&prompt.relay);
        unsafe {
            SendMessageW(
                field(PROFILE_RELAY),
                EM_SETCUEBANNER,
                0,
                relay.as_ptr() as LPARAM,
            )
        };
        return 1;
    }
    if msg == WM_CLOSE {
        unsafe { EndDialog(hwnd, 0) };
        return 1;
    }
    if msg != WM_COMMAND {
        return 0;
    }
    let id = i32::from(wp as u16);
    match id {
        IDCANCEL => {
            unsafe { EndDialog(hwnd, 0) };
        }
        PROFILE_RELAY if (wp >> 16) as u16 == EN_CHANGE as u16 => {
            // A token is specific to the relay entered with it.
            set_text(field(PROFILE_TOKEN), "");
        }
        PROFILE_BROWSE => match choose_file(hwnd, FileAction::Key) {
            Ok(Some(path)) => set_text(field(PROFILE_KEY), &path.to_string_lossy()),
            Ok(None) => {}
            Err(error) => set_text(field(PROFILE_ERROR), &format!("{error:#}")),
        },
        IDOK | PROFILE_OPEN | PROFILE_FILES => {
            let pointer =
                unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut ProfilePrompt<'_>;
            let Some(prompt) = (unsafe { pointer.as_mut() }) else {
                return 0;
            };
            let result = read_profile(hwnd, &prompt.relay).and_then(|profile| {
                if id == IDOK {
                    prompt.saved = Some(prompt.view.persist_profile(profile, prompt.index)?);
                } else {
                    let mode = if id == PROFILE_OPEN {
                        "--terminal"
                    } else {
                        "--files"
                    };
                    prompt.view.launch_terminal(profile, mode, &[])?;
                }
                Ok(())
            });
            if let Err(error) = result {
                set_text(field(PROFILE_ERROR), &format!("{error:#}"));
            } else if id == IDOK {
                unsafe { EndDialog(hwnd, 1) };
            } else {
                set_text(field(PROFILE_ERROR), "");
            }
        }
        _ => return 0,
    }
    1
}

fn read_profile(hwnd: HWND, default: &str) -> Result<Profile> {
    let value = |id| text(unsafe { GetDlgItem(hwnd, id) });
    let name = value(PROFILE_NAME).trim().to_owned();
    let user = value(PROFILE_USER).trim().to_owned();
    ensure!(
        iroh_ssh::profiles::valid_user(&user),
        tr("Укажите имя пользователя SSH", "Enter an SSH username")
    );
    let endpoint = value(PROFILE_ENDPOINT).trim().parse().context(tr(
        "Проверьте Endpoint ID получателя",
        "Check the receiver endpoint ID",
    ))?;
    let relay_input = value(PROFILE_RELAY);
    let inherit = relay_input.trim().is_empty();
    let relay = relay_url(if inherit { default } else { relay_input.trim() })
        .context(tr("Проверьте URL relay", "Check the relay URL"))?;
    let token = value(PROFILE_TOKEN);
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
        notes: String::new(),
        ssh_key: value(PROFILE_KEY).trim().to_owned(),
        relay_token: crypt(token.trim().as_bytes(), true)?,
    })
}

fn popup(owner: HWND, button: HWND, entries: &[(u16, &str)]) -> Result<Option<u16>> {
    let menu = unsafe { CreatePopupMenu() };
    ensure!(
        !menu.is_null(),
        "{}: {}",
        tr("Создать меню", "Create the menu"),
        last_error()
    );
    let result = (|| {
        let language = if lang::english() { LANG_EN } else { LANG_RU };
        for (id, title) in entries {
            let flags = if *id == 0 {
                MF_SEPARATOR
            } else if *id == language {
                MF_CHECKED
            } else {
                MF_STRING
            };
            ensure!(
                unsafe { AppendMenuW(menu, flags, usize::from(*id), wide(title).as_ptr()) } != 0,
                "{}: {}",
                tr("Добавить действие в меню", "Add a menu action"),
                last_error()
            );
        }
        let mut anchor = RECT::default();
        ensure!(
            unsafe { GetWindowRect(button, &mut anchor) } != 0,
            "{}: {}",
            tr("Найти кнопку меню", "Locate the menu button"),
            last_error()
        );
        let command = unsafe {
            TrackPopupMenu(
                menu,
                TPM_RIGHTALIGN | TPM_RETURNCMD | TPM_NONOTIFY,
                anchor.right,
                anchor.bottom,
                0,
                owner,
                null(),
            )
        };
        Ok(u16::try_from(command).ok().filter(|id| *id != 0))
    })();
    unsafe { DestroyMenu(menu) };
    result
}

struct NotesPrompt<'a> {
    store: &'a Store,
    name: &'a str,
    notes: &'a str,
    saved: Option<String>,
}

unsafe extern "system" fn notes_dialog(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    if msg == WM_INITDIALOG {
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp) };
        let prompt = unsafe { &*(lp as *const NotesPrompt<'_>) };
        let field = unsafe { GetDlgItem(hwnd, NOTES_TEXT) };
        set_text(hwnd, tr("Заметки", "Notes"));
        set_text(unsafe { GetDlgItem(hwnd, NOTES_PROFILE) }, prompt.name);
        set_text(unsafe { GetDlgItem(hwnd, IDOK) }, tr("Сохранить", "Save"));
        set_text(
            unsafe { GetDlgItem(hwnd, IDCANCEL) },
            tr("Отмена", "Cancel"),
        );
        unsafe { SendMessageW(field, EM_LIMITTEXT, 0, 0) };
        set_text(
            field,
            &prompt
                .notes
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .replace('\n', "\r\n"),
        );
        return 1;
    }
    if msg == WM_COMMAND {
        match (wp & 0xffff) as i32 {
            IDOK => {
                let pointer =
                    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut NotesPrompt<'_>;
                let Some(prompt) = (unsafe { pointer.as_mut() }) else {
                    return 0;
                };
                let field = unsafe { GetDlgItem(hwnd, NOTES_TEXT) };
                let notes = text(field).replace("\r\n", "\n");
                if notes != prompt.notes.replace("\r\n", "\n").replace('\r', "\n") {
                    if let Err(error) = prompt.store.set_notes(prompt.name, &notes) {
                        set_text(
                            unsafe { GetDlgItem(hwnd, NOTES_ERROR) },
                            &format!("{error:#}"),
                        );
                        return 1;
                    }
                    prompt.saved = Some(notes);
                }
                unsafe { EndDialog(hwnd, 1) };
                return 1;
            }
            IDCANCEL => {
                unsafe { EndDialog(hwnd, 0) };
                return 1;
            }
            _ => {}
        }
    }
    0
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
