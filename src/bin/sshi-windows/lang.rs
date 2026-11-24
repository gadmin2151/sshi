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
