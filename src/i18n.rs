//! 界面语言（中文 / English）。
//!
//! 文本直接写在调用处：`tr!("中文", "English")` 返回 `&'static str`，
//! `trf!("共 {n} 个", "{n} total")` 走 `format!` 返回 `String`。
//! 当前语言是全局状态，GUI 切换后下一帧就会全部重绘成新语言。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

static ENGLISH: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Zh,
    En,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::Zh, Lang::En];

    pub fn label(self) -> &'static str {
        match self {
            Lang::Zh => "中文",
            Lang::En => "English",
        }
    }

    fn code(self) -> &'static str {
        match self {
            Lang::Zh => "zh",
            Lang::En => "en",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        if s.starts_with("zh") {
            Some(Lang::Zh)
        } else if s.starts_with("en") {
            Some(Lang::En)
        } else {
            None
        }
    }
}

pub fn is_en() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

pub fn current() -> Lang {
    if is_en() { Lang::En } else { Lang::Zh }
}

pub fn set(lang: Lang) {
    ENGLISH.store(lang == Lang::En, Ordering::Relaxed);
}

/// 优先级：环境变量 CARD_FORGE_LANG > 上次在界面里的选择 > 系统语言 > English。
pub fn init() {
    let saved = || std::fs::read_to_string(config_path()?).ok().and_then(|s| Lang::parse(&s));
    let lang = std::env::var("CARD_FORGE_LANG")
        .ok()
        .and_then(|s| Lang::parse(&s))
        .or_else(saved)
        .or_else(system_lang)
        .unwrap_or(Lang::En);
    set(lang);
}

/// 记住界面里选择的语言。
pub fn save(lang: Lang) {
    let Some(path) = config_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, lang.code());
}

fn system_lang() -> Option<Lang> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|v| !v.is_empty())
        .map(|v| if v.starts_with("zh") { Lang::Zh } else { Lang::En })
}

fn config_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("card-forge").join("lang"))
}

macro_rules! tr {
    ($zh:literal, $en:literal $(,)?) => {
        if $crate::i18n::is_en() { $en } else { $zh }
    };
}

macro_rules! trf {
    ($zh:literal, $en:literal $(, $($arg:tt)*)?) => {
        if $crate::i18n::is_en() { format!($en $(, $($arg)*)?) } else { format!($zh $(, $($arg)*)?) }
    };
}

pub(crate) use {tr, trf};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_codes() {
        assert_eq!(Lang::parse("zh_CN.UTF-8"), Some(Lang::Zh));
        assert_eq!(Lang::parse(" EN\n"), Some(Lang::En));
        assert_eq!(Lang::parse("fr"), None);
    }

    #[test]
    fn macros_follow_language() {
        let n = 2;
        set(Lang::En);
        assert_eq!(tr!("是", "yes"), "yes");
        assert_eq!(trf!("{n} 张", "{n} images"), "2 images");
        set(Lang::Zh);
        assert_eq!(trf!("{n} 张，{}", "{n} images, {}", "x"), "2 张，x");
    }
}
