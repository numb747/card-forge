use std::collections::HashMap;
use std::path::{Path, PathBuf};

use eframe::egui::{self, Color32, Key, KeyboardShortcut, Modifiers, RichText, Vec2};

use crate::card::{self, BookEntry, CardData, CharacterBook, ChunkInfo};
use crate::i18n::{self, Lang, tr, trf};
use crate::images::{self, DlState, Downloader, ImageRef};
use crate::settings::Settings;
use crate::tavern::{self, FolderSource, GalleryTarget, Tavern};

const SAVE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
const OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const IMAGE_EXTS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "gif"];

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Basic,
    Dialogue,
    Book,
    Images,
    Raw,
    Info,
}

/// 当前卡片的来源，切换语言时标签跟着变。
enum Origin {
    New,
    FromImage,
    Loaded(String),
}

/// 需要先确认是否放弃未保存修改的操作。
enum Pending {
    Open(PathBuf),
    New,
    Quit,
}

pub struct App {
    data: CardData,
    path: Option<PathBuf>,
    origin: Origin,
    avatar: Option<Vec<u8>>,
    avatar_uri: String,
    avatar_dims: Option<(u32, u32)>,
    chunks: Vec<ChunkInfo>,
    trailing_len: usize,
    warnings: Vec<String>,
    dirty: bool,
    tab: Tab,
    status: String,

    settings: Settings,
    show_settings: bool,
    tavern: Option<Tavern>,
    /// (角色名, 用户目录) → Gallery 目标，名字或用户变化时重新计算。
    gallery: Option<(String, PathBuf, Option<GalleryTarget>)>,
    /// 图片库根目录（来自设置，缓存起来避免每帧读取 user-dirs）。
    image_root: PathBuf,
    /// 当前卡片的图片文件夹。在载入时按角色名确定；改名后要等保存才跟着改，
    /// 避免边输入边切换文件夹、正在进行的下载落到别处。
    image_dir: PathBuf,
    refs: Vec<ImageRef>,
    states: HashMap<String, DlState>,
    downloader: Downloader,
    thumb: f32,
    preview: Option<PathBuf>,

    raw: String,
    raw_err: Option<String>,
    entry_sel: usize,
    book_delete_armed: bool,
    strip_chunks: bool,
    new_tag: String,
    pending: Option<Pending>,
    allow_close: bool,
    avatar_gen: u64,
    title: String,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        settings: Settings,
        warning: Option<String>,
        initial: Option<PathBuf>,
    ) -> Self {
        let tavern = tavern::detect(settings.tavern_dir.as_deref());
        let image_root = settings.image_root();
        let mut app = Self {
            data: CardData::default(),
            path: None,
            origin: Origin::New,
            avatar: None,
            avatar_uri: String::new(),
            avatar_dims: None,
            chunks: Vec::new(),
            trailing_len: 0,
            warnings: Vec::new(),
            dirty: false,
            tab: Tab::Basic,
            status: tr!(
                "打开一张角色卡，或者直接把 PNG / JSON 拖进窗口",
                "Open a character card, or drop a PNG / JSON onto the window"
            )
            .into(),
            downloader: Downloader::new(4, cc.egui_ctx.clone()),
            settings,
            show_settings: false,
            tavern,
            gallery: None,
            image_root,
            image_dir: PathBuf::new(),
            refs: Vec::new(),
            states: HashMap::new(),
            thumb: 180.0,
            preview: None,
            raw: String::new(),
            raw_err: None,
            entry_sel: 0,
            book_delete_armed: false,
            strip_chunks: false,
            new_tag: String::new(),
            pending: None,
            allow_close: false,
            avatar_gen: 0,
            title: String::new(),
        };
        if let Some(p) = initial {
            app.open_path(&p);
        }
        if let Some(w) = warning {
            app.status = w;
        }
        app
    }

    fn origin_label(&self) -> String {
        match &self.origin {
            Origin::New => tr!("新建", "new").into(),
            Origin::FromImage => tr!("新建（来自图片）", "new (from image)").into(),
            Origin::Loaded(s) => s.clone(),
        }
    }

    // ---- 文件操作 ----

    fn request(&mut self, action: Pending) {
        if self.dirty {
            self.pending = Some(action);
        } else {
            self.perform(action);
        }
    }

    fn perform(&mut self, action: Pending) {
        match action {
            Pending::Open(p) => self.open_path(&p),
            Pending::New => {
                self.reset(CardData::default(), None, Origin::New, None);
                self.status = tr!("已新建空白角色卡", "Created a blank card").into();
            }
            Pending::Quit => self.allow_close = true,
        }
    }

    fn open_path(&mut self, path: &Path) {
        let loaded = match card::load_file(path) {
            Ok(l) => l,
            Err(e) => {
                self.status = trf!("打开失败：{e}", "Failed to open: {e}");
                return;
            }
        };
        let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match loaded.card {
            Some(c) => {
                let origin = Origin::Loaded(format!("{} · {}", c.spec, c.source));
                self.reset(c.data, Some(path.to_owned()), origin, loaded.image);
                self.status = trf!("已打开 {file_name}", "Opened {file_name}");
            }
            None => {
                // 普通图片：新建一张以它为头像的卡
                let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let data = CardData { name, ..Default::default() };
                self.reset(data, None, Origin::FromImage, loaded.image);
                self.status = trf!(
                    "{file_name} 里没有角色卡数据，已用它作为头像新建卡片",
                    "{file_name} has no card data; created a new card using it as the avatar"
                );
            }
        }
        self.chunks = loaded.chunks;
        self.trailing_len = loaded.trailing_len;
        self.warnings = loaded.warnings;
    }

    fn reset(&mut self, data: CardData, path: Option<PathBuf>, origin: Origin, avatar: Option<Vec<u8>>) {
        self.data = data;
        self.path = path;
        self.origin = origin;
        self.chunks.clear();
        self.trailing_len = 0;
        self.warnings.clear();
        self.entry_sel = 0;
        self.book_delete_armed = false;
        self.raw_err = None;
        match avatar {
            Some(bytes) => {
                self.set_avatar(bytes);
            }
            None => {
                self.avatar = None;
                self.avatar_dims = None;
            }
        }
        self.dirty = false;
        self.image_dir = images::card_dir(&self.image_root, &self.data.name);
        self.states.clear();
        self.gallery = None;
        self.rescan();
        self.refresh_raw();
    }

    fn set_avatar(&mut self, bytes: Vec<u8>) -> bool {
        let dims = image::ImageReader::new(std::io::Cursor::new(&bytes))
            .with_guessed_format()
            .ok()
            .and_then(|r| r.into_dimensions().ok());
        if dims.is_none() {
            self.status = tr!("无法识别这张图片", "Unrecognized image format").into();
            return false;
        }
        self.avatar_gen += 1;
        self.avatar_uri = format!("bytes://avatar-{}", self.avatar_gen);
        self.avatar_dims = dims;
        self.avatar = Some(bytes);
        self.dirty = true;
        true
    }

    fn set_avatar_from_file(&mut self, path: &Path) {
        match std::fs::read(path) {
            Ok(bytes) => {
                if self.set_avatar(bytes) {
                    self.status = trf!("头像已替换为 {}", "Avatar replaced with {}", path.display());
                }
            }
            Err(e) => self.status = trf!("读取图片失败：{e}", "Failed to read image: {e}"),
        }
    }

    /// 保存到当前路径；没有路径或当前文件不是 PNG 时弹出另存为。
    fn save(&mut self) {
        match self.path.clone().filter(|p| has_ext(p, &["png"])) {
            Some(p) => self.save_png(&p),
            None => self.save_as(),
        }
    }

    fn save_as(&mut self) {
        let dialog = self
            .dialog_dir(rfd::FileDialog::new())
            .set_title(tr!("保存角色卡 PNG", "Save card as PNG"))
            .add_filter(tr!("PNG 角色卡", "PNG character card"), &["png"])
            .set_file_name(format!("{}.png", safe_name(&self.data.name)));
        if let Some(p) = dialog.save_file() {
            let p = if has_ext(&p, &["png"]) { p } else { p.with_extension("png") };
            self.save_png(&p);
        }
    }

    fn save_png(&mut self, path: &Path) {
        let avatar = self.avatar.clone().unwrap_or_else(card::placeholder_png);
        let result = card::build_png(&avatar, &self.data, self.strip_chunks)
            .and_then(|bytes| std::fs::write(path, &bytes).map(|_| bytes).map_err(|e| e.to_string()));
        match result {
            Ok(bytes) => {
                if let Ok(l) = card::load_png(bytes) {
                    self.chunks = l.chunks;
                    self.trailing_len = l.trailing_len;
                    self.warnings = l.warnings;
                }
                self.path = Some(path.to_owned());
                self.dirty = false;
                self.status = trf!("已保存 {}", "Saved {}", path.display());
                self.follow_rename();
            }
            Err(e) => self.status = trf!("保存失败：{e}", "Save failed: {e}"),
        }
    }

    fn export_json(&mut self, v3: bool) {
        let suffix = if v3 { "v3" } else { "v2" };
        let dialog = self
            .dialog_dir(rfd::FileDialog::new())
            .set_title(tr!("导出 JSON", "Export JSON"))
            .add_filter("JSON", &["json"])
            .set_file_name(format!("{}.{suffix}.json", safe_name(&self.data.name)));
        let Some(p) = dialog.save_file() else { return };
        let value = if v3 { self.data.to_v3() } else { self.data.to_v2() };
        let text = serde_json::to_string_pretty(&value).unwrap();
        self.status = match std::fs::write(&p, text) {
            Ok(()) => trf!("已导出 {}", "Exported {}", p.display()),
            Err(e) => trf!("导出失败：{e}", "Export failed: {e}"),
        };
    }

    fn dialog_dir(&self, d: rfd::FileDialog) -> rfd::FileDialog {
        match self.path.as_ref().and_then(|p| p.parent()) {
            Some(dir) => d.set_directory(dir),
            None => d,
        }
    }

    fn pick_card(&mut self) {
        let dialog = self
            .dialog_dir(rfd::FileDialog::new())
            .set_title(tr!("打开角色卡", "Open character card"))
            .add_filter(tr!("角色卡", "Character card"), &["png", "json"])
            .add_filter(tr!("所有文件", "All files"), &["*"]);
        if let Some(p) = dialog.pick_file() {
            self.request(Pending::Open(p));
        }
    }

    fn pick_avatar(&mut self) {
        let dialog = self
            .dialog_dir(rfd::FileDialog::new())
            .set_title(tr!("选择头像图片", "Choose avatar image"))
            .add_filter(tr!("图片", "Images"), &IMAGE_EXTS);
        if let Some(p) = dialog.pick_file() {
            self.set_avatar_from_file(&p);
        }
    }

    // ---- 图片 ----

    /// 重新扫描图片链接，并到卡片文件夹里查找已下载的文件（下载中的保持不变）。
    fn rescan(&mut self) {
        self.refs = images::scan_card(&self.data);
        for (url, state) in images::lookup(&self.image_dir, &self.refs) {
            if self.states.get(&url) != Some(&DlState::Queued) {
                self.states.insert(url, state);
            }
        }
    }

    /// 保存后角色名变了：图片文件夹跟着改名。新名字的文件夹已存在、或还有下载在进行时只切换不搬移，
    /// 缺的图片之后会从图片库里复制过来。
    fn follow_rename(&mut self) {
        let new_dir = images::card_dir(&self.image_root, &self.data.name);
        if new_dir == self.image_dir {
            return;
        }
        let busy = self.states.values().any(|s| *s == DlState::Queued);
        if !busy {
            match images::move_card_dir(&self.image_dir, &new_dir) {
                Ok(true) => {
                    self.status += &trf!("；图片文件夹已改名为 {}", "; image folder renamed to {}", new_dir.display());
                }
                Ok(false) => {}
                Err(e) => {
                    self.status += &trf!("；图片文件夹改名失败：{e}", "; could not rename the image folder: {e}");
                }
            }
        }
        self.image_dir = new_dir;
        self.states.clear();
        self.rescan();
    }

    fn state(&self, url: &str) -> &DlState {
        self.states.get(url).unwrap_or(&DlState::Missing)
    }

    fn queue(&mut self, url: &str) {
        self.states.insert(url.to_owned(), DlState::Queued);
        self.downloader.enqueue(&self.image_root, &self.image_dir, url);
    }

    fn download_all(&mut self) {
        let todo: Vec<String> = self
            .refs
            .iter()
            .filter(|r| matches!(self.state(&r.url), DlState::Missing | DlState::Failed(_)))
            .map(|r| r.url.clone())
            .collect();
        self.status = trf!("开始下载 {} 张图片", "Downloading {} images", todo.len());
        for url in todo {
            self.queue(&url);
        }
    }

    fn image_counts(&self) -> (usize, usize, usize) {
        let mut c = (0, 0, 0);
        for r in &self.refs {
            match self.state(&r.url) {
                DlState::Done(_) => c.0 += 1,
                DlState::Queued => c.1 += 1,
                DlState::Failed(_) => c.2 += 1,
                DlState::Missing => {}
            }
        }
        c
    }

    fn export_images(&mut self, dir: &Path, allowed_exts: Option<&[&str]>) {
        self.status = match images::export_to(dir, &self.refs, &self.states, allowed_exts) {
            Ok(r) => {
                let mut msg = trf!("已复制 {} 张图片到 {}", "Copied {} images to {}", r.copied, dir.display());
                if r.existing > 0 {
                    msg += &trf!("；{} 张已存在，跳过", "; skipped {} already there", r.existing);
                }
                if r.unsupported > 0 {
                    msg += &trf!("；{} 张格式不受支持", "; {} in unsupported formats", r.unsupported);
                }
                msg
            }
            Err(e) => e,
        };
    }

    /// 当前卡片在酒馆里的 Gallery 目录（带缓存）。
    fn gallery_target(&mut self) -> Option<GalleryTarget> {
        let tavern = self.tavern.as_ref()?;
        let user = self.tavern_user()?;
        let user_dir = tavern.user_dir(&user);
        let fresh = matches!(&self.gallery, Some((name, dir, _)) if *name == self.data.name && *dir == user_dir);
        if !fresh {
            let target = tavern.gallery_target(&user, &self.data.name);
            self.gallery = Some((self.data.name.clone(), user_dir, target));
        }
        self.gallery.as_ref().and_then(|(_, _, t)| t.clone())
    }

    /// 设置里选的酒馆用户；不存在时用第一个。
    fn tavern_user(&self) -> Option<String> {
        let users = &self.tavern.as_ref()?.users;
        self.settings.tavern_user.clone().filter(|u| users.contains(u)).or_else(|| users.first().cloned())
    }

    fn save_settings(&mut self) {
        if let Err(e) = self.settings.save() {
            self.status = trf!("保存设置失败：{e}", "Failed to save settings: {e}");
        }
    }

    fn refresh_raw(&mut self) {
        self.raw = serde_json::to_string_pretty(&self.data.to_v3()).unwrap();
        self.raw_err = None;
    }

    fn images_in(&self, field: &str) -> usize {
        self.refs.iter().filter(|r| r.fields.iter().any(|f| f == field)).count()
    }

    // ---- 界面 ----

    fn handle_input(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|i| i.consume_shortcut(&SAVE)) {
            self.save();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&OPEN)) {
            self.pick_card();
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().map(|f| f.path().to_owned()).filter(|p| !p.as_os_str().is_empty()).collect()
        });
        if let Some(p) = dropped.into_iter().next() {
            self.handle_drop(p);
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.dirty && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending = Some(Pending::Quit);
        }
    }

    /// JSON 和带卡片数据的 PNG 当作打开，其他图片当作更换头像。
    fn handle_drop(&mut self, p: PathBuf) {
        if has_ext(&p, &["json"]) {
            return self.request(Pending::Open(p));
        }
        let is_card = has_ext(&p, &["png"]) && card::load_file(&p).is_ok_and(|l| l.card.is_some());
        if is_card {
            self.request(Pending::Open(p));
        } else if has_ext(&p, &IMAGE_EXTS) {
            self.set_avatar_from_file(&p);
        } else {
            self.status = tr!("不支持的文件类型", "Unsupported file type").into();
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui.button(tr!("🗋 新建", "🗋 New")).clicked() {
                self.request(Pending::New);
            }
            if ui.button(tr!("📂 打开…", "📂 Open…")).on_hover_text("Ctrl+O").clicked() {
                self.pick_card();
            }
            let save_tip = tr!("Ctrl+S，保存为 PNG 角色卡", "Ctrl+S, save as a PNG character card");
            if ui.button(tr!("💾 保存", "💾 Save")).on_hover_text(save_tip).clicked() {
                self.save();
            }
            if ui.button(tr!("另存为…", "Save as…")).clicked() {
                self.save_as();
            }
            ui.menu_button(tr!("导出 JSON", "Export JSON"), |ui| {
                if ui.button(tr!("V2 JSON（兼容性最好）", "V2 JSON (most compatible)")).clicked() {
                    self.export_json(false);
                }
                if ui.button("V3 JSON").clicked() {
                    self.export_json(true);
                }
            });
            if ui.button(tr!("⚙ 设置", "⚙ Settings")).clicked() {
                self.show_settings = !self.show_settings;
            }
            let mut lang = i18n::current();
            egui::ComboBox::from_id_salt("lang").selected_text(lang.label()).width(80.0).show_ui(ui, |ui| {
                for l in Lang::ALL {
                    ui.selectable_value(&mut lang, l, l.label());
                }
            });
            if lang != i18n::current() {
                i18n::set(lang);
                self.settings.lang = Some(lang.code().to_owned());
                self.save_settings();
            }
            ui.separator();
            let path =
                self.path.as_ref().map_or(tr!("（未保存）", "(unsaved)").into(), |p| p.display().to_string());
            ui.add(egui::Label::new(RichText::new(path).weak()).truncate());
        });
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (done, queued, failed) = self.image_counts();
            ui.label(
                RichText::new(trf!(
                    "图片 {}：已下载 {done} / 下载中 {queued} / 失败 {failed}",
                    "Images {}: downloaded {done} / downloading {queued} / failed {failed}",
                    self.refs.len()
                ))
                .weak(),
            );
            if queued > 0 {
                ui.spinner();
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(&self.status).truncate());
            });
        });
    }

    fn avatar_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        let w = ui.available_width();
        match &self.avatar {
            Some(bytes) => {
                ui.add(egui::Image::from_bytes(self.avatar_uri.clone(), bytes.clone()).max_width(w).corner_radius(6));
            }
            None => {
                let (rect, _) = ui.allocate_exact_size(Vec2::new(w, w * 1.3), egui::Sense::hover());
                ui.painter().rect_filled(rect, 6.0, ui.visuals().faint_bg_color);
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    tr!("无头像\n保存时使用占位图", "No avatar\nA placeholder is used on save"),
                    egui::FontId::proportional(14.0),
                    ui.visuals().weak_text_color(),
                );
            }
        }
        if let Some((w, h)) = self.avatar_dims {
            ui.label(RichText::new(format!("{w} × {h}")).weak().small());
        }
        ui.add_space(4.0);
        if ui.button(tr!("更换头像…", "Change avatar…")).clicked() {
            self.pick_avatar();
        }
        ui.label(RichText::new(tr!("也可以直接把图片拖进窗口", "Or drop an image onto the window")).weak().small());
        ui.separator();

        egui::Grid::new("summary").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            ui.label(tr!("格式", "Format"));
            ui.label(self.origin_label());
            ui.end_row();
            ui.label(tr!("图片链接", "Image links"));
            ui.label(self.refs.len().to_string());
            ui.end_row();
            ui.label(tr!("备选开场白", "Alt. greetings"));
            ui.label(self.data.alternate_greetings.len().to_string());
            ui.end_row();
            ui.label(tr!("世界书", "Lorebook"));
            let book = self.data.character_book.as_ref();
            ui.label(book.map_or(tr!("无", "none").into(), |b| trf!("{} 条", "{} entries", b.entries.len())));
            ui.end_row();
        });
        let model_imgs: usize = images::MODEL_FIELDS.iter().map(|f| self.images_in(f)).sum();
        if model_imgs > 0 {
            ui.add_space(6.0);
            ui.colored_label(
                ui.visuals().warn_fg_color,
                trf!(
                    "⚠ 有 {model_imgs} 张图片写在会发给模型的字段里，会白白占用 token。可在「图片」页一键移出。",
                    "⚠ {model_imgs} images sit in fields that are sent to the model, wasting tokens. \
                     Move them out from the Images tab."
                ),
            );
        }
    }

    fn tabs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            let before = self.tab;
            let book = self.data.character_book.as_ref().map_or(0, |b| b.entries.len());
            for (tab, label) in [
                (Tab::Basic, tr!("角色", "Character").to_owned()),
                (Tab::Dialogue, trf!("对话 ({})", "Dialogue ({})", 1 + self.data.alternate_greetings.len())),
                (Tab::Book, trf!("世界书 ({book})", "Lorebook ({book})")),
                (Tab::Images, trf!("图片 ({})", "Images ({})", self.refs.len())),
                (Tab::Raw, tr!("原始 JSON", "Raw JSON").to_owned()),
                (Tab::Info, tr!("文件信息", "File info").to_owned()),
            ] {
                ui.selectable_value(&mut self.tab, tab, label);
            }
            if self.tab != before {
                self.gallery = None; // 酒馆那边可能有变化（导入了角色、改了 Gallery 文件夹）
                // 离开编辑页时重新扫描，进入 JSON 页时重新生成
                self.rescan();
                if self.tab == Tab::Raw {
                    self.refresh_raw();
                }
            }
        });
    }

    fn basic_tab(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        for (label, value) in [
            (tr!("名称", "Name"), &mut self.data.name),
            (tr!("作者", "Creator"), &mut self.data.creator),
            (tr!("版本", "Version"), &mut self.data.character_version),
        ] {
            ui.horizontal(|ui| {
                ui.add_sized([60.0, 20.0], egui::Label::new(label));
                changed |= ui.add(egui::TextEdit::singleline(value).desired_width(ui.available_width())).changed();
            });
        }
        ui.horizontal_wrapped(|ui| {
            ui.add_sized([60.0, 20.0], egui::Label::new(tr!("标签", "Tags")));
            let mut remove = None;
            for (i, tag) in self.data.tags.iter().enumerate() {
                if ui.small_button(format!("{tag}  ✕")).on_hover_text(tr!("点击删除", "Click to remove")).clicked()
                {
                    remove = Some(i);
                }
            }
            if let Some(i) = remove {
                self.data.tags.remove(i);
                changed = true;
            }
            let hint = tr!("新标签，回车添加", "New tag, press Enter");
            let r = ui.add(egui::TextEdit::singleline(&mut self.new_tag).hint_text(hint).desired_width(160.0));
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && !self.new_tag.trim().is_empty() {
                self.data.tags.push(self.new_tag.trim().to_owned());
                self.new_tag.clear();
                changed = true;
                r.request_focus();
            }
        });
        let counts = [self.images_in("description"), self.images_in("personality"), self.images_in("scenario")];
        let d = &mut self.data;
        changed |= text_field(
            ui,
            tr!("描述 description", "Description"),
            tr!("角色的主要设定，会发给模型", "Main character definition, sent to the model"),
            &mut d.description,
            14,
            counts[0],
        );
        changed |= text_field(
            ui,
            tr!("性格 personality", "Personality"),
            tr!("性格摘要，会发给模型", "Personality summary, sent to the model"),
            &mut d.personality,
            6,
            counts[1],
        );
        changed |= text_field(
            ui,
            tr!("场景 scenario", "Scenario"),
            tr!("对话发生的情境，会发给模型", "Circumstances of the chat, sent to the model"),
            &mut d.scenario,
            4,
            counts[2],
        );
        self.dirty |= changed;
    }

    fn dialogue_tab(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        let n = self.images_in("first_mes");
        changed |= text_field(
            ui,
            tr!("开场白 first_mes", "First message"),
            tr!(
                "聊天的第一条消息，里面的图片会在聊天中显示",
                "Opening chat message; images here are shown in the chat"
            ),
            &mut self.data.first_mes,
            10,
            n,
        );

        let mut remove = None;
        let mut move_up = None;
        for (i, g) in self.data.alternate_greetings.iter_mut().enumerate() {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.strong(trf!("备选开场白 #{}", "Alternate greeting #{}", i + 1));
                ui.label(RichText::new(char_count(g)).weak().small());
                if i > 0 && ui.small_button(tr!("⬆ 上移", "⬆ Move up")).clicked() {
                    move_up = Some(i);
                }
                if ui.small_button(tr!("🗑 删除", "🗑 Delete")).clicked() {
                    remove = Some(i);
                }
            });
            changed |= ui.add(egui::TextEdit::multiline(g).desired_rows(6).desired_width(f32::INFINITY)).changed();
        }
        if let Some(i) = move_up {
            self.data.alternate_greetings.swap(i, i - 1);
            changed = true;
        }
        if let Some(i) = remove {
            self.data.alternate_greetings.remove(i);
            changed = true;
        }
        ui.add_space(4.0);
        if ui.button(tr!("＋ 添加备选开场白", "＋ Add alternate greeting")).clicked() {
            self.data.alternate_greetings.push(String::new());
            changed = true;
        }

        let counts = [
            self.images_in("mes_example"),
            self.images_in("system_prompt"),
            self.images_in("post_history_instructions"),
            self.images_in("creator_notes"),
        ];
        let d = &mut self.data;
        changed |= text_field(
            ui,
            tr!("对话示例 mes_example", "Example dialogue"),
            tr!("用 <START> 分隔多段示例，会发给模型", "Separate examples with <START>; sent to the model"),
            &mut d.mes_example,
            8,
            counts[0],
        );
        changed |= text_field(
            ui,
            tr!("系统提示 system_prompt", "System prompt"),
            tr!("覆盖酒馆的主提示词，留空则使用默认", "Overrides the main prompt; leave empty for the default"),
            &mut d.system_prompt,
            4,
            counts[1],
        );
        changed |= text_field(
            ui,
            tr!("历史后指令 post_history_instructions", "Post-history instructions"),
            tr!("插在聊天记录之后（越狱位置）", "Inserted after the chat history (jailbreak slot)"),
            &mut d.post_history_instructions,
            4,
            counts[2],
        );
        changed |= text_field(
            ui,
            tr!("作者备注 creator_notes", "Creator notes"),
            tr!("写给使用者看的说明，不会发给模型", "Notes for users; not sent to the model"),
            &mut d.creator_notes,
            6,
            counts[3],
        );
        self.dirty |= changed;
    }

    fn book_tab(&mut self, ui: &mut egui::Ui) {
        let Some(book) = &mut self.data.character_book else {
            ui.label(tr!("这张卡没有内嵌世界书。", "This card has no embedded lorebook."));
            if ui.button(tr!("创建世界书", "Create lorebook")).clicked() {
                self.data.character_book = Some(CharacterBook::default());
                self.dirty = true;
            }
            return;
        };
        let mut changed = false;
        let mut delete_book = false;
        ui.horizontal(|ui| {
            ui.label(tr!("世界书名称", "Lorebook name"));
            let mut name = book.extra.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_owned();
            if ui.text_edit_singleline(&mut name).changed() {
                book.extra.insert("name".into(), name.into());
                changed = true;
            }
            if ui.button(tr!("＋ 新条目", "＋ New entry")).clicked() {
                book.entries.push(BookEntry::new(book.entries.len()));
                self.entry_sel = book.entries.len() - 1;
                changed = true;
            }
            if self.book_delete_armed {
                let confirm = RichText::new(tr!("确认删除整个世界书", "Really delete the whole lorebook"))
                    .color(Color32::LIGHT_RED);
                if ui.button(confirm).clicked() {
                    delete_book = true;
                }
                if ui.button(tr!("取消", "Cancel")).clicked() {
                    self.book_delete_armed = false;
                }
            } else if ui.button(tr!("删除世界书…", "Delete lorebook…")).clicked() {
                self.book_delete_armed = true;
            }
        });
        ui.separator();

        egui::Panel::left("entries").resizable(true).default_size(240.0).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("entry_list").show(ui, |ui| {
                for (i, e) in book.entries.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        changed |= ui.checkbox(&mut e.enabled, "").on_hover_text(tr!("启用", "Enabled")).changed();
                        let title = entry_title(e, i);
                        if ui.selectable_label(self.entry_sel == i, title).clicked() {
                            self.entry_sel = i;
                        }
                    });
                }
                if book.entries.is_empty() {
                    ui.label(RichText::new(tr!("还没有条目", "No entries yet")).weak());
                }
            });
        });
        egui::CentralPanel::default().show(ui, |ui| {
            let Some(e) = book.entries.get_mut(self.entry_sel) else { return };
            let mut remove = false;
            egui::ScrollArea::vertical().id_salt("entry_edit").show(ui, |ui| {
                ui.horizontal(|ui| {
                    changed |= ui.checkbox(&mut e.enabled, tr!("启用", "Enabled")).changed();
                    if ui.button(tr!("🗑 删除此条目", "🗑 Delete entry")).clicked() {
                        remove = true;
                    }
                });
                ui.label(tr!("备注（comment）", "Comment"));
                changed |= ui.add(egui::TextEdit::singleline(&mut e.comment).desired_width(f32::INFINITY)).changed();
                ui.label(tr!("触发关键词（每行一个）", "Trigger keys (one per line)"));
                let mut keys = e.keys.join("\n");
                if ui.add(egui::TextEdit::multiline(&mut keys).desired_rows(3).desired_width(f32::INFINITY)).changed() {
                    e.keys = keys.split('\n').map(str::to_owned).collect();
                    changed = true;
                }
                let hint = tr!("关键词命中时插入的文本", "Text inserted when a key matches");
                changed |= text_field(ui, tr!("内容 content", "Content"), hint, &mut e.content, 14, 0);
                if !e.extra.is_empty() {
                    ui.collapsing(tr!("其他字段（只读）", "Other fields (read-only)"), |ui| {
                        let text = serde_json::to_string_pretty(&e.extra).unwrap();
                        ui.label(RichText::new(text).monospace().small());
                    });
                }
            });
            if remove {
                book.entries.remove(self.entry_sel);
                self.entry_sel = self.entry_sel.saturating_sub(1);
                changed = true;
            }
        });
        if delete_book {
            self.data.character_book = None;
            self.book_delete_armed = false;
            changed = true;
        }
        self.dirty |= changed;
    }

    fn images_tab(&mut self, ui: &mut egui::Ui) {
        let (done, queued, failed) = self.image_counts();
        let missing = self.refs.len() - done - queued;
        let gallery = self.gallery_target();
        let card_dir = self.image_dir.clone();
        let model_imgs: usize = images::MODEL_FIELDS.iter().map(|f| self.images_in(f)).sum();

        ui.horizontal_wrapped(|ui| {
            if ui.button(tr!("🔄 重新扫描", "🔄 Rescan")).clicked() {
                self.gallery = None;
                self.rescan();
            }
            if ui
                .add_enabled(
                    missing > 0,
                    egui::Button::new(trf!("⬇ 下载全部 ({missing})", "⬇ Download all ({missing})")),
                )
                .clicked()
            {
                self.download_all();
            }
            if ui.add_enabled(done > 0, egui::Button::new(tr!("导出到文件夹…", "Export to folder…"))).clicked()
                && let Some(dir) =
                    rfd::FileDialog::new().set_title(tr!("选择导出目录", "Choose export folder")).pick_folder()
            {
                self.export_images(&dir, None);
            }
            let tip = match (&self.tavern, &gallery) {
                (None, _) => {
                    tr!("没有找到酒馆，请在设置里指定酒馆目录", "SillyTavern not found; set its folder in Settings")
                        .to_owned()
                }
                (Some(_), None) => tr!(
                    "角色名为空，或清理非法字符后不能用作文件夹名",
                    "The card name is empty, or not usable as a folder name once illegal characters are removed"
                )
                .to_owned(),
                (Some(_), Some(g)) => {
                    let why = match g.source {
                        FolderSource::Override => tr!(
                            "这个角色在酒馆里设置了自定义 Gallery 文件夹",
                            "This character has a custom gallery folder in SillyTavern"
                        ),
                        FolderSource::Character => {
                            tr!(
                                "酒馆里已有这个角色，使用角色名文件夹",
                                "Character found in SillyTavern; using its name"
                            )
                        }
                        FolderSource::NotImported => tr!(
                            "酒馆里还没导入这个角色，先按角色名放好，导入后即可在 Gallery 看到",
                            "Not imported into SillyTavern yet; images will show up in the gallery once it is"
                        ),
                    };
                    format!("{}\n{why}", g.dir.display())
                }
            };
            let gallery_btn = egui::Button::new(tr!("导出到酒馆 Gallery", "Export to SillyTavern gallery"));
            if ui.add_enabled(done > 0 && gallery.is_some(), gallery_btn).on_hover_text(tip).clicked() {
                // 导出前重新确认目标，提示里显示的可能已经过时
                self.gallery = None;
                if let Some(g) = self.gallery_target() {
                    self.export_images(&g.dir, Some(&tavern::GALLERY_EXTS));
                }
            }
            let open = egui::Button::new(tr!("打开图片文件夹", "Open image folder"));
            if ui.add_enabled(card_dir.is_dir(), open).on_hover_text(card_dir.display().to_string()).clicked() {
                open_external(&card_dir);
            }
            let btn = ui
                .add_enabled(
                    model_imgs > 0,
                    egui::Button::new(tr!("把图片移出模型字段", "Move images out of model fields")),
                )
                .on_hover_text(tr!(
                    "把 description / personality 等会发给模型的字段里的图片标签移到作者备注，开场白不受影响",
                    "Move image tags from description / personality and other model-visible fields into the \
                     creator notes. Greetings are left untouched."
                ));
            if btn.clicked() {
                let (n, leftovers) = images::move_images_to_notes(&mut self.data);
                self.dirty = true;
                self.rescan();
                self.status = if leftovers.is_empty() {
                    trf!("已把 {n} 个图片标签移到作者备注", "Moved {n} image tags into the creator notes")
                } else {
                    trf!(
                        "已把 {n} 个图片标签移到作者备注；请检查剩余文字：{}",
                        "Moved {n} image tags into the creator notes; please check the leftover text: {}",
                        leftovers.join("; ")
                    )
                };
            }
            ui.add(egui::Slider::new(&mut self.thumb, 100.0..=360.0).text(tr!("缩略图", "Thumbnails")));
        });
        ui.label(
            RichText::new(trf!(
                "共 {} 个链接，已下载 {done}，下载中 {queued}，失败 {failed}。图片文件夹：{}",
                "{} links: {done} downloaded, {queued} downloading, {failed} failed. Folder: {}",
                self.refs.len(),
                card_dir.display()
            ))
            .weak()
            .small(),
        );
        ui.separator();

        if self.refs.is_empty() {
            ui.label(tr!("卡片字段中没有图片链接。", "No image links found in the card."));
            return;
        }
        let size = self.thumb;
        let mut actions: Vec<(usize, ImgAction)> = Vec::new();
        egui::ScrollArea::vertical().id_salt("gallery").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::splat(10.0);
                for (i, r) in self.refs.iter().enumerate() {
                    let state = self.states.get(&r.url).cloned().unwrap_or(DlState::Missing);
                    ui.allocate_ui(Vec2::new(size + 16.0, size + 110.0), |ui| {
                        ui.group(|ui| {
                            ui.set_width(size);
                            ui.vertical(|ui| {
                                if let Some(a) = image_card(ui, i, r, &state, size) {
                                    actions.push((i, a));
                                }
                            });
                        });
                    });
                }
            });
        });
        for (i, a) in actions {
            let url = self.refs[i].url.clone();
            match a {
                ImgAction::Download => self.queue(&url),
                ImgAction::Preview(p) => self.preview = Some(p),
                ImgAction::Avatar(p) => self.set_avatar_from_file(&p),
                ImgAction::Open(p) => open_external(&p),
                ImgAction::Copy => {
                    ui.ctx().copy_text(url);
                    self.status = tr!("链接已复制", "Link copied").into();
                }
            }
        }
    }

    fn raw_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button(tr!("从当前卡片重新生成", "Regenerate from card")).clicked() {
                self.refresh_raw();
            }
            if ui.button(tr!("✔ 应用到卡片", "✔ Apply to card")).clicked() {
                let parsed = serde_json::from_str(&self.raw)
                    .map_err(|e| trf!("JSON 语法错误：{e}", "JSON syntax error: {e}"))
                    .and_then(|v| card::parse_json(v, "json"));
                match parsed {
                    Ok(c) => {
                        self.data = c.data;
                        self.dirty = true;
                        self.raw_err = None;
                        self.rescan();
                        self.status = tr!("已应用 JSON 修改", "Applied JSON changes").into();
                    }
                    Err(e) => self.raw_err = Some(e),
                }
            }
            ui.label(
                RichText::new(tr!(
                    "保存时写入 ccv3 块的 V3 JSON。修改后需点「应用」，切换页面会丢弃未应用的修改。",
                    "The V3 JSON written to the ccv3 chunk on save. Click Apply after editing; \
                     switching tabs discards unapplied changes."
                ))
                .weak(),
            );
        });
        if let Some(e) = &self.raw_err {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        egui::ScrollArea::vertical().id_salt("raw").show(ui, |ui| {
            ui.add(egui::TextEdit::multiline(&mut self.raw).code_editor().desired_width(f32::INFINITY));
        });
    }

    fn info_tab(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().id_salt("info").show(ui, |ui| {
            egui::Grid::new("fileinfo").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
                ui.label(tr!("文件", "File"));
                ui.label(self.path.as_ref().map_or(tr!("（未保存）", "(unsaved)").into(), |p| p.display().to_string()));
                ui.end_row();
                ui.label(tr!("读取格式", "Loaded as"));
                ui.label(self.origin_label());
                ui.end_row();
                ui.label(tr!("头像", "Avatar"));
                ui.label(match (&self.avatar, self.avatar_dims) {
                    (Some(b), Some((w, h))) => format!("{w} × {h}, {}", human_size(b.len())),
                    _ => tr!("无", "none").into(),
                });
                ui.end_row();
                ui.label(tr!("IEND 之后", "After IEND"));
                ui.label(trf!("{} 字节", "{} bytes", self.trailing_len));
                ui.end_row();
            });
            for w in &self.warnings {
                ui.colored_label(ui.visuals().warn_fg_color, format!("⚠ {w}"));
            }
            ui.add_space(6.0);
            ui.checkbox(
                &mut self.strip_chunks,
                tr!(
                    "保存时移除非必需的附加块（例如导出工具留下的私有块）",
                    "Strip non-essential chunks on save (e.g. private chunks left by export tools)"
                ),
            );
            ui.separator();

            if self.chunks.is_empty() {
                ui.label(tr!(
                    "当前卡片不是从 PNG 读取的，没有块信息。",
                    "This card was not loaded from a PNG; no chunk info."
                ));
                return;
            }
            ui.strong(tr!("PNG 块（保存后会刷新）", "PNG chunks (refreshed after saving)"));
            let idat: Vec<&ChunkInfo> = self.chunks.iter().filter(|c| c.kind == "IDAT").collect();
            egui::Grid::new("chunks").striped(true).num_columns(5).spacing([16.0, 4.0]).show(ui, |ui| {
                let headers = if i18n::is_en() {
                    ["Type", "Offset", "Size", "Keyword", "Meaning"]
                } else {
                    ["类型", "偏移", "大小", "关键字", "说明"]
                };
                for h in headers {
                    ui.strong(h);
                }
                ui.end_row();
                let mut idat_shown = false;
                for c in &self.chunks {
                    if c.kind == "IDAT" {
                        if idat_shown {
                            continue;
                        }
                        idat_shown = true;
                        let total: usize = idat.iter().map(|c| c.len).sum();
                        ui.label(RichText::new("IDAT").monospace());
                        ui.label(c.offset.to_string());
                        ui.label(human_size(total));
                        ui.label("");
                        ui.label(trf!("图像数据（{} 个块）", "Image data ({} chunks)", idat.len()));
                        ui.end_row();
                        continue;
                    }
                    ui.label(RichText::new(&c.kind).monospace());
                    ui.label(c.offset.to_string());
                    ui.label(human_size(c.len));
                    ui.label(c.keyword.clone().unwrap_or_default());
                    let note = match c.keyword.as_deref() {
                        Some("chara") => tr!("角色卡数据（V1/V2）", "Card data (V1/V2)"),
                        Some("ccv3") => tr!("角色卡数据（V3）", "Card data (V3)"),
                        Some(_) => tr!("其他文本元数据", "Other text metadata"),
                        None if c.essential => tr!("图像必需", "Required for the image"),
                        None => tr!("私有/附加块", "Private / ancillary chunk"),
                    };
                    ui.label(note);
                    ui.end_row();
                }
            });
        });
    }

    fn pending_modal(&mut self, ctx: &egui::Context) {
        let Some(action) = &self.pending else { return };
        let verb = match action {
            Pending::Open(_) => tr!("打开其他文件前", "before opening another file"),
            Pending::New => tr!("新建卡片前", "before creating a new card"),
            Pending::Quit => tr!("退出前", "before quitting"),
        };
        let mut choice = None;
        egui::Modal::new(egui::Id::new("unsaved")).show(ctx, |ui| {
            ui.heading(tr!("有未保存的修改", "Unsaved changes"));
            ui.label(trf!("{verb}要保存当前卡片吗？", "Save the current card {verb}?"));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(tr!("保存", "Save")).clicked() {
                    choice = Some(0);
                }
                if ui.button(tr!("不保存", "Don't save")).clicked() {
                    choice = Some(1);
                }
                if ui.button(tr!("取消", "Cancel")).clicked() {
                    choice = Some(2);
                }
            });
        });
        let Some(choice) = choice else { return };
        let action = self.pending.take().unwrap();
        if choice == 0 {
            self.save();
            if self.dirty {
                return; // 保存被取消或失败
            }
        }
        if choice <= 1 {
            let quit = matches!(action, Pending::Quit);
            self.perform(action);
            if quit {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = true;
        let mut changed = false;
        egui::Window::new(tr!("设置", "Settings"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(560.0)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .show(ctx, |ui| {
                ui.heading(tr!("图片库", "Image library"));
                ui.label(
                    RichText::new(tr!(
                        "每张卡的图片放在以角色名命名的子文件夹里。",
                        "Each card's images are stored in a subfolder named after the character."
                    ))
                    .weak(),
                );
                let root = self.image_root.clone();
                ui.label(RichText::new(root.display().to_string()).monospace());
                ui.horizontal(|ui| {
                    if ui.button(tr!("更改…", "Change…")).clicked()
                        && let Some(dir) = rfd::FileDialog::new()
                            .set_title(tr!("选择图片库目录", "Choose image library folder"))
                            .pick_folder()
                    {
                        self.settings.image_dir = Some(dir);
                        changed = true;
                    }
                    let reset = egui::Button::new(tr!("恢复默认", "Reset to default"));
                    if ui.add_enabled(self.settings.image_dir.is_some(), reset).clicked() {
                        self.settings.image_dir = None;
                        changed = true;
                    }
                    if ui.add_enabled(root.is_dir(), egui::Button::new(tr!("打开", "Open"))).clicked() {
                        open_external(&root);
                    }
                });

                ui.separator();
                ui.heading("SillyTavern");
                match &self.tavern {
                    Some(t) => {
                        ui.label(trf!("数据目录：{}", "Data folder: {}", t.data_root.display()));
                    }
                    None => {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            tr!(
                                "没有找到酒馆。请选择酒馆的安装目录（含 config.yaml）或数据目录。",
                                "SillyTavern not found. Choose its install folder (the one with config.yaml) \
                                 or its data folder."
                            ),
                        );
                    }
                }
                let how = if self.settings.tavern_dir.is_some() {
                    tr!("手动指定", "Set manually")
                } else {
                    tr!(
                        "自动检测：SILLYTAVERN_DATAROOT、~/SillyTavern、全局安装位置等",
                        "Auto-detected: SILLYTAVERN_DATAROOT, ~/SillyTavern, the global install location, …"
                    )
                };
                ui.label(RichText::new(how).weak().small());
                ui.horizontal(|ui| {
                    if ui.button(tr!("选择目录…", "Choose folder…")).clicked()
                        && let Some(dir) = rfd::FileDialog::new()
                            .set_title(tr!("选择酒馆目录", "Choose SillyTavern folder"))
                            .pick_folder()
                    {
                        match tavern::resolve(&dir) {
                            Some(t) => {
                                self.tavern = Some(t);
                                self.settings.tavern_dir = Some(dir);
                                changed = true;
                            }
                            None => {
                                self.status = tr!(
                                    "这个目录里没有找到酒馆的数据（characters 或 user/images）",
                                    "No SillyTavern data (characters or user/images) found in that folder"
                                )
                                .into();
                            }
                        }
                    }
                    let auto = egui::Button::new(tr!("自动检测", "Auto-detect"));
                    if ui.add_enabled(self.settings.tavern_dir.is_some(), auto).clicked() {
                        self.settings.tavern_dir = None;
                        self.tavern = tavern::detect(None);
                        changed = true;
                    }
                });
                let users = self.tavern.as_ref().map(|t| t.users.clone()).unwrap_or_default();
                if users.len() > 1 {
                    let mut user = self.tavern_user().unwrap_or_default();
                    egui::ComboBox::from_label(tr!("导出到哪个酒馆用户", "SillyTavern user to export to"))
                        .selected_text(&user)
                        .show_ui(ui, |ui| {
                            for u in &users {
                                ui.selectable_value(&mut user, u.clone(), u);
                            }
                        });
                    if self.settings.tavern_user.as_ref() != Some(&user) && self.tavern_user() != Some(user.clone()) {
                        self.settings.tavern_user = Some(user);
                        changed = true;
                    }
                }
            });
        self.show_settings = open;
        if changed {
            self.gallery = None;
            let root = self.settings.image_root();
            if root != self.image_root {
                self.image_root = root;
                self.image_dir = images::card_dir(&self.image_root, &self.data.name);
                self.states.clear();
                self.rescan();
            }
            self.save_settings();
        }
    }

    fn preview_window(&mut self, ctx: &egui::Context) {
        let Some(path) = self.preview.clone() else { return };
        let screen = ctx.content_rect().size();
        let r = egui::Modal::new(egui::Id::new("preview")).show(ctx, |ui| {
            ui.add(egui::Image::new(file_uri(&path)).max_size(screen * 0.85).sense(egui::Sense::click()))
                .on_hover_text(tr!("点击或按 Esc 关闭", "Click or press Esc to close"))
                .clicked()
        });
        if r.inner || r.should_close() {
            self.preview = None;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.downloader.poll(&self.image_dir, &mut self.states) > 0 {
            let (done, queued, failed) = self.image_counts();
            if queued == 0 {
                self.status = trf!(
                    "下载完成：已下载 {done}，失败 {failed}",
                    "Downloads finished: {done} downloaded, {failed} failed"
                );
            }
        }
        self.handle_input(&ctx);

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui);
            ui.add_space(2.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("avatar").resizable(true).default_size(260.0).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("avatar_scroll").show(ui, |ui| self.avatar_panel(ui));
        });
        egui::CentralPanel::default().show(ui, |ui| {
            self.tabs(ui);
            ui.separator();
            match self.tab {
                Tab::Basic => {
                    egui::ScrollArea::vertical().id_salt("basic").show(ui, |ui| self.basic_tab(ui));
                }
                Tab::Dialogue => {
                    egui::ScrollArea::vertical().id_salt("dialogue").show(ui, |ui| self.dialogue_tab(ui));
                }
                Tab::Book => self.book_tab(ui),
                Tab::Images => self.images_tab(ui),
                Tab::Raw => self.raw_tab(ui),
                Tab::Info => self.info_tab(ui),
            }
        });
        self.pending_modal(&ctx);
        self.preview_window(&ctx);
        self.settings_window(&ctx);

        let name = if self.data.name.is_empty() { tr!("未命名", "Untitled") } else { &self.data.name };
        let title = format!("{}{name} — Card Forge", if self.dirty { "● " } else { "" });
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}

enum ImgAction {
    Download,
    Preview(PathBuf),
    Avatar(PathBuf),
    Open(PathBuf),
    Copy,
}

/// 图库中的单个图片卡片。
fn image_card(ui: &mut egui::Ui, i: usize, r: &ImageRef, state: &DlState, size: f32) -> Option<ImgAction> {
    let mut action = None;
    match state {
        DlState::Done(p) => {
            let img = egui::Image::new(file_uri(p)).max_size(Vec2::splat(size)).sense(egui::Sense::click());
            let resp = ui.add_sized(Vec2::splat(size), img).on_hover_text(tr!("点击放大", "Click to enlarge"));
            if resp.clicked() {
                action = Some(ImgAction::Preview(p.clone()));
            }
        }
        other => {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
            ui.painter().rect_filled(rect, 4.0, ui.visuals().faint_bg_color);
            let text = match other {
                DlState::Queued => tr!("下载中…", "Downloading…").to_owned(),
                DlState::Failed(e) => {
                    let e: String = e.chars().take(60).collect();
                    trf!("下载失败\n{e}", "Download failed\n{e}")
                }
                _ => tr!("未下载", "Not downloaded").to_owned(),
            };
            let color = if matches!(other, DlState::Failed(_)) {
                ui.visuals().error_fg_color
            } else {
                ui.visuals().weak_text_color()
            };
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                text,
                egui::FontId::proportional(13.0),
                color,
            );
        }
    }
    ui.label(RichText::new(format!("#{:02} {}", i + 1, images::url_stem(&r.url))).small()).on_hover_text(&r.url);
    ui.label(RichText::new(r.fields.join(", ")).weak().small())
        .on_hover_text(tr!("出现在这些字段中", "Fields containing this image"));
    ui.horizontal_wrapped(|ui| {
        match state {
            DlState::Done(p) => {
                if ui.small_button(tr!("设为头像", "Set as avatar")).clicked() {
                    action = Some(ImgAction::Avatar(p.clone()));
                }
                if ui.small_button(tr!("打开", "Open")).clicked() {
                    action = Some(ImgAction::Open(p.clone()));
                }
            }
            DlState::Queued => {
                ui.spinner();
            }
            DlState::Failed(_) => {
                if ui.small_button(tr!("重试", "Retry")).clicked() {
                    action = Some(ImgAction::Download);
                }
            }
            DlState::Missing => {
                if ui.small_button(tr!("下载", "Download")).clicked() {
                    action = Some(ImgAction::Download);
                }
            }
        }
        if ui.small_button(tr!("复制链接", "Copy link")).clicked() {
            action = Some(ImgAction::Copy);
        }
    });
    action
}

/// 带字数和图片提示的多行文本框。
fn text_field(ui: &mut egui::Ui, label: &str, hint: &str, text: &mut String, rows: usize, images: usize) -> bool {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.strong(label);
        ui.label(RichText::new(char_count(text)).weak().small());
        if images > 0 {
            let warn = trf!("含 {images} 张图片", "{images} images");
            ui.label(RichText::new(warn).small().color(ui.visuals().warn_fg_color));
        }
        ui.label(RichText::new(hint).weak().small());
    });
    ui.add(egui::TextEdit::multiline(text).desired_rows(rows).desired_width(f32::INFINITY)).changed()
}

fn entry_title(e: &BookEntry, i: usize) -> String {
    let keys: Vec<&str> = e.keys.iter().map(|k| k.trim()).filter(|k| !k.is_empty()).collect();
    let base = if !e.comment.trim().is_empty() {
        e.comment.trim().to_owned()
    } else if !keys.is_empty() {
        keys.join(", ")
    } else {
        tr!("（未命名）", "(untitled)").to_owned()
    };
    let short: String = base.chars().take(28).collect();
    format!("{}. {short}", i + 1)
}

fn char_count(s: &str) -> String {
    let n = s.chars().count();
    trf!("{n} 字", "{n} chars")
}

fn file_uri(p: &Path) -> String {
    format!("file://{}", p.display())
}

fn has_ext(p: &Path, exts: &[&str]) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

fn safe_name(name: &str) -> String {
    let s: String = name.chars().filter(|c| !"/\\:*?\"<>|".contains(*c)).collect();
    if s.trim().is_empty() { "card".into() } else { s.trim().to_owned() }
}

fn human_size(n: usize) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.1} KB", n as f64 / (1 << 10) as f64),
        n => format!("{n} B"),
    }
}

fn open_external(p: &Path) {
    let _ = std::process::Command::new("xdg-open").arg(p).spawn();
}
