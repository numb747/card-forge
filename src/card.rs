//! 角色卡数据模型与读写。
//!
//! 内部统一用 V2/V3 的 `data` 结构表示；读取时兼容 V1、V2、V3，
//! 保存 PNG 时同时写入 `chara`（V2）和 `ccv3`（V3）两个文本块，和 SillyTavern 的做法一致。
//! 没有建模的字段都放在 `extra` 里原样保留，避免编辑后丢数据。

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_PAD_INDIFFERENT};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, json};

use crate::i18n::{tr, trf};
use crate::png::{self, Png};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CardData {
    #[serde(default, deserialize_with = "de_string")]
    pub name: String,
    #[serde(default, deserialize_with = "de_string")]
    pub description: String,
    #[serde(default, deserialize_with = "de_string")]
    pub personality: String,
    #[serde(default, deserialize_with = "de_string")]
    pub scenario: String,
    #[serde(default, deserialize_with = "de_string")]
    pub first_mes: String,
    #[serde(default, deserialize_with = "de_string")]
    pub mes_example: String,
    #[serde(default, deserialize_with = "de_string")]
    pub creator_notes: String,
    #[serde(default, deserialize_with = "de_string")]
    pub system_prompt: String,
    #[serde(default, deserialize_with = "de_string")]
    pub post_history_instructions: String,
    #[serde(default, deserialize_with = "de_strings")]
    pub alternate_greetings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_book")]
    pub character_book: Option<CharacterBook>,
    #[serde(default, deserialize_with = "de_strings")]
    pub tags: Vec<String>,
    #[serde(default, deserialize_with = "de_string")]
    pub creator: String,
    #[serde(default, deserialize_with = "de_string")]
    pub character_version: String,
    #[serde(default, deserialize_with = "de_object")]
    pub extensions: Map<String, Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CharacterBook {
    #[serde(default, deserialize_with = "de_entries")]
    pub entries: Vec<BookEntry>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BookEntry {
    #[serde(default, deserialize_with = "de_strings")]
    pub keys: Vec<String>,
    #[serde(default, deserialize_with = "de_string")]
    pub content: String,
    #[serde(default = "yes", deserialize_with = "de_bool")]
    pub enabled: bool,
    #[serde(default, deserialize_with = "de_string")]
    pub comment: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl BookEntry {
    /// 新建条目，补齐 V2/V3 规范要求的字段。
    pub fn new(insertion_order: usize) -> Self {
        let mut extra = Map::new();
        extra.insert("insertion_order".into(), json!(insertion_order));
        extra.insert("extensions".into(), json!({}));
        extra.insert("use_regex".into(), json!(false));
        Self { keys: Vec::new(), content: String::new(), enabled: true, comment: String::new(), extra }
    }
}

/// 解析出的角色卡以及它原本的格式。
#[derive(Clone, Debug)]
pub struct ParsedCard {
    pub data: CardData,
    /// "V1"、"chara_card_v2"、"chara_card_v3" 等。
    pub spec: String,
    /// 数据来自哪个文本块（PNG）或 "json"。
    pub source: String,
}

/// PNG 中一个块的摘要，用于"文件信息"页。
#[derive(Clone, Debug)]
pub struct ChunkInfo {
    pub kind: String,
    pub len: usize,
    pub offset: usize,
    pub keyword: Option<String>,
    pub essential: bool,
}

#[derive(Clone, Debug, Default)]
pub struct LoadedFile {
    pub card: Option<ParsedCard>,
    /// 来源是 PNG 时的原始文件内容，用作头像。
    pub image: Option<Vec<u8>>,
    pub chunks: Vec<ChunkInfo>,
    pub trailing_len: usize,
    pub warnings: Vec<String>,
}

const V1_FIELDS: [&str; 6] = ["name", "description", "personality", "scenario", "first_mes", "mes_example"];

/// Pygmalion / 旧版 Tavern 的字段名到 V2 字段名的映射。
const LEGACY_ALIASES: [(&str, &str); 5] = [
    ("char_name", "name"),
    ("char_persona", "personality"),
    ("world_scenario", "scenario"),
    ("char_greeting", "first_mes"),
    ("example_dialogue", "mes_example"),
];

pub fn parse_json(value: Value, source: &str) -> Result<ParsedCard, String> {
    let Value::Object(mut obj) = value else {
        return Err(tr!("角色卡 JSON 顶层不是对象", "Card JSON is not an object").into());
    };
    let spec = obj.get("spec").and_then(Value::as_str).unwrap_or_default().to_owned();
    let data_value = match obj.remove("data") {
        Some(Value::Object(d)) if spec.starts_with("chara_card_v") => Value::Object(d),
        other => {
            // V1：字段都在顶层
            if let Some(d) = other {
                obj.insert("data".into(), d);
            }
            for (old, new) in LEGACY_ALIASES {
                let blank = obj.get(new).and_then(Value::as_str).is_none_or(str::is_empty);
                if blank && let Some(v) = obj.remove(old) {
                    obj.insert(new.into(), v);
                }
            }
            obj.remove("spec");
            obj.remove("spec_version");
            Value::Object(obj)
        }
    };
    let data = CardData::deserialize(data_value)
        .map_err(|e| trf!("角色卡字段解析失败：{e}", "Failed to parse card fields: {e}"))?;
    let spec = if spec.starts_with("chara_card_v") { spec } else { "V1".into() };
    Ok(ParsedCard { data, spec, source: source.into() })
}

/// 文本块里通常是 base64 编码的 JSON，少数工具会直接写 JSON。
pub fn decode_payload(text: &[u8]) -> Result<Value, String> {
    let trimmed: Vec<u8> = text.iter().copied().filter(|c| !c.is_ascii_whitespace()).collect();
    if let Ok(raw) = STANDARD_PAD_INDIFFERENT.decode(&trimmed)
        && let Ok(v) = serde_json::from_slice(&raw)
    {
        return Ok(v);
    }
    serde_json::from_slice(text)
        .map_err(|_| tr!("既不是 base64 编码的 JSON，也不是 JSON", "Neither base64-encoded JSON nor JSON").into())
}

pub fn load_file(path: &Path) -> Result<LoadedFile, String> {
    let bytes = std::fs::read(path).map_err(|e| trf!("读取失败：{e}", "Read failed: {e}"))?;
    if png::is_png(&bytes) {
        return load_png(bytes);
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| tr!("既不是 PNG 也不是 JSON 文件", "Not a PNG or JSON file").to_owned())?;
    Ok(LoadedFile { card: Some(parse_json(value, "json")?), ..Default::default() })
}

pub fn load_png(bytes: Vec<u8>) -> Result<LoadedFile, String> {
    let png = Png::parse(&bytes)?;
    let mut out = LoadedFile {
        chunks: png
            .chunks
            .iter()
            .map(|c| ChunkInfo {
                kind: c.kind_str(),
                len: c.data.len(),
                offset: c.offset,
                keyword: c.text().map(|t| t.keyword),
                essential: c.is_essential(),
            })
            .collect(),
        trailing_len: png.trailing.len(),
        ..Default::default()
    };
    let texts: Vec<_> = png.texts().collect();
    // V3 优先，和 SillyTavern 一致
    for key in ["ccv3", "chara"] {
        let Some(t) = texts.iter().find(|t| t.keyword.eq_ignore_ascii_case(key)) else { continue };
        match decode_payload(&t.text).and_then(|v| parse_json(v, key)) {
            Ok(card) if out.card.is_none() => out.card = Some(card),
            Ok(_) => {}
            Err(e) => out.warnings.push(trf!("{key} 块无法解析：{e}", "Could not parse the {key} chunk: {e}")),
        }
    }
    if out.trailing_len > 0 {
        out.warnings.push(trf!("IEND 之后夹带了 {} 字节数据", "{} bytes of extra data after IEND", out.trailing_len));
    }
    out.image = Some(bytes);
    Ok(out)
}

impl CardData {
    /// 导出前的整理：去掉空标签、空的备选开场白和空关键词。
    pub fn cleaned(&self) -> Self {
        let mut d = self.clone();
        let tidy = |v: &mut Vec<String>| {
            for s in v.iter_mut() {
                *s = s.trim().to_owned();
            }
            v.retain(|s| !s.is_empty());
        };
        tidy(&mut d.tags);
        d.alternate_greetings.retain(|g| !g.trim().is_empty());
        if let Some(book) = &mut d.character_book {
            for e in &mut book.entries {
                tidy(&mut e.keys);
            }
        }
        d
    }

    pub fn to_v2(&self) -> Value {
        let d = self.cleaned();
        let mut card = json!({ "spec": "chara_card_v2", "spec_version": "2.0" });
        // 顶层再放一份 V1 字段，兼容只认 V1 的老前端
        let data = serde_json::to_value(&d).unwrap();
        for f in V1_FIELDS {
            card[f] = data[f].clone();
        }
        card["data"] = data;
        card
    }

    pub fn to_v3(&self) -> Value {
        let mut d = self.cleaned();
        d.extra.entry("group_only_greetings").or_insert_with(|| json!([]));
        json!({ "spec": "chara_card_v3", "spec_version": "3.0", "data": d })
    }
}

/// 把头像图片（任意支持的格式）和角色数据打包成角色卡 PNG。
pub fn build_png(image: &[u8], data: &CardData, strip_extra_chunks: bool) -> Result<Vec<u8>, String> {
    let base = if png::is_png(image) { image.to_vec() } else { to_png(image)? };
    let mut png = Png::parse(&base)?;
    png.remove_texts(&["chara", "ccv3"]);
    if strip_extra_chunks {
        png.chunks.retain(png::Chunk::is_essential);
    }
    png.trailing.clear();
    png.insert_text("chara", STANDARD.encode(data.to_v2().to_string()).as_bytes());
    png.insert_text("ccv3", STANDARD.encode(data.to_v3().to_string()).as_bytes());
    Ok(png.encode())
}

pub fn to_png(image: &[u8]) -> Result<Vec<u8>, String> {
    let img = image::load_from_memory(image).map_err(|e| trf!("无法解码图片：{e}", "Could not decode image: {e}"))?;
    encode_png(&img)
}

/// 没有头像时使用的纯色占位图。
pub fn placeholder_png() -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(400, 600, image::Rgba([72, 76, 88, 255]));
    encode_png(&img.into()).unwrap()
}

fn encode_png(img: &image::DynamicImage) -> Result<Vec<u8>, String> {
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(out.into_inner())
}

// ---- 宽松的反序列化：网上的卡经常类型不规范（数字当字符串、标签写成字符串等）----

fn yes() -> bool {
    true
}

fn value_to_string(v: Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s,
        other => other.to_string(),
    }
}

fn de_string<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(value_to_string(Value::deserialize(d)?))
}

fn de_strings<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Array(a) => a.into_iter().map(value_to_string).collect(),
        Value::Null => Vec::new(),
        Value::String(s) if s.is_empty() => Vec::new(),
        other => vec![value_to_string(other)],
    })
}

fn de_bool<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Bool(b) => b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !matches!(s.trim().to_ascii_lowercase().as_str(), "false" | "0" | ""),
        _ => true,
    })
}

fn de_object<'de, D: Deserializer<'de>>(d: D) -> Result<Map<String, Value>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Object(m) => m,
        _ => Map::new(),
    })
}

fn de_book<'de, D: Deserializer<'de>>(d: D) -> Result<Option<CharacterBook>, D::Error> {
    Ok(match Value::deserialize(d)? {
        v @ Value::Object(_) => CharacterBook::deserialize(v).ok(),
        _ => None,
    })
}

fn de_entries<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<BookEntry>, D::Error> {
    // 有的工具把 entries 存成 { "0": {...}, "1": {...} }
    let items = match Value::deserialize(d)? {
        Value::Array(a) => a,
        Value::Object(m) => m.into_iter().map(|(_, v)| v).collect(),
        _ => Vec::new(),
    };
    Ok(items.into_iter().filter_map(|v| BookEntry::deserialize(v).ok()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CardData {
        let mut d = CardData {
            name: "测试角色".into(),
            description: "desc".into(),
            personality: "<img src=\"https://x/a.webp\">".into(),
            first_mes: "hi".into(),
            alternate_greetings: vec!["hello".into(), "  ".into()],
            tags: vec![" a ".into(), "".into()],
            character_book: Some(CharacterBook {
                entries: vec![BookEntry { keys: vec!["k".into(), "".into()], ..BookEntry::new(0) }],
                ..Default::default()
            }),
            ..Default::default()
        };
        d.extra.insert("nickname".into(), json!("nick"));
        d.extensions.insert("depth_prompt".into(), json!({ "depth": 4 }));
        d
    }

    #[test]
    fn v1_and_legacy_fields() {
        let c = parse_json(
            json!({ "name": "A", "char_persona": "p", "char_greeting": "g", "metadata": { "x": 1 } }),
            "chara",
        )
        .unwrap();
        assert_eq!(c.spec, "V1");
        assert_eq!((c.data.name.as_str(), c.data.personality.as_str()), ("A", "p"));
        assert_eq!(c.data.first_mes, "g");
        assert_eq!(c.data.extra["metadata"], json!({ "x": 1 }));
    }

    #[test]
    fn lenient_types() {
        let c = parse_json(
            json!({ "spec": "chara_card_v2", "data": {
                "name": "A", "tags": "solo", "character_version": 2, "extensions": null,
                "character_book": { "entries": { "0": { "keys": "k", "content": "c", "enabled": "false" } } }
            }}),
            "chara",
        )
        .unwrap();
        assert_eq!(c.data.tags, ["solo"]);
        assert_eq!(c.data.character_version, "2");
        let e = &c.data.character_book.unwrap().entries[0];
        assert_eq!((e.keys.as_slice(), e.enabled), (&["k".to_owned()][..], false));
    }

    #[test]
    fn png_roundtrip_keeps_everything() {
        let d = sample();
        let bytes = build_png(&png::tiny_png(), &d, false).unwrap();
        image::load_from_memory(&bytes).unwrap();

        let loaded = load_png(bytes.clone()).unwrap();
        let card = loaded.card.unwrap();
        assert_eq!(card.spec, "chara_card_v3");
        let mut expected = d.cleaned();
        expected.extra.insert("group_only_greetings".into(), json!([]));
        assert_eq!(card.data, expected);
        assert_eq!(card.data.tags, ["a"]);
        assert_eq!(card.data.alternate_greetings, ["hello"]);

        // chara 块是带顶层 V1 字段的 V2
        let png = Png::parse(&bytes).unwrap();
        let chara = png.texts().find(|t| t.keyword == "chara").unwrap();
        let v2 = decode_payload(&chara.text).unwrap();
        assert_eq!(v2["spec"], "chara_card_v2");
        assert_eq!(v2["name"], "测试角色");
        assert_eq!(v2["data"]["nickname"], "nick");
    }

    #[test]
    fn resave_replaces_old_card_chunks() {
        let first = build_png(&png::tiny_png(), &sample(), false).unwrap();
        let mut d = sample();
        d.name = "改名".into();
        let second = build_png(&first, &d, false).unwrap();
        let png = Png::parse(&second).unwrap();
        assert_eq!(png.texts().count(), 2);
        assert_eq!(load_png(second).unwrap().card.unwrap().data.name, "改名");
    }

    #[test]
    fn converts_non_png_avatar() {
        let img = image::RgbImage::from_pixel(3, 3, image::Rgb([0, 255, 0]));
        let mut jpg = std::io::Cursor::new(Vec::new());
        image::DynamicImage::from(img).write_to(&mut jpg, image::ImageFormat::Jpeg).unwrap();
        let bytes = build_png(&jpg.into_inner(), &sample(), true).unwrap();
        assert!(load_png(bytes).unwrap().card.is_some());
    }
}
