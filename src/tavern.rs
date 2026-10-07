//! 定位 SillyTavern 的数据目录和角色 Gallery 目录。
//!
//! 规则照搬 SillyTavern 1.19 的实现：
//! - 数据目录：环境变量 SILLYTAVERN_DATAROOT > config.yaml 的 `dataRoot`（相对安装目录）> `./data`；
//!   全局安装模式下是 `~/.local/share/SillyTavern/data`。
//! - 用户目录：`<数据目录>/<用户名>`，未开启多用户时是 `default-user`。
//! - Gallery：`<用户目录>/user/images/<sanitize(文件夹)>`，文件夹默认是角色名，
//!   可在酒馆里按角色改写，改写记录在 settings.json 的
//!   `extension_settings.gallery.folders`，键是角色头像文件名。
//! - 导入、新建、改名时角色名本身也会被 sanitize，头像文件名是 `sanitize(角色名)`，重名时追加数字。

use std::path::{Component, Path, PathBuf};

use serde_json::Value;

/// 酒馆 Gallery 能显示的图片格式（MEDIA_EXTENSIONS 中的图片部分）。
pub const GALLERY_EXTS: [&str; 7] = ["bmp", "png", "jpg", "jpeg", "jfif", "webp", "gif"];

#[derive(Clone, Debug, PartialEq)]
pub struct Tavern {
    pub data_root: PathBuf,
    /// 用户目录名，`default-user` 排在最前。
    pub users: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FolderSource {
    /// 在酒馆里给这个角色设置过自定义 Gallery 文件夹。
    Override,
    /// 酒馆里有这个角色，使用角色名。
    Character,
    /// 酒馆里还没有导入这个角色，按角色名预先放好。
    NotImported,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GalleryTarget {
    pub dir: PathBuf,
    pub source: FolderSource,
    /// 匹配到的酒馆角色头像文件名。
    pub avatar: Option<String>,
}

/// 移植自 npm 包 sanitize-filename（酒馆用它清理文件夹和文件名）。
pub fn sanitize_filename(input: &str) -> String {
    let mut s: String = input
        .chars()
        .filter(|&c| !"/?<>\\:*|\"".contains(c) && !matches!(c as u32, 0x00..=0x1f | 0x80..=0x9f))
        .collect();
    if !s.is_empty() && s.chars().all(|c| c == '.') {
        s.clear();
    }
    if is_windows_reserved(&s) {
        s.clear();
    }
    let trimmed = s.trim_end_matches(['.', ' ']).len();
    s.truncate(trimmed);
    truncate_utf8(&s, 255).to_owned()
}

fn is_windows_reserved(s: &str) -> bool {
    let stem = s.split('.').next().unwrap_or("").to_ascii_lowercase();
    let reserved = matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && stem.as_bytes()[3].is_ascii_digit());
    // 正则是 ^(con|...)(\..*)?$，即保留名本身或保留名后跟扩展名
    reserved && (s.len() == stem.len() || s.as_bytes()[stem.len()] == b'.')
}

fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// 依次尝试用户指定的目录、环境变量和常见安装位置。
pub fn detect(custom: Option<&Path>) -> Option<Tavern> {
    if let Some(p) = custom {
        return resolve(p);
    }
    if let Some(root) = std::env::var_os("SILLYTAVERN_DATAROOT").map(PathBuf::from)
        && let Some(t) = from_data_root(&root)
    {
        return Some(t);
    }
    let home = dirs::home_dir()?;
    let mut candidates = vec![
        home.join("SillyTavern"),
        home.join("sillytavern"),
        home.join("SillyTavern-Launcher/SillyTavern"),
        home.join("Documents/SillyTavern"),
    ];
    if let Some(data) = dirs::data_dir() {
        candidates.push(data.join("SillyTavern")); // npm 全局安装
    }
    candidates.iter().find_map(|p| resolve(p))
}

/// 接受酒馆安装目录、数据目录或单个用户目录。
pub fn resolve(path: &Path) -> Option<Tavern> {
    if path.join("config.yaml").is_file() {
        let root = config_data_root(&path.join("config.yaml")).unwrap_or_else(|| "./data".into());
        let root = if Path::new(&root).is_absolute() { PathBuf::from(root) } else { normalize(&path.join(root)) };
        if let Some(t) = from_data_root(&root) {
            return Some(t);
        }
    }
    if let Some(t) = from_data_root(path).or_else(|| from_data_root(&path.join("data"))) {
        return Some(t);
    }
    // 直接选中了某个用户目录
    let parent = path.parent()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
    is_user_dir(path).then(|| Tavern { data_root: parent.to_owned(), users: vec![name] })
}

/// 去掉路径中的 `.`，让显示的路径更干净。
fn normalize(p: &Path) -> PathBuf {
    p.components().filter(|c| !matches!(c, Component::CurDir)).collect()
}

fn from_data_root(root: &Path) -> Option<Tavern> {
    let mut users: Vec<String> = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with(['_', '.']) && is_user_dir(&root.join(n)))
        .collect();
    if users.is_empty() {
        return None;
    }
    users.sort_by_key(|u| (u != "default-user", u.clone()));
    Some(Tavern { data_root: root.to_owned(), users })
}

fn is_user_dir(p: &Path) -> bool {
    p.join("characters").is_dir() || p.join("user/images").is_dir()
}

/// 只取 config.yaml 顶层的 `dataRoot`，不引入 YAML 解析库。
fn config_data_root(config: &Path) -> Option<String> {
    let text = std::fs::read_to_string(config).ok()?;
    let line = text.lines().find(|l| l.starts_with("dataRoot:"))?;
    let value = line["dataRoot:".len()..].split(" #").next()?.trim();
    let value = value.trim_matches(|c| c == '"' || c == '\'');
    (!value.is_empty()).then(|| value.to_owned())
}

impl Tavern {
    pub fn user_dir(&self, user: &str) -> PathBuf {
        self.data_root.join(user)
    }

    pub fn gallery_target(&self, user: &str, card_name: &str) -> Option<GalleryTarget> {
        if card_name.trim().is_empty() {
            return None;
        }
        let user_dir = self.user_dir(user);
        let avatars = find_avatars(&user_dir.join("characters"), card_name);
        let overrides = read_overrides(&user_dir.join("settings.json"));
        let images = user_dir.join("user/images");

        let overridden = avatars.iter().find_map(|a| overrides.iter().find(|(k, _)| k == a));
        let (folder, source, avatar) = match overridden {
            Some((avatar, folder)) => (folder.clone(), FolderSource::Override, Some(avatar.clone())),
            None if !avatars.is_empty() => (card_name.to_owned(), FolderSource::Character, avatars.first().cloned()),
            None => (card_name.to_owned(), FolderSource::NotImported, None),
        };
        let folder = sanitize_filename(&folder);
        if folder.is_empty() {
            return None;
        }
        Some(GalleryTarget { dir: images.join(folder), source, avatar })
    }
}

/// 找出酒馆里名字和这张卡相同的角色头像文件（`名字.png`、`名字1.png`……）。
/// 酒馆保存的角色名是 sanitize 过的，比较时两种都接受。
fn find_avatars(characters: &Path, card_name: &str) -> Vec<String> {
    let base = sanitize_filename(card_name);
    let Ok(entries) = std::fs::read_dir(characters) else { return Vec::new() };
    let mut found: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|f| {
            f.strip_suffix(".png")
                .and_then(|stem| stem.strip_prefix(base.as_str()))
                .is_some_and(|suffix| suffix.chars().all(|c| c.is_ascii_digit()))
        })
        .filter(|f| {
            // 头像文件名只是根据导入时的名字生成的，需要读卡确认
            let stored = crate::card::load_file(&characters.join(f)).ok().and_then(|l| l.card).map(|c| c.data.name);
            stored.is_some_and(|n| n == base || n == card_name)
        })
        .collect();
    found.sort_by_key(|f| (f.len(), f.clone()));
    found
}

fn read_overrides(settings: &Path) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(settings) else { return Vec::new() };
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return Vec::new() };
    let Some(Value::Object(folders)) = v.pointer("/extension_settings/gallery/folders") else { return Vec::new() };
    folders.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned()))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_matches_npm_package() {
        assert_eq!(sanitize_filename("Hisami Yomotsu, Beautiful"), "Hisami Yomotsu, Beautiful");
        assert_eq!(sanitize_filename("A/B:C*D?\"E<F>G|H\\"), "ABCDEFGH");
        assert_eq!(sanitize_filename("  lead and trail. . "), "  lead and trail");
        assert_eq!(sanitize_filename(".."), "");
        assert_eq!(sanitize_filename("CON"), "");
        assert_eq!(sanitize_filename("com1.txt"), "");
        assert_eq!(sanitize_filename("console"), "console");
        assert_eq!(sanitize_filename("tab\there\u{85}"), "tabhere");
        assert_eq!(sanitize_filename("Princess | Aurelia"), "Princess  Aurelia");
        let long = "字".repeat(100); // 300 字节
        assert_eq!(sanitize_filename(&long).len(), 255);
    }

    fn write_card(path: &Path, name: &str) {
        let data = crate::card::CardData { name: name.into(), ..Default::default() };
        std::fs::write(path, crate::card::build_png(&crate::png::tiny_png(), &data, false).unwrap()).unwrap();
    }

    #[test]
    fn resolves_install_dir_with_custom_data_root() {
        let dir = tempfile::tempdir().unwrap();
        let st = dir.path().join("ST");
        std::fs::create_dir_all(st.join("store/alice/characters")).unwrap();
        std::fs::create_dir_all(st.join("store/default-user/user/images")).unwrap();
        std::fs::create_dir_all(st.join("store/_storage")).unwrap();
        std::fs::write(st.join("config.yaml"), "port: 8000\ndataRoot: \"./store\" # comment\n").unwrap();

        let t = resolve(&st).unwrap();
        assert_eq!(t.data_root, st.join("store"));
        assert_eq!(t.users, ["default-user", "alice"]);
        // 也能直接选数据目录或用户目录
        assert_eq!(resolve(&st.join("store")).unwrap().users.len(), 2);
        assert_eq!(resolve(&st.join("store/alice")).unwrap().users, ["alice"]);
        assert!(resolve(dir.path()).is_none());
    }

    #[test]
    fn gallery_target_follows_tavern_rules() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("default-user");
        std::fs::create_dir_all(user.join("characters")).unwrap();
        let t = Tavern { data_root: dir.path().to_owned(), users: vec!["default-user".into()] };
        let images = user.join("user/images");

        // 未导入
        let g = t.gallery_target("default-user", "Princess | Aurelia").unwrap();
        assert_eq!((g.dir, g.source), (images.join("Princess  Aurelia"), FolderSource::NotImported));

        // 已导入：酒馆存的是 sanitize 后的名字；第二个同名角色带数字后缀；文件名相同但卡内名字不同的要排除
        write_card(&user.join("characters/Princess  Aurelia1.png"), "Princess  Aurelia");
        write_card(&user.join("characters/Princess  Aurelia2.png"), "Someone else");
        let g = t.gallery_target("default-user", "Princess | Aurelia").unwrap();
        assert_eq!(g.source, FolderSource::Character);
        assert_eq!(g.avatar.as_deref(), Some("Princess  Aurelia1.png"));

        // 酒馆里改写过文件夹
        let settings = r#"{"extension_settings":{"gallery":{"folders":{"Princess  Aurelia1.png":"elf: pics"}}}}"#;
        std::fs::write(user.join("settings.json"), settings).unwrap();
        let g = t.gallery_target("default-user", "Princess | Aurelia").unwrap();
        assert_eq!((g.dir, g.source), (images.join("elf pics"), FolderSource::Override));

        assert!(t.gallery_target("default-user", "  ").is_none());
    }
}
