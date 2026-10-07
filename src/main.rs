mod i18n;

mod app;
mod card;
mod images;
mod png;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use eframe::egui;

use crate::i18n::{tr, trf};

const USAGE_ZH: &str = "\
card-forge —— 酒馆角色卡编辑器

用法：
  card-forge [文件]        打开 GUI，可选地直接载入一张卡（.png / .json）
  card-forge info <文件>   在终端里打印卡片信息（块列表、字段长度、图片链接）

环境变量 CARD_FORGE_LANG=zh|en 可强制界面语言。";

const USAGE_EN: &str = "\
card-forge — SillyTavern character card editor

Usage:
  card-forge [FILE]        open the GUI, optionally loading a card (.png / .json)
  card-forge info <FILE>   print card info in the terminal (chunks, field sizes, image links)

Set CARD_FORGE_LANG=zh|en to force the UI language.";

fn usage() -> &'static str {
    if i18n::is_en() { USAGE_EN } else { USAGE_ZH }
}

fn main() -> eframe::Result {
    i18n::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-h" | "--help") => {
            println!("{}", usage());
            return Ok(());
        }
        Some("info") => {
            let Some(path) = args.get(1) else {
                eprintln!("{}", usage());
                std::process::exit(2);
            };
            std::process::exit(print_info(Path::new(path)));
        }
        _ => {}
    }

    let initial = args.first().map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Card Forge")
            .with_app_id("card-forge")
            .with_inner_size([1280.0, 840.0])
            .with_min_inner_size([900.0, 560.0])
            .with_drag_and_drop(true)
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/icons/card-forge-256.png")).unwrap(),
            ),
        ..Default::default()
    };
    eframe::run_native(
        "card-forge",
        options,
        Box::new(move |cc| {
            setup_fonts(&cc.egui_ctx);
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(app::App::new(cc, initial)))
        }),
    )
}

fn print_info(path: &Path) -> i32 {
    let loaded = match card::load_file(path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{}", trf!("错误：{e}", "Error: {e}"));
            return 1;
        }
    };
    if !loaded.chunks.is_empty() {
        println!("{}", tr!("PNG 块：", "PNG chunks:"));
        for c in loaded.chunks.iter().filter(|c| c.kind != "IDAT") {
            let kw = c.keyword.as_deref().map(|k| trf!("  关键字={k}", "  keyword={k}")).unwrap_or_default();
            println!("  {:<4} @{:<9} {:>9} {}{kw}", c.kind, c.offset, c.len, tr!("字节", "bytes"));
        }
        let idat = loaded.chunks.iter().filter(|c| c.kind == "IDAT").count();
        println!("{}", trf!("  （另有 {idat} 个 IDAT 图像数据块）", "  (plus {idat} IDAT image data chunks)"));
        println!("{}", trf!("IEND 之后的数据：{} 字节", "Data after IEND: {} bytes", loaded.trailing_len));
    }
    for w in &loaded.warnings {
        println!("{}", trf!("警告：{w}", "Warning: {w}"));
    }
    let Some(c) = loaded.card else {
        println!("{}", tr!("没有找到角色卡数据", "No character card data found"));
        return 1;
    };
    println!("\n{}", trf!("格式：{}（来自 {}）", "Format: {} (from {})", c.spec, c.source));
    let d = &c.data;
    println!("{}", trf!("名称：{}", "Name: {}", d.name));
    for (k, v) in [
        ("description", &d.description),
        ("personality", &d.personality),
        ("scenario", &d.scenario),
        ("first_mes", &d.first_mes),
        ("mes_example", &d.mes_example),
        ("creator_notes", &d.creator_notes),
        ("system_prompt", &d.system_prompt),
        ("post_history_instructions", &d.post_history_instructions),
    ] {
        println!("  {k:<26} {:>6} {}", v.chars().count(), tr!("字", "chars"));
    }
    let entries = tr!("条", "entries");
    println!("  {:<26} {:>6} {entries}", "alternate_greetings", d.alternate_greetings.len());
    let book = d.character_book.as_ref().map_or(0, |b| b.entries.len());
    println!("  {:<26} {:>6} {entries}", "character_book", book);
    if !d.extra.is_empty() {
        let keys = d.extra.keys().cloned().collect::<Vec<_>>().join(", ");
        println!("{}", trf!("  其他字段：{keys}", "  Other fields: {keys}"));
    }
    let refs = images::scan_card(d);
    println!("\n{}", trf!("图片链接：{} 个", "Image links: {}", refs.len()));
    for (i, r) in refs.iter().enumerate() {
        println!("  {:02}. {}  [{}]", i + 1, r.url, r.fields.join(", "));
    }
    0
}

/// egui 自带字体没有中文，从系统里找一个 CJK 字体作为后备。
fn setup_fonts(ctx: &egui::Context) {
    let Some((bytes, index)) = find_cjk_font() else {
        eprintln!(
            "{}",
            tr!(
                "警告：没有找到中文字体，界面中的中文将无法显示",
                "Warning: no CJK font found; Chinese text will not render"
            )
        );
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    let mut data = egui::FontData::from_owned(bytes);
    data.index = index;
    fonts.font_data.insert("cjk".into(), Arc::new(data));
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push("cjk".into());
    }
    ctx.set_fonts(fonts);
}

fn find_cjk_font() -> Option<(Vec<u8>, u32)> {
    // 优先体积适中的常见字体，避免载入几十 MB 的字体集合
    let known: [(&str, u32); 5] = [
        ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 2),
        ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 2),
        ("/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc", 2),
        ("/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc", 0),
        ("/usr/share/fonts/truetype/wqy/wqy-microhei.ttc", 0),
    ];
    for (path, index) in known {
        if let Ok(bytes) = std::fs::read(path) {
            return Some((bytes, index));
        }
    }
    let out = Command::new("fc-match").args(["-f", "%{file}\n%{index}", "sans-serif:lang=zh-cn"]).output().ok()?;
    let out = String::from_utf8(out.stdout).ok()?;
    let mut lines = out.lines();
    let file = lines.next()?;
    let index = lines.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    Some((std::fs::read(file).ok()?, index))
}
