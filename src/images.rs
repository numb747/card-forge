//! 扫描卡片字段里的图片链接、下载到本地缓存、整理图片 HTML。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use base64::Engine as _;
use regex::Regex;
use serde_json::Value;

use crate::card::CardData;
use crate::i18n::{tr, trf};

static IMG_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?is)<img\b[^>]*>"#).unwrap());
static IMG_SRC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)\bsrc\s*=\s*(?:"([^"]+)"|'([^']+)'|([^\s>]+))"#).unwrap());
static MD_IMAGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"!\[[^\]]*\]\(\s*<?([^)\s>]+)>?(?:\s+"[^"]*")?\s*\)"#).unwrap());
static BARE_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)https?://[^\s"'<>()\[\]\\]+?\.(?:png|jpe?g|webp|gif|avif|bmp)(?:\?[^\s"'<>()\[\]\\]*)?"#).unwrap()
});
static HTML_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").unwrap());

/// 会发给模型的定义字段。开场白不算：图片放在开场白里是为了在聊天中显示。
pub const MODEL_FIELDS: [&str; 6] =
    ["description", "personality", "scenario", "mes_example", "system_prompt", "post_history_instructions"];

#[derive(Clone, Debug, PartialEq)]
pub struct ImageRef {
    pub url: String,
    /// 出现在哪些字段，如 "personality"、"alternate_greetings[0]"。
    pub fields: Vec<String>,
}

/// 从一段文本中按出现顺序提取图片链接（去重）。
pub fn extract_urls(text: &str) -> Vec<String> {
    let mut found: Vec<(usize, String)> = Vec::new();
    for tag in IMG_TAG.find_iter(text) {
        if let Some(c) = IMG_SRC.captures(tag.as_str()) {
            let src = c.get(1).or(c.get(2)).or(c.get(3)).unwrap();
            found.push((tag.start(), html_unescape(src.as_str())));
        }
    }
    for c in MD_IMAGE.captures_iter(text) {
        found.push((c.get(0).unwrap().start(), c[1].to_owned()));
    }
    for m in BARE_URL.find_iter(text) {
        found.push((m.start(), html_unescape(m.as_str())));
    }
    found.sort_by_key(|(pos, _)| *pos);
    let mut out: Vec<String> = Vec::new();
    for (_, url) in found {
        let ok = url.starts_with("http://") || url.starts_with("https://") || url.starts_with("data:image/");
        if ok && !out.contains(&url) {
            out.push(url);
        }
    }
    out
}

/// 扫描整张卡（包括世界书和未建模的扩展字段）中的图片链接。
pub fn scan_card(data: &CardData) -> Vec<ImageRef> {
    let mut refs: Vec<ImageRef> = Vec::new();
    let value = serde_json::to_value(data).unwrap();
    walk(&value, String::new(), &mut |path, text| {
        for url in extract_urls(text) {
            match refs.iter_mut().find(|r| r.url == url) {
                Some(r) if !r.fields.contains(&path.to_owned()) => r.fields.push(path.to_owned()),
                Some(_) => {}
                None => refs.push(ImageRef { url, fields: vec![path.to_owned()] }),
            }
        }
    });
    refs
}

fn walk(v: &Value, path: String, f: &mut impl FnMut(&str, &str)) {
    match v {
        Value::String(s) => f(&path, s),
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                walk(x, format!("{path}[{i}]"), f);
            }
        }
        Value::Object(m) => {
            for (k, x) in m {
                let p = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                walk(x, p, f);
            }
        }
        _ => {}
    }
}

/// 把模型可见字段里的图片（`<img>` 和 Markdown 图片）移到作者备注。
/// 返回 (移动的图片标签数, 移动后仍有文字残留、需要人工检查的字段)。
pub fn move_images_to_notes(data: &mut CardData) -> (usize, Vec<String>) {
    let mut moved: Vec<String> = Vec::new();
    let mut leftovers = Vec::new();
    for name in MODEL_FIELDS {
        let field = field_mut(data, name);
        let mut snippets: Vec<String> = IMG_TAG.find_iter(field).map(|m| m.as_str().to_owned()).collect();
        snippets.extend(MD_IMAGE.find_iter(field).map(|m| m.as_str().to_owned()));
        if snippets.is_empty() {
            continue;
        }
        let stripped = MD_IMAGE.replace_all(&IMG_TAG.replace_all(field, ""), "").into_owned();
        let text = visible_text(&stripped);
        if text.is_empty() {
            field.clear();
        } else {
            *field = stripped;
            let left = truncate(&text, 30);
            leftovers.push(trf!("{name}（剩余文字：{left}）", "{name} (leftover text: {left})"));
        }
        moved.extend(snippets);
    }
    if !moved.is_empty() {
        let notes = &mut data.creator_notes;
        if !notes.trim().is_empty() {
            notes.push_str("\n\n");
        }
        notes.push_str(&moved.join("\n"));
    }
    (moved.len(), leftovers)
}

fn field_mut<'a>(d: &'a mut CardData, name: &str) -> &'a mut String {
    match name {
        "description" => &mut d.description,
        "personality" => &mut d.personality,
        "scenario" => &mut d.scenario,
        "mes_example" => &mut d.mes_example,
        "system_prompt" => &mut d.system_prompt,
        "post_history_instructions" => &mut d.post_history_instructions,
        _ => unreachable!("unknown field {name}"),
    }
}

/// 去掉 HTML 标签后的可见文字。
pub fn visible_text(s: &str) -> String {
    let t = HTML_TAG.replace_all(s, " ");
    html_unescape(&t).split_whitespace().collect::<Vec<_>>().join(" ")
}

fn html_unescape(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_owned() } else { s.chars().take(n).collect::<String>() + "…" }
}

// ---- 缓存 ----

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("card-forge").join("images")
}

/// FNV-1a，用作缓存文件名前缀（稳定、无需额外依赖）。
fn url_hash(url: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in url.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// 链接里的文件名（不含扩展名），用于让缓存文件名可读。
pub fn url_stem(url: &str) -> String {
    if url.starts_with("data:") {
        return "inline".into();
    }
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let name = path.rsplit('/').next().unwrap_or("");
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    let clean: String = stem.chars().filter(|c| c.is_ascii_alphanumeric() || "-_".contains(*c)).take(40).collect();
    if clean.is_empty() { "image".into() } else { clean }
}

/// 在缓存目录里查找某个链接已下载的文件。
pub fn find_cached(dir: &Path, url: &str) -> Option<PathBuf> {
    let prefix = format!("{}_", url_hash(url));
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .map(|e| e.path())
}

fn ext_for(bytes: &[u8]) -> &'static str {
    match image::guess_format(bytes) {
        Ok(image::ImageFormat::Png) => "png",
        Ok(image::ImageFormat::Jpeg) => "jpg",
        Ok(image::ImageFormat::WebP) => "webp",
        Ok(image::ImageFormat::Gif) => "gif",
        Ok(image::ImageFormat::Avif) => "avif",
        Ok(image::ImageFormat::Bmp) => "bmp",
        _ => "bin",
    }
}

fn fetch(url: &str) -> Result<Vec<u8>, String> {
    if let Some(rest) = url.strip_prefix("data:") {
        let (meta, payload) = rest.split_once(',').ok_or(tr!("data URI 格式错误", "Malformed data URI"))?;
        if !meta.ends_with(";base64") {
            return Err(tr!("只支持 base64 的 data URI", "Only base64 data URIs are supported").into());
        }
        return base64::engine::general_purpose::STANDARD.decode(payload.trim()).map_err(|e| e.to_string());
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) card-forge")
        .build()
        .into();
    let mut resp = agent.get(url).call().map_err(|e| e.to_string())?;
    resp.body_mut().with_config().limit(64 * 1024 * 1024).read_to_vec().map_err(|e| e.to_string())
}

/// 下载一个链接到缓存目录，已存在则直接返回。
pub fn download(dir: &Path, url: &str) -> Result<PathBuf, String> {
    if let Some(p) = find_cached(dir, url) {
        return Ok(p);
    }
    let bytes = fetch(url)?;
    let ext = ext_for(&bytes);
    if ext == "bin" {
        return Err(tr!("下载的内容不是可识别的图片", "Downloaded content is not a recognizable image").into());
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{}_{}.{ext}", url_hash(url), url_stem(url)));
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(path)
}

#[derive(Clone, Debug, PartialEq)]
pub enum DlState {
    Missing,
    Queued,
    Done(PathBuf),
    Failed(String),
}

/// 后台下载器：固定数量的工作线程从共享队列取任务。
pub struct Downloader {
    jobs: Sender<String>,
    results: Receiver<(String, Result<PathBuf, String>)>,
}

impl Downloader {
    pub fn new(dir: PathBuf, workers: usize, ctx: eframe::egui::Context) -> Self {
        let (jobs, job_rx) = channel::<String>();
        let (res_tx, results) = channel();
        let job_rx = Arc::new(Mutex::new(job_rx));
        for _ in 0..workers {
            let (job_rx, res_tx, dir, ctx) = (job_rx.clone(), res_tx.clone(), dir.clone(), ctx.clone());
            std::thread::spawn(move || {
                loop {
                    let Ok(url) = job_rx.lock().unwrap().recv() else { return };
                    let r = download(&dir, &url);
                    if res_tx.send((url, r)).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            });
        }
        Self { jobs, results }
    }

    pub fn enqueue(&self, url: &str) {
        let _ = self.jobs.send(url.to_owned());
    }

    pub fn poll(&self, states: &mut HashMap<String, DlState>) -> usize {
        let mut n = 0;
        while let Ok((url, r)) = self.results.try_recv() {
            states.insert(url, r.map_or_else(DlState::Failed, DlState::Done));
            n += 1;
        }
        n
    }
}

/// 按卡片中的顺序把已缓存的图片复制到目标目录，返回复制数量。
pub fn export_to(dir: &Path, refs: &[ImageRef], states: &HashMap<String, DlState>) -> Result<usize, String> {
    std::fs::create_dir_all(dir).map_err(|e| trf!("无法创建目录：{e}", "Could not create folder: {e}"))?;
    let mut n = 0;
    for (i, r) in refs.iter().enumerate() {
        let Some(DlState::Done(src)) = states.get(&r.url) else { continue };
        let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("img");
        let dst = dir.join(format!("{:02}_{}.{ext}", i + 1, url_stem(&r.url)));
        std::fs::copy(src, &dst).map_err(|e| trf!("复制 {} 失败：{e}", "Copying {} failed: {e}", dst.display()))?;
        n += 1;
    }
    Ok(n)
}

/// SillyTavern 的图库目录：`<酒馆>/data/default-user/user/images/<角色名>`。
pub fn tavern_gallery_dir(card_name: &str) -> Option<PathBuf> {
    let root = dirs::home_dir()?.join("SillyTavern/data/default-user/user/images");
    if !root.is_dir() || card_name.trim().is_empty() {
        return None;
    }
    let safe: String = card_name.chars().filter(|c| !"/\\:*?\"<>|".contains(*c)).collect();
    Some(root.join(safe.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_in_order_without_duplicates() {
        let text = r#"<p><img src="https://a.com/1.webp" width=10><img src='https://a.com/2.png?x=1&amp;y=2'></p>
            ![alt](https://b.com/3.jpg "t") plain https://c.com/4.gif and https://a.com/1.webp again
            <img src="data:image/png;base64,AAAA"> https://not-image.com/page"#;
        assert_eq!(
            extract_urls(text),
            [
                "https://a.com/1.webp",
                "https://a.com/2.png?x=1&y=2",
                "https://b.com/3.jpg",
                "https://c.com/4.gif",
                "data:image/png;base64,AAAA"
            ]
        );
    }

    #[test]
    fn scan_reports_fields() {
        let d = CardData {
            personality: r#"<img src="https://x.com/a.webp">"#.into(),
            first_mes: "![](https://x.com/a.webp)".into(),
            alternate_greetings: vec!["https://x.com/b.png".into()],
            ..Default::default()
        };
        let refs = scan_card(&d);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].fields, ["personality", "first_mes"]);
        assert_eq!(refs[1].fields, ["alternate_greetings[0]"]);
    }

    #[test]
    fn moves_gallery_out_of_personality() {
        let mut d = CardData {
            personality: r#"<p><img src="https://x.com/a.webp"></p><p><img src="https://x.com/b.webp"></p>"#.into(),
            description: "Tall. <img src=\"https://x.com/c.webp\">".into(),
            first_mes: "<img src=\"https://x.com/d.webp\">".into(),
            creator_notes: "notes".into(),
            ..Default::default()
        };
        let (n, left) = move_images_to_notes(&mut d);
        assert_eq!(n, 3);
        assert_eq!(d.personality, "");
        assert_eq!(d.description, "Tall. ");
        assert_eq!(left.len(), 1);
        assert!(d.first_mes.contains("d.webp"), "开场白不应被改动");
        assert!(d.creator_notes.starts_with("notes\n\n<img"));
        assert_eq!(scan_card(&d).len(), 4);
    }

    #[test]
    fn data_uri_download_and_cache() {
        let dir = tempfile::tempdir().unwrap();
        let png = crate::png::tiny_png();
        let url = format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(&png));
        let p = download(dir.path(), &url).unwrap();
        assert_eq!(p.extension().unwrap(), "png");
        assert_eq!(find_cached(dir.path(), &url), Some(p));
    }

    /// 用真实卡片走一遍完整流程（需要联网）：
    /// CARD_FORGE_SAMPLE=卡片.png cargo test -- --ignored
    #[test]
    #[ignore]
    fn real_card_end_to_end() {
        let path = std::env::var("CARD_FORGE_SAMPLE").expect("设置 CARD_FORGE_SAMPLE");
        let loaded = crate::card::load_file(Path::new(&path)).unwrap();
        let mut data = loaded.card.unwrap().data;
        let refs = scan_card(&data);
        assert!(!refs.is_empty());

        let dir = tempfile::tempdir().unwrap();
        for r in refs.iter().take(2) {
            let p = download(dir.path(), &r.url).unwrap();
            image::open(&p).expect("下载的是可解码的图片");
        }

        let (moved, _) = move_images_to_notes(&mut data);
        assert!(moved >= refs.len(), "同一张图可能出现多次");
        let png = crate::card::build_png(loaded.image.as_ref().unwrap(), &data, true).unwrap();
        let back = crate::card::load_png(png).unwrap();
        assert_eq!(back.card.unwrap().data.name, data.name);
        assert!(back.chunks.iter().all(|c| c.essential || c.keyword.is_some()), "deBG 等附加块已移除");
        assert_eq!(scan_card(&data).len(), refs.len(), "图片链接都还在（移到了作者备注）");
    }

    #[test]
    fn stem() {
        assert_eq!(url_stem("https://e.com/media/qvC-f_A.webp?w=1"), "qvC-f_A");
        assert_eq!(url_stem("https://e.com/"), "image");
    }
}
