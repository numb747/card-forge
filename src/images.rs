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

// ---- 图片库：每张卡一个文件夹 ----
//
// 文件名是 `<链接里的文件名>_<链接哈希>.<扩展名>`：既能看出来源，又能按链接精确查找。
// 0.1.0 的扁平缓存（`~/.cache/card-forge/images/<哈希>_<文件名>`）仍会被查找和复用。

/// 0.1.0 使用的扁平缓存目录。
fn legacy_cache_dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("card-forge").join("images")
}

/// 卡片在图片库中的文件夹，用和酒馆相同的规则清理名字。
pub fn card_dir(root: &Path, card_name: &str) -> PathBuf {
    let name: String = crate::tavern::sanitize_filename(card_name.trim()).chars().take(100).collect();
    let name = name.trim_end_matches(['.', ' ']);
    root.join(if name.is_empty() { "untitled" } else { name })
}

/// 角色改名后把卡片文件夹一起改名。新名字的文件夹已存在时不合并，返回 Ok(false)。
pub fn move_card_dir(old: &Path, new: &Path) -> Result<bool, String> {
    if !old.is_dir() || new.exists() {
        return Ok(false);
    }
    std::fs::rename(old, new).map(|_| true).map_err(|e| e.to_string())
}

/// FNV-1a，用于文件名中的链接哈希（稳定、无需额外依赖）。
fn url_hash(url: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in url.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// 链接里的文件名（不含扩展名），用于让文件名可读。
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

/// 从文件名里取出链接哈希；未下载完的 `.part` 文件不算。
/// 图片库里是 `<文件名>_<哈希>.<扩展名>`，0.1.0 的旧缓存是 `<哈希>_<文件名>.<扩展名>`，两种分开解析以免混淆。
fn hash_of(file_name: &str, legacy: bool) -> Option<&str> {
    let (stem, ext) = file_name.rsplit_once('.')?;
    if ext == "part" {
        return None;
    }
    let h = if legacy { stem.split_once('_')?.0 } else { stem.rsplit_once('_')?.1 };
    (h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit())).then_some(h)
}

/// 某个文件夹里已下载的图片：链接哈希 → 文件。
fn index_dir(dir: &Path, legacy: bool) -> HashMap<String, PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return HashMap::new() };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            Some((hash_of(&name, legacy)?.to_owned(), e.path()))
        })
        .collect()
}

/// 查出这张卡的每个图片链接在卡片文件夹里的状态。
pub fn lookup(dir: &Path, refs: &[ImageRef]) -> HashMap<String, DlState> {
    let index = index_dir(dir, false);
    refs.iter()
        .map(|r| {
            let state = index.get(&url_hash(&r.url)).map_or(DlState::Missing, |p| DlState::Done(p.clone()));
            (r.url.clone(), state)
        })
        .collect()
}

/// 整个图片库（所有卡片文件夹 + 旧版缓存）的索引，用来复用其他卡下载过的同一张图。
pub struct Library {
    root: PathBuf,
    index: HashMap<String, PathBuf>,
}

impl Library {
    pub fn scan(root: &Path) -> Self {
        let mut index = index_dir(&legacy_cache_dir(), true);
        for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
            if entry.path().is_dir() {
                index.extend(index_dir(&entry.path(), false));
            }
        }
        Self { root: root.to_owned(), index }
    }

    fn find(&self, hash: &str) -> Option<PathBuf> {
        self.index.get(hash).filter(|p| p.is_file()).cloned()
    }
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

fn read_image(path: &Path) -> Option<Vec<u8>> {
    std::fs::read(path).ok().filter(|b| ext_for(b) != "bin")
}

/// 把一个链接的图片放进卡片文件夹：已有则直接返回；图片库里有可用的副本就复制；否则下载。
pub fn download(library: &Mutex<Library>, dir: &Path, url: &str) -> Result<PathBuf, String> {
    let hash = url_hash(url);
    if let Some(p) = index_dir(dir, false).remove(&hash) {
        return Ok(p);
    }
    // 只在查索引时持锁，复制和下载期间不阻塞其他线程
    let copy = library.lock().unwrap().find(&hash);
    // 别处的副本损坏时退回到网络下载
    let bytes = match copy.and_then(|p| read_image(&p)) {
        Some(b) => b,
        None => fetch(url)?,
    };
    let ext = ext_for(&bytes);
    if ext == "bin" {
        return Err(tr!("下载的内容不是可识别的图片", "Downloaded content is not a recognizable image").into());
    }
    std::fs::create_dir_all(dir).map_err(|e| trf!("无法创建目录：{e}", "Could not create folder: {e}"))?;
    let path = dir.join(format!("{}_{hash}.{ext}", url_stem(url)));
    let tmp = dir.join(format!("{}_{hash}.{ext}.part", url_stem(url)));
    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    library.lock().unwrap().index.insert(hash, path.clone());
    Ok(path)
}

#[derive(Clone, Debug, PartialEq)]
pub enum DlState {
    Missing,
    Queued,
    Done(PathBuf),
    Failed(String),
}

struct Job {
    root: PathBuf,
    dir: PathBuf,
    url: String,
}

struct JobResult {
    dir: PathBuf,
    url: String,
    result: Result<PathBuf, String>,
}

/// 后台下载器：固定数量的工作线程从共享队列取任务，共用一份图片库索引。
pub struct Downloader {
    jobs: Sender<Job>,
    results: Receiver<JobResult>,
}

impl Downloader {
    pub fn new(workers: usize, ctx: eframe::egui::Context) -> Self {
        let (jobs, job_rx) = channel::<Job>();
        let (res_tx, results) = channel();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let library: Arc<Mutex<Option<Arc<Mutex<Library>>>>> = Arc::default();
        for _ in 0..workers {
            let (job_rx, res_tx, ctx, library) = (job_rx.clone(), res_tx.clone(), ctx.clone(), library.clone());
            std::thread::spawn(move || {
                loop {
                    // 取到任务后立刻释放锁，其他线程才能并行下载
                    let job = job_rx.lock().unwrap().recv();
                    let Ok(job) = job else { return };
                    let lib = {
                        // 第一次使用或图片库位置改变时才扫描整个图片库
                        let mut slot = library.lock().unwrap();
                        let stale = slot.as_ref().is_none_or(|l| l.lock().unwrap().root != job.root);
                        if stale {
                            *slot = Some(Arc::new(Mutex::new(Library::scan(&job.root))));
                        }
                        slot.clone().unwrap()
                    };
                    let result = download(&lib, &job.dir, &job.url);
                    if res_tx.send(JobResult { dir: job.dir, url: job.url, result }).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            });
        }
        Self { jobs, results }
    }

    pub fn enqueue(&self, root: &Path, dir: &Path, url: &str) {
        let _ = self.jobs.send(Job { root: root.to_owned(), dir: dir.to_owned(), url: url.to_owned() });
    }

    /// 收取完成的任务，只更新属于当前卡片文件夹的状态，返回其中的数量。
    /// 其他文件夹的结果直接丢弃：文件已经落盘，切回那张卡时会被重新找到。
    pub fn poll(&self, current_dir: &Path, states: &mut HashMap<String, DlState>) -> usize {
        let mut n = 0;
        while let Ok(r) = self.results.try_recv() {
            if r.dir == current_dir {
                states.insert(r.url, r.result.map_or_else(DlState::Failed, DlState::Done));
                n += 1;
            }
        }
        n
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct ExportReport {
    pub copied: usize,
    /// 目标文件夹里已有内容相同的文件。
    pub existing: usize,
    /// 目标不支持的格式。
    pub unsupported: usize,
}

/// 目标文件夹里已有文件的内容指纹。只对大小和待导出文件相同的文件计算哈希，
/// 避免每次导出都把整个 Gallery（可能有视频）读进内存。
struct Existing {
    by_size: HashMap<u64, Vec<PathBuf>>,
    hashed: std::collections::HashSet<(u64, u32)>,
}

impl Existing {
    fn scan(dir: &Path) -> Result<Self, String> {
        let mut by_size: HashMap<u64, Vec<PathBuf>> = HashMap::new();
        for e in std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
            if let Ok(m) = e.metadata()
                && m.is_file()
            {
                by_size.entry(m.len()).or_default().push(e.path());
            }
        }
        Ok(Self { by_size, hashed: Default::default() })
    }

    /// 已有相同内容时返回 true，否则把它登记为已有。
    fn contains_or_insert(&mut self, bytes: &[u8]) -> bool {
        let key = (bytes.len() as u64, crc32fast::hash(bytes));
        for p in self.by_size.remove(&key.0).unwrap_or_default() {
            if let Ok(b) = std::fs::read(&p) {
                self.hashed.insert((key.0, crc32fast::hash(&b)));
            }
        }
        !self.hashed.insert(key)
    }
}

/// 按卡片中的顺序把已下载的图片复制到目标文件夹。
/// 已存在相同内容的文件会跳过（比如之前在酒馆里手动上传过），不会覆盖任何已有文件。
pub fn export_to(
    dir: &Path,
    refs: &[ImageRef],
    states: &HashMap<String, DlState>,
    allowed_exts: Option<&[&str]>,
) -> Result<ExportReport, String> {
    std::fs::create_dir_all(dir).map_err(|e| trf!("无法创建目录：{e}", "Could not create folder: {e}"))?;
    let mut existing = Existing::scan(dir)?;
    let mut report = ExportReport::default();
    for (i, r) in refs.iter().enumerate() {
        let Some(DlState::Done(src)) = states.get(&r.url) else { continue };
        let ext = src.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
        if allowed_exts.is_some_and(|a| !a.contains(&ext.as_str())) {
            report.unsupported += 1;
            continue;
        }
        let bytes = std::fs::read(src).map_err(|e| e.to_string())?;
        if existing.contains_or_insert(&bytes) {
            report.existing += 1;
            continue;
        }
        let base = format!("{:02}_{}", i + 1, url_stem(&r.url));
        let mut dst = dir.join(format!("{base}.{ext}"));
        let mut n = 2;
        while dst.exists() {
            dst = dir.join(format!("{base}-{n}.{ext}"));
            n += 1;
        }
        std::fs::write(&dst, &bytes).map_err(|e| trf!("复制 {} 失败：{e}", "Copying {} failed: {e}", dst.display()))?;
        report.copied += 1;
    }
    Ok(report)
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

    fn png_data_uri(color: u8) -> String {
        let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([color, 0, 0, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(out.into_inner()))
    }

    #[test]
    fn per_card_folders_reuse_other_cards() {
        let root = tempfile::tempdir().unwrap();
        let url = png_data_uri(1);
        let a = card_dir(root.path(), "Card A");
        let b = card_dir(root.path(), "Card | B");
        assert_eq!(b, root.path().join("Card  B"));
        assert_eq!(card_dir(root.path(), " .. "), root.path().join("untitled"));

        let lib = Mutex::new(Library::scan(root.path()));
        let pa = download(&lib, &a, &url).unwrap();
        assert_eq!(pa.parent().unwrap(), a);
        assert!(pa.file_name().unwrap().to_string_lossy().starts_with("inline_"));
        assert_eq!(download(&lib, &a, &url).unwrap(), pa, "已存在时直接返回");

        // 另一张卡引用同一链接：从图片库复制，而不是重新下载
        let pb = download(&lib, &b, &url).unwrap();
        assert_eq!(pb.parent().unwrap(), b);
        assert_eq!(std::fs::read(&pa).unwrap(), std::fs::read(&pb).unwrap());

        let refs =
            vec![ImageRef { url: url.clone(), fields: vec![] }, ImageRef { url: png_data_uri(2), fields: vec![] }];
        let states = lookup(&b, &refs);
        assert_eq!(states[&url], DlState::Done(pb));
        assert_eq!(states[&refs[1].url], DlState::Missing);
    }

    #[test]
    fn card_dir_follows_rename_without_merging() {
        let root = tempfile::tempdir().unwrap();
        let (a, b, c) = (root.path().join("A"), root.path().join("B"), root.path().join("C"));
        assert_eq!(move_card_dir(&a, &b), Ok(false), "没有旧文件夹");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("x_0123456789abcdef.png"), b"x").unwrap();
        assert_eq!(move_card_dir(&a, &b), Ok(true));
        assert!(b.join("x_0123456789abcdef.png").exists() && !a.exists());
        std::fs::create_dir_all(&c).unwrap();
        assert_eq!(move_card_dir(&b, &c), Ok(false), "目标已存在时不合并");
        assert!(b.exists());
    }

    #[test]
    fn file_name_hashes() {
        assert_eq!(hash_of("x_0123456789abcdef.webp", false), Some("0123456789abcdef"));
        assert_eq!(hash_of("x_0123456789abcdef.webp.part", false), None);
        assert_eq!(hash_of("01_qvCfAyt6zx.webp", false), None);
        // 旧缓存的文件名里，链接本身的文件名也可能以 _<16 位十六进制> 结尾
        let legacy = "a1b2c3d4e5f60718_photo_0123456789abcdef.png";
        assert_eq!(hash_of(legacy, true), Some("a1b2c3d4e5f60718"));
        assert_eq!(hash_of("a1b2c3d4e5f60718_photo.png.part", true), None);
    }

    #[test]
    fn corrupt_copy_elsewhere_falls_back_to_fetch() {
        let root = tempfile::tempdir().unwrap();
        let url = png_data_uri(7);
        let other = root.path().join("other card");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join(format!("inline_{}.png", url_hash(&url))), b"truncated").unwrap();

        let lib = Mutex::new(Library::scan(root.path()));
        let p = download(&lib, &root.path().join("card"), &url).unwrap();
        image::open(p).expect("从链接重新获取了完整图片");
    }

    #[test]
    fn export_skips_duplicates_and_unsupported() {
        let root = tempfile::tempdir().unwrap();
        let card = root.path().join("card");
        let refs: Vec<ImageRef> =
            [1, 2, 3].iter().map(|&c| ImageRef { url: png_data_uri(c), fields: vec![] }).collect();
        let lib = Mutex::new(Library::scan(root.path()));
        for r in &refs[..2] {
            download(&lib, &card, &r.url).unwrap();
        }
        let mut states = lookup(&card, &refs);
        // 伪造一个酒馆不支持的格式
        let avif = card.join("pic_ffffffffffffffff.avif");
        std::fs::write(&avif, b"fake").unwrap();
        states.insert(refs[2].url.clone(), DlState::Done(avif));

        let gallery = root.path().join("gallery");
        std::fs::create_dir_all(&gallery).unwrap();
        // 之前已经手动上传过第一张（文件名不同、内容相同）
        std::fs::copy(done(&states[&refs[0].url]), gallery.join("1791351280640.png")).unwrap();
        std::fs::write(gallery.join("02_inline.png"), b"unrelated file with the same name").unwrap();

        let report = export_to(&gallery, &refs, &states, Some(&crate::tavern::GALLERY_EXTS)).unwrap();
        assert_eq!(report, ExportReport { copied: 1, existing: 1, unsupported: 1 });
        assert!(gallery.join("02_inline-2.png").exists(), "同名文件不被覆盖");
        let again = export_to(&gallery, &refs, &states, Some(&crate::tavern::GALLERY_EXTS)).unwrap();
        assert_eq!(again, ExportReport { copied: 0, existing: 2, unsupported: 1 });
    }

    fn done(state: &DlState) -> PathBuf {
        match state {
            DlState::Done(p) => p.clone(),
            other => panic!("not downloaded: {other:?}"),
        }
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
        let card = card_dir(dir.path(), &data.name);
        let lib = Mutex::new(Library::scan(dir.path()));
        for r in refs.iter().take(2) {
            let p = download(&lib, &card, &r.url).unwrap();
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

    /// 导出到已手动上传过同样图片的 Gallery 时应全部跳过（需要联网，原 Gallery 不会被修改）：
    /// CARD_FORGE_SAMPLE=卡片.png CARD_FORGE_GALLERY=酒馆图库目录 cargo test -- --ignored
    #[test]
    #[ignore]
    fn real_gallery_dedupe() {
        let card = std::env::var("CARD_FORGE_SAMPLE").expect("设置 CARD_FORGE_SAMPLE");
        let gallery = PathBuf::from(std::env::var("CARD_FORGE_GALLERY").expect("设置 CARD_FORGE_GALLERY"));
        let data = crate::card::load_file(Path::new(&card)).unwrap().card.unwrap().data;
        let tmp = tempfile::tempdir().unwrap();
        let copy = tmp.path().join("gallery");
        std::fs::create_dir_all(&copy).unwrap();
        for e in std::fs::read_dir(&gallery).unwrap().flatten() {
            std::fs::copy(e.path(), copy.join(e.file_name())).unwrap();
        }
        let before = std::fs::read_dir(&copy).unwrap().count();

        let refs = scan_card(&data);
        let dir = card_dir(&tmp.path().join("library"), &data.name);
        let lib = Mutex::new(Library::scan(&tmp.path().join("library")));
        for r in &refs {
            download(&lib, &dir, &r.url).unwrap();
        }
        let report = export_to(&copy, &refs, &lookup(&dir, &refs), Some(&crate::tavern::GALLERY_EXTS)).unwrap();
        println!("{report:?}");
        assert_eq!(report.existing + report.copied, refs.len());
        assert_eq!(std::fs::read_dir(&copy).unwrap().count(), before + report.copied);
    }

    #[test]
    fn stem() {
        assert_eq!(url_stem("https://e.com/media/qvC-f_A.webp?w=1"), "qvC-f_A");
        assert_eq!(url_stem("https://e.com/"), "image");
    }
}
