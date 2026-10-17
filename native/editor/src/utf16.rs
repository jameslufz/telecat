//! Shell offsets are UTF-16 code units. Mid-surrogate input clips to the preceding scalar.
use std::ops::Range;
pub fn to_byte(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (byte, ch) in text.char_indices() {
        if units + ch.len_utf16() > utf16 {
            return byte;
        }
        units += ch.len_utf16();
    }
    text.len()
}
pub fn from_byte(text: &str, byte: usize) -> usize {
    text.char_indices()
        .take_while(|(i, _)| *i < byte.min(text.len()))
        .map(|(_, c)| c.len_utf16())
        .sum()
}
pub fn range_to_byte(text: &str, range: Range<usize>) -> Range<usize> {
    let a = to_byte(text, range.start);
    let b = to_byte(text, range.end);
    a.min(b)..a.max(b)
}
