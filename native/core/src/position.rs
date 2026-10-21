//! Where in the SQL text a server error points: line and column (1-based, in characters) of a
//! document that may hold several statements, CRLF line ends, tabs and multi-byte text.
use crate::sql::re;

/// A place in a document. `utf16` is the offset editors use for carets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    pub line: usize,
    pub column: usize,
    pub utf16: usize,
}
/// The location of character `index` (0-based, in characters) of `doc`.
pub fn at_char(doc: &str, index: usize) -> Location {
    let (mut line, mut column, mut utf16) = (1, 1, 0);
    for c in doc.chars().take(index) {
        utf16 += c.len_utf16();
        if c == '\n' {
            line += 1;
            column = 1;
        } else if c != '\r' {
            column += 1;
        }
    }
    Location {
        line,
        column,
        utf16,
    }
}
/// The character index of UTF-16 offset `utf16` in `doc`.
fn char_index(doc: &str, utf16: usize) -> usize {
    let mut units = 0;
    doc.chars()
        .take_while(|c| {
            let fits = units < utf16;
            units += c.len_utf16();
            fits
        })
        .count()
}
/// The start (in characters) of 1-based line `line` of `text`.
fn line_start(text: &str, line: usize) -> Option<usize> {
    if line <= 1 {
        return Some(0);
    }
    let mut seen = 1;
    for (i, c) in text.chars().enumerate() {
        if c == '\n' {
            seen += 1;
            if seen == line {
                return Some(i + 1);
            }
        }
    }
    None
}
/// PostgreSQL's ErrorResponse position: a 1-based character index into the statement, which
/// starts at UTF-16 offset `start` of `doc`.
pub fn postgres(doc: &str, start: usize, position: usize) -> Location {
    at_char(doc, char_index(doc, start) + position.saturating_sub(1))
}
/// MySQL's "… near '<text>' at line N": N counts lines of the statement, which starts at UTF-16
/// offset `start` of `doc`; the column is where the quoted text begins on that line.
pub fn mysql(doc: &str, start: usize, statement: &str, message: &str) -> Option<Location> {
    let m = re(r"(?s)near '(.*)' at line (\d+)\s*$").captures(message)?;
    let line: usize = m[2].parse().ok()?;
    let near = &m[1];
    let from = line_start(statement, line)?;
    let text: Vec<char> = statement.chars().collect();
    let line_end = text[from..]
        .iter()
        .position(|&c| c == '\n')
        .map_or(text.len(), |p| from + p);
    // The quoted text is the rest of the statement from the error on, cut off by the server.
    let probe: String = near.chars().take_while(|&c| c != '\n').take(40).collect();
    // The quoted text is exactly the statement's remainder at the error, so the error is where
    // the remainder starts with it (the same words may appear earlier on the line).
    let suffix_at = (from..=line_end).find(|&i| {
        let rest: String = text[i..].iter().collect();
        !near.is_empty() && rest.starts_with(near)
    });
    let column = if let Some(i) = suffix_at {
        i - from
    } else if probe.is_empty() {
        line_end - from
    } else {
        let line_text: String = text[from..line_end].iter().collect();
        // The longest start of the quoted text found on the line.
        let chars: Vec<char> = probe.trim_end_matches('\r').chars().collect();
        (1..=chars.len())
            .rev()
            .find_map(|n| line_text.find(&chars[..n].iter().collect::<String>()))
            .map_or(0, |byte| line_text[..byte].chars().count())
    };
    Some(at_char(doc, char_index(doc, start) + from + column))
}
/// An error inside a PL/pgSQL body (a DO block or a function created by the statement): the
/// server's CONTEXT gives the line within the body ("… line 4 at PERFORM"), and the internal
/// query position the spot on it. The body is the statement's first dollar-quoted string;
/// `None` when that can't be established.
pub fn plpgsql(
    doc: &str,
    start: usize,
    statement: &str,
    context: &str,
    internal: Option<(usize, &str)>,
) -> Option<Location> {
    let line: usize = re(r"PL/pgSQL function .*? line (\d+)").captures(context)?[1]
        .parse()
        .ok()?;
    let open = re(r"\$([A-Za-z_][A-Za-z0-9_]*)?\$").find(statement)?;
    let body_start = statement[..open.end()].chars().count();
    let body: Vec<char> = statement.chars().skip(body_start).collect();
    let body_text: String = body.iter().collect();
    let from = line_start(&body_text, line)?;
    let end = body[from..]
        .iter()
        .position(|&c| c == '\n')
        .map_or(body.len(), |p| from + p);
    let line_text: String = body[from..end].iter().collect();
    let column = internal
        .and_then(|(p, q)| {
            let token: String = q
                .chars()
                .skip(p.saturating_sub(1))
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
                .collect();
            (!token.is_empty())
                .then(|| {
                    line_text
                        .find(&token)
                        .map(|b| line_text[..b].chars().count())
                })
                .flatten()
        })
        .unwrap_or_else(|| line_text.chars().take_while(|c| c.is_whitespace()).count());
    Some(at_char(
        doc,
        char_index(doc, start) + body_start + from + column,
    ))
}
/// The line `location` points into, with a caret under it. Tabs are kept so the caret lines up.
pub fn caret(doc: &str, location: Location) -> String {
    let text = doc.lines().nth(location.line - 1).unwrap_or("");
    let text = text.trim_end_matches('\r');
    let pad: String = text
        .chars()
        .take(location.column - 1)
        .map(|c| if c == '\t' { '\t' } else { ' ' })
        .collect();
    format!("{text}\n{pad}^")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn postgres_positions_across_statements_crlf_and_thai() {
        let doc = "SELECT 1;\r\n-- ธนาคาร 😺\r\nSELECT ชื่อ,\r\n\tbad FROM t;";
        let start = doc
            .find("SELECT ชื่อ")
            .map(|b| doc[..b].encode_utf16().count())
            .unwrap();
        let statement = "SELECT ชื่อ,\r\n\tbad FROM t;";
        let position = statement
            .find("bad")
            .map(|b| statement[..b].chars().count() + 1)
            .unwrap();
        let at = postgres(doc, start, position);
        assert_eq!((at.line, at.column), (4, 2), "after the tab");
        let utf16 = doc
            .find("bad")
            .map(|b| doc[..b].encode_utf16().count())
            .unwrap();
        assert_eq!(at.utf16, utf16, "an emoji counts two UTF-16 units");
        assert_eq!(caret(doc, at), "\tbad FROM t;\n\t^");
        assert_eq!(
            postgres(doc, 0, 1),
            Location {
                line: 1,
                column: 1,
                utf16: 0
            }
        );
    }
    #[test]
    fn plpgsql_body_errors_map_through_the_context_line() {
        let doc = "SELECT 1;\nDO $$\nBEGIN\n  PERFORM nope_fn();\nEND $$;";
        let start = doc
            .find("DO")
            .map(|b| doc[..b].encode_utf16().count())
            .unwrap();
        let statement = "DO $$\nBEGIN\n  PERFORM nope_fn();\nEND $$";
        let at = plpgsql(
            doc,
            start,
            statement,
            "PL/pgSQL function inline_code_block line 3 at PERFORM",
            Some((8, "SELECT nope_fn()")),
        )
        .unwrap();
        assert_eq!((at.line, at.column), (4, 11));
        assert_eq!(plpgsql(doc, start, "SELECT 1", "x", None), None);
    }
    #[test]
    fn mysql_near_text_at_line_n_of_the_statement() {
        let doc = "SELECT 1;\nSELECT\n  ไทย,\n  FROM t";
        let start = doc
            .find("SELECT\n")
            .map(|b| doc[..b].encode_utf16().count())
            .unwrap();
        let statement = "SELECT\n  ไทย,\n  FROM t";
        let msg = "You have an error in your SQL syntax; check the manual that corresponds to your MySQL server version for the right syntax to use near 'FROM t' at line 3";
        let at = mysql(doc, start, statement, msg).unwrap();
        assert_eq!((at.line, at.column), (4, 3));
        // Near nothing: the end of the statement.
        let msg = "... syntax to use near '' at line 3";
        assert_eq!(
            mysql(doc, start, statement, msg).map(|l| (l.line, l.column)),
            Some((4, 9))
        );
        assert_eq!(
            mysql(doc, start, statement, "Table 't' doesn't exist"),
            None
        );
        // The near text also appears earlier on the line: the statement's remainder decides.
        let doc = "SELECT 1 FROM dual WHERE 1 = 1 FROM dual LIMIT 1000";
        let msg = "... near 'FROM dual LIMIT 1000' at line 1";
        assert_eq!(
            mysql(doc, 0, doc, msg).map(|l| (l.line, l.column)),
            Some((1, 32))
        );
    }
}
