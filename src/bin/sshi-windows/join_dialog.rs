use std::{
    cell::RefCell,
    ptr::null,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use anyhow::{Context, Result, ensure};
use iroh::EndpointId;
use iroh_ssh::{
    invite::{Code, InvitationRoute, MAX_INVITATION_BYTES, SharedInvitation},
    run_async,
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::{
            BST_CHECKED, CheckDlgButton, EM_LIMITTEXT, EM_SETCUEBANNER, EM_SETREADONLY, EM_SETSEL,
            IsDlgButtonChecked,
        },
        Input::KeyboardAndMouse::EnableWindow,
        WindowsAndMessaging::*,
    },
};

use crate::{
    broker,
    lang::{tr, tr_format},
    store::{Profile, Store, crypt, last_error, wide},
    terminal::{default_relay, relay_route},
    window::{set_text, text},
};

const DIALOG: usize = 5;
const INVITATION: i32 = 2201;
const ENDPOINT: i32 = 2202;
const USER: i32 = 2203;
const RELAY: i32 = 2204;
const NAME: i32 = 2205;
const TOKEN: i32 = 2206;
const OWN_ID: i32 = 2207;
const COPY: i32 = 2208;
const OPEN: i32 = 2209;
const STATUS: i32 = 2210;
const MANUAL: i32 = 2218;
const PROGRESS_TIMER: usize = 1;

pub enum Outcome {
    Closed,
    Manual,
    Saved { name: String, open: bool },
}

enum Progress {
    Editing,
    Redeeming(Receiver<Result<()>>, Profile),
    Granted(Profile),
}

struct State<'a> {
    store: &'a Store,
    own: EndpointId,
    initial: Option<&'a Profile>,
    default_relay: String,
    progress: Progress,
    outcome: Outcome,
}

pub fn show(
    owner: HWND,
    store: &Store,
    own: EndpointId,
    initial: Option<&Profile>,
) -> Result<Outcome> {
    let state = RefCell::new(State {
        store,
        own,
        initial,
        default_relay: default_relay()?,
        progress: Progress::Editing,
        outcome: Outcome::Closed,
    });
    let result = unsafe {
        DialogBoxParamW(
            GetModuleHandleW(null()),
            DIALOG as *const u16,
            owner,
            Some(dialog_proc),
            (&state as *const RefCell<State<'_>>) as LPARAM,
        )
    };
    if result == -1 {
        return Err(last_error()).context(tr("Открыть приглашение", "Open invitation"));
    }
    Ok(state.into_inner().outcome)
}

fn field(dialog: HWND, id: i32) -> HWND {
    unsafe { GetDlgItem(dialog, id) }
}
fn say(dialog: HWND, message: &str) {
    set_text(field(dialog, STATUS), message);
}

impl State<'_> {
    fn init(&self, dialog: HWND) {
        set_text(
            dialog,
            tr("Подключение по приглашению", "Join by invitation"),
        );
        for (id, caption) in [
            (
                2211,
                tr("Код или ссылка приглашения", "Invitation code or link"),
            ),
            (2212, tr("Endpoint ID получателя", "Receiver endpoint ID")),
            (2213, tr("Пользователь SSH", "SSH username")),
            (2214, tr("URL relay", "Relay URL")),
            (2215, tr("Название подключения", "Connection name")),
            (
                2216,
                tr(
                    "Токен этого relay (если требуется)",
                    "Token for this relay (if required)",
                ),
            ),
            (
                2217,
                tr(
                    "Ещё нет приглашения? Отправьте свой Endpoint ID владельцу получателя или уже авторизованному пользователю.",
                    "Need an invitation? Send your endpoint ID to the receiver’s owner or someone who already has access.",
                ),
            ),
            (COPY, tr("Копировать ID", "Copy my ID")),
            (
                OPEN,
                tr(
                    "Открыть терминал SSH после сохранения",
                    "Open an SSH terminal after saving",
                ),
            ),
            (MANUAL, tr("Ввести вручную…", "Set up manually…")),
            (IDOK, tr("Принять и сохранить", "Join and save")),
            (IDCANCEL, tr("Отмена", "Cancel")),
        ] {
            set_text(field(dialog, id), caption);
        }
        for (id, hint) in [
            (
                INVITATION,
                tr(
                    "Вставьте ссылку sshi:// или 64-значный код",
                    "Paste an sshi:// link or a 64-character code",
                ),
            ),
            (
                ENDPOINT,
                tr(
                    "Вставьте ID сервера или вставьте ссылку в поле выше",
                    "Paste the server ID, or paste an invitation link above",
                ),
            ),
            (
                NAME,
                tr("По умолчанию — пользователь@ID", "Defaults to username@ID"),
            ),
        ] {
            unsafe {
                SendMessageW(
                    field(dialog, id),
                    EM_SETCUEBANNER,
                    0,
                    wide(hint).as_ptr() as LPARAM,
                );
            }
        }
        unsafe {
            CheckDlgButton(dialog, OPEN, BST_CHECKED);
            SendMessageW(
                field(dialog, INVITATION),
                EM_LIMITTEXT,
                MAX_INVITATION_BYTES + 1,
                0,
            );
        }
        set_text(field(dialog, OWN_ID), &self.own.to_string());
        set_text(
            field(dialog, USER),
            self.initial.map_or("root", |p| &p.user),
        );
        set_text(
            field(dialog, RELAY),
            self.initial
                .map(|p| p.relay.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(&self.default_relay),
        );
        if let Some(p) = self.initial {
            set_text(field(dialog, ENDPOINT), &p.endpoint.to_string());
        }
        self.update_preview(dialog, false);
    }

    fn update_preview(&self, dialog: HWND, invitation_changed: bool) {
        let invitation = text(field(dialog, INVITATION));
        let shared = invitation.parse::<SharedInvitation>().ok();
        if invitation_changed && let Some(parsed) = &shared {
            set_text(field(dialog, ENDPOINT), &parsed.route.endpoint.to_string());
            set_text(field(dialog, USER), &parsed.route.user);
            let relay = parsed
                .route
                .relay
                .as_ref()
                .map(|r| r.as_str())
                .unwrap_or(&self.default_relay);
            if text(field(dialog, RELAY)) != relay {
                set_text(field(dialog, TOKEN), "");
                set_text(field(dialog, RELAY), relay);
            }
        }
        for id in [ENDPOINT, USER, RELAY] {
            let fixed = shared
                .as_ref()
                .is_some_and(|s| id != RELAY || s.route.relay.is_some());
            unsafe {
                SendMessageW(field(dialog, id), EM_SETREADONLY, fixed as WPARAM, 0);
            }
        }
        let valid_code = shared.is_some() || invitation.parse::<Code>().is_ok();
        let endpoint = text(field(dialog, ENDPOINT))
            .trim()
            .parse::<EndpointId>()
            .ok();
        let valid_route = endpoint
            .and_then(|id| {
                InvitationRoute::new(
                    id,
                    text(field(dialog, USER)).trim(),
                    Some(text(field(dialog, RELAY)).trim()),
                )
                .ok()
            })
            .is_some();
        let message = if invitation.trim().is_empty() {
            tr(
                "Вставьте приглашение. Если вам дали отдельно ID получателя и код, заполните первые два поля.",
                "Paste your invitation. If you received a receiver ID and a separate code, enter both in the first two fields.",
            )
        } else if !valid_code {
            tr(
                "Проверьте ссылку или код приглашения (64 шестнадцатеричных символа).",
                "Check the invitation link or the 64-character hexadecimal code.",
            )
        } else if !valid_route {
            tr(
                "Проверьте Endpoint ID получателя, пользователя SSH и URL relay.",
                "Check the receiver endpoint ID, SSH username and relay URL.",
            )
        } else {
            tr(
                "Проверьте получателя, пользователя SSH и relay, затем примите приглашение.",
                "Review the receiver, SSH username and relay, then join.",
            )
        };
        say(dialog, message);
        unsafe {
            EnableWindow(field(dialog, IDOK), (valid_route && valid_code) as i32);
        }
    }

    fn profile(&self, dialog: HWND) -> Result<(Profile, Code)> {
        let input = text(field(dialog, INVITATION));
        let (mut route, code) = if input.trim().starts_with("sshi:") {
            let shared = input.parse::<SharedInvitation>()?;
            (shared.route, shared.code)
        } else {
            let endpoint = text(field(dialog, ENDPOINT)).trim().parse()?;
            (
                InvitationRoute::new(endpoint, text(field(dialog, USER)).trim(), None)?,
                input.parse::<Code>()?,
            )
        };
        let relay = match route.relay.take() {
            Some(included) => included,
            None => InvitationRoute::relay_url(text(field(dialog, RELAY)).trim())?,
        }
        .to_string();
        let endpoint = route.endpoint;
        let matching = |p: &Profile| {
            p.endpoint == endpoint
                && p.user == route.user
                && (p.relay == relay || (p.relay.is_empty() && self.default_relay == relay))
        };
        let mut name = text(field(dialog, NAME)).trim().to_owned();
        if name.is_empty() {
            name = self
                .initial
                .filter(|p| matching(p))
                .map(|p| p.name.clone())
                .unwrap_or_else(|| format!("{}@{}", route.user, endpoint.fmt_short()));
        }
        let existing = self
            .store
            .profiles()?
            .into_iter()
            .find(|p| p.name.to_lowercase() == name.to_lowercase());
        ensure!(
            existing.as_ref().is_none_or(matching),
            tr(
                "Это название занято другим подключением. Укажите другое.",
                "Another connection uses this name. Enter another name."
            )
        );
        let mut profile = existing.unwrap_or_else(|| Profile {
            name,
            endpoint,
            user: route.user,
            relay: relay.clone(),
            ssh_key: String::new(),
            relay_token: Vec::new(),
        });
        let token_field = text(field(dialog, TOKEN));
        let token = token_field.trim();
        ensure!(
            token.is_empty() || relay.starts_with("https://"),
            tr(
                "Для relay с токеном требуется HTTPS.",
                "A relay with a token requires HTTPS."
            )
        );
        ensure!(
            !token.chars().any(|c| c.is_whitespace() || c.is_control()),
            tr("Проверьте токен relay.", "Check the relay token.")
        );
        if !token.is_empty() {
            profile.relay_token = crypt(token.as_bytes(), true)?;
        }
        profile.relay = relay;
        profile.portable().validate()?;
        Ok((profile, code))
    }

    fn begin(&mut self, dialog: HWND) -> Result<()> {
        if matches!(self.progress, Progress::Granted(_)) {
            return self.save(dialog);
        }
        if !matches!(self.progress, Progress::Editing) {
            return Ok(());
        }
        let (profile, code) = self.profile(dialog)?;
        let route = relay_route(&profile)?;
        let endpoint = profile.endpoint;
        let store = self.store.clone();
        let (sender, receiver) = mpsc::channel();
        ensure!(
            unsafe { SetTimer(dialog, PROGRESS_TIMER, 120, None) } != 0,
            tr(
                "Не удалось запустить ожидание",
                "Could not start the progress timer"
            )
        );
        if let Err(error) = thread::Builder::new()
            .name("sshi-invite".into())
            .spawn(move || {
                let result = run_async(broker::redeem(&store, &route, endpoint, code));
                let _ = sender.send(result);
            })
        {
            unsafe {
                KillTimer(dialog, PROGRESS_TIMER);
            }
            return Err(error.into());
        }
        self.progress = Progress::Redeeming(receiver, profile);
        self.enable_form(dialog, false);
        say(
            dialog,
            tr(
                "Получатель проверяет приглашение через Iroh…",
                "The receiver is checking your invitation over Iroh…",
            ),
        );
        Ok(())
    }

    fn enable_form(&self, dialog: HWND, enabled: bool) {
        for id in [
            INVITATION, ENDPOINT, USER, RELAY, NAME, TOKEN, MANUAL, IDOK, IDCANCEL,
        ] {
            unsafe {
                EnableWindow(field(dialog, id), enabled as i32);
            }
        }
    }

    fn poll(&mut self, dialog: HWND) -> Result<()> {
        let Progress::Redeeming(receiver, _) = &self.progress else {
            return Ok(());
        };
        let result = match receiver.try_recv() {
            Ok(done) => done,
            Err(TryRecvError::Empty) => return Ok(()),
            Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!(tr(
                "Проверка приглашения прервана.",
                "Invitation check was interrupted."
            ))),
        };
        unsafe {
            KillTimer(dialog, PROGRESS_TIMER);
        }
        let Progress::Redeeming(_, profile) =
            std::mem::replace(&mut self.progress, Progress::Editing)
        else {
            unreachable!()
        };
        if let Err(e) = result {
            self.enable_form(dialog, true);
            self.update_preview(dialog, false);
            return Err(e.context(tr("Не удалось принять приглашение. Проверьте срок действия, свой ID и доступ к relay, затем повторите.", "Could not redeem the invitation. Check its expiry, your endpoint ID and relay access, then try again.")));
        }
        self.progress = Progress::Granted(profile);
        set_text(field(dialog, INVITATION), "");
        set_text(field(dialog, TOKEN), "");
        unsafe {
            EnableWindow(field(dialog, IDOK), 1);
            EnableWindow(field(dialog, IDCANCEL), 1);
        }
        set_text(field(dialog, IDOK), tr("Сохранить", "Save"));
        self.save(dialog)
    }

    fn save(&mut self, dialog: HWND) -> Result<()> {
        let Progress::Granted(profile) = &self.progress else {
            return Ok(());
        };
        let mut profiles = self.store.profiles()?;
        let index = profiles
            .iter()
            .position(|p| p.name.to_lowercase() == profile.name.to_lowercase());
        if let Some(i) = index {
            profiles[i] = profile.clone();
        } else {
            profiles.push(profile.clone());
        }
        self.store.save_profiles(&profiles).context(tr(
            "Доступ уже разрешён. Повторите сохранение подключения.",
            "Access has been granted. Try saving the connection again.",
        ))?;
        self.outcome = Outcome::Saved {
            name: profile.name.clone(),
            open: unsafe { IsDlgButtonChecked(dialog, OPEN) } == BST_CHECKED,
        };
        unsafe {
            EndDialog(dialog, 1);
        }
        Ok(())
    }
}

unsafe extern "system" fn dialog_proc(dialog: HWND, msg: u32, w: WPARAM, l: LPARAM) -> isize {
    if msg == WM_INITDIALOG {
        unsafe {
            SetWindowLongPtrW(dialog, GWLP_USERDATA, l);
        }
    }
    let ptr = unsafe { GetWindowLongPtrW(dialog, GWLP_USERDATA) } as *const RefCell<State<'_>>;
    let Some(cell) = (unsafe { ptr.as_ref() }) else {
        return 0;
    };
    let Ok(mut state) = cell.try_borrow_mut() else {
        return 0;
    };
    let result = match msg {
        WM_INITDIALOG => {
            state.init(dialog);
            return 1;
        }
        WM_TIMER if w == PROGRESS_TIMER => state.poll(dialog),
        WM_COMMAND => {
            let id = i32::from(w as u16);
            match id {
                IDOK => state.begin(dialog),
                IDCANCEL | MANUAL => {
                    if !matches!(state.progress, Progress::Redeeming(..)) {
                        if id == MANUAL {
                            state.outcome = Outcome::Manual;
                        }
                        unsafe {
                            EndDialog(dialog, 0);
                        }
                    }
                    return 1;
                }
                COPY => {
                    let id = field(dialog, OWN_ID);
                    unsafe {
                        SendMessageW(id, EM_SETSEL, 0, -1);
                        SendMessageW(id, WM_COPY, 0, 0);
                    }
                    say(
                        dialog,
                        tr(
                            "Ваш Endpoint ID скопирован.",
                            "Your endpoint ID was copied.",
                        ),
                    );
                    return 1;
                }
                INVITATION | ENDPOINT | USER | RELAY
                    if (w >> 16) as u16 == EN_CHANGE as u16
                        && matches!(state.progress, Progress::Editing) =>
                {
                    if id == RELAY {
                        set_text(field(dialog, TOKEN), "");
                    }
                    state.update_preview(dialog, id == INVITATION);
                    return 1;
                }
                _ => return 0,
            }
        }
        WM_CLOSE => {
            if !matches!(state.progress, Progress::Redeeming(..)) {
                unsafe {
                    EndDialog(dialog, 0);
                }
            }
            return 1;
        }
        _ => return 0,
    };
    if let Err(e) = result {
        say(dialog, &tr_format!("Ошибка: {e:#}", "Error: {e:#}"));
    }
    1
}

pub fn issue(owner: HWND, profile: &Profile) -> Result<Option<(EndpointId, String)>> {
    struct Issuer<'a> {
        profile: &'a Profile,
        guest: Option<(EndpointId, String)>,
    }
    unsafe extern "system" fn issuer_proc(dialog: HWND, msg: u32, w: WPARAM, l: LPARAM) -> isize {
        if msg == WM_INITDIALOG {
            unsafe {
                SetWindowLongPtrW(dialog, GWLP_USERDATA, l);
            }
        }
        let ptr = unsafe { GetWindowLongPtrW(dialog, GWLP_USERDATA) } as *const RefCell<Issuer<'_>>;
        let Some(cell) = (unsafe { ptr.as_ref() }) else {
            return 0;
        };
        let Ok(mut state) = cell.try_borrow_mut() else {
            return 0;
        };
        match msg {
            WM_INITDIALOG => {
                set_text(dialog, tr("Создать приглашение", "Create invitation"));
                let name = &state.profile.name;
                let endpoint = state.profile.endpoint.fmt_short();
                set_text(
                    field(dialog, 2301),
                    &tr_format!(
                        "Для доступа к «{name}» ({endpoint}) получите Endpoint ID с экрана приглашения нового компьютера.",
                        "To grant access to “{name}” ({endpoint}), ask for the endpoint ID shown on the guest computer’s invitation screen."
                    ),
                );
                for (id, label) in [
                    (
                        2302,
                        tr(
                            "Endpoint ID приглашённого компьютера",
                            "Guest computer’s endpoint ID",
                        ),
                    ),
                    (
                        2304,
                        tr(
                            "Пользователь службы sshi на получателе",
                            "Account running SSHI on the receiver",
                        ),
                    ),
                    (IDOK, tr("Создать ссылку", "Create link")),
                    (IDCANCEL, tr("Отмена", "Cancel")),
                ] {
                    set_text(field(dialog, id), label);
                }
                set_text(field(dialog, 2305), &state.profile.user);
                unsafe {
                    EnableWindow(field(dialog, IDOK), 0);
                }
                1
            }
            WM_COMMAND => {
                let id = i32::from(w as u16);
                if id == IDCANCEL {
                    unsafe {
                        EndDialog(dialog, 0);
                    }
                    return 1;
                }
                if id == IDOK || (matches!(id, 2303 | 2305) && (w >> 16) as u16 == EN_CHANGE as u16)
                {
                    let guest = text(field(dialog, 2303)).trim().parse::<EndpointId>().ok();
                    let account = text(field(dialog, 2305)).trim().to_owned();
                    let valid = guest.is_some() && iroh_ssh::profiles::valid_user(&account);
                    unsafe {
                        EnableWindow(field(dialog, IDOK), valid as i32);
                    }
                    if id == IDOK
                        && valid
                        && let Some(guest) = guest
                    {
                        state.guest = Some((guest, account));
                        unsafe {
                            EndDialog(dialog, 1);
                        }
                    }
                    return 1;
                }
                0
            }
            WM_CLOSE => {
                unsafe {
                    EndDialog(dialog, 0);
                }
                1
            }
            _ => 0,
        }
    }
    let state = RefCell::new(Issuer {
        profile,
        guest: None,
    });
    let result = unsafe {
        DialogBoxParamW(
            GetModuleHandleW(null()),
            6 as *const u16,
            owner,
            Some(issuer_proc),
            (&state as *const RefCell<Issuer<'_>>) as LPARAM,
        )
    };
    if result == -1 {
        return Err(last_error()).context(tr("Создать приглашение", "Create invitation"));
    }
    Ok(state.into_inner().guest)
}
