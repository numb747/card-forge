//! 最小化的 PNG 块（chunk）读写，只做角色卡需要的部分：
//! 列出所有块、读取文本块、在 IEND 之前插入新的文本块。

use std::io::Read;

use crate::i18n::tr;

pub const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// 解码图像时必需或影响显示效果的块，其余都视为可丢弃的附加数据。
const ESSENTIAL: &[&[u8; 4]] = &[
    b"IHDR", b"PLTE", b"IDAT", b"IEND", b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"iCCP", b"sBIT", b"pHYs", b"cICP",
    b"acTL", b"fcTL", b"fdAT",
];

#[derive(Clone, Debug)]
pub struct Chunk {
    pub kind: [u8; 4],
    pub data: Vec<u8>,
    /// 在原文件中的偏移，新建的块为 0。
    pub offset: usize,
}

#[derive(Clone, Debug)]
pub struct Png {
    pub chunks: Vec<Chunk>,
    /// IEND 之后夹带的数据。
    pub trailing: Vec<u8>,
}

/// 文本块解析结果。
pub struct TextChunk {
    pub keyword: String,
    pub text: Vec<u8>,
}

pub fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(SIGNATURE)
}

impl Chunk {
    pub fn new(kind: &[u8; 4], data: Vec<u8>) -> Self {
        Self { kind: *kind, data, offset: 0 }
    }

    pub fn kind_str(&self) -> String {
        String::from_utf8_lossy(&self.kind).into_owned()
    }

    pub fn is_essential(&self) -> bool {
        ESSENTIAL.iter().any(|k| **k == self.kind)
    }

    /// 解析 tEXt / zTXt / iTXt，压缩内容会被解压。
    pub fn text(&self) -> Option<TextChunk> {
        let (keyword, rest) = split_nul(&self.data)?;
        let keyword = latin1(keyword);
        let text = match &self.kind {
            b"tEXt" => rest.to_vec(),
            b"zTXt" => inflate(rest.get(1..)?)?,
            b"iTXt" => {
                let (&flag, rest) = rest.split_first()?;
                let rest = rest.get(1..)?; // 压缩方式
                let (_lang, rest) = split_nul(rest)?;
                let (_translated, rest) = split_nul(rest)?;
                if flag == 1 { inflate(rest)? } else { rest.to_vec() }
            }
            _ => return None,
        };
        Some(TextChunk { keyword, text })
    }
}

impl Png {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if !is_png(bytes) {
            return Err(tr!("不是 PNG 文件", "Not a PNG file").into());
        }
        let mut chunks = Vec::new();
        let mut i = SIGNATURE.len();
        loop {
            let header =
                bytes.get(i..i + 8).ok_or(tr!("PNG 数据被截断（缺少 IEND）", "Truncated PNG (missing IEND)"))?;
            let len = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
            let kind: [u8; 4] = header[4..8].try_into().unwrap();
            let end = i + 12 + len;
            let data = bytes
                .get(i + 8..i + 8 + len)
                .ok_or(tr!("PNG 块长度超出文件范围", "PNG chunk length exceeds the file"))?;
            if end > bytes.len() {
                return Err(tr!("PNG 块缺少 CRC", "PNG chunk is missing its CRC").into());
            }
            chunks.push(Chunk { kind, data: data.to_vec(), offset: i });
            i = end;
            if &kind == b"IEND" {
                break;
            }
        }
        Ok(Self { chunks, trailing: bytes[i..].to_vec() })
    }

    pub fn texts(&self) -> impl Iterator<Item = TextChunk> + '_ {
        self.chunks.iter().filter_map(Chunk::text)
    }

    /// 在 IEND 之前插入一个 tEXt 块。
    pub fn insert_text(&mut self, keyword: &str, text: &[u8]) {
        let mut data = keyword.as_bytes().to_vec();
        data.push(0);
        data.extend_from_slice(text);
        let pos = self.chunks.iter().position(|c| &c.kind == b"IEND").unwrap_or(self.chunks.len());
        self.chunks.insert(pos, Chunk::new(b"tEXt", data));
    }

    /// 删除关键字匹配（不区分大小写）的文本块。
    pub fn remove_texts(&mut self, keywords: &[&str]) {
        self.chunks.retain(|c| c.text().is_none_or(|t| !keywords.iter().any(|k| t.keyword.eq_ignore_ascii_case(k))));
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = SIGNATURE.to_vec();
        for c in &self.chunks {
            out.extend_from_slice(&(c.data.len() as u32).to_be_bytes());
            out.extend_from_slice(&c.kind);
            out.extend_from_slice(&c.data);
            let mut h = crc32fast::Hasher::new();
            h.update(&c.kind);
            h.update(&c.data);
            out.extend_from_slice(&h.finalize().to_be_bytes());
        }
        out
    }
}

fn split_nul(b: &[u8]) -> Option<(&[u8], &[u8])> {
    let i = b.iter().position(|&c| c == 0)?;
    Some((&b[..i], &b[i + 1..]))
}

fn latin1(b: &[u8]) -> String {
    b.iter().map(|&c| c as char).collect()
}

fn inflate(b: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(b).read_to_end(&mut out).ok()?;
    Some(out)
}

#[cfg(test)]
pub(crate) fn tiny_png() -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_text_chunks() {
        let mut png = Png::parse(&tiny_png()).unwrap();
        png.insert_text("chara", b"hello");
        png.insert_text("other", b"x");
        let bytes = png.encode();
        image::load_from_memory(&bytes).expect("still a valid image");

        let mut png = Png::parse(&bytes).unwrap();
        assert_eq!(png.chunks.last().unwrap().kind_str(), "IEND");
        let t: Vec<_> = png.texts().map(|t| t.keyword).collect();
        assert_eq!(t, ["chara", "other"]);

        png.remove_texts(&["CHARA"]);
        let t: Vec<_> = png.texts().map(|t| t.keyword).collect();
        assert_eq!(t, ["other"]);
    }

    #[test]
    fn trailing_data_is_reported() {
        let mut bytes = tiny_png();
        bytes.extend_from_slice(b"hidden");
        assert_eq!(Png::parse(&bytes).unwrap().trailing, b"hidden");
    }

    #[test]
    fn ztxt_and_itxt() {
        use std::io::Write;
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(b"compressed").unwrap();
        let deflated = z.finish().unwrap();

        let mut ztxt = b"k1\0\0".to_vec();
        ztxt.extend_from_slice(&deflated);
        let mut itxt = b"k2\0\x01\0en\0\0".to_vec();
        itxt.extend_from_slice(&deflated);

        let a = Chunk::new(b"zTXt", ztxt).text().unwrap();
        let b = Chunk::new(b"iTXt", itxt).text().unwrap();
        assert_eq!((a.keyword.as_str(), a.text.as_slice()), ("k1", &b"compressed"[..]));
        assert_eq!((b.keyword.as_str(), b.text.as_slice()), ("k2", &b"compressed"[..]));
    }
}
