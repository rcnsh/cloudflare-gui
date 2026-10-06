//! Decides whether a SQL batch could modify a D1 database.
//!
//! The app is read-only by default, so this is a safety gate rather than a
//! parser: when in doubt it answers "write" and the user has to opt in. Only
//! plain `SELECT` (including `WITH ... SELECT`), `EXPLAIN ...` and getter-style
//! `PRAGMA`s count as reads.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementKind {
    Read,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    /// The statement text with surrounding whitespace trimmed and no trailing `;`.
    pub text: String,
    /// Leading keyword in upper case, e.g. `SELECT`, `DELETE`, or `?` when the
    /// statement does not start with a keyword.
    pub keyword: String,
    pub kind: StatementKind,
    /// Why a statement was treated as a write, shown in the confirmation dialog.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub statements: Vec<Statement>,
}

impl Classification {
    pub fn is_empty(&self) -> bool {
        self.statements.is_empty()
    }

    pub fn is_read_only(&self) -> bool {
        self.statements
            .iter()
            .all(|s| s.kind == StatementKind::Read)
    }

    pub fn writes(&self) -> impl Iterator<Item = &Statement> {
        self.statements
            .iter()
            .filter(|s| s.kind == StatementKind::Write)
    }
}

impl fmt::Display for StatementKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StatementKind::Read => f.write_str("read"),
            StatementKind::Write => f.write_str("write"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    Punct(char),
    /// String literals, quoted identifiers, numbers and blobs. Their contents never
    /// matter for classification, which is the point of tokenizing at all.
    Opaque,
}

#[derive(Debug)]
struct RawStatement {
    text: String,
    tokens: Vec<Token>,
    /// A quote or block comment was never closed, so the token stream can't be trusted.
    unterminated: bool,
}

/// Pragmas that take an argument but only read. Anything else called with an
/// argument (`PRAGMA user_version(3)`) is a setter.
const READ_PRAGMAS_WITH_ARGS: &[&str] = &[
    "TABLE_INFO",
    "TABLE_XINFO",
    "TABLE_LIST",
    "INDEX_LIST",
    "INDEX_INFO",
    "INDEX_XINFO",
    "FOREIGN_KEY_LIST",
    "FOREIGN_KEY_CHECK",
    "INTEGRITY_CHECK",
    "QUICK_CHECK",
];

/// Pragmas that change state even when called with no argument.
const SIDE_EFFECT_PRAGMAS: &[&str] = &[
    "OPTIMIZE",
    "WAL_CHECKPOINT",
    "INCREMENTAL_VACUUM",
    "SHRINK_MEMORY",
];

pub fn classify(sql: &str) -> Classification {
    let statements = split(sql)
        .into_iter()
        .filter(|s| !s.tokens.is_empty())
        .map(classify_statement)
        .collect();
    Classification { statements }
}

fn classify_statement(raw: RawStatement) -> Statement {
    let keyword = match raw.tokens.first() {
        Some(Token::Word(w)) => w.clone(),
        _ => "?".to_string(),
    };
    let write = |reason: &str| Statement {
        text: raw.text.clone(),
        keyword: keyword.clone(),
        kind: StatementKind::Write,
        reason: Some(reason.to_string()),
    };
    if raw.unterminated {
        return write("unterminated quote or comment");
    }
    let verdict = match keyword.as_str() {
        "SELECT" => Ok(()),
        "EXPLAIN" => Ok(()),
        "WITH" => classify_with(&raw.tokens),
        "PRAGMA" => classify_pragma(&raw.tokens),
        "?" => Err("does not start with a keyword".to_string()),
        other => Err(format!("{other} statement")),
    };
    match verdict {
        Ok(()) => Statement {
            text: raw.text,
            keyword,
            kind: StatementKind::Read,
            reason: None,
        },
        Err(reason) => write(&reason),
    }
}

/// A CTE can front any DML statement, so find the statement keyword that follows
/// the CTE definitions at the top nesting level.
fn classify_with(tokens: &[Token]) -> Result<(), String> {
    let mut depth = 0i32;
    for token in &tokens[1..] {
        match token {
            Token::Punct('(') => depth += 1,
            Token::Punct(')') => depth -= 1,
            Token::Word(w) if depth == 0 => match w.as_str() {
                "SELECT" => return Ok(()),
                "INSERT" | "UPDATE" | "DELETE" | "REPLACE" | "VALUES" => {
                    return Err(format!("WITH ... {w} statement"));
                }
                _ => {}
            },
            _ => {}
        }
    }
    Err("WITH without a SELECT".to_string())
}

fn classify_pragma(tokens: &[Token]) -> Result<(), String> {
    // PRAGMA [schema.]name [= value | (value)]
    let mut rest = &tokens[1..];
    let name = match rest {
        [
            Token::Word(_),
            Token::Punct('.'),
            Token::Word(name),
            tail @ ..,
        ] => {
            rest = tail;
            name
        }
        [Token::Word(name), tail @ ..] => {
            rest = tail;
            name
        }
        _ => return Err("PRAGMA without a name".to_string()),
    };
    if SIDE_EFFECT_PRAGMAS.contains(&name.as_str()) {
        return Err(format!("PRAGMA {} has side effects", name.to_lowercase()));
    }
    match rest.first() {
        None => Ok(()),
        Some(Token::Punct('=')) => Err(format!("PRAGMA {} assignment", name.to_lowercase())),
        Some(Token::Punct('(')) if READ_PRAGMAS_WITH_ARGS.contains(&name.as_str()) => Ok(()),
        Some(Token::Punct('(')) => Err(format!(
            "PRAGMA {} with an argument may change settings",
            name.to_lowercase()
        )),
        Some(_) => Err("unexpected PRAGMA syntax".to_string()),
    }
}

/// Splits on `;` outside literals and comments, tokenizing as it goes.
fn split(sql: &str) -> Vec<RawStatement> {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = Vec::new();
    let mut tokens = Vec::new();
    let mut start = 0;
    let mut unterminated = false;
    let mut i = 0;

    let flush = |out: &mut Vec<RawStatement>,
                 tokens: &mut Vec<Token>,
                 from: usize,
                 to: usize,
                 unterminated: bool| {
        let text: String = chars[from..to].iter().collect();
        out.push(RawStatement {
            text: text.trim().to_string(),
            tokens: std::mem::take(tokens),
            unterminated,
        });
    };

    while i < chars.len() {
        let c = chars[i];
        match c {
            ';' => {
                flush(&mut out, &mut tokens, start, i, unterminated);
                unterminated = false;
                i += 1;
                start = i;
            }
            '-' if chars.get(i + 1) == Some(&'-') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => match find_from(&chars, i + 2, &['*', '/']) {
                Some(end) => i = end + 2,
                None => {
                    unterminated = true;
                    i = chars.len();
                }
            },
            '\'' | '"' | '`' => {
                match skip_quoted(&chars, i, c) {
                    Some(end) => i = end,
                    None => {
                        unterminated = true;
                        i = chars.len();
                    }
                }
                tokens.push(Token::Opaque);
            }
            '[' => {
                match chars[i + 1..].iter().position(|&ch| ch == ']') {
                    Some(offset) => i += offset + 2,
                    None => {
                        unterminated = true;
                        i = chars.len();
                    }
                }
                tokens.push(Token::Opaque);
            }
            c if c.is_whitespace() => i += 1,
            c if c.is_ascii_digit() => {
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
                    i += 1;
                }
                tokens.push(Token::Opaque);
            }
            c if is_word_char(c) => {
                let begin = i;
                while i < chars.len() && is_word_char(chars[i]) {
                    i += 1;
                }
                let word: String = chars[begin..i].iter().collect();
                tokens.push(Token::Word(word.to_ascii_uppercase()));
            }
            other => {
                tokens.push(Token::Punct(other));
                i += 1;
            }
        }
    }
    flush(&mut out, &mut tokens, start, chars.len(), unterminated);
    out
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Returns the index just past the closing quote. A doubled quote is an escape.
fn skip_quoted(chars: &[char], open: usize, quote: char) -> Option<usize> {
    let mut i = open + 1;
    while i < chars.len() {
        if chars[i] == quote {
            if chars.get(i + 1) == Some(&quote) {
                i += 2;
                continue;
            }
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

fn find_from(chars: &[char], from: usize, needle: &[char]) -> Option<usize> {
    (from..chars.len().saturating_sub(needle.len() - 1))
        .find(|&i| chars[i..i + needle.len()] == *needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_only(sql: &str) -> bool {
        let c = classify(sql);
        !c.is_empty() && c.is_read_only()
    }

    #[track_caller]
    fn assert_read(sql: &str) {
        assert!(
            read_only(sql),
            "expected read-only: {sql:?} -> {:?}",
            classify(sql)
        );
    }

    #[track_caller]
    fn assert_write(sql: &str) {
        let c = classify(sql);
        assert!(!c.is_empty(), "expected statements in {sql:?}");
        assert!(!c.is_read_only(), "expected write: {sql:?} -> {c:?}");
    }

    #[test]
    fn plain_selects_are_reads() {
        assert_read("SELECT 1");
        assert_read("select * from users");
        assert_read("  SeLeCt id, name FROM users WHERE id = 1  ");
        assert_read("SELECT * FROM t;");
        assert_read("SELECT count(*) FROM sqlite_master WHERE type='table'");
        assert_read("SELECT * FROM a; SELECT * FROM b;");
    }

    #[test]
    fn explain_is_a_read_even_for_writes() {
        assert_read("EXPLAIN SELECT 1");
        assert_read("EXPLAIN QUERY PLAN SELECT * FROM t WHERE x = 1");
        assert_read("explain query plan delete from t");
    }

    #[test]
    fn dml_and_ddl_are_writes() {
        for sql in [
            "INSERT INTO t VALUES (1)",
            "insert or replace into t(a) values (1)",
            "REPLACE INTO t VALUES (1)",
            "UPDATE t SET a = 1",
            "DELETE FROM t",
            "CREATE TABLE t (id INTEGER)",
            "CREATE INDEX i ON t(a)",
            "DROP TABLE t",
            "ALTER TABLE t ADD COLUMN b TEXT",
            "VACUUM",
            "ANALYZE",
            "REINDEX",
            "ATTACH DATABASE 'x' AS y",
            "DETACH y",
            "BEGIN",
            "COMMIT",
            "ROLLBACK",
            "SAVEPOINT a",
            "VALUES (1, 2)",
        ] {
            assert_write(sql);
        }
    }

    #[test]
    fn any_write_in_a_batch_makes_it_a_write() {
        assert_write("SELECT 1; DELETE FROM t");
        assert_write("DELETE FROM t; SELECT 1");
        let c = classify("SELECT 1; DROP TABLE t; SELECT 2");
        assert_eq!(c.statements.len(), 3);
        let writes: Vec<_> = c.writes().map(|s| s.keyword.as_str()).collect();
        assert_eq!(writes, ["DROP"]);
    }

    #[test]
    fn ctes_are_classified_by_their_final_statement() {
        assert_read("WITH x AS (SELECT 1) SELECT * FROM x");
        assert_read(
            "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM c WHERE n<5) SELECT n FROM c",
        );
        assert_read("with a as (select 1), b as materialized (select 2) select * from a, b");
        assert_write("WITH x AS (SELECT id FROM t) DELETE FROM t WHERE id IN (SELECT id FROM x)");
        assert_write("WITH x AS (SELECT 1) INSERT INTO t SELECT * FROM x");
        assert_write("WITH x AS (SELECT 1) UPDATE t SET a = (SELECT * FROM x)");
        assert_write("WITH x AS (SELECT 1) REPLACE INTO t SELECT * FROM x");
        assert_write("WITH x AS (SELECT 1)");
    }

    #[test]
    fn keywords_inside_literals_and_comments_are_ignored() {
        assert_read("SELECT 'DELETE FROM t; DROP TABLE x' AS s");
        assert_read("SELECT \"delete\" FROM t");
        assert_read("SELECT `drop` FROM [insert]");
        assert_read("-- DROP TABLE t\nSELECT 1");
        assert_read("/* DELETE FROM t; */ SELECT 1");
        assert_read("SELECT 1 -- ; DELETE FROM t");
        assert_read("SELECT 'it''s; DELETE' FROM t");
        assert_write("/* SELECT */ DELETE FROM t");
        assert_write("-- SELECT\nDELETE FROM t");
    }

    #[test]
    fn semicolons_in_strings_do_not_split_statements() {
        let c = classify("SELECT ';' ; SELECT 2");
        assert_eq!(c.statements.len(), 2);
        assert_eq!(c.statements[0].text, "SELECT ';'");
    }

    #[test]
    fn unterminated_input_is_treated_as_a_write() {
        assert_write("SELECT 'oops");
        assert_write("SELECT \"oops");
        assert_write("SELECT 1 /* never closed");
        assert_write("SELECT [oops");
    }

    #[test]
    fn empty_input_has_no_statements() {
        assert!(classify("").is_empty());
        assert!(classify("   ;  ; ").is_empty());
        assert!(classify("-- just a comment").is_empty());
        assert!(classify("/* block */").is_empty());
        assert!(!read_only(""));
    }

    #[test]
    fn pragma_getters_are_reads() {
        assert_read("PRAGMA table_info(users)");
        assert_read("PRAGMA table_info('users')");
        assert_read("pragma main.table_info(users)");
        assert_read("PRAGMA table_list");
        assert_read("PRAGMA index_list(users)");
        assert_read("PRAGMA foreign_key_list(users)");
        assert_read("PRAGMA user_version");
        assert_read("PRAGMA foreign_keys");
        assert_read("PRAGMA quick_check");
    }

    #[test]
    fn pragma_setters_and_side_effects_are_writes() {
        assert_write("PRAGMA user_version = 3");
        assert_write("PRAGMA foreign_keys = ON");
        assert_write("PRAGMA main.user_version = 3");
        assert_write("PRAGMA user_version(3)");
        assert_write("PRAGMA defer_foreign_keys(1)");
        assert_write("PRAGMA optimize");
        assert_write("PRAGMA wal_checkpoint(TRUNCATE)");
        assert_write("PRAGMA incremental_vacuum");
        assert_write("PRAGMA");
    }

    #[test]
    fn statements_that_do_not_start_with_a_keyword_are_writes() {
        assert_write("(SELECT 1)");
        assert_write("'SELECT'");
    }

    #[test]
    fn triggers_with_inner_semicolons_are_writes() {
        let sql = "CREATE TRIGGER t AFTER INSERT ON a BEGIN UPDATE b SET x = 1; END;";
        assert_write(sql);
        assert!(classify(sql).writes().count() >= 1);
    }

    #[test]
    fn write_reasons_name_the_statement() {
        let c = classify("DELETE FROM t");
        assert_eq!(c.statements[0].keyword, "DELETE");
        assert_eq!(c.statements[0].reason.as_deref(), Some("DELETE statement"));
        let c = classify("PRAGMA foreign_keys = OFF");
        assert_eq!(
            c.statements[0].reason.as_deref(),
            Some("PRAGMA foreign_keys assignment")
        );
    }

    #[test]
    fn unicode_identifiers_do_not_break_tokenizing() {
        assert_read("SELECT ünïcødé FROM tablé WHERE naïve = '☃'");
        assert_write("DELETE FROM tablé");
    }
}
