use regex::{Regex, RegexBuilder};
use std::ops::Range;
#[derive(Clone, Default, Debug)]
pub struct SearchQuery {
    pub query: String,
    pub replacement: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
}
impl SearchQuery {
    pub fn compile(&self) -> Result<Option<Regex>, regex::Error> {
        if self.query.is_empty() {
            return Ok(None);
        }
        let mut pattern = if self.regex {
            self.query.clone()
        } else {
            regex::escape(&unquote(&self.query))
        };
        if self.whole_word {
            pattern = format!(r"\b(?:{pattern})\b");
        }
        RegexBuilder::new(&pattern)
            .case_insensitive(!self.case_sensitive)
            .multi_line(true)
            .build()
            .map(Some)
    }
    pub fn matches(&self, text: &str) -> Result<Vec<Range<usize>>, regex::Error> {
        Ok(self
            .compile()?
            .map_or_else(Vec::new, |r| r.find_iter(text).map(|m| m.range()).collect()))
    }
    pub fn edits(&self, text: &str) -> Result<Vec<(Range<usize>, String)>, regex::Error> {
        Ok(self.compile()?.map_or_else(Vec::new, |r| {
            r.captures_iter(text)
                .map(|c| {
                    let mut replacement = String::new();
                    if self.regex {
                        replacement = expand_replacement(&unquote(&self.replacement), &c);
                    } else {
                        replacement.push_str(&unquote(&self.replacement));
                    }
                    (c.get(0).unwrap().range(), replacement)
                })
                .collect()
        }))
    }
}

// CodeMirror/JavaScript replacement syntax: $$, $&, $1..$99; `$2_` means
// capture 2 followed by an underscore, unlike Rust regex's named captures.
fn expand_replacement(value: &str, captures: &regex::Captures<'_>) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < value.len() {
        let s = &value[i..];
        if s.starts_with("$$") {
            out.push('$');
            i += 2;
        } else if s.starts_with("$&") {
            out.push_str(captures.get(0).unwrap().as_str());
            i += 2;
        } else if s.starts_with('$') && s.as_bytes().get(1).is_some_and(u8::is_ascii_digit) {
            let n = s[1..].bytes().take_while(u8::is_ascii_digit).count();
            let mut found = false;
            for len in (1..=n).rev() {
                if let Ok(index) = s[1..1 + len].parse::<usize>() {
                    if index > 0 && index < captures.len() {
                        out.push_str(captures.get(index).map_or("", |m| m.as_str()));
                        i += 1 + len;
                        found = true;
                        break;
                    }
                }
            }
            if !found {
                out.push('$');
                i += 1;
            }
        } else {
            let c = s.chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

fn unquote(value: &str) -> String {
    let mut result = String::new();
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&n) = chars.peek() {
                let mapped = match n {
                    'n' => Some('\n'),
                    'r' => Some('\r'),
                    't' => Some('\t'),
                    '\\' => Some('\\'),
                    _ => None,
                };
                if let Some(mapped) = mapped {
                    chars.next();
                    result.push(mapped);
                    continue;
                }
            }
        }
        result.push(c);
    }
    result
}
