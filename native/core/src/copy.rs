//! Copy Rows As / Copy Selected Cells As: rows written as text in TablePlus's formats. CSV, JSON
//! and INSERT reuse the export writers, so a copy reads exactly like an export of those rows.
use crate::export::{ExportFormat, ExportWriter};
use crate::types::Driver;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyFormat {
    PlainText,
    Json,
    Html,
    Markdown,
    Csv,
    CsvWithHeader,
    Insert,
}
impl CopyFormat {
    pub const ALL: [CopyFormat; 7] = [
        Self::PlainText,
        Self::Json,
        Self::Html,
        Self::Markdown,
        Self::Csv,
        Self::CsvWithHeader,
        Self::Insert,
    ];
    /// The menu id, stable across releases.
    pub fn id(self) -> &'static str {
        match self {
            Self::PlainText => "plain",
            Self::Json => "json",
            Self::Html => "html",
            Self::Markdown => "markdown",
            Self::Csv => "csv",
            Self::CsvWithHeader => "csv-header",
            Self::Insert => "insert",
        }
    }
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.id() == id)
    }
    /// TablePlus's label: a query result's menu says "CSV include fields name" and
    /// "SQL Insert Statement" where a table's says "CSV with Header" and "INSERT Statement".
    pub fn label(self, query_result: bool) -> &'static str {
        match (self, query_result) {
            (Self::PlainText, _) => "Plain Text",
            (Self::Json, _) => "JSON",
            (Self::Html, _) => "HTML",
            (Self::Markdown, _) => "Markdown Table",
            (Self::Csv, _) => "CSV",
            (Self::CsvWithHeader, false) => "CSV with Header",
            (Self::CsvWithHeader, true) => "CSV include fields name",
            (Self::Insert, false) => "INSERT Statement",
            (Self::Insert, true) => "SQL Insert Statement",
        }
    }
    /// Where TablePlus puts separators in the submenu: before these.
    pub fn separated(self) -> bool {
        matches!(self, Self::Json | Self::Markdown | Self::Csv | Self::Insert)
    }
}
/// Rows to copy: the chosen columns (in the order shown) and their values.
pub struct CopyRows<'a> {
    pub driver: Driver,
    /// The quoted table name INSERT statements use (see `export::insert_target`).
    pub target: String,
    pub columns: &'a [String],
    pub binary: &'a [bool],
    pub rows: &'a [Vec<Option<String>>],
}
/// `rows` as `format`. NULL is written as each format writes it: `NULL` in plain text, HTML and
/// Markdown, an empty field in CSV, `null` in JSON and `NULL` in SQL.
pub fn copy_text(format: CopyFormat, rows: &CopyRows) -> String {
    let writer = |f| {
        ExportWriter::new(
            f,
            rows.driver,
            rows.target.clone(),
            rows.columns.to_vec(),
            rows.binary.to_vec(),
        )
    };
    let shown = |v: &Option<String>| v.clone().unwrap_or_else(|| "NULL".into());
    match format {
        // As ⌘C writes it, so it pastes back cell for cell.
        CopyFormat::PlainText => tsv_block(
            &rows
                .rows
                .iter()
                .map(|r| r.iter().map(shown).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
        ),
        CopyFormat::Csv | CopyFormat::CsvWithHeader => {
            let mut w = writer(ExportFormat::Csv);
            let header = if format == CopyFormat::CsvWithHeader {
                // A clipboard has no use for the file's byte order mark.
                w.header().trim_start_matches('\u{feff}').to_owned()
            } else {
                String::new()
            };
            header + &w.rows(rows.rows)
        }
        CopyFormat::Json => {
            let mut w = writer(ExportFormat::Json);
            let mut out = w.header();
            out += &w.rows(rows.rows);
            out + &w.footer()
        }
        CopyFormat::Insert => writer(ExportFormat::Sql).rows(rows.rows),
        CopyFormat::Html => {
            let mut out = String::from("<table>\n<tr>");
            for c in rows.columns {
                out += &format!("<th>{}</th>", html_escape(c));
            }
            out += "</tr>\n";
            for r in rows.rows {
                out += "<tr>";
                for v in r {
                    out += &format!("<td>{}</td>", html_escape(&shown(v)));
                }
                out += "</tr>\n";
            }
            out + "</table>\n"
        }
        CopyFormat::Markdown => {
            let line = |cells: Vec<String>| format!("| {} |\n", cells.join(" | "));
            let mut out = line(rows.columns.iter().map(|c| markdown_escape(c)).collect());
            out += &line(rows.columns.iter().map(|_| "---".to_owned()).collect());
            for r in rows.rows {
                out += &line(r.iter().map(|v| markdown_escape(&shown(v))).collect());
            }
            out
        }
    }
}
/// One value of tab-separated clipboard text. A value with a tab, a line break, or a leading
/// quote is quoted (`"…"`, quotes doubled), so [`parse_tsv`] reads it back as one value; any
/// other value is written as it is, so text pastes into other apps unchanged.
pub fn tsv_field(value: &str) -> std::borrow::Cow<'_, str> {
    if value.contains(['\t', '\n', '\r']) || value.starts_with('"') {
        format!("\"{}\"", value.replace('"', "\"\"")).into()
    } else {
        value.into()
    }
}
/// Rows as tab-separated clipboard text that [`parse_tsv`] reads back exactly: every value
/// through [`tsv_field`], and in a one-column block an empty value on the last row written as
/// `""` (a bare one would vanish with the final line break). Nothing else is quoted for it.
pub fn tsv_block<S: AsRef<str>>(rows: &[Vec<S>]) -> String {
    let last = rows.len().saturating_sub(1);
    rows.iter()
        .enumerate()
        .map(|(i, row)| {
            if i == last && row.len() == 1 && row[0].as_ref().is_empty() {
                return "\"\"".to_owned();
            }
            row.iter()
                .map(|v| tsv_field(v.as_ref()).into_owned())
                .collect::<Vec<_>>()
                .join("\t")
        })
        .collect::<Vec<_>>()
        .join("\n")
}
/// Rows of tab-separated values, as [`tsv_field`] writes them and as spreadsheets copy them: a
/// field starting with `"` and closed by `"` before a tab, a line break or the end is one value
/// (`""` inside is a quote); anything else is plain text up to the next tab or line break, so
/// text copied from other apps without quotes pastes as it reads. A final line break ends the
/// last row rather than starting an empty one.
pub fn parse_tsv(text: &str) -> Vec<Vec<String>> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut rows = vec![];
    let mut row: Vec<String> = vec![];
    let mut i = 0;
    loop {
        let (field, next) = quoted_field(&chars, i).unwrap_or_else(|| plain_field(&chars, i));
        row.push(field);
        i = next;
        match chars.get(i) {
            Some('\t') => i += 1,
            Some('\r') if chars.get(i + 1) == Some(&'\n') => {
                i += 2;
                rows.push(std::mem::take(&mut row));
                if i == n {
                    break;
                }
            }
            Some('\n') => {
                i += 1;
                rows.push(std::mem::take(&mut row));
                if i == n {
                    break;
                }
            }
            _ => {
                rows.push(std::mem::take(&mut row));
                break;
            }
        }
    }
    rows
}
/// A quoted field at `start`: its value and where it ends, if it is closed properly.
fn quoted_field(chars: &[char], start: usize) -> Option<(String, usize)> {
    if chars.get(start) != Some(&'"') {
        return None;
    }
    let mut value = String::new();
    let mut i = start + 1;
    while i < chars.len() {
        if chars[i] == '"' {
            if chars.get(i + 1) == Some(&'"') {
                value.push('"');
                i += 2;
                continue;
            }
            let end = i + 1;
            let closes = match chars.get(end) {
                None | Some('\t') | Some('\n') => true,
                Some('\r') => chars.get(end + 1) == Some(&'\n'),
                _ => false,
            };
            return closes.then_some((value, end));
        }
        value.push(chars[i]);
        i += 1;
    }
    None
}
/// A plain field at `start`, up to the next tab or line break. A lone carriage return is
/// dropped, as before.
fn plain_field(chars: &[char], start: usize) -> (String, usize) {
    let mut value = String::new();
    let mut i = start;
    while i < chars.len() {
        match chars[i] {
            '\t' | '\n' => break,
            '\r' if chars.get(i + 1) == Some(&'\n') => break,
            '\r' => {}
            c => value.push(c),
        }
        i += 1;
    }
    (value, i)
}
/// Copy All Column Values: every value of one column, one per line.
pub fn column_values(values: &[Option<String>]) -> String {
    values
        .iter()
        .map(|v| v.as_deref().unwrap_or("NULL"))
        .collect::<Vec<_>>()
        .join("\n")
}
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out += "&amp;",
            '<' => out += "&lt;",
            '>' => out += "&gt;",
            '"' => out += "&quot;",
            '\'' => out += "&#39;",
            c => out.push(c),
        }
    }
    out
}
fn markdown_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace("\r\n", "<br>")
        .replace(['\n', '\r'], "<br>")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(d: Driver, format: CopyFormat) -> String {
        let columns = vec!["id".to_owned(), "note".to_owned()];
        let rows = vec![
            vec![
                Some("1".to_owned()),
                Some("a|b <i>\"x\"</i>, it's".to_owned()),
            ],
            vec![Some("2".to_owned()), None],
        ];
        copy_text(
            format,
            &CopyRows {
                driver: d,
                target: crate::changes::quote_ident(d, "t"),
                columns: &columns,
                binary: &[false, false],
                rows: &rows,
            },
        )
    }
    #[test]
    fn every_format_writes_nulls_and_escapes_its_own_way() {
        let my = |f| sample(Driver::Mysql, f);
        assert_eq!(
            my(CopyFormat::PlainText),
            "1\ta|b <i>\"x\"</i>, it's\n2\tNULL"
        );
        assert_eq!(
            my(CopyFormat::Csv),
            "1,\"a|b <i>\"\"x\"\"</i>, it's\"\r\n2,\r\n"
        );
        assert_eq!(
            my(CopyFormat::CsvWithHeader),
            "id,note\r\n1,\"a|b <i>\"\"x\"\"</i>, it's\"\r\n2,\r\n"
        );
        let json: serde_json::Value = serde_json::from_str(&my(CopyFormat::Json)).unwrap();
        assert_eq!(json[1]["note"], serde_json::Value::Null);
        assert_eq!(json[0]["id"], "1");
        assert_eq!(
            my(CopyFormat::Insert),
            "INSERT INTO `t` (`id`, `note`) VALUES ('1', 'a|b <i>\"x\"</i>, it''s');\nINSERT INTO `t` (`id`, `note`) VALUES ('2', NULL);\n"
        );
        assert!(
            sample(Driver::Postgres, CopyFormat::Insert)
                .starts_with("INSERT INTO \"t\" (\"id\", \"note\")")
        );
        assert_eq!(
            my(CopyFormat::Html),
            "<table>\n<tr><th>id</th><th>note</th></tr>\n<tr><td>1</td><td>a|b &lt;i&gt;&quot;x&quot;&lt;/i&gt;, it&#39;s</td></tr>\n<tr><td>2</td><td>NULL</td></tr>\n</table>\n"
        );
        assert_eq!(
            my(CopyFormat::Markdown),
            "| id | note |\n| --- | --- |\n| 1 | a\\|b <i>\"x\"</i>, it's |\n| 2 | NULL |\n"
        );
    }
    #[test]
    fn copies_match_the_export_writers() {
        let columns = vec!["k".to_owned()];
        let rows = vec![vec![Some("ไทย 😺\nline".to_owned())]];
        let copy = copy_text(
            CopyFormat::Csv,
            &CopyRows {
                driver: Driver::Postgres,
                target: "\"t\"".into(),
                columns: &columns,
                binary: &[false],
                rows: &rows,
            },
        );
        let mut w = ExportWriter::new(
            ExportFormat::Csv,
            Driver::Postgres,
            "\"t\"".into(),
            columns.clone(),
            vec![false],
        );
        assert_eq!(copy, w.rows(&rows));
        assert!(
            !copy_text(
                CopyFormat::CsvWithHeader,
                &CopyRows {
                    driver: Driver::Postgres,
                    target: "\"t\"".into(),
                    columns: &columns,
                    binary: &[false],
                    rows: &rows,
                }
            )
            .contains('\u{feff}')
        );
    }
    #[test]
    fn clipboard_values_round_trip_through_quoting() {
        let values = [
            "plain",
            "line1\nline2",
            "x\ty",
            "\"quoted\" at start",
            "in \"the\" middle",
            "crlf\r\nkept",
            "",
            "ไทย 😺",
            "\"",
        ];
        let line = values
            .iter()
            .map(|v| tsv_field(v).into_owned())
            .collect::<Vec<_>>()
            .join("\t");
        let text = format!("{line}\n{line}\n");
        let parsed = parse_tsv(&text);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], values);
        assert_eq!(parsed[1], values);
        // Only values that need it are quoted.
        assert_eq!(tsv_field("in \"the\" middle"), "in \"the\" middle");
        assert_eq!(tsv_field("a\nb"), "\"a\nb\"");
    }
    #[test]
    fn empty_values_survive_only_where_they_would_vanish() {
        // One column ending in an empty value: written "" so it reads back (QA r1 probe).
        let block = tsv_block(&[vec!["a"], vec![""]]);
        assert_eq!(block, "a\n\"\"");
        assert_eq!(parse_tsv(&block), vec![vec!["a"], vec![""]]);
        // A single empty cell copies as "" and pastes EMPTY.
        assert_eq!(parse_tsv(&tsv_block(&[vec![""]])), vec![vec![""]]);
        // Elsewhere an empty value needs nothing.
        assert_eq!(tsv_block(&[vec!["", "x"], vec!["y", ""]]), "\tx\ny\t");
        assert_eq!(parse_tsv("\tx\ny\t"), vec![vec!["", "x"], vec!["y", ""]]);
        assert_eq!(tsv_block(&[vec![""], vec!["a"]]), "\na");
        assert_eq!(parse_tsv("\na"), vec![vec![""], vec!["a"]]);
    }
    #[test]
    fn unquoted_text_from_other_apps_pastes_as_it_reads() {
        assert_eq!(
            parse_tsv("a\tb\nc\td"),
            vec![vec!["a", "b"], vec!["c", "d"]]
        );
        assert_eq!(
            parse_tsv("a\r\nb\r\t\t\n\n"),
            vec![vec!["a"], vec!["b", "", ""], vec![""]]
        );
        // A quote that doesn't close a field is just a character.
        assert_eq!(parse_tsv("\"half\tb"), vec![vec!["\"half", "b"]]);
        assert_eq!(parse_tsv("\"ab\"c\td"), vec![vec!["\"ab\"c", "d"]]);
        assert_eq!(parse_tsv("6\" pipe"), vec![vec!["6\" pipe"]]);
        assert_eq!(parse_tsv(""), vec![vec![""]]);
        assert_eq!(parse_tsv("x\n"), vec![vec!["x"]]);
        // A spreadsheet's quoted multi-line cell.
        assert_eq!(parse_tsv("\"a\r\nb\"\tc\r\n"), vec![vec!["a\r\nb", "c"]]);
    }
    #[test]
    fn markdown_keeps_one_line_per_row_and_labels_follow_the_grid() {
        assert_eq!(markdown_escape("a\r\nb\nc\\"), "a<br>b<br>c\\\\");
        assert_eq!(
            column_values(&[Some("a".into()), None, Some(String::new())]),
            "a\nNULL\n"
        );
        assert_eq!(CopyFormat::CsvWithHeader.label(false), "CSV with Header");
        assert_eq!(
            CopyFormat::CsvWithHeader.label(true),
            "CSV include fields name"
        );
        assert_eq!(CopyFormat::Insert.label(true), "SQL Insert Statement");
        for f in CopyFormat::ALL {
            assert_eq!(CopyFormat::from_id(f.id()), Some(f));
        }
        assert_eq!(CopyFormat::from_id("nope"), None);
    }
}
