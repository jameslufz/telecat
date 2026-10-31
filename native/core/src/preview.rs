//! What Quick Look shows for a value: JSON, an image, binary bytes or text.
use crate::columns::ValueKind;
use crate::types::Driver;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
    Gif,
    Webp,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preview {
    Json,
    Image(ImageKind, Vec<u8>),
    Binary(Vec<u8>),
    Text,
}
/// The bytes of a binary value as the grid shows it: MySQL `0x…` hex, PostgreSQL `\x…` hex.
/// MySQL shows printable binary values as text, so those are their own UTF-8 bytes.
pub fn binary_bytes(d: Driver, shown: &str) -> Vec<u8> {
    let prefix = if d == Driver::Mysql { "0x" } else { "\\x" };
    shown
        .strip_prefix(prefix)
        .filter(|h| h.len() % 2 == 0 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .and_then(|h| {
            (0..h.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&h[i..i + 2], 16).ok())
                .collect()
        })
        .unwrap_or_else(|| shown.as_bytes().to_vec())
}
pub fn image_kind(bytes: &[u8]) -> Option<ImageKind> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageKind::Png)
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(ImageKind::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(ImageKind::Gif)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(ImageKind::Webp)
    } else {
        None
    }
}
/// How Quick Look shows `value` of a column of `kind` (`binary` as the result reported it).
pub fn detect(d: Driver, value: &str, kind: ValueKind, binary: bool) -> Preview {
    if binary || kind == ValueKind::Binary {
        let bytes = binary_bytes(d, value);
        return match image_kind(&bytes) {
            Some(image) => Preview::Image(image, bytes),
            None => Preview::Binary(bytes),
        };
    }
    let trimmed = value.trim_start();
    if kind == ValueKind::Json
        || (trimmed.starts_with(['{', '['])
            && serde_json::from_str::<serde_json::Value>(value).is_ok())
    {
        Preview::Json
    } else {
        Preview::Text
    }
}
/// Bytes as a hex dump: offset, 16 bytes in hex, then the printable ASCII. At most `max_bytes`
/// are listed; a final line says how many more there are.
pub fn hex_dump(bytes: &[u8], max_bytes: usize) -> String {
    let mut out = String::new();
    for (line, chunk) in bytes[..bytes.len().min(max_bytes)].chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02X}")).collect();
        let ascii: String = chunk
            .iter()
            .map(|&b| {
                if (0x20..0x7f).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        out += &format!("{:08X}  {:<47}  {ascii}\n", line * 16, hex.join(" "));
    }
    if bytes.len() > max_bytes {
        out += &format!("… {} more bytes\n", bytes.len() - max_bytes);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binary_values_decode_per_driver() {
        assert_eq!(binary_bytes(Driver::Mysql, "0x00FF41"), [0, 255, 65]);
        assert_eq!(binary_bytes(Driver::Postgres, "\\x00ff41"), [0, 255, 65]);
        assert_eq!(binary_bytes(Driver::Mysql, "hello"), b"hello");
        assert_eq!(binary_bytes(Driver::Mysql, "0xF"), b"0xF");
        assert_eq!(binary_bytes(Driver::Postgres, "0x00"), b"0x00");
    }
    #[test]
    fn previews_tell_images_binary_json_and_text_apart() {
        let png = "0x89504E470D0A1A0A0000";
        assert!(matches!(
            detect(Driver::Mysql, png, ValueKind::Binary, true),
            Preview::Image(ImageKind::Png, _)
        ));
        assert!(matches!(
            detect(Driver::Postgres, "\\xffd8ffe0", ValueKind::Binary, true),
            Preview::Image(ImageKind::Jpeg, _)
        ));
        assert_eq!(image_kind(b"GIF89a.."), Some(ImageKind::Gif));
        assert_eq!(image_kind(b"RIFF\0\0\0\0WEBPVP8 "), Some(ImageKind::Webp));
        assert_eq!(image_kind(b"RIFF\0\0\0\0WAVE"), None);
        assert_eq!(
            detect(Driver::Mysql, "0x0001", ValueKind::Binary, true),
            Preview::Binary(vec![0, 1])
        );
        assert_eq!(
            detect(Driver::Mysql, "{\"a\":1}", ValueKind::Other, false),
            Preview::Json
        );
        assert_eq!(
            detect(Driver::Mysql, "{not json", ValueKind::Other, false),
            Preview::Text
        );
        assert_eq!(
            detect(Driver::Postgres, "oops", ValueKind::Json, false),
            Preview::Json
        );
        assert_eq!(
            detect(Driver::Postgres, "plain", ValueKind::Other, false),
            Preview::Text
        );
    }
    #[test]
    fn hex_dump_lines_and_limit() {
        let dump = hex_dump(b"ABCDEFGHIJKLMNOPQ\x00", 64);
        let lines: Vec<&str> = dump.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("00000000  41 42 43"));
        assert!(lines[0].ends_with("ABCDEFGHIJKLMNOP"));
        assert!(lines[1].starts_with("00000010  51 00"));
        assert!(lines[1].ends_with("Q."));
        assert!(hex_dump(&[0; 40], 16).ends_with("… 24 more bytes\n"));
        assert_eq!(hex_dump(&[], 16), "");
    }
}
