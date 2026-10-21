//! Reading SQL dump files (mysqldump, pg_dump plain format) as a stream of statements.
//!
//! The editor's splitter ([`crate::sql::split_statements`]) works on a whole document in memory.
//! Dumps can be gigabytes, so this reads them line by line with the same lexical rules: quotes
//! and their escapes, backticks, comments (MySQL `#`, `-- `, executable `/*!…*/`; nested
//! PostgreSQL block comments), PostgreSQL dollar quotes and `E''` strings, MySQL `DELIMITER`
//! directives, pg_dump `COPY … FROM stdin` data blocks ending in `\.`, and psql meta-commands.
//! Every statement keeps the line it starts on, so errors can point into the file.
use crate::types::Driver;
use anyhow::{Result, bail};

/// One unit of work read from a dump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DumpItem {
    /// A statement, without its delimiter. `line` is the 1-based line of its first byte (leading
    /// comments included) in the file.
    Statement { sql: String, line: usize },
    /// Raw data rows of the preceding `COPY … FROM stdin`, as they appear in the file.
    CopyData(Vec<u8>),
    /// The `\.` that ends a COPY data block.
    CopyEnd { line: usize, data_line: usize },
    /// A psql meta-command line such as `\connect db`.
    Meta { command: String, line: usize },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lex {
    Code,
    /// A quoted string; `backslash` when `\` escapes the next character.
    Single {
        backslash: bool,
    },
    Double {
        backslash: bool,
    },
    Backtick,
    Block {
        depth: usize,
    },
}
const COPY_CHUNK: usize = 64 * 1024;
/// Splits a dump fed in arbitrary chunks. Call [`DumpSplitter::push`] with each chunk, then
/// [`DumpSplitter::finish`].
pub struct DumpSplitter {
    driver: Driver,
    carry: Vec<u8>,
    line: usize,
    lex: Lex,
    dollar: Option<Vec<u8>>,
    delimiter: Vec<u8>,
    buf: Vec<u8>,
    buf_line: usize,
    has_code: bool,
    in_copy: bool,
    copy: Vec<u8>,
    copy_start: usize,
    started: bool,
}
fn ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}
impl DumpSplitter {
    pub fn new(driver: Driver) -> Self {
        Self {
            driver,
            carry: vec![],
            line: 0,
            lex: Lex::Code,
            dollar: None,
            delimiter: b";".to_vec(),
            buf: vec![],
            buf_line: 1,
            has_code: false,
            in_copy: false,
            copy: vec![],
            copy_start: 0,
            started: false,
        }
    }
    /// Feeds the next bytes of the file.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<DumpItem>> {
        if !self.started {
            self.started = true;
            check_text_start(chunk)?;
        }
        if chunk.contains(&0) {
            bail!("This file contains binary data (NUL bytes), so it isn't a plain-text SQL dump.");
        }
        let mut out = vec![];
        self.carry.extend_from_slice(chunk);
        let mut start = 0;
        while let Some(n) = self.carry[start..].iter().position(|&b| b == b'\n') {
            let end = start + n + 1;
            let line = self.carry[start..end].to_vec();
            self.line(&line, &mut out)?;
            start = end;
        }
        self.carry.drain(..start);
        Ok(out)
    }
    /// The end of the file: the last statement may lack a delimiter; anything left open is an
    /// error, because the server would read it differently.
    pub fn finish(mut self) -> Result<Vec<DumpItem>> {
        let mut out = vec![];
        if !self.carry.is_empty() {
            let line = std::mem::take(&mut self.carry);
            self.line(&line, &mut out)?;
        }
        if self.in_copy {
            bail!(
                "The COPY data that starts before line {} never ends with \\.",
                self.line
            );
        }
        if self.dollar.is_some() || self.lex != Lex::Code {
            bail!(
                "A string, quoted name or comment that starts at or after line {} is never closed.",
                self.buf_line
            );
        }
        self.flush_copy(&mut out);
        self.emit(&mut out)?;
        Ok(out)
    }
    fn flush_copy(&mut self, out: &mut Vec<DumpItem>) {
        if !self.copy.is_empty() {
            out.push(DumpItem::CopyData(std::mem::take(&mut self.copy)));
        }
    }
    fn emit(&mut self, out: &mut Vec<DumpItem>) -> Result<()> {
        let bytes = std::mem::take(&mut self.buf);
        let has_code = std::mem::replace(&mut self.has_code, false);
        if has_code {
            let sql = String::from_utf8(bytes).map_err(|_| {
                anyhow::anyhow!(
                    "The statement at line {} isn't valid UTF-8 text.",
                    self.buf_line
                )
            })?;
            let copy = self.driver == Driver::Postgres && is_copy_from_stdin(&sql);
            out.push(DumpItem::Statement {
                sql,
                line: self.buf_line,
            });
            if copy {
                self.in_copy = true;
                self.copy_start = self.line + 1;
            }
        }
        Ok(())
    }
    fn line(&mut self, line: &[u8], out: &mut Vec<DumpItem>) -> Result<()> {
        self.line += 1;
        // A UTF-8 byte order mark (Windows editors) isn't SQL.
        let line = if self.line == 1 {
            line.strip_prefix(b"\xEF\xBB\xBF".as_slice())
                .unwrap_or(line)
        } else {
            line
        };
        if self.in_copy {
            let text = trim_eol(line);
            if text == b"\\." {
                self.flush_copy(out);
                self.in_copy = false;
                out.push(DumpItem::CopyEnd {
                    line: self.line,
                    data_line: self.copy_start,
                });
            } else {
                self.copy.extend_from_slice(line);
                if self.copy.len() >= COPY_CHUNK {
                    self.flush_copy(out);
                }
            }
            return Ok(());
        }
        let between = self.lex == Lex::Code && self.dollar.is_none() && !self.has_code;
        if between {
            let trimmed = trim_ascii(trim_eol(line));
            if self.driver == Driver::Mysql && starts_with_word(trimmed, b"DELIMITER") {
                let rest = trim_ascii(&trimmed[9..]);
                let token: Vec<u8> = rest
                    .iter()
                    .copied()
                    .take_while(|b| !b.is_ascii_whitespace())
                    .collect();
                if token.is_empty() {
                    bail!("DELIMITER without a delimiter at line {}", self.line);
                }
                self.delimiter = token;
                self.buf.clear();
                return Ok(());
            }
            if self.driver == Driver::Postgres && trimmed.first() == Some(&b'\\') {
                self.buf.clear();
                out.push(DumpItem::Meta {
                    command: String::from_utf8_lossy(trimmed).into_owned(),
                    line: self.line,
                });
                return Ok(());
            }
        }
        let mysql = self.driver == Driver::Mysql;
        let mut i = 0;
        while i < line.len() {
            let b = line[i];
            if self.buf.is_empty() {
                // Whitespace between statements isn't part of either.
                if self.lex == Lex::Code && self.dollar.is_none() && b.is_ascii_whitespace() {
                    i += 1;
                    continue;
                }
                self.buf_line = self.line;
            }
            if let Some(tag) = &self.dollar {
                if line[i..].starts_with(tag) {
                    let n = tag.len();
                    self.buf.extend_from_slice(&line[i..i + n]);
                    self.dollar = None;
                    i += n;
                } else {
                    self.buf.push(b);
                    i += 1;
                }
                continue;
            }
            match self.lex {
                Lex::Single { backslash } | Lex::Double { backslash } => {
                    let quote = if matches!(self.lex, Lex::Single { .. }) {
                        b'\''
                    } else {
                        b'"'
                    };
                    self.buf.push(b);
                    if backslash && b == b'\\' {
                        if let Some(&next) = line.get(i + 1) {
                            self.buf.push(next);
                            i += 1;
                        }
                    } else if b == quote {
                        if line.get(i + 1) == Some(&quote) {
                            self.buf.push(quote);
                            i += 1;
                        } else {
                            self.lex = Lex::Code;
                        }
                    }
                    i += 1;
                }
                Lex::Backtick => {
                    self.buf.push(b);
                    if b == b'`' {
                        if line.get(i + 1) == Some(&b'`') {
                            self.buf.push(b'`');
                            i += 1;
                        } else {
                            self.lex = Lex::Code;
                        }
                    }
                    i += 1;
                }
                Lex::Block { depth } => {
                    if line[i..].starts_with(b"*/") {
                        self.buf.extend_from_slice(b"*/");
                        self.lex = if depth > 1 {
                            Lex::Block { depth: depth - 1 }
                        } else {
                            Lex::Code
                        };
                        i += 2;
                    } else if !mysql && line[i..].starts_with(b"/*") {
                        self.buf.extend_from_slice(b"/*");
                        self.lex = Lex::Block { depth: depth + 1 };
                        i += 2;
                    } else {
                        self.buf.push(b);
                        i += 1;
                    }
                }
                Lex::Code => {
                    if line[i..].starts_with(&self.delimiter) {
                        i += self.delimiter.len();
                        self.emit(out)?;
                        if self.in_copy {
                            // Data starts on the next line.
                            return Ok(());
                        }
                        continue;
                    }
                    let rest = &line[i..];
                    let prev = self.buf.last().copied();
                    let line_comment = (rest.starts_with(b"--")
                        && (!mysql
                            || rest
                                .get(2)
                                .is_none_or(|c| c.is_ascii_whitespace() || *c < 32)))
                        || (mysql && b == b'#');
                    if line_comment {
                        // The rest of the line, newline included, belongs to the comment.
                        self.buf.extend_from_slice(rest);
                        return Ok(());
                    }
                    if rest.starts_with(b"/*") {
                        // MySQL runs `/*!…*/` and optimizer hints: they are code.
                        if mysql && matches!(rest.get(2), Some(b'!' | b'+')) {
                            self.has_code = true;
                        }
                        self.buf.extend_from_slice(b"/*");
                        self.lex = Lex::Block { depth: 1 };
                        i += 2;
                        continue;
                    }
                    if !b.is_ascii_whitespace() {
                        self.has_code = true;
                    }
                    match b {
                        b'\'' => {
                            // PostgreSQL E'…' strings take backslash escapes.
                            let escape = !mysql
                                && matches!(prev, Some(b'E' | b'e'))
                                && (self.buf.len() < 2
                                    || !ident_byte(self.buf[self.buf.len() - 2]));
                            self.lex = Lex::Single {
                                backslash: mysql || escape,
                            };
                        }
                        b'"' => self.lex = Lex::Double { backslash: mysql },
                        b'`' if mysql => self.lex = Lex::Backtick,
                        b'$' if !mysql && prev.is_none_or(|p| !ident_byte(p)) => {
                            if let Some(tag) = dollar_tag(rest) {
                                self.buf.extend_from_slice(&tag);
                                i += tag.len();
                                self.dollar = Some(tag);
                                continue;
                            }
                        }
                        _ => {}
                    }
                    self.buf.push(b);
                    i += 1;
                }
            }
        }
        Ok(())
    }
}
/// `$$` or `$tag$` at the start of `s`.
fn dollar_tag(s: &[u8]) -> Option<Vec<u8>> {
    let end = s[1..].iter().position(|&b| b == b'$')? + 1;
    let tag = &s[1..end];
    let ok = tag.is_empty()
        || (!tag[0].is_ascii_digit()
            && tag
                .iter()
                .all(|&b| b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80));
    ok.then(|| s[..=end].to_vec())
}
fn trim_eol(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}
fn trim_ascii(s: &[u8]) -> &[u8] {
    let start = s
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(s.len());
    let end = s
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(start, |e| e + 1);
    &s[start..end.max(start)]
}
fn starts_with_word(s: &[u8], word: &[u8]) -> bool {
    s.len() > word.len()
        && s[..word.len()].eq_ignore_ascii_case(word)
        && s[word.len()].is_ascii_whitespace()
}
/// Refuses files that aren't plain SQL text before anything is read as SQL.
pub fn check_text_start(head: &[u8]) -> Result<()> {
    if head.starts_with(b"PGDMP") {
        bail!(
            "This is a pg_dump custom-format archive, not a plain SQL dump. Restore it with pg_restore, or export it again with pg_dump --format=plain."
        );
    }
    if head.starts_with(&[0x1f, 0x8b]) {
        bail!("This file is gzip-compressed. Choose a .sql.gz file so it can be decompressed.");
    }
    if head.starts_with(b"PK\x03\x04")
        || head.starts_with(b"BZh")
        || head.starts_with(&[0xfd, b'7', b'z', b'X', b'Z'])
    {
        bail!("This is a compressed archive, not a plain SQL dump.");
    }
    if head.contains(&0) {
        bail!("This file contains binary data (NUL bytes), so it isn't a plain-text SQL dump.");
    }
    Ok(())
}
/// Code words of a statement, upper-cased, and quoted names as written; strings and comments are
/// skipped. MySQL `/*!…*/` content is code. `.` is kept to see qualified names.
pub fn words(sql: &str, d: Driver) -> Vec<String> {
    let s = sql.as_bytes();
    let mysql = d == Driver::Mysql;
    let mut out = vec![];
    let mut i = 0;
    while i < s.len() {
        let b = s[i];
        if s[i..].starts_with(b"/*!") && mysql {
            // `/*!50003 CREATE …`: skip the marker and version, keep the code.
            i += 3;
            while i < s.len() && s[i].is_ascii_digit() {
                i += 1;
            }
            continue;
        }
        if mysql && s[i..].starts_with(b"*/") {
            i += 2;
            continue;
        }
        if s[i..].starts_with(b"/*") {
            i = s[i..]
                .windows(2)
                .position(|w| w == b"*/")
                .map_or(s.len(), |p| i + p + 2);
            continue;
        }
        if s[i..].starts_with(b"--") || (mysql && b == b'#') {
            i = s[i..]
                .iter()
                .position(|&c| c == b'\n')
                .map_or(s.len(), |p| i + p);
            continue;
        }
        match b {
            b'\'' => {
                i += 1;
                while i < s.len() {
                    if s[i] == b'\\' && mysql {
                        i += 2;
                        continue;
                    }
                    if s[i] == b'\'' {
                        if s.get(i + 1) == Some(&b'\'') {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            b'`' | b'"' => {
                let q = b;
                let start = i + 1;
                i += 1;
                while i < s.len() && s[i] != q {
                    i += 1;
                }
                out.push(String::from_utf8_lossy(&s[start..i.min(s.len())]).into_owned());
                i += 1;
            }
            b'.' => {
                out.push(".".into());
                i += 1;
            }
            b'(' | b')' | b',' | b'=' => {
                out.push((b as char).to_string());
                i += 1;
            }
            _ if ident_byte(b) => {
                let start = i;
                while i < s.len() && ident_byte(s[i]) {
                    i += 1;
                }
                out.push(String::from_utf8_lossy(&s[start..i]).to_ascii_uppercase());
            }
            _ => i += 1,
        }
    }
    out
}
fn is_copy_from_stdin(sql: &str) -> bool {
    let w = words(sql, Driver::Postgres);
    w.first().is_some_and(|w| w == "COPY")
        && w.windows(2).any(|p| p[0] == "FROM" && p[1] == "STDIN")
}
/// Kinds of statements that reach beyond the target database or that can't be checked. Each
/// is blocked unless the user allows that kind explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BlockKind {
    /// Writes into, creates, drops, alters or switches to another database.
    OtherDatabase,
    /// SQL built at run time (PREPARE/EXECUTE, DO blocks, routine bodies running dynamic SQL,
    /// CALL): what it does can't be read from the dump.
    DynamicSql,
    /// Reads or writes files on the database server (LOAD DATA, INTO OUTFILE, COPY to/from a
    /// server file, pg_read_file, lo_import, ...).
    ServerFiles,
    /// Changes the whole server: global settings, plugins, shutdown, KILL, FLUSH, RESET,
    /// ALTER SYSTEM, ALTER DATABASE.
    ServerWide,
    /// Connects to other servers or databases (dblink, foreign data wrappers, subscriptions).
    Remote,
    /// Creates or changes users and roles, grants server privileges, or switches role.
    Roles,
}
impl BlockKind {
    pub const ALL: [BlockKind; 6] = [
        Self::OtherDatabase,
        Self::DynamicSql,
        Self::ServerFiles,
        Self::ServerWide,
        Self::Remote,
        Self::Roles,
    ];
    /// The opt-in's label.
    pub fn allow_label(self) -> &'static str {
        match self {
            Self::OtherDatabase => "Allow statements that use other databases",
            Self::DynamicSql => "Allow dynamic SQL and procedure calls Telecat can't check",
            Self::ServerFiles => "Allow reading and writing files on the database server",
            Self::ServerWide => {
                "Allow server-wide changes (global settings, plugins, KILL, FLUSH, ...)"
            }
            Self::Remote => "Allow connections to other servers (dblink, foreign data wrappers)",
            Self::Roles => "Allow creating or changing users and roles",
        }
    }
}
/// What a dump statement does that the user must see before importing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DumpRisk {
    /// Blocked unless its kind is allowed.
    Blocked(BlockKind, String),
    /// Ends or opens a transaction: refused when the import runs as one transaction.
    TransactionControl,
    /// Writes into, creates, drops or switches to a database other than the target. Blocked
    /// unless the user allows it.
    OtherDatabase(String),
    DropDatabase,
    DropSchema,
    DropTable,
    Truncate,
    /// DELETE or UPDATE; the guard's reason when it has one.
    DataChange(String),
    Privileges,
    CreateDatabase,
    SearchPath,
}
impl DumpRisk {
    pub fn label(&self) -> String {
        match self {
            Self::OtherDatabase(db) => format!("uses another database: {db}"),
            Self::Blocked(_, what) => what.clone(),
            Self::TransactionControl => {
                "transaction control (BEGIN, COMMIT, ROLLBACK, SAVEPOINT, ...)".into()
            }
            Self::DropDatabase => "DROP DATABASE".into(),
            Self::DropSchema => "DROP SCHEMA".into(),
            Self::DropTable => "DROP TABLE / VIEW".into(),
            Self::Truncate => "TRUNCATE".into(),
            Self::DataChange(r) => r.clone(),
            Self::Privileges => "privileges or users (GRANT, REVOKE, users, roles, owners)".into(),
            Self::CreateDatabase => "CREATE DATABASE".into(),
            Self::SearchPath => "changes search_path".into(),
        }
    }
    pub fn blocked(&self) -> bool {
        self.block_kind().is_some()
    }
    pub fn block_kind(&self) -> Option<BlockKind> {
        match self {
            Self::OtherDatabase(_) => Some(BlockKind::OtherDatabase),
            Self::Blocked(k, _) => Some(*k),
            _ => None,
        }
    }
}
/// What [`classify`] found in one statement.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatementInfo {
    pub risks: Vec<DumpRisk>,
    /// PostgreSQL: can't run inside a transaction block.
    pub no_transaction: bool,
    /// MySQL `USE db`: the database later statements go to.
    pub uses: Option<String>,
}
fn other(name: &str, target: &str) -> bool {
    !name.eq_ignore_ascii_case(target)
}
/// The object a statement acts on, `[db, ., name]` or `[name]`, after its leading keywords.
fn object_after<'a>(w: &'a [String], keywords: &[&str]) -> Option<&'a [String]> {
    let mut i = 0;
    for k in keywords {
        while w.get(i).is_some_and(|x| {
            [
                "IF",
                "NOT",
                "EXISTS",
                "IGNORE",
                "LOW_PRIORITY",
                "DELAYED",
                "HIGH_PRIORITY",
                "TEMPORARY",
                "ONLY",
                "QUICK",
            ]
            .contains(&x.as_str())
        }) {
            i += 1;
        }
        if w.get(i).map(String::as_str) != Some(*k) {
            return None;
        }
        i += 1;
    }
    while w
        .get(i)
        .is_some_and(|x| ["IF", "NOT", "EXISTS", "IGNORE", "ONLY"].contains(&x.as_str()))
    {
        i += 1;
    }
    let end = if w.get(i + 1).map(String::as_str) == Some(".") {
        i + 3
    } else {
        i + 1
    };
    w.get(i..end.min(w.len()))
}
/// Risks of a dump statement for import into database `target`.
pub fn classify(sql: &str, d: Driver, target: &str) -> StatementInfo {
    classify_in(sql, d, target, &[])
}
/// [`classify`], also flagging every `db.name` qualifier naming another of the server's
/// `databases` (MySQL), wherever it appears in the statement.
pub fn classify_in(sql: &str, d: Driver, target: &str, databases: &[String]) -> StatementInfo {
    let w = words(sql, d);
    let mut info = StatementInfo::default();
    let word = |i: usize| w.get(i).map(String::as_str).unwrap_or("");
    let mysql = d == Driver::Mysql;
    match (word(0), word(1)) {
        ("USE", _) if mysql => {
            let db = w.get(1).cloned().unwrap_or_default();
            if other(&db, target) {
                info.risks.push(DumpRisk::OtherDatabase(db.clone()));
            }
            info.uses = Some(db);
        }
        ("CREATE", "DATABASE") | ("CREATE", "SCHEMA") if mysql || word(1) == "DATABASE" => {
            let name = object_after(&w, &["CREATE", word(1)])
                .and_then(|o| o.last().cloned())
                .unwrap_or_default();
            info.risks.push(DumpRisk::CreateDatabase);
            if other(&name, target) {
                info.risks.push(DumpRisk::OtherDatabase(name));
            }
            info.no_transaction = !mysql;
        }
        ("DROP", "DATABASE") | ("DROP", "SCHEMA") if mysql || word(1) == "DATABASE" => {
            let name = object_after(&w, &["DROP", word(1)])
                .and_then(|o| o.last().cloned())
                .unwrap_or_default();
            info.risks.push(DumpRisk::DropDatabase);
            if other(&name, target) {
                info.risks.push(DumpRisk::OtherDatabase(name));
            }
            info.no_transaction = !mysql;
        }
        ("DROP", "SCHEMA") => info.risks.push(DumpRisk::DropSchema),
        ("DROP", "TABLE" | "VIEW" | "MATERIALIZED") => info.risks.push(DumpRisk::DropTable),
        ("TRUNCATE", _) => info.risks.push(DumpRisk::Truncate),
        ("DELETE" | "UPDATE", _) => info.risks.push(DumpRisk::DataChange(
            crate::sql::dangerous_reason(sql, d)
                .unwrap_or_else(|| format!("{} with WHERE", word(0))),
        )),
        ("GRANT" | "REVOKE", _)
        | ("CREATE" | "ALTER" | "DROP" | "RENAME", "USER" | "ROLE")
        | ("SET", "PASSWORD") => info.risks.push(DumpRisk::Privileges),
        ("ALTER", _) if w.windows(2).any(|p| p[0] == "OWNER" && p[1] == "TO") => {
            info.risks.push(DumpRisk::Privileges)
        }
        ("SET", "SEARCH_PATH") => info.risks.push(DumpRisk::SearchPath),
        ("ALTER", "DATABASE") if !mysql => {
            let name = w.get(2).cloned().unwrap_or_default();
            if other(&name, target) {
                info.risks.push(DumpRisk::OtherDatabase(name));
            }
        }
        ("ALTER", "DATABASE" | "SCHEMA") if mysql => {
            let name = w.get(2).cloned().unwrap_or_default();
            if other(&name, target) && name != "DEFAULT" && name != "CHARACTER" && name != "COLLATE"
            {
                info.risks.push(DumpRisk::OtherDatabase(name));
            }
        }
        ("SELECT", _)
            if !mysql
                && sql
                    .to_ascii_lowercase()
                    .contains("set_config('search_path'")
                && !sql.contains("'search_path', ''") =>
        {
            info.risks.push(DumpRisk::SearchPath)
        }
        _ => {}
    }
    for r in escapes(sql, &w, d) {
        if !info.risks.contains(&r) {
            info.risks.push(r);
        }
    }
    if !mysql {
        let concurrent = w.iter().any(|x| x == "CONCURRENTLY");
        let head = matches!(word(0), "VACUUM" | "CLUSTER")
            || (word(0) == "ALTER" && word(1) == "SYSTEM")
            || (matches!(word(0), "CREATE" | "DROP") && word(1) == "TABLESPACE");
        info.no_transaction |= concurrent || head;
    }
    if mysql {
        // Objects named in another database: `INSERT INTO other.t`, `CREATE TABLE other.t`, …
        let heads: [&[&str]; 9] = [
            &["INSERT", "INTO"],
            &["REPLACE", "INTO"],
            &["CREATE", "TABLE"],
            &["DROP", "TABLE"],
            &["ALTER", "TABLE"],
            &["TRUNCATE", "TABLE"],
            &["DELETE", "FROM"],
            &["LOCK", "TABLES"],
            &["UPDATE"],
        ];
        for h in heads {
            if let Some([db, dot, _]) = object_after(&w, h) {
                if dot == "." && other(db, target) {
                    info.risks.push(DumpRisk::OtherDatabase(db.clone()));
                }
            }
        }
        for (i, x) in w.iter().enumerate() {
            let qualifier = w.get(i + 1).is_some_and(|n| n == ".") && (i == 0 || w[i - 1] != ".");
            if qualifier
                && other(x, target)
                && databases.iter().any(|db| db.eq_ignore_ascii_case(x))
                && !info.risks.contains(&DumpRisk::OtherDatabase(x.clone()))
            {
                info.risks.push(DumpRisk::OtherDatabase(x.clone()));
            }
        }
    }
    info
}
const PG_ADMIN_FUNCTIONS: [&str; 15] = [
    "PG_TERMINATE_BACKEND",
    "PG_CANCEL_BACKEND",
    "PG_RELOAD_CONF",
    "PG_ROTATE_LOGFILE",
    "PG_PROMOTE",
    "PG_SWITCH_WAL",
    "PG_CREATE_RESTORE_POINT",
    "PG_BACKUP_",
    "PG_START_BACKUP",
    "PG_STOP_BACKUP",
    "PG_WAL_REPLAY_",
    "PG_LOG_BACKEND_MEMORY_CONTEXTS",
    "PG_CREATE_PHYSICAL_REPLICATION_SLOT",
    "PG_CREATE_LOGICAL_REPLICATION_SLOT",
    "PG_DROP_REPLICATION_SLOT",
];
const MYSQL_PRIVILEGED_SESSION_VARS: [&str; 11] = [
    "SQL_LOG_BIN",
    "SQL_LOG_OFF",
    "GTID_NEXT",
    "GTID_PURGED",
    "PSEUDO_THREAD_ID",
    "PSEUDO_SLAVE_MODE",
    "PSEUDO_REPLICA_MODE",
    "BINLOG_FORMAT",
    "BINLOG_ROW_IMAGE",
    "RBR_EXEC_MODE",
    "DEBUG",
];
/// `CONNECTION='mysql://…'` as a table option, not the word inside a string.
static MYSQL_CONNECTION_OPTION: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(?i)(^|[^'"\w])connection\s*=?\s*(\w+\s*)?['"]"#).unwrap()
    });
static MYSQL_FEDERATED_ENGINE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r#"(?i)\bengine\s*=?\s*[`'"]?federated\b"#).unwrap()
});
/// `SET [LOCAL|SESSION] ROLE x` / `SET SESSION AUTHORIZATION x` inside a routine body, but not
/// `UPDATE … SET role = …`.
static PG_ROUTINE_SET_ROLE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"\bSET\s+((LOCAL|SESSION)\s+)?(ROLE\s+[^=\s]|SESSION\s+AUTHORIZATION\b)")
        .unwrap()
});
static PG_SET_CONFIG_CALL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^set_config\s*\(\s*'([^']*)'\s*,\s*'([^']*)'\s*[,)]").unwrap()
});
/// A `set_config` that switches role or session authorization or sets a search_path. pg_dump's
/// own `set_config('search_path', '', false)` only empties it and stays allowed; any call whose
/// arguments can't be read as plain literals counts as risky.
fn risky_set_config(sql: &str) -> bool {
    let lower = sql.to_lowercase();
    lower.match_indices("set_config").any(|(i, _)| {
        let Some(call) = PG_SET_CONFIG_CALL.captures(&lower[i..]) else {
            return true;
        };
        let (name, value) = (call[1].trim(), &call[2]);
        matches!(name, "role" | "session_authorization")
            || (name == "search_path" && !value.is_empty())
    })
}
/// The statement with dollar-quoted bodies removed: what's left is a routine's header.
fn without_dollar_quotes(sql: &str) -> String {
    static TAG: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\$([A-Za-z_][A-Za-z0-9_]*)?\$").unwrap());
    let mut out = String::with_capacity(sql.len());
    let mut rest = sql;
    while let Some(open) = TAG.find(rest) {
        out.push_str(&rest[..open.start()]);
        let after = &rest[open.end()..];
        match after.find(open.as_str()) {
            Some(close) => rest = &after[close + open.len()..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}
fn has_word_prefix(w: &[String], prefixes: &[&str]) -> bool {
    // Quoted names keep their case: `"pg_read_file"(…)` is the same function.
    w.iter().any(|x| {
        let x = x.to_ascii_uppercase();
        prefixes.iter().any(|p| x.starts_with(p))
    })
}
/// `word` in `upper` as a whole word: routine bodies are searched as text, strings included.
fn text_has_word(upper: &str, word: &str) -> bool {
    upper.match_indices(word).any(|(i, _)| {
        let before = upper[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_');
        let after = upper[i + word.len()..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_');
        before && after
    })
}
/// The known ways a statement reaches past the target database or past what can be checked.
fn escapes(sql: &str, w: &[String], d: Driver) -> Vec<DumpRisk> {
    use BlockKind::*;
    let word = |i: usize| w.get(i).map(String::as_str).unwrap_or("");
    let has = |x: &str| w.iter().any(|y| y == x);
    let mut out = vec![];
    let mut block = |k: BlockKind, what: &str| out.push(DumpRisk::Blocked(k, what.to_owned()));
    let upper = sql.to_uppercase();
    // A routine's body runs later, maybe with dynamic SQL inside.
    let routine = word(0) == "DO"
        || (matches!(word(0), "CREATE" | "ALTER")
            && w.iter().take(16).any(|x| {
                matches!(
                    x.as_str(),
                    "PROCEDURE" | "FUNCTION" | "ROUTINE" | "EVENT" | "TRIGGER"
                )
            }));
    let transaction = match d {
        Driver::Mysql => {
            matches!(
                word(0),
                "BEGIN" | "COMMIT" | "ROLLBACK" | "SAVEPOINT" | "RELEASE" | "XA"
            ) || (word(0) == "START" && word(1) == "TRANSACTION")
                || (word(0) == "SET" && has("AUTOCOMMIT"))
        }
        Driver::Postgres => {
            matches!(
                word(0),
                "BEGIN" | "COMMIT" | "END" | "ROLLBACK" | "ABORT" | "SAVEPOINT" | "RELEASE"
            ) || (word(0) == "START" && word(1) == "TRANSACTION")
                || (word(0) == "PREPARE" && word(1) == "TRANSACTION")
        }
    };
    match d {
        Driver::Mysql => {
            if matches!(word(0), "PREPARE" | "EXECUTE" | "DEALLOCATE")
                || (word(0) == "DROP" && word(1) == "PREPARE")
            {
                block(
                    DynamicSql,
                    "PREPARE/EXECUTE (SQL built while the dump runs)",
                );
            }
            if word(0) == "CALL" {
                block(DynamicSql, "CALL (runs a stored procedure)");
            }
            if routine
                && ["PREPARE", "EXECUTE", "CALL"]
                    .iter()
                    .any(|k| text_has_word(&upper, k))
            {
                block(
                    DynamicSql,
                    "a routine body that runs dynamic SQL or calls procedures",
                );
            }
            if word(0) == "LOAD" || (has("INTO") && (has("OUTFILE") || has("DUMPFILE"))) {
                block(
                    ServerFiles,
                    "LOAD DATA / SELECT ... INTO OUTFILE (server files)",
                );
            }
            if word(0) == "SET" && (has("GLOBAL") || has("PERSIST") || has("PERSIST_ONLY")) {
                block(ServerWide, "SET GLOBAL / PERSIST (server settings)");
            }
            if matches!(
                word(0),
                "INSTALL" | "UNINSTALL" | "SHUTDOWN" | "RESTART" | "KILL" | "FLUSH" | "RESET"
            ) {
                block(
                    ServerWide,
                    "INSTALL/UNINSTALL, SHUTDOWN, RESTART, KILL, FLUSH or RESET",
                );
            }
            if matches!(word(0), "GRANT" | "REVOKE")
                || (matches!(word(0), "CREATE" | "DROP" | "ALTER" | "RENAME")
                    && matches!(word(1), "USER" | "ROLE"))
                || (word(0) == "SET" && matches!(word(1), "PASSWORD" | "DEFAULT" | "ROLE"))
            {
                block(Roles, "users, roles or privileges (server-wide)");
            }
            if (matches!(word(0), "CREATE" | "ALTER" | "DROP") && word(1) == "SERVER")
                || (matches!(word(0), "CREATE" | "ALTER")
                    && has("TABLE")
                    && (MYSQL_FEDERATED_ENGINE.is_match(sql)
                        || MYSQL_CONNECTION_OPTION.is_match(sql)))
            {
                block(
                    Remote,
                    "FEDERATED servers or tables (CREATE SERVER, ENGINE=FEDERATED, CONNECTION=)",
                );
            }
            // Reading these into a user variable is harmless; mysqldump does it around GTID dumps.
            if word(0) == "SET"
                && w.windows(2).any(|p| {
                    p[1] == "="
                        && MYSQL_PRIVILEGED_SESSION_VARS
                            .contains(&p[0].to_ascii_uppercase().as_str())
                })
            {
                block(
                    ServerWide,
                    "privileged session settings (sql_log_bin, gtid_next, binlog_format, ...)",
                );
            }
        }
        Driver::Postgres => {
            if word(0) == "DO" {
                block(DynamicSql, "DO block (code run while the dump runs)");
            } else if routine && text_has_word(&upper, "EXECUTE") {
                block(
                    DynamicSql,
                    "a function body that runs dynamic SQL (EXECUTE)",
                );
            }
            if has_word_prefix(w, &["DBLINK", "POSTGRES_FDW"])
                || (word(0) == "CREATE" && matches!(word(1), "SERVER" | "SUBSCRIPTION" | "FOREIGN"))
                || (matches!(word(0), "CREATE" | "ALTER")
                    && word(1) == "USER"
                    && word(2) == "MAPPING")
                || (word(0) == "IMPORT" && word(1) == "FOREIGN")
            {
                block(Remote, "dblink / foreign data wrappers / subscriptions");
            }
            if word(0) == "CREATE"
                && word(1) == "EXTENSION"
                && (has("FILE_FDW") || has("ADMINPACK"))
            {
                block(
                    ServerFiles,
                    "an extension that reads or writes server files",
                );
            }
            if word(0) == "COPY" && !(has("FROM") && has("STDIN")) {
                block(ServerFiles, "COPY to or from a server file");
            }
            if has_word_prefix(
                w,
                &[
                    "LO_IMPORT",
                    "LO_EXPORT",
                    "PG_READ_FILE",
                    "PG_READ_BINARY_FILE",
                    "PG_LS_",
                    "PG_STAT_FILE",
                    "PG_FILE_",
                ],
            ) {
                block(ServerFiles, "a function that reads or writes server files");
            }
            if word(0) == "ALTER" && matches!(word(1), "DATABASE" | "SYSTEM") {
                block(
                    ServerWide,
                    "ALTER DATABASE / ALTER SYSTEM (database or server settings)",
                );
            }
            if word(0) == "LOAD" {
                block(ServerFiles, "LOAD (loads a library into the server)");
            }
            if has_word_prefix(w, &PG_ADMIN_FUNCTIONS)
                || (routine && PG_ADMIN_FUNCTIONS.iter().any(|f| upper.contains(f)))
            {
                block(
                    ServerWide,
                    "server administration functions (pg_terminate_backend, pg_reload_conf, ...)",
                );
            }
            if matches!(word(0), "CREATE" | "ALTER" | "DROP") && word(1) == "PUBLICATION" {
                block(Remote, "publications (logical replication)");
            }
            if routine
                && w.windows(2)
                    .any(|p| p[0] == "SECURITY" && p[1] == "DEFINER")
            {
                block(Roles, "SECURITY DEFINER (runs with its owner's privileges)");
            }
            let set_role = word(0) == "SET"
                && (word(1) == "ROLE"
                    || (word(1) == "SESSION" && word(2) == "AUTHORIZATION")
                    || (matches!(word(1), "LOCAL" | "SESSION")
                        && matches!(word(2), "ROLE" | "SESSION")));
            // `CREATE FUNCTION … SET role = x` switches role on every call.
            let routine_sets_role = routine && {
                let header = words(&without_dollar_quotes(sql), d);
                header.windows(3).any(|p| {
                    p[0] == "SET"
                        && matches!(p[1].as_str(), "ROLE" | "SESSION_AUTHORIZATION")
                        && matches!(p[2].as_str(), "=" | "TO")
                }) || PG_ROUTINE_SET_ROLE.is_match(&upper)
            };
            if (matches!(word(0), "CREATE" | "ALTER" | "DROP")
                && matches!(word(1), "ROLE" | "USER" | "GROUP")
                && word(2) != "MAPPING")
                || set_role
                || routine_sets_role
                || ((routine || w.iter().any(|x| x.eq_ignore_ascii_case("SET_CONFIG")))
                    && risky_set_config(sql))
            {
                block(
                    Roles,
                    "roles (CREATE/ALTER ROLE, SET ROLE, SET SESSION AUTHORIZATION, set_config)",
                );
            }
        }
    }
    if transaction {
        out.push(DumpRisk::TransactionControl);
    }
    out
}
/// A psql meta-command in a dump: `Ok(true)` to skip it, `Ok(false)` when it switches to
/// another database (blocked), `Err` for commands that can't be run safely.
pub fn meta_command(command: &str, target: &str) -> Result<bool> {
    let mut parts = command.split_whitespace();
    let name = parts.next().unwrap_or("");
    match name {
        "\\connect" | "\\c" => {
            let db = parts.next().unwrap_or("").trim_matches('"');
            Ok(db.is_empty() || db == "-" || !other(db, target))
        }
        "\\restrict" | "\\unrestrict" | "\\set" | "\\unset" | "\\echo" | "\\encoding"
        | "\\pset" | "\\timing" => Ok(true),
        _ => bail!("Unsupported psql command {name}: only plain pg_dump output can be imported."),
    }
}
/// Why a dump statement can't be imported safely at all; `None` when it can.
pub fn unsupported(sql: &str, d: Driver) -> Option<String> {
    if !crate::guard::single_statement(sql, d) {
        return Some("the server could read this as more than one statement".into());
    }
    let w = words(sql, d);
    if d == Driver::Postgres
        && w.first().is_some_and(|x| x == "COPY")
        && w.iter().any(|x| x == "PROGRAM")
    {
        return Some("COPY ... PROGRAM runs a shell command on the database server".into());
    }
    if d == Driver::Postgres
        && w.first().is_some_and(|x| x == "SET")
        && w.iter().any(|x| x == "STANDARD_CONFORMING_STRINGS")
        && w.iter().any(|x| x == "OFF")
    {
        return Some("standard_conforming_strings = off changes how strings are read; export the dump again with a current pg_dump".into());
    }
    None
}
/// A whole dump read ahead of importing: what the confirmation shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DumpScan {
    /// The server's databases, to recognise `other_db.table` (MySQL).
    pub databases: Vec<String>,
    pub statements: u64,
    pub copy_blocks: u64,
    /// (line, risk) for each risky statement, at most [`DumpScan::LISTED`] of them.
    pub risks: Vec<(usize, DumpRisk)>,
    /// How many risky statements of each kind, by label.
    pub risk_counts: std::collections::BTreeMap<String, u64>,
    /// Statements blocked unless their kind is allowed: (line, what).
    pub blocked: Vec<(usize, String)>,
    /// The kinds among `blocked`, each needing its own opt-in.
    pub blocked_kinds: std::collections::BTreeSet<BlockKind>,
    /// Never run: psql switching to another database. (line, what)
    pub refused: Vec<(usize, String)>,
    /// Lines with transaction control, refused when importing in one transaction.
    pub transaction_control: Vec<usize>,
    /// PostgreSQL statements that can't run in a transaction.
    pub no_transaction: Vec<(usize, String)>,
}
impl DumpScan {
    pub const LISTED: usize = 200;
    pub fn add(&mut self, item: &DumpItem, d: Driver, target: &str) -> Result<()> {
        match item {
            DumpItem::Statement { sql, line } => {
                if let Some(why) = unsupported(sql, d) {
                    bail!(
                        "Line {line}: can't import this statement: {why}.\n{}",
                        excerpt(sql, 120)
                    );
                }
                self.statements += 1;
                if d == Driver::Postgres && is_copy_from_stdin(sql) {
                    self.copy_blocks += 1;
                }
                let info = classify_in(sql, d, target, &self.databases);
                if info.no_transaction {
                    self.no_transaction.push((*line, excerpt(sql, 80)));
                }
                for r in info.risks {
                    *self.risk_counts.entry(r.label()).or_default() += 1;
                    if let Some(kind) = r.block_kind() {
                        self.blocked.push((*line, r.label()));
                        self.blocked_kinds.insert(kind);
                    }
                    if r == DumpRisk::TransactionControl {
                        self.transaction_control.push(*line);
                    }
                    if self.risks.len() < Self::LISTED {
                        self.risks.push((*line, r));
                    }
                }
            }
            DumpItem::Meta { command, line } => {
                if !meta_command(command, target)? {
                    self.refused
                        .push((*line, format!("{command} (switches to another database)")));
                }
            }
            DumpItem::CopyData(_) | DumpItem::CopyEnd { .. } => {}
        }
        Ok(())
    }
}
/// The first `max` characters of a statement on one line, for messages.
pub fn excerpt(sql: &str, max: usize) -> String {
    let flat: String = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        format!("{}…", flat.chars().take(max).collect::<String>())
    } else {
        flat
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn split(d: Driver, text: &str, chunk: usize) -> Vec<DumpItem> {
        let mut s = DumpSplitter::new(d);
        let mut out = vec![];
        for c in text.as_bytes().chunks(chunk) {
            out.extend(s.push(c).unwrap());
        }
        out.extend(s.finish().unwrap());
        out
    }
    fn statements(items: &[DumpItem]) -> Vec<(String, usize)> {
        items
            .iter()
            .filter_map(|i| match i {
                DumpItem::Statement { sql, line } => Some((sql.trim().to_owned(), *line)),
                _ => None,
            })
            .collect()
    }
    const MYSQLDUMP: &str = "-- MySQL dump 10.13\n/*!40101 SET @OLD_CHARACTER_SET_CLIENT=@@CHARACTER_SET_CLIENT */;\n/*!40014 SET FOREIGN_KEY_CHECKS=0 */;\nDROP TABLE IF EXISTS `t`;\nCREATE TABLE `t` (\n  `id` int NOT NULL,\n  `name` varchar(40) DEFAULT 'a;b',\n  PRIMARY KEY (`id`)\n) ENGINE=InnoDB;\n# hash comment; with a semicolon\nINSERT INTO `t` VALUES (1,'it\\'s; fine'),(2,'ธนาคาร \"x\" `y`'),(3,'back\\\\slash'),(4,'a''b');\nDELIMITER ;;\nCREATE PROCEDURE `p`()\nBEGIN\n  SELECT 1; SELECT 2;\nEND ;;\n/*!50003 CREATE*/ /*!50017 DEFINER=`root`@`%`*/ /*!50003 TRIGGER `tr` BEFORE INSERT ON `t` FOR EACH ROW BEGIN SET NEW.name = UPPER(NEW.name); END */;;\nDELIMITER ;\n--not a comment in MySQL\n;\nSELECT 'last'";
    #[test]
    fn mysqldump_statements_survive_any_chunking() {
        let expected = statements(&split(Driver::Mysql, MYSQLDUMP, MYSQLDUMP.len()));
        for chunk in [1, 2, 3, 7, 64] {
            assert_eq!(
                statements(&split(Driver::Mysql, MYSQLDUMP, chunk)),
                expected,
                "{chunk}"
            );
        }
        let sql: Vec<_> = expected.iter().map(|s| s.0.as_str()).collect();
        assert_eq!(sql.len(), 9, "{sql:#?}");
        assert!(sql[0].starts_with("-- MySQL dump") && sql[0].ends_with("*/"));
        assert_eq!(sql[2], "DROP TABLE IF EXISTS `t`");
        assert!(sql[3].contains("'a;b'") && sql[3].ends_with("ENGINE=InnoDB"));
        assert!(sql[4].starts_with("# hash comment; with a semicolon\nINSERT"));
        assert!(sql[4].ends_with("(4,'a''b')"));
        assert!(sql[5].starts_with("CREATE PROCEDURE") && sql[5].contains("SELECT 1; SELECT 2;"));
        assert!(sql[6].starts_with("/*!50003 CREATE*/") && sql[6].ends_with("END */"));
        assert_eq!(
            sql[7], "--not a comment in MySQL",
            "`--` needs a space after it in MySQL"
        );
        assert_eq!(sql[8], "SELECT 'last'");
        assert_eq!(expected[3].1, 5, "line of CREATE TABLE");
        // The editor's splitter agrees on the plain part.
        let plain =
            "DROP TABLE IF EXISTS `t`;\nINSERT INTO `t` VALUES (1,'it\\'s; fine');\nSELECT 1";
        let editor: Vec<String> = crate::sql::split_statements(plain, Driver::Mysql)
            .into_iter()
            .map(|s| s.text.trim().trim_end_matches(';').trim().to_owned())
            .collect();
        let dump: Vec<String> = statements(&split(Driver::Mysql, plain, 5))
            .into_iter()
            .map(|s| s.0)
            .collect();
        assert_eq!(dump, editor);
    }
    const PGDUMP: &str = "--\n-- PostgreSQL database dump\n--\n\\restrict abc123\nSET statement_timeout = 0;\nSELECT pg_catalog.set_config('search_path', '', false);\nCREATE FUNCTION public.f() RETURNS text\n    LANGUAGE plpgsql\n    AS $_$\nBEGIN\n  RETURN 'a;b' || $$x;y$$; -- inner\nEND;\n$_$;\nCREATE TABLE public.t (\n    id integer NOT NULL,\n    name text /* nested /* comment; */ still; */\n);\nCOPY public.t (id, name) FROM stdin;\n1\tธนาคาร\n2\ttab\\there\\nnewline \\\\ back\\.slash\n3\t\\N\n\\.\nSELECT E'it\\'s; ok', 'plain\\';\n\\unrestrict abc123\n";
    #[test]
    fn pg_dump_plain_with_functions_copy_and_meta_commands() {
        for chunk in [1, 3, 9, 4096] {
            let items = split(Driver::Postgres, PGDUMP, chunk);
            let sql = statements(&items);
            assert_eq!(sql.len(), 6, "{chunk}: {sql:#?}");
            assert!(sql[2].0.contains("$$x;y$$; -- inner\nEND;\n$_$"));
            assert!(sql[3].0.contains("nested /* comment; */ still; */\n)"));
            assert_eq!(sql[4].0, "COPY public.t (id, name) FROM stdin");
            assert_eq!(sql[5].0, "SELECT E'it\\'s; ok', 'plain\\'");
            let data: Vec<u8> = items
                .iter()
                .filter_map(|i| match i {
                    DumpItem::CopyData(d) => Some(d.clone()),
                    _ => None,
                })
                .flatten()
                .collect();
            assert_eq!(
                String::from_utf8(data).unwrap(),
                "1\tธนาคาร\n2\ttab\\there\\nnewline \\\\ back\\.slash\n3\t\\N\n"
            );
            let metas: Vec<_> = items
                .iter()
                .filter_map(|i| match i {
                    DumpItem::Meta { command, line } => Some((command.as_str(), *line)),
                    _ => None,
                })
                .collect();
            assert_eq!(
                metas,
                [("\\restrict abc123", 4), ("\\unrestrict abc123", 24)]
            );
            assert!(items.contains(&DumpItem::CopyEnd {
                line: 22,
                data_line: 19
            }));
        }
    }
    #[test]
    fn non_text_and_unterminated_files_are_refused() {
        assert!(
            check_text_start(b"PGDMP\x01\x0e")
                .unwrap_err()
                .to_string()
                .contains("pg_restore")
        );
        assert!(check_text_start(&[0x1f, 0x8b, 8]).is_err());
        let mut s = DumpSplitter::new(Driver::Mysql);
        assert!(s.push(b"SELECT 1;\x00\x01").is_err());
        let mut s = DumpSplitter::new(Driver::Mysql);
        s.push(b"INSERT INTO t VALUES ('open").unwrap();
        assert!(s.finish().unwrap_err().to_string().contains("never closed"));
        let mut s = DumpSplitter::new(Driver::Postgres);
        s.push(b"COPY t FROM stdin;\n1\n").unwrap();
        assert!(s.finish().unwrap_err().to_string().contains("never ends"));
        // A leading UTF-8 BOM is not part of the first statement.
        let items = split(Driver::Postgres, "\u{feff}-- c\nSELECT 1;\n", 1);
        assert_eq!(statements(&items), [("-- c\nSELECT 1".to_owned(), 1)]);
    }
    #[test]
    fn risks_and_other_databases() {
        let m = |sql: &str| classify(sql, Driver::Mysql, "shop");
        assert_eq!(m("USE `shop`").risks, []);
        assert_eq!(
            m("USE other").risks,
            [DumpRisk::OtherDatabase("OTHER".into())]
        );
        assert_eq!(
            m("USE `Other`").risks,
            [DumpRisk::OtherDatabase("Other".into())]
        );
        assert!(
            m("CREATE DATABASE /*!32312 IF NOT EXISTS*/ `x`")
                .risks
                .contains(&DumpRisk::OtherDatabase("x".into()))
        );
        assert_eq!(m("CREATE DATABASE shop").risks, [DumpRisk::CreateDatabase]);
        assert!(
            m("INSERT INTO other.t VALUES (1)")
                .risks
                .contains(&DumpRisk::OtherDatabase("OTHER".into()))
        );
        assert_eq!(m("INSERT INTO `shop`.`t` VALUES (1)").risks, []);
        assert_eq!(m("DROP TABLE IF EXISTS `t`").risks, [DumpRisk::DropTable]);
        assert_eq!(m("/*!40000 DROP DATABASE IF EXISTS `x`*/").risks.len(), 2);
        assert_eq!(m("TRUNCATE t").risks, [DumpRisk::Truncate]);
        assert_eq!(
            m("GRANT ALL ON *.* TO 'u'@'%'").risks,
            [
                DumpRisk::Privileges,
                DumpRisk::Blocked(
                    BlockKind::Roles,
                    "users, roles or privileges (server-wide)".into()
                )
            ]
        );
        assert!(matches!(
            &m("DELETE FROM t").risks[..],
            [DumpRisk::DataChange(_)]
        ));
        assert_eq!(m("INSERT INTO t VALUES ('USE other')").risks, []);
        let p = |sql: &str| classify(sql, Driver::Postgres, "app");
        assert!(p("CREATE INDEX CONCURRENTLY i ON t (a)").no_transaction);
        assert!(p("CREATE DATABASE x").no_transaction);
        assert_eq!(
            p("ALTER TABLE public.t OWNER TO postgres").risks,
            [DumpRisk::Privileges]
        );
        assert_eq!(
            p("SELECT pg_catalog.set_config('search_path', '', false)").risks,
            []
        );
        assert_eq!(
            p("SET search_path = other, public").risks,
            [DumpRisk::SearchPath]
        );
        assert!(meta_command("\\connect app", "app").unwrap());
        assert!(!meta_command("\\connect other", "app").unwrap());
        assert!(meta_command("\\! rm -rf /", "app").is_err());
        // Any qualifier naming another of the server's databases.
        let dbs = ["shop".to_owned(), "zixma".to_owned()];
        let q = |sql: &str| classify_in(sql, Driver::Mysql, "shop", &dbs).risks;
        let zixma = DumpRisk::OtherDatabase("ZIXMA".into());
        for sql in [
            "RENAME TABLE t TO zixma.t",
            "DROP TABLE a, zixma.b",
            "CREATE VIEW zixma.v AS SELECT 1",
            "CREATE INDEX i ON zixma.t (a)",
            "INSERT INTO t SELECT * FROM zixma.u",
            "ALTER DATABASE zixma CHARACTER SET utf8mb4",
        ] {
            assert!(q(sql).contains(&zixma), "{sql}: {:?}", q(sql));
        }
        assert!(
            q("CREATE TABLE `zixma`.`t` (a int)")
                .contains(&DumpRisk::OtherDatabase("zixma".into()))
        );
        assert_eq!(q("SELECT a.x FROM t a"), [], "an alias isn't a database");
        assert_eq!(q("ALTER DATABASE CHARACTER SET utf8mb4"), []);
        assert_eq!(q("INSERT INTO shop.t VALUES (1)"), []);
    }
    #[test]
    fn known_escapes_are_blocked_by_kind() {
        use BlockKind::*;
        let kinds = |sql: &str, d: Driver| -> Vec<BlockKind> {
            classify_in(sql, d, "app", &[])
                .risks
                .iter()
                .filter_map(|r| r.block_kind())
                .collect()
        };
        for (sql, kind) in [
            ("PREPARE p FROM @s", DynamicSql),
            ("EXECUTE p", DynamicSql),
            ("DEALLOCATE PREPARE p", DynamicSql),
            ("CALL p2()", DynamicSql),
            (
                "CREATE PROCEDURE p2() BEGIN SET @s = 'x'; PREPARE x FROM @s; EXECUTE x; END",
                DynamicSql,
            ),
            (
                "/*!50003 CREATE*/ /*!50003 PROCEDURE `p`() BEGIN CALL other(); END */",
                DynamicSql,
            ),
            (
                "LOAD DATA LOCAL INFILE '/etc/passwd' INTO TABLE t",
                ServerFiles,
            ),
            ("SELECT 1 INTO OUTFILE '/tmp/x'", ServerFiles),
            ("SET GLOBAL max_connections = 151", ServerWide),
            ("SET @@global.max_connections = 151", ServerWide),
            ("SET PERSIST max_connections = 151", ServerWide),
            ("INSTALL PLUGIN x SONAME 'x.so'", ServerWide),
            ("SHUTDOWN", ServerWide),
            ("KILL 12", ServerWide),
            ("FLUSH PRIVILEGES", ServerWide),
            ("CREATE USER 'u'@'%'", Roles),
        ] {
            assert_eq!(kinds(sql, Driver::Mysql), [kind], "{sql}");
        }
        for (sql, kind) in [
            (
                "SELECT dblink_exec('dbname=x', 'CREATE TABLE p (id int)')",
                Remote,
            ),
            ("CREATE EXTENSION IF NOT EXISTS dblink", Remote),
            ("CREATE SERVER s FOREIGN DATA WRAPPER postgres_fdw", Remote),
            ("CREATE SUBSCRIPTION s CONNECTION 'x' PUBLICATION p", Remote),
            ("COPY t FROM '/etc/passwd'", ServerFiles),
            ("COPY t TO '/tmp/out'", ServerFiles),
            ("SELECT lo_import('/etc/passwd')", ServerFiles),
            ("SELECT pg_read_file('/etc/passwd')", ServerFiles),
            ("CREATE EXTENSION file_fdw", ServerFiles),
            ("ALTER SYSTEM SET work_mem = '1GB'", ServerWide),
            ("CREATE ROLE r", Roles),
            ("SET ROLE postgres", Roles),
            ("SET SESSION AUTHORIZATION postgres", Roles),
            (
                "DO $$ BEGIN EXECUTE 'CREATE TABLE p (id int)'; END $$",
                DynamicSql,
            ),
            (
                "CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS 'BEGIN EXECUTE ''DROP TABLE t''; END'",
                DynamicSql,
            ),
        ] {
            assert_eq!(kinds(sql, Driver::Postgres), [kind], "{sql}");
        }
        assert!(
            kinds("ALTER DATABASE app SET search_path = x", Driver::Postgres).contains(&ServerWide)
        );
        assert!(
            kinds("ALTER DATABASE other SET search_path = x", Driver::Postgres)
                .contains(&OtherDatabase)
        );
        // Ordinary dump statements stay free.
        for sql in [
            "COPY public.t (id) FROM stdin",
            "CREATE FUNCTION f() RETURNS int LANGUAGE sql AS $$ SELECT 1 $$",
            "ALTER TABLE ONLY public.t OWNER TO postgres",
            "SELECT pg_catalog.setval('s', 1, false)",
        ] {
            assert_eq!(kinds(sql, Driver::Postgres), [], "{sql}");
        }
        assert_eq!(
            unsupported("COPY (SELECT 1) TO PROGRAM 'sh -c id'", Driver::Postgres).is_some(),
            true
        );
        // Transaction control.
        for sql in [
            "BEGIN",
            "COMMIT",
            "END",
            "ROLLBACK",
            "ABORT",
            "SAVEPOINT a",
            "RELEASE a",
            "START TRANSACTION",
            "PREPARE TRANSACTION 'x'",
        ] {
            assert!(
                classify(sql, Driver::Postgres, "app")
                    .risks
                    .contains(&DumpRisk::TransactionControl),
                "{sql}"
            );
        }
        assert!(
            classify("SET autocommit = 0", Driver::Mysql, "app")
                .risks
                .contains(&DumpRisk::TransactionControl)
        );
    }
    #[test]
    fn server_admin_and_privilege_escapes_are_blocked() {
        use BlockKind::*;
        let kinds = |sql: &str, d: Driver| -> Vec<BlockKind> {
            classify_in(sql, d, "app", &[])
                .risks
                .iter()
                .filter_map(|r| r.block_kind())
                .collect()
        };
        for (sql, kind) in [
            (
                "CREATE SERVER fed FOREIGN DATA WRAPPER mysql OPTIONS (HOST '10.0.0.1')",
                Remote,
            ),
            ("ALTER SERVER fed OPTIONS (HOST '10.0.0.2')", Remote),
            ("DROP SERVER IF EXISTS fed", Remote),
            ("CREATE TABLE t (id int) ENGINE=FEDERATED", Remote),
            (
                "CREATE TABLE t (id int) CONNECTION='mysql://u@h/db/t'",
                Remote,
            ),
            (
                "CREATE TABLE t (id int) ENGINE = InnoDB CONNECTION 'fed/t'",
                Remote,
            ),
            ("ALTER TABLE t ENGINE=FEDERATED CONNECTION='fed/t'", Remote),
            ("SET sql_log_bin = 0", ServerWide),
            ("SET SESSION sql_log_bin=0", ServerWide),
            ("SET @@SESSION.SQL_LOG_BIN= 0", ServerWide),
            ("SET @@session.gtid_next = 'AUTOMATIC'", ServerWide),
            ("SET pseudo_thread_id = 5", ServerWide),
            ("SET SESSION binlog_format = 'STATEMENT'", ServerWide),
            ("SET NAMES utf8mb4, sql_log_bin = 0", ServerWide),
            ("SET @@session.`sql_log_bin` = 0", ServerWide),
            ("SET `SQL_LOG_BIN` = 0", ServerWide),
            ("CREATE TABLE t (id int) ENGINE='federated'", Remote),
            ("CREATE TABLE t (id int) ENGINE=`Federated`", Remote),
            (
                "/*!40101 CREATE TABLE t (id int) */ /*!50100 ENGINE=federated */",
                Remote,
            ),
            (
                "CREATE TABLE t (id int) CONNECTION=_utf8mb4'mysql://u@h/db/t'",
                Remote,
            ),
            ("CREATE TABLE t (id int) CONNECTION = X'6d7973716c'", Remote),
        ] {
            assert_eq!(kinds(sql, Driver::Mysql), [kind], "{sql}");
        }
        for sql in [
            "SET FOREIGN_KEY_CHECKS = 0",
            "SET @@SESSION.FOREIGN_KEY_CHECKS=0",
            "SET @MYSQLDUMP_TEMP_LOG_BIN = @@SESSION.SQL_LOG_BIN",
            "CREATE TABLE t (`connection` varchar(10) DEFAULT 'connection')",
            "CREATE TABLE t (id int) ENGINE=InnoDB COMMENT='connection pool'",
            "UPDATE t SET debug = 1",
            "CREATE TABLE t (federated int, `connection` text COMMENT 'x')",
        ] {
            assert_eq!(kinds(sql, Driver::Mysql), [], "{sql}");
        }
        for (sql, kind) in [
            ("LOAD '/tmp/evil.so'", ServerFiles),
            ("LOAD 'auto_explain'", ServerFiles),
            ("SELECT pg_terminate_backend(123)", ServerWide),
            (
                "SELECT pg_catalog.pg_cancel_backend(pid) FROM pg_stat_activity",
                ServerWide,
            ),
            ("SELECT pg_reload_conf()", ServerWide),
            ("SELECT pg_rotate_logfile()", ServerWide),
            ("SELECT pg_promote()", ServerWide),
            ("SELECT pg_switch_wal()", ServerWide),
            ("SELECT pg_drop_replication_slot('s')", ServerWide),
            (
                "CREATE FUNCTION f() RETURNS void LANGUAGE sql AS 'SELECT pg_terminate_backend(1)'",
                ServerWide,
            ),
            ("CREATE PUBLICATION p FOR ALL TABLES", Remote),
            ("ALTER PUBLICATION p ADD TABLE t", Remote),
            ("DROP PUBLICATION p", Remote),
            (
                "CREATE FUNCTION f() RETURNS int LANGUAGE sql SECURITY DEFINER AS $$ SELECT 1 $$",
                Roles,
            ),
            ("ALTER FUNCTION f() SECURITY DEFINER", Roles),
            (
                "CREATE PROCEDURE p() LANGUAGE sql SECURITY DEFINER AS $$ CREATE TABLE x () $$",
                Roles,
            ),
            ("SELECT set_config('role', 'postgres', false)", Roles),
            (
                "SELECT pg_catalog.set_config('session_authorization', 'postgres', false)",
                Roles,
            ),
            (
                "SELECT set_config('search_path', 'evil, public', false)",
                Roles,
            ),
            ("SELECT set_config( 'ROLE' , 'postgres', true)", Roles),
            (
                "SELECT set_config(current_setting('x.name'), 'postgres', false)",
                Roles,
            ),
            ("SET LOCAL ROLE postgres", Roles),
            (
                "SELECT set_config('search_path', '' || 'secret, public', false)",
                Roles,
            ),
            (
                "SELECT pg_catalog.\"set_config\"('role', 'postgres', false)",
                Roles,
            ),
            ("SELECT \"SET_CONFIG\"('role', 'postgres', false)", Roles),
            ("SELECT \"pg_terminate_backend\"(1)", ServerWide),
            ("SELECT pg_catalog.\"pg_reload_conf\"()", ServerWide),
            ("SELECT \"pg_read_file\"('/etc/passwd')", ServerFiles),
            ("ALTER ROUTINE f() SECURITY DEFINER", Roles),
            ("ALTER ROUTINE f() SET role = 'postgres'", Roles),
            (
                "ALTER FUNCTION f() SET session_authorization TO postgres",
                Roles,
            ),
            ("SET LOCAL SESSION AUTHORIZATION postgres", Roles),
            ("SET SESSION ROLE postgres", Roles),
            (
                "CREATE FUNCTION f() RETURNS text LANGUAGE sql SET role = 'postgres' AS $$ SELECT 1 $$",
                Roles,
            ),
            (
                "CREATE FUNCTION f() RETURNS text LANGUAGE sql AS 'SELECT 1' SET session_authorization TO postgres",
                Roles,
            ),
            (
                "CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$ BEGIN SET LOCAL ROLE postgres; END $$",
                Roles,
            ),
            (
                "CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $b$ BEGIN PERFORM set_config('role', 'postgres', true); END $b$",
                Roles,
            ),
        ] {
            assert_eq!(kinds(sql, Driver::Postgres), [kind], "{sql}");
        }
        for sql in [
            "SELECT pg_catalog.set_config('search_path', '', false)",
            "SELECT set_config('statement_timeout', '0', false)",
            "CREATE FUNCTION f() RETURNS int LANGUAGE sql SET search_path TO 'public' AS $$ SELECT 1 $$",
            "CREATE FUNCTION f(r text) RETURNS void LANGUAGE plpgsql AS $$ BEGIN UPDATE u SET role = r; END $$",
            "UPDATE users SET role = 'admin'",
            "INSERT INTO notes VALUES ('SELECT pg_terminate_backend(1); LOAD x; set_config(''role'', ''x'', false)')",
            "CREATE TABLE publication (id int)",
        ] {
            assert_eq!(kinds(sql, Driver::Postgres), [], "{sql}");
        }
    }
    #[test]
    fn statements_the_server_could_read_differently_are_refused() {
        // With NO_BACKSLASH_ESCAPES the server ends the string at `\'` and runs `USE zixma`.
        let sql = "SELECT 'a\\'; USE zixma; SELECT 1; SELECT ''";
        assert!(unsupported(sql, Driver::Mysql).is_some());
        assert_eq!(
            unsupported("INSERT INTO t VALUES ('it''s; ok')", Driver::Mysql),
            None
        );
        assert_eq!(
            unsupported(
                "CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END",
                Driver::Mysql
            ),
            None,
            "a stored program's body is one statement"
        );
        assert!(unsupported("SET standard_conforming_strings = off", Driver::Postgres).is_some());
        assert_eq!(
            unsupported("SET standard_conforming_strings = on", Driver::Postgres),
            None
        );
    }
}
