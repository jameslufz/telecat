use serde_json::{Value, json};
use telecat_core::changes::js_number;

/// mysql2's text protocol FLOAT/DOUBLE parser, including its short-decimal arithmetic.
pub(crate) fn float_cell(s: &str) -> String {
    let n = if s.len() > 17 || s.contains(['e', 'E']) {
        s.parse().unwrap_or(f64::NAN)
    } else {
        let mut result = 0.0;
        let mut factor = if s.starts_with('-') { -1.0 } else { 1.0 };
        let mut dot = false;
        for b in s.trim_start_matches(['-', '+']).bytes() {
            if b == b'.' {
                dot = true;
            } else {
                result = result * 10.0 + (f64::from(b) - 48.0);
                if dot {
                    factor *= 10.0;
                }
            }
        }
        result / factor
    };
    js_number(n)
}

fn index_key(s: &str) -> Option<u32> {
    s.parse::<u32>()
        .ok()
        .filter(|n| *n != u32::MAX && n.to_string() == s)
}

/// JSON.stringify ordering and number spelling. mysql2 preserves unsafe integer
/// JSON tokens as strings when supportBigNumbers is enabled (Node 22+/Electron).
fn stringify(v: &Value, preserve_big: bool) -> String {
    match v {
        Value::Number(n) => {
            let source = n.to_string();
            let f = n.as_f64().unwrap_or(f64::INFINITY);
            if preserve_big
                && f.abs() > 9_007_199_254_740_991.0
                && !source.contains(['.', 'e', 'E'])
            {
                json!(source).to_string()
            } else if f.is_finite() {
                js_number(f)
            } else {
                "null".into()
            }
        }
        Value::Array(a) => format!(
            "[{}]",
            a.iter()
                .map(|v| stringify(v, preserve_big))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(o) => {
            let mut keys: Vec<_> = o.keys().collect();
            keys.sort_by_key(|k| index_key(k).map_or((1, 0), |i| (0, i)));
            format!(
                "{{{}}}",
                keys.iter()
                    .map(|k| format!("{}:{}", json!(k), stringify(&o[*k], preserve_big)))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        _ => v.to_string(),
    }
}

pub(crate) fn json_cell(bytes: &[u8]) -> Option<String> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Null) => None,
        Ok(Value::String(s)) => Some(s),
        Ok(Value::Number(n)) => {
            let source = n.to_string();
            let f = n.as_f64().unwrap_or(f64::INFINITY);
            Some(
                if f.abs() > 9_007_199_254_740_991.0 && !source.contains(['.', 'e', 'E']) {
                    source
                } else {
                    js_number(f)
                },
            )
        }
        Ok(v) => Some(stringify(&v, true)),
        Err(_) => Some(String::from_utf8_lossy(bytes).into_owned()),
    }
}

/// MySQL's SRID-prefixed WKB to mysql2's {x,y}/nested-array representation.
pub(crate) fn geometry_cell(bytes: &[u8]) -> Option<String> {
    struct Reader<'a> {
        data: &'a [u8],
        offset: usize,
    }
    impl Reader<'_> {
        fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
            let out = self
                .data
                .get(self.offset..self.offset.checked_add(N)?)?
                .try_into()
                .ok()?;
            self.offset += N;
            Some(out)
        }
        fn count(&mut self, little: bool) -> Option<usize> {
            let b = self.take()?;
            Some(if little {
                u32::from_le_bytes(b)
            } else {
                u32::from_be_bytes(b)
            } as usize)
        }
        fn point(&mut self, little: bool) -> Option<Value> {
            let x = self.take()?;
            let y = self.take()?;
            Some(if little {
                json!({"x":f64::from_le_bytes(x),"y":f64::from_le_bytes(y)})
            } else {
                json!({"x":f64::from_be_bytes(x),"y":f64::from_be_bytes(y)})
            })
        }
        fn line(&mut self, little: bool) -> Option<Value> {
            let n = self.count(little)?;
            if n > self.data.len().saturating_sub(self.offset) / 16 {
                return None;
            }
            (0..n)
                .map(|_| self.point(little))
                .collect::<Option<Vec<_>>>()
                .map(Value::Array)
        }
        fn geometry(&mut self, depth: usize) -> Option<Value> {
            if depth > 128 {
                return None;
            }
            let little = self.take::<1>()?[0] != 0;
            let kind = self.count(little)?;
            match kind {
                1 => self.point(little),
                2 => self.line(little),
                3..=7 => {
                    let n = self.count(little)?;
                    if n > self.data.len().saturating_sub(self.offset)
                        / if kind == 3 { 4 } else { 9 }
                    {
                        return None;
                    }
                    (0..n)
                        .map(|_| {
                            if kind == 3 {
                                self.line(little)
                            } else {
                                self.geometry(depth + 1)
                            }
                        })
                        .collect::<Option<Vec<_>>>()
                        .map(Value::Array)
                }
                _ => None,
            }
        }
    }
    let v = Reader {
        data: bytes,
        offset: 4,
    }
    .geometry(0)?;
    Some(stringify(&v, false))
}
