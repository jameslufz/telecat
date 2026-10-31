//! What the Data view knows about each column beyond its name: the kind of value it holds, the
//! values an enum or SET allows, whether the server computes it, and the foreign keys it is
//! part of. Read by `Session::column_details`.
use crate::filters::ForeignKey;
use crate::types::Driver;
use serde::{Deserialize, Serialize};

/// The kind of value a column holds, as far as the Data view's menus and editors care.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValueKind {
    #[default]
    Other,
    Boolean,
    Date,
    Time,
    DateTime,
    Json,
    /// One value of a list (MySQL ENUM, PostgreSQL enum types).
    Enum,
    /// MySQL SET: any number of the listed values, comma separated.
    Set,
    Binary,
}
impl ValueKind {
    pub fn is_temporal(self) -> bool {
        matches!(self, Self::Date | Self::Time | Self::DateTime)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnDetail {
    pub name: String,
    /// As the catalog writes it: `enum('a','b')`, `datetime(3)`, `timestamp with time zone`…
    pub data_type: String,
    pub kind: ValueKind,
    /// The values an Enum or Set column allows, in their declared order.
    pub values: Vec<String>,
    pub nullable: bool,
    /// Computed by the server (GENERATED … AS): it can't be written.
    pub generated: bool,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TableColumns {
    pub columns: Vec<ColumnDetail>,
    pub foreign_keys: Vec<ForeignKey>,
}
impl TableColumns {
    pub fn get(&self, name: &str) -> Option<&ColumnDetail> {
        self.columns.iter().find(|c| c.name == name)
    }
    /// The foreign key a column belongs to (the first one, by name, when there are several).
    pub fn foreign_key(&self, column: &str) -> Option<&ForeignKey> {
        self.foreign_keys
            .iter()
            .find(|fk| fk.columns.iter().any(|c| c == column))
    }
}
/// The kind of a MySQL column from information_schema's DATA_TYPE and COLUMN_TYPE.
pub fn mysql_kind(data_type: &str, column_type: &str) -> ValueKind {
    let column_type = column_type.trim().to_ascii_lowercase();
    match data_type.trim().to_ascii_lowercase().as_str() {
        "tinyint" if column_type.starts_with("tinyint(1)") => ValueKind::Boolean,
        "bool" | "boolean" => ValueKind::Boolean,
        "date" => ValueKind::Date,
        "time" => ValueKind::Time,
        "datetime" | "timestamp" => ValueKind::DateTime,
        "json" => ValueKind::Json,
        "enum" => ValueKind::Enum,
        "set" => ValueKind::Set,
        "binary" | "varbinary" | "tinyblob" | "blob" | "mediumblob" | "longblob" | "bit"
        | "geometry" | "point" | "linestring" | "polygon" | "multipoint" | "multilinestring"
        | "multipolygon" | "geometrycollection" => ValueKind::Binary,
        _ => ValueKind::Other,
    }
}
/// Whether a MySQL / MariaDB column is computed by the server, from information_schema's EXTRA:
/// `VIRTUAL GENERATED` / `STORED GENERATED` (MariaDB also `VIRTUAL` / `PERSISTENT`). MySQL 8
/// writes `DEFAULT_GENERATED` for an expression default (`DEFAULT CURRENT_TIMESTAMP`,
/// `DEFAULT (uuid())`), which is an ordinary writable column.
pub fn mysql_generated(extra: &str) -> bool {
    let extra = extra.to_ascii_uppercase();
    extra.contains("VIRTUAL GENERATED")
        || extra.contains("STORED GENERATED")
        || extra
            .split_whitespace()
            .any(|word| word == "VIRTUAL" || word == "PERSISTENT")
}
/// The kind of a PostgreSQL column from `format_type` and whether its type is an enum.
pub fn pg_kind(type_name: &str, is_enum: bool) -> ValueKind {
    if is_enum {
        return ValueKind::Enum;
    }
    let t = type_name.trim().to_ascii_lowercase();
    if t.ends_with("[]") {
        return ValueKind::Other;
    }
    if t == "boolean" {
        ValueKind::Boolean
    } else if t == "date" {
        ValueKind::Date
    } else if t.starts_with("timestamp") {
        ValueKind::DateTime
    } else if t.starts_with("time") {
        ValueKind::Time
    } else if t == "json" || t == "jsonb" {
        ValueKind::Json
    } else if t == "bytea" {
        ValueKind::Binary
    } else {
        ValueKind::Other
    }
}
/// The values of a MySQL `enum('a','b')` or `set('a','b')` COLUMN_TYPE. Quotes inside a value
/// are doubled (`''`); a backslash escapes the next character.
pub fn mysql_values(column_type: &str) -> Vec<String> {
    let Some(open) = column_type.find('(') else {
        return vec![];
    };
    let mut out = vec![];
    let mut chars = column_type[open + 1..].chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| *c == ',' || c.is_whitespace()) {
            chars.next();
        }
        if chars.next() != Some('\'') {
            break;
        }
        let mut value = String::new();
        loop {
            match chars.next() {
                Some('\'') if chars.peek() == Some(&'\'') => {
                    chars.next();
                    value.push('\'');
                }
                Some('\'') | None => break,
                Some('\\') => {
                    if let Some(c) = chars.next() {
                        value.push(c);
                    }
                }
                Some(c) => value.push(c),
            }
        }
        out.push(value);
    }
    out
}
/// The fractional-second digits a temporal type keeps: `datetime(3)` → 3.
fn precision(data_type: &str) -> Option<u32> {
    let open = data_type.find('(')?;
    let close = data_type[open..].find(')')? + open;
    data_type[open + 1..close].trim().parse().ok()
}
/// The server-side query whose text is "now" for this column (Set Value ▸ NOW()), formatted as
/// the column stores it, so the staged value is exactly what the server would write. `None` for
/// a column that isn't a date or time.
pub fn now_sql(d: Driver, column: &ColumnDetail) -> Option<String> {
    let fsp = precision(&column.data_type).unwrap_or(0).min(6);
    let lower = column.data_type.to_ascii_lowercase();
    Some(match (d, column.kind) {
        (Driver::Mysql, ValueKind::Date) => "SELECT CAST(CURDATE() AS CHAR)".into(),
        (Driver::Mysql, ValueKind::Time) => format!("SELECT CAST(CURTIME({fsp}) AS CHAR)"),
        (Driver::Mysql, ValueKind::DateTime) => format!("SELECT CAST(NOW({fsp}) AS CHAR)"),
        (Driver::Postgres, ValueKind::Date) => "SELECT CURRENT_DATE::text".into(),
        (Driver::Postgres, kind @ (ValueKind::Time | ValueKind::DateTime)) => {
            let zoned = lower.contains("with time zone");
            let function = match (kind, zoned) {
                (ValueKind::Time, true) => "CURRENT_TIME",
                (ValueKind::Time, false) => "LOCALTIME",
                (_, true) => "CURRENT_TIMESTAMP",
                (_, false) => "LOCALTIMESTAMP",
            };
            // Without a declared precision PostgreSQL keeps microseconds.
            let p = precision(&column.data_type).unwrap_or(6).min(6);
            format!("SELECT {function}({p})::text")
        }
        _ => return None,
    })
}
/// The literal a boolean column stores for true and false: MySQL's BOOLEAN is TINYINT(1), which
/// refuses 'true' in strict mode.
pub fn boolean_literals(d: Driver) -> [(&'static str, &'static str); 2] {
    if d == Driver::Mysql {
        [("true", "1"), ("false", "0")]
    } else {
        [("true", "true"), ("false", "false")]
    }
}
/// A SET value with `item` switched on or off, its members kept in declaration order.
pub fn toggle_set_member(current: Option<&str>, item: &str, allowed: &[String]) -> String {
    let mut members: Vec<&str> = current
        .unwrap_or("")
        .split(',')
        .filter(|m| !m.is_empty())
        .collect();
    if let Some(at) = members.iter().position(|m| *m == item) {
        members.remove(at);
    } else {
        members.push(item);
    }
    let rank = |m: &&str| allowed.iter().position(|a| a == m).unwrap_or(usize::MAX);
    members.sort_by_key(rank);
    members.join(",")
}
/// The message the Quick Look and Set Value ▸ PRETTY / MINIFY show for text that isn't JSON.
pub const NOT_JSON: &str = "Not a valid JSON object - not saved until it is fixed";
/// JSON text indented for reading. Numbers and key order are kept exactly.
pub fn json_pretty(text: &str) -> Result<String, &'static str> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|_| NOT_JSON)?;
    serde_json::to_string_pretty(&value).map_err(|_| NOT_JSON)
}
/// JSON text without any spacing. Numbers and key order are kept exactly.
pub fn json_minify(text: &str) -> Result<String, &'static str> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|_| NOT_JSON)?;
    serde_json::to_string(&value).map_err(|_| NOT_JSON)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn column(data_type: &str, kind: ValueKind) -> ColumnDetail {
        ColumnDetail {
            name: "c".into(),
            data_type: data_type.into(),
            kind,
            values: vec![],
            nullable: true,
            generated: false,
        }
    }
    #[test]
    fn kinds_follow_the_catalog_types() {
        assert_eq!(mysql_kind("tinyint", "tinyint(1)"), ValueKind::Boolean);
        assert_eq!(mysql_kind("tinyint", "tinyint(4)"), ValueKind::Other);
        assert_eq!(mysql_kind("TIMESTAMP", "timestamp(3)"), ValueKind::DateTime);
        assert_eq!(mysql_kind("enum", "enum('a')"), ValueKind::Enum);
        assert_eq!(mysql_kind("set", "set('a')"), ValueKind::Set);
        assert_eq!(mysql_kind("longblob", "longblob"), ValueKind::Binary);
        assert_eq!(mysql_kind("double", "double"), ValueKind::Other);
        assert_eq!(
            pg_kind("timestamp(3) with time zone", false),
            ValueKind::DateTime
        );
        assert_eq!(pg_kind("time without time zone", false), ValueKind::Time);
        assert_eq!(pg_kind("jsonb", false), ValueKind::Json);
        assert_eq!(pg_kind("boolean[]", false), ValueKind::Other);
        assert_eq!(pg_kind("mood", true), ValueKind::Enum);
        assert_eq!(pg_kind("bytea", false), ValueKind::Binary);
    }
    #[test]
    fn only_computed_mysql_columns_are_generated() {
        for extra in [
            "VIRTUAL GENERATED",
            "STORED GENERATED",
            "virtual generated",
            "PERSISTENT",
            "VIRTUAL",
        ] {
            assert!(mysql_generated(extra), "{extra}");
        }
        for extra in [
            "",
            "auto_increment",
            "DEFAULT_GENERATED",
            "DEFAULT_GENERATED on update CURRENT_TIMESTAMP(3)",
            "on update CURRENT_TIMESTAMP",
            "INVISIBLE",
            "DEFAULT_GENERATED INVISIBLE",
        ] {
            assert!(!mysql_generated(extra), "{extra}");
        }
    }
    #[test]
    fn enum_values_are_unquoted() {
        assert_eq!(mysql_values("enum('a','b')"), ["a", "b"]);
        assert_eq!(
            mysql_values("set('it''s','a,b','back\\\\slash','')"),
            ["it's", "a,b", "back\\slash", ""]
        );
        assert_eq!(mysql_values("enum('ไทย','😺')"), ["ไทย", "😺"]);
        assert!(mysql_values("int").is_empty());
        assert!(mysql_values("enum(").is_empty());
        assert_eq!(mysql_values("enum('unterminated"), ["unterminated"]);
    }
    #[test]
    fn now_is_read_in_the_column_format() {
        let my = |t, k| now_sql(Driver::Mysql, &column(t, k));
        assert_eq!(
            my("datetime", ValueKind::DateTime).as_deref(),
            Some("SELECT CAST(NOW(0) AS CHAR)")
        );
        assert_eq!(
            my("timestamp(3)", ValueKind::DateTime).as_deref(),
            Some("SELECT CAST(NOW(3) AS CHAR)")
        );
        assert_eq!(
            my("date", ValueKind::Date).as_deref(),
            Some("SELECT CAST(CURDATE() AS CHAR)")
        );
        assert_eq!(
            my("time(9)", ValueKind::Time).as_deref(),
            Some("SELECT CAST(CURTIME(6) AS CHAR)")
        );
        assert_eq!(my("int", ValueKind::Other), None);
        let pg = |t, k| now_sql(Driver::Postgres, &column(t, k));
        assert_eq!(
            pg("timestamp with time zone", ValueKind::DateTime).as_deref(),
            Some("SELECT CURRENT_TIMESTAMP(6)::text")
        );
        assert_eq!(
            pg("timestamp(0) without time zone", ValueKind::DateTime).as_deref(),
            Some("SELECT LOCALTIMESTAMP(0)::text")
        );
        assert_eq!(
            pg("time(2) with time zone", ValueKind::Time).as_deref(),
            Some("SELECT CURRENT_TIME(2)::text")
        );
        assert_eq!(
            pg("date", ValueKind::Date).as_deref(),
            Some("SELECT CURRENT_DATE::text")
        );
    }
    #[test]
    fn set_members_toggle_in_declared_order() {
        let allowed: Vec<String> = ["a", "b", "c"].map(String::from).to_vec();
        assert_eq!(toggle_set_member(None, "b", &allowed), "b");
        assert_eq!(toggle_set_member(Some("c"), "a", &allowed), "a,c");
        assert_eq!(toggle_set_member(Some("a,c"), "c", &allowed), "a");
        assert_eq!(toggle_set_member(Some("a"), "a", &allowed), "");
    }
    #[test]
    fn json_pretty_and_minify_keep_numbers_and_order() {
        let text = r#"{"z":1,"a":[1.10,12345678901234567890],"s":"ไทย"}"#;
        let pretty = json_pretty(text).unwrap();
        assert!(pretty.starts_with("{\n  \"z\": 1,"));
        assert!(pretty.contains("1.10") && pretty.contains("12345678901234567890"));
        assert_eq!(json_minify(&pretty).unwrap(), text);
        assert_eq!(json_pretty("{oops"), Err(NOT_JSON));
        assert_eq!(json_minify(""), Err(NOT_JSON));
    }
    #[test]
    fn booleans_use_literals_the_server_accepts() {
        assert_eq!(boolean_literals(Driver::Mysql)[0].1, "1");
        assert_eq!(boolean_literals(Driver::Postgres)[1].1, "false");
    }
}
