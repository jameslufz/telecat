//! CSV / JSON / SQL INSERT export, written in pieces so a whole table can be streamed to a file
//! without holding it in memory.
use crate::{
    changes::{literal, quote_ident},
    types::*,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
    Sql,
}
impl ExportFormat {
    /// The format of a file name's extension; CSV unless it is `json` or `sql`.
    pub fn from_extension(extension: &str) -> Self {
        match extension.to_ascii_lowercase().as_str() {
            "json" => Self::Json,
            "sql" => Self::Sql,
            _ => Self::Csv,
        }
    }
}
/// The table an SQL export inserts into: the bare name, so the file can be loaded into another
/// database, but schema-qualified for a PostgreSQL schema other than `public`.
pub fn insert_target(d: Driver, table: &TableRef) -> String {
    if d == Driver::Postgres && !table.schema.is_empty() && table.schema != "public" {
        format!(
            "{}.{}",
            quote_ident(d, &table.schema),
            quote_ident(d, &table.name)
        )
    } else {
        quote_ident(d, &table.name)
    }
}
pub struct ExportWriter {
    format: ExportFormat,
    driver: Driver,
    target: String,
    columns: Vec<String>,
    binary: Vec<bool>,
    rows: u64,
}
impl ExportWriter {
    /// `target` is the already-quoted table name SQL INSERTs use (see [`insert_target`]).
    pub fn new(
        format: ExportFormat,
        driver: Driver,
        target: String,
        columns: Vec<String>,
        binary: Vec<bool>,
    ) -> Self {
        Self {
            format,
            driver,
            target,
            columns,
            binary,
            rows: 0,
        }
    }
    pub fn rows_written(&self) -> u64 {
        self.rows
    }
    pub fn header(&self) -> String {
        match self.format {
            ExportFormat::Csv => format!(
                "\u{feff}{}\r\n",
                self.columns
                    .iter()
                    .map(|c| csv_field(c))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            ExportFormat::Json => "[".into(),
            ExportFormat::Sql => String::new(),
        }
    }
    pub fn rows(&mut self, rows: &[Vec<Option<String>>]) -> String {
        let mut out = String::new();
        for row in rows {
            match self.format {
                ExportFormat::Csv => {
                    out += &row
                        .iter()
                        .map(|v| csv_field(v.as_deref().unwrap_or("")))
                        .collect::<Vec<_>>()
                        .join(",");
                    out += "\r\n";
                }
                ExportFormat::Json => {
                    let object: serde_json::Map<_, _> = self
                        .columns
                        .iter()
                        .enumerate()
                        .map(|(i, c)| {
                            (
                                c.clone(),
                                row.get(i)
                                    .cloned()
                                    .flatten()
                                    .map_or(serde_json::Value::Null, serde_json::Value::String),
                            )
                        })
                        .collect();
                    let pretty = serde_json::to_string_pretty(&object).unwrap_or_default();
                    out += if self.rows == 0 { "\n" } else { ",\n" };
                    out += &pretty
                        .lines()
                        .map(|l| format!("  {l}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                }
                ExportFormat::Sql => {
                    out += &format!(
                        "INSERT INTO {} ({}) VALUES ({});\n",
                        self.target,
                        self.columns
                            .iter()
                            .map(|c| quote_ident(self.driver, c))
                            .collect::<Vec<_>>()
                            .join(", "),
                        row.iter()
                            .enumerate()
                            .map(|(i, v)| self.sql_value(i, v.as_deref()))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
            }
            self.rows += 1;
        }
        out
    }
    pub fn footer(&self) -> String {
        match self.format {
            ExportFormat::Json if self.rows == 0 => "]\n".into(),
            ExportFormat::Json => "\n]\n".into(),
            _ => String::new(),
        }
    }
    fn sql_value(&self, column: usize, value: Option<&str>) -> String {
        let Some(v) = value else {
            return "NULL".into();
        };
        // MySQL shows binary values that aren't text as 0x… hex, which is also its hex literal.
        let hex =
            v.len() > 2 && v.starts_with("0x") && v[2..].bytes().all(|b| b.is_ascii_hexdigit());
        if self.driver == Driver::Mysql && self.binary.get(column) == Some(&true) && hex {
            v.to_owned()
        } else {
            literal(self.driver, v)
        }
    }
}
fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn write(format: ExportFormat, d: Driver, chunks: &[&[Vec<Option<String>>]]) -> String {
        let mut w = ExportWriter::new(
            format,
            d,
            insert_target(
                d,
                &TableRef {
                    schema: "sales".into(),
                    name: "order items".into(),
                },
            ),
            vec!["id".into(), "note".into(), "blob".into()],
            vec![false, false, true],
        );
        let mut out = w.header();
        for c in chunks {
            out += &w.rows(c);
        }
        out + &w.footer()
    }
    fn row(id: &str, note: Option<&str>, blob: Option<&str>) -> Vec<Option<String>> {
        vec![Some(id.into()), note.map(Into::into), blob.map(Into::into)]
    }
    #[test]
    fn chunks_join_into_one_valid_file() {
        let a = [row("1", Some("a,\"b\"\nc"), None)];
        let b = [row("2", None, Some("0xFF00"))];
        assert_eq!(
            write(ExportFormat::Csv, Driver::Mysql, &[&a, &b]),
            "\u{feff}id,note,blob\r\n1,\"a,\"\"b\"\"\nc\",\r\n2,,0xFF00\r\n"
        );
        let json = write(ExportFormat::Json, Driver::Mysql, &[&a, &[], &b]);
        let all = [a[0].clone(), b[0].clone()];
        let whole: Vec<serde_json::Value> = all
            .iter()
            .map(|r| serde_json::json!({"id": r[0], "note": r[1], "blob": r[2]}))
            .collect();
        assert_eq!(json, serde_json::to_string_pretty(&whole).unwrap() + "\n");
        assert_eq!(write(ExportFormat::Json, Driver::Mysql, &[]), "[]\n");
        assert!(serde_json::from_str::<serde_json::Value>(&json).is_ok());
    }
    #[test]
    fn sql_inserts_quote_identifiers_and_escape_per_driver() {
        let rows = [row("1", Some("it's a \\ path"), Some("0xFF00"))];
        assert_eq!(
            write(ExportFormat::Sql, Driver::Mysql, &[&rows]),
            "INSERT INTO `order items` (`id`, `note`, `blob`) VALUES ('1', 'it''s a \\\\ path', 0xFF00);\n"
        );
        assert_eq!(
            write(ExportFormat::Sql, Driver::Postgres, &[&rows]),
            "INSERT INTO \"sales\".\"order items\" (\"id\", \"note\", \"blob\") VALUES ('1', 'it''s a \\ path', '0xFF00');\n"
        );
        // A text column that merely looks like hex stays a string.
        let text = [row("1", Some("0xFF"), Some("0xZZ"))];
        assert!(write(ExportFormat::Sql, Driver::Mysql, &[&text]).contains("'0xFF', '0xZZ'"));
        assert_eq!(
            insert_target(
                Driver::Postgres,
                &TableRef {
                    schema: "public".into(),
                    name: "t\"x".into()
                }
            ),
            "\"t\"\"x\""
        );
    }
    #[test]
    fn format_follows_the_extension() {
        assert_eq!(ExportFormat::from_extension("JSON"), ExportFormat::Json);
        assert_eq!(ExportFormat::from_extension("sql"), ExportFormat::Sql);
        assert_eq!(ExportFormat::from_extension("csv"), ExportFormat::Csv);
        assert_eq!(ExportFormat::from_extension(""), ExportFormat::Csv);
    }
}
