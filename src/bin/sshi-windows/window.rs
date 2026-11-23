use std::{
    cell::RefCell,
    env,
    ffi::OsString,
    mem::size_of,
    os::windows::ffi::OsStringExt,
    path::PathBuf,
    process::Command,
    ptr::{null, null_mut},
};

use anyhow::{Context, Result, bail, ensure};
use iroh::EndpointId;
use iroh_ssh::relay_url;
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
            EM_SETCUEBANNER, EM_SETSEL,
        },
        Input::KeyboardAndMouse::SetFocus,
        Shell::ShellExecuteW,
        WindowsAndMessaging::*,
    },
};

use crate::{
    SESSION_ENV,
    store::{Profile, Store, crypt, last_error, wide},
    terminal::default_relay,
};

const WIDTH: i32 = 852;
const HEIGHT: i32 = 556;
const HEADER: u32 = 0x322315;
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
const APP_MENU_RESOURCE: usize = 3;
const STYLE: u32 = WS_OVERLAPPEDWINDOW;

const INVITATION_DIALOG: usize = 2;
const INVITATION_CODE: i32 = 2001;
const COPY_INVITATION: i32 = 2002;

#[derive(Default)]
struct Controls {
    title: HWND,
    caption: HWND,
    list: HWND,
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
    controls: Controls,
    font: HFONT,
    title_font: HFONT,
    header_brush: windows_sys::Win32::Graphics::Gdi::HBRUSH,
    header_icon: Icon,
    instance: HINSTANCE,
    layout: RefCell<Vec<ChildLayout>>,
}

struct ChildLayout {
    window: HWND,
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
    ensure!(!instance.is_null(), "Загрузить окно sshi");
    let large_icon = Icon::load(instance, unsafe { GetSystemMetrics(SM_CXICON) }, unsafe {
        GetSystemMetrics(SM_CYICON)
    })?;
    let small_icon = Icon::load(instance, unsafe { GetSystemMetrics(SM_CXSMICON) }, unsafe {
        GetSystemMetrics(SM_CYSMICON)
    })?;
    let header_icon = Icon::load(instance, HEADER_ICON_SIZE, HEADER_ICON_SIZE)?;
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
        return Err(last_error()).context("Зарегистрировать окно sshi");
    }
    let view = RefCell::new(View {
        store,
        id,
        profiles,
        selected: None,
        controls: Controls::default(),
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
        return Err(last_error()).context("Открыть окно подключений");
    }
    unsafe {
        SendMessageW(root, WM_SETICON, ICON_BIG as WPARAM, large_icon.0 as LPARAM);
        SendMessageW(
            root,
            WM_SETICON,
            ICON_SMALL as WPARAM,
            small_icon.0 as LPARAM,
        );
        ShowWindow(root, SW_SHOW);
    }
    let mut message: MSG = unsafe { std::mem::zeroed() };
    loop {
        match unsafe { GetMessageW(&mut message, null_mut(), 0, 0) } {
            0 => return Ok(()),
            -1 => return Err(last_error()).context("Прочитать события окна"),
            _ => {
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
            return Err(last_error()).context("Загрузить иконку sshi");
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
            WM_COMMAND => {
                if let Ok(mut view) = view.try_borrow_mut()
                    && let Err(error) = view.command(hwnd, wparam as u16, (wparam >> 16) as u16)
                {
                    message_box(hwnd, &format!("{error:#}"), MB_ICONERROR);
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
        self.controls.caption = self.label(hwnd, "SSH через iroh", [162, 28, 355, 23])?;
        self.button(hwnd, "Папка данных", [698, 20, 132, 32], OPEN_DATA)?;

        self.label(hwnd, "Подключения", [22, 92, 236, 23])?;
        self.controls.list = self.control(
            hwnd,
            "LISTBOX",
            "",
            [22, 124, 236, 351],
            LIST,
            WS_TABSTOP | WS_VSCROLL | LBS_NOTIFY as u32 | LBS_NOINTEGRALHEIGHT as u32,
            WS_EX_CLIENTEDGE,
        )?;
        self.button(hwnd, "Новое", [22, 490, 110, 29], NEW)?;
        self.button(hwnd, "Удалить", [148, 490, 110, 29], DELETE)?;

        self.label(hwnd, "Название подключения", [288, 92, 526, 20])?;
        self.controls.name = self.edit(hwnd, [288, 115, 542, 27], 0)?;
        self.label(hwnd, "Пользователь", [288, 154, 126, 20])?;
        self.label(hwnd, "Endpoint ID получателя", [418, 154, 412, 20])?;
        self.controls.user = self.edit(hwnd, [288, 177, 116, 27], 0)?;
        self.controls.endpoint = self.edit(hwnd, [418, 177, 412, 27], 0)?;
        self.label(hwnd, "URL relay", [288, 216, 542, 20])?;
        self.controls.relay = self.control(
            hwnd,
            "EDIT",
            "",
            [288, 239, 542, 27],
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
        self.label(hwnd, "Токен relay (если требуется)", [288, 278, 542, 20])?;
        self.controls.token = self.edit(hwnd, [288, 301, 542, 27], ES_PASSWORD as u32)?;
        self.label(hwnd, "Файл SSH-ключа (необязательно)", [288, 340, 542, 20])?;
        self.controls.ssh_key = self.edit(hwnd, [288, 363, 424, 27], 0)?;
        self.button(hwnd, "Выбрать…", [723, 363, 107, 27], BROWSE)?;
        self.label(hwnd, "Мой Endpoint ID", [288, 402, 542, 20])?;
        self.controls.my_id = self.edit(hwnd, [288, 425, 424, 27], ES_READONLY as u32)?;
        self.button(hwnd, "Копировать", [723, 425, 107, 27], COPY)?;
        self.button(hwnd, "Ввести приглашение…", [288, 459, 168, 26], ENROLL)?;
        self.button(
            hwnd,
            "Добавить SSH-ключ…",
            [468, 459, 176, 26],
            INSTALL_SSH_KEY,
        )?;
        self.button(
            hwnd,
            "Создать приглашение…",
            [656, 459, 174, 26],
            ISSUE_INVITE,
        )?;
        self.button(hwnd, "Проброс портов…", [288, 490, 169, 29], FORWARD)?;
        self.button(hwnd, "Сохранить", [552, 490, 125, 29], SAVE)?;
        self.button(hwnd, "Подключиться", [690, 490, 140, 29], CONNECT)?;
        self.controls.status = self.label(hwnd, "", [22, 528, 808, 20])?;
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
        text: &str,
        rect: [i32; 4],
        id: u16,
        style: u32,
        ex_style: u32,
    ) -> Result<HWND> {
        let is_edit = class == "EDIT";
        let class = wide(class);
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
            return Err(last_error()).context("Создать элемент окна sshi");
        }
        unsafe { SendMessageW(result, WM_SETFONT, self.font as usize, 1) };
        let horizontal = match id {
            OPEN_DATA | BROWSE | COPY | ENROLL | INSTALL_SSH_KEY | ISSUE_INVITE | SAVE
            | CONNECT | FORWARD => Anchor::End,
            _ if is_edit && rect[2] >= 400 => Anchor::Stretch,
            _ => Anchor::Start,
        };
        let vertical = match id {
            LIST => Anchor::Stretch,
            NEW | DELETE | ENROLL | INSTALL_SSH_KEY | ISSUE_INVITE | SAVE | CONNECT | FORWARD => {
                Anchor::End
            }
            _ if rect[1] >= 528 => Anchor::End,
            _ => Anchor::Start,
        };
        self.layout.borrow_mut().push(ChildLayout {
            window: result,
            rect,
            horizontal,
            vertical,
        });
        Ok(result)
    }

    fn label(&self, parent: HWND, text: &str, rect: [i32; 4]) -> Result<HWND> {
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

    fn button(&self, parent: HWND, text: &str, rect: [i32; 4], id: u16) -> Result<HWND> {
        self.control(parent, "BUTTON", text, rect, id, WS_TABSTOP, 0)
    }

    fn command(&mut self, hwnd: HWND, id: u16, event: u16) -> Result<()> {
        match id {
            LIST if matches!(event as u32, LBN_SELCHANGE | LBN_DBLCLK) => {
                let index = unsafe { SendMessageW(self.controls.list, LB_GETCURSEL, 0, 0) };
                if index >= 0 {
                    self.select(index as usize)?;
                    if event as u32 == LBN_DBLCLK {
                        self.connect()?;
                    }
                }
            }
            NEW => self.new_profile(),
            DELETE => self.delete(hwnd)?,
            SAVE => self.save()?,
            CONNECT => self.connect()?,
            ENROLL => self.spawn_terminal("--enroll")?,
            INSTALL_SSH_KEY => self.spawn_terminal("--copy-ssh-key")?,
            ISSUE_INVITE => self.spawn_terminal("--issue-invite")?,
            FORWARD => self.spawn_terminal("--forward")?,
            EXPORT => {
                if let Some(path) = choose_file(hwnd, FileAction::Backup)? {
                    self.store.backup(&path)?;
                    self.status("Резервная копия подключений сохранена");
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
                    self.status(&format!("Добавлено подключений: {count}"));
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
                self.status("Endpoint ID скопирован");
            }
            OPEN_DATA => {
                let path = wide(self.store.path());
                let verb = wide("open");
                let result = unsafe {
                    ShellExecuteW(hwnd, verb.as_ptr(), path.as_ptr(), null(), null(), SW_SHOW)
                };
                ensure!(result as isize > 32, "Не удалось открыть папку данных");
            }
            _ => {}
        }
        Ok(())
    }

    fn refresh_list(&self, selected: Option<usize>) {
        unsafe { SendMessageW(self.controls.list, LB_RESETCONTENT, 0, 0) };
        for profile in &self.profiles {
            let name = wide(&profile.name);
            unsafe { SendMessageW(self.controls.list, LB_ADDSTRING, 0, name.as_ptr() as LPARAM) };
        }
        if let Some(index) = selected {
            unsafe { SendMessageW(self.controls.list, LB_SETCURSEL, index, 0) };
        }
    }

    fn new_profile(&mut self) {
        self.selected = None;
        unsafe { SendMessageW(self.controls.list, LB_SETCURSEL, usize::MAX, 0) };
        set_text(self.controls.name, "");
        set_text(self.controls.user, "root");
        set_text(self.controls.endpoint, "");
        set_text(self.controls.relay, "");
        set_text(self.controls.token, "");
        set_text(self.controls.ssh_key, "");
        self.status("Добавьте Endpoint ID получателя, чтобы подключиться");
        unsafe { SetFocus(self.controls.name) };
    }

    fn select(&mut self, index: usize) -> Result<()> {
        let profile = self
            .profiles
            .get(index)
            .context("Подключение отсутствует")?;
        let token = String::from_utf8(crypt(&profile.relay_token, false)?)
            .context("Прочитать токен relay")?;
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
            "Укажите имя пользователя SSH"
        );
        let endpoint = text(self.controls.endpoint)
            .trim()
            .parse()
            .context("Проверьте Endpoint ID получателя")?;
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
        .context("Проверьте URL relay")?;
        ensure!(
            token.trim().is_empty() || relay.scheme() == "https",
            "Для relay с токеном требуется HTTPS"
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

    fn save(&mut self) -> Result<()> {
        let profile = self.form()?;
        profile.portable().validate()?;
        let mut updated = self.profiles.clone();
        if updated.iter().enumerate().any(|(index, old)| {
            Some(index) != self.selected && old.name.to_lowercase() == profile.name.to_lowercase()
        }) {
            bail!("Подключение с таким названием уже есть");
        }
        let selected = if let Some(index) = self.selected {
            *updated.get_mut(index).context("Подключение отсутствует")? = profile;
            index
        } else {
            updated.push(profile);
            updated.len() - 1
        };
        self.store.save_profiles(&updated)?;
        self.profiles = updated;
        self.selected = Some(selected);
        self.refresh_list(Some(selected));
        self.select(selected)?;
        self.status("Подключение сохранено");
        Ok(())
    }

    fn delete(&mut self, hwnd: HWND) -> Result<()> {
        let index = self.selected.context("Выберите подключение для удаления")?;
        let profile = self
            .profiles
            .get(index)
            .context("Подключение отсутствует")?;
        if message_box(
            hwnd,
            &format!("Удалить подключение «{}»?", profile.name),
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
        self.status("Подключение удалено");
        Ok(())
    }

    fn connect(&self) -> Result<()> {
        self.spawn_terminal("--terminal")
    }

    fn spawn_terminal(&self, mode: &str) -> Result<()> {
        let mut profile = self.form()?;
        if profile.name.is_empty() {
            profile.name = format!("{}@{}", profile.user, profile.endpoint.fmt_short());
        }
        Command::new(env::current_exe()?)
            .arg(mode)
            .env(SESSION_ENV, toml::to_string(&profile)?)
            .spawn()
            .context("Запустить терминал sshi")?;
        self.status("Терминал открыт");
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

fn set_text(handle: HWND, value: &str) {
    let value = wide(value);
    unsafe { SetWindowTextW(handle, value.as_ptr()) };
}

fn text(handle: HWND) -> String {
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
        FileAction::Key => ("Выберите приватный ключ SSH", "Все файлы\0*.*\0"),
        FileAction::Backup => (
            "Сохранить резервную копию подключений",
            "Резервная копия SSHI (*.json)\0*.json\0",
        ),
        FileAction::Restore => (
            "Восстановить подключения",
            "Резервная копия SSHI (*.json)\0*.json\0Все файлы\0*.*\0",
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
            "Не удалось открыть диалог выбора файла (0x{error:08x})"
        );
        return Ok(None);
    }
    let length = filename
        .iter()
        .position(|&ch| ch == 0)
        .unwrap_or(filename.len());
    Ok(Some(OsString::from_wide(&filename[..length]).into()))
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
        return Err(last_error()).context("Открыть окно с кодом приглашения");
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
            unsafe { SetDlgItemTextW(dialog, INVITATION_CODE, lparam as *const u16) };
            1
        }
        WM_COMMAND => match i32::from(wparam as u16) {
            COPY_INVITATION => {
                let code = unsafe { GetDlgItem(dialog, INVITATION_CODE) };
                let copied = wide("Код скопирован");
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
