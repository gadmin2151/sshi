use std::sync::atomic::{AtomicBool, Ordering};

static ENGLISH: AtomicBool = AtomicBool::new(false);

pub fn english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

pub fn set_english(value: bool) {
    ENGLISH.store(value, Ordering::Relaxed);
}

pub fn tr(ru: &'static str, en: &'static str) -> &'static str {
    if english() { en } else { ru }
}

macro_rules! tr_format {
    ($ru:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if crate::lang::english() { format!($en $(, $arg)*) } else { format!($ru $(, $arg)*) }
    };
}

pub(crate) use tr_format;

pub fn caption(source: &'static str) -> &'static str {
    if !english() {
        return source;
    }
    match source {
        "SSH через iroh" => "SSH over Iroh",
        "Папка данных" => "Data folder",
        "Подключения" => "Connections",
        "Новое" => "New…",
        "По приглашению…" => "Join by invitation…",
        "Ввести вручную…" => "Enter manually…",
        "Чтобы подключиться, используйте приглашение от владельца сервера или введите адрес вручную." => {
            "Use an invitation from the server owner or enter the connection details manually."
        }
        "Удалить" => "Delete",
        "Название подключения" => "Connection name",
        "Пользователь" => "Username",
        "Endpoint ID получателя" => "Receiver endpoint ID",
        "URL relay" => "Relay URL",
        "Токен relay (если требуется)" => "Relay token (if required)",
        "Файл SSH-ключа (необязательно)" => "SSH key file (optional)",
        "Выбрать…" => "Browse…",
        "Мой Endpoint ID" => "My endpoint ID",
        "Копировать" => "Copy",
        "Ввести приглашение…" => "Redeem invitation…",
        "Добавить SSH-ключ…" => "Install SSH key…",
        "Создать приглашение…" => "Create invitation…",
        "Проброс портов…" => "Port forwarding…",
        "Сохранить" => "Save",
        "Подключиться" => "Connect",
        "Терминал" => "Terminal",
        "Файлы" => "Files",
        _ => source,
    }
}
