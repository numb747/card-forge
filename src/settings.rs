//! 持久化的用户设置：`~/.config/card-forge/settings.json`。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::i18n::trf;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// 界面语言代码（"zh" / "en"）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// 图片库根目录，每张卡一个子文件夹。为空时使用默认位置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_dir: Option<PathBuf>,
    /// 酒馆安装目录或数据目录。为空时自动查找。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tavern_dir: Option<PathBuf>,
    /// 酒馆多用户模式下导出到哪个用户。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tavern_user: Option<String>,
}

fn config_dir() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("card-forge"))
}

impl Settings {
    /// 读取设置。文件损坏时先改名备份再用默认值，并返回一条提示，
    /// 避免下次保存时悄悄覆盖掉用户的设置。
    pub fn load() -> (Self, Option<String>) {
        let Some(dir) = config_dir() else { return Default::default() };
        let (mut s, warning) = Self::load_from(&dir);
        if s.lang.is_none() {
            // 0.1.0 把语言单独存在 lang 文件里
            s.lang = std::fs::read_to_string(dir.join("lang")).ok().map(|t| t.trim().to_owned());
        }
        (s, warning)
    }

    fn load_from(dir: &std::path::Path) -> (Self, Option<String>) {
        let path = dir.join("settings.json");
        let Ok(text) = std::fs::read_to_string(&path) else { return Default::default() };
        match serde_json::from_str(&text) {
            Ok(s) => (s, None),
            Err(e) => {
                let backup = dir.join("settings.json.broken");
                let _ = std::fs::rename(&path, &backup);
                let msg = trf!(
                    "设置文件无法解析（{e}），已备份为 {} 并使用默认设置",
                    "Could not parse the settings file ({e}); backed it up as {} and using defaults",
                    backup.display()
                );
                (Self::default(), Some(msg))
            }
        }
    }

    pub fn save(&self) -> Result<(), String> {
        self.save_to(&config_dir().ok_or("no config directory")?)
    }

    /// 先写临时文件再改名，写到一半退出也不会留下损坏的设置文件。
    fn save_to(&self, dir: &std::path::Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let tmp = dir.join("settings.json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, dir.join("settings.json")).map_err(|e| e.to_string())
    }

    pub fn image_root(&self) -> PathBuf {
        self.image_dir.clone().unwrap_or_else(default_image_root)
    }
}

/// 默认放在「图片」目录下，方便直接用看图软件浏览。
pub fn default_image_root() -> PathBuf {
    dirs::picture_dir()
        .map(|p| p.join("card-forge"))
        .or_else(|| dirs::data_dir().map(|p| p.join("card-forge/images")))
        .unwrap_or_else(|| std::env::temp_dir().join("card-forge"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_broken_file() {
        let dir = tempfile::tempdir().unwrap();
        let s = Settings { image_dir: Some("/x".into()), tavern_user: Some("alice".into()), ..Default::default() };
        s.save_to(dir.path()).unwrap();
        assert_eq!(Settings::load_from(dir.path()), (s, None));

        std::fs::write(dir.path().join("settings.json"), "{ \"lang\": \"zh\", }").unwrap();
        let (loaded, warning) = Settings::load_from(dir.path());
        assert_eq!(loaded, Settings::default());
        assert!(warning.is_some());
        assert!(dir.path().join("settings.json.broken").exists(), "损坏的文件被保留下来");
        assert!(!dir.path().join("settings.json").exists());
    }
}
