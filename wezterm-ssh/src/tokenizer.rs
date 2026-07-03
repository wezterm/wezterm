//! Shell-style tokenizer that mirrors OpenSSH's `argv_split` in
//! `openssh-portable/misc.c` (the `argv_split` function around line
//! 2129). Used by the `ssh_config` parser so that quoting and
//! backslash-escape rules match upstream OpenSSH byte-for-byte.
//!
//! Rules (from `misc.c::argv_split`):
//!
//! - Tokens are separated by ASCII space or tab outside of quotes.
//! - `"..."` and `'...'` are both valid quoting and are treated
//!   identically. Quotes are removed; their content is taken
//!   literally.
//! - Backslash escapes: `\\`, `\"` and `\'` work inside and outside
//!   quotes. `\ ` (backslash followed by a space) only works outside
//!   quotes. Any other backslash is kept as a literal character.
//! - If `terminate_on_comment` is `true`, an unquoted `#` terminates
//!   the input — the rest of the line is ignored.
//! - An unterminated quote is a hard error
//!   (`TokenizeError::UnbalancedQuote`).

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TokenizeError {
    #[error("unbalanced quotes in value")]
    UnbalancedQuote,
}

/// Split `s` into tokens using OpenSSH's `argv_split` rules.
///
/// When `terminate_on_comment` is `true`, an unquoted `#` ends the
/// input at that position. Returns `UnbalancedQuote` if a quoted
/// section is never closed.
pub fn argv_split(s: &str, terminate_on_comment: bool) -> Result<Vec<String>, TokenizeError> {
    let bytes = s.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0usize;

    while i < bytes.len() {
        let c = bytes[i];
        if c == b' ' || c == b'\t' {
            i += 1;
            continue;
        }
        if terminate_on_comment && c == b'#' {
            break;
        }

        let mut buf: Vec<u8> = Vec::new();
        let mut quote: u8 = 0;

        while i < bytes.len() {
            let c = bytes[i];
            if c == b'\\' {
                let next = bytes.get(i + 1).copied();
                let recognised = match next {
                    Some(b'\'') | Some(b'"') | Some(b'\\') => next,
                    Some(b' ') if quote == 0 => Some(b' '),
                    _ => None,
                };
                match recognised {
                    Some(b) => {
                        buf.push(b);
                        i += 2;
                    }
                    None => {
                        // Unrecognised escape: emit the backslash
                        // literally and re-process the following byte
                        // in the next iteration, matching the
                        // `arg[j++] = s[i]; i++` path in misc.c.
                        buf.push(b'\\');
                        i += 1;
                    }
                }
                continue;
            }
            if quote == 0 && (c == b' ' || c == b'\t') {
                break;
            }
            if quote == 0 && (c == b'"' || c == b'\'') {
                quote = c;
                i += 1;
                continue;
            }
            if quote != 0 && c == quote {
                quote = 0;
                i += 1;
                continue;
            }
            buf.push(c);
            i += 1;
        }

        if quote != 0 {
            return Err(TokenizeError::UnbalancedQuote);
        }
        // The token was accumulated transparently from valid UTF-8
        // input bytes, so re-interpreting it as `String` is
        // infallible.
        out.push(String::from_utf8(buf).expect("tokenizer preserves UTF-8"));
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(s: &str) -> Vec<String> {
        argv_split(s, true).expect("split succeeded")
    }

    #[test]
    fn empty_input() {
        assert_eq!(split(""), Vec::<String>::new());
    }

    #[test]
    fn only_whitespace() {
        assert_eq!(split("   \t  "), Vec::<String>::new());
    }

    #[test]
    fn simple_tokens() {
        assert_eq!(split("foo bar baz"), vec!["foo", "bar", "baz"]);
    }

    #[test]
    fn tab_separator() {
        assert_eq!(split("foo\tbar"), vec!["foo", "bar"]);
    }

    #[test]
    fn leading_whitespace_skipped() {
        assert_eq!(split("   foo bar"), vec!["foo", "bar"]);
    }

    #[test]
    fn double_quoted_preserves_space() {
        assert_eq!(split(r#""foo bar" baz"#), vec!["foo bar", "baz"]);
    }

    #[test]
    fn single_quoted_preserves_space() {
        assert_eq!(split(r#"'foo bar' baz"#), vec!["foo bar", "baz"]);
    }

    #[test]
    fn mixed_quote_styles() {
        // Each token uses a different quote style; both are stripped.
        assert_eq!(split(r#"'"' "'""#), vec![r#"""#, r#"'"#]);
    }

    #[test]
    fn quote_concatenation_forms_single_token() {
        // `a'b c'd` → `ab cd`: the single-quoted section is glued to
        // the unquoted neighbours, matching shell-style concat.
        assert_eq!(split(r#"a'b c'd"#), vec!["ab cd"]);
    }

    #[test]
    fn empty_quotes_produce_empty_token() {
        assert_eq!(split(r#""""#), vec![""]);
    }

    #[test]
    fn backslash_space_outside_quotes() {
        assert_eq!(split(r"foo\ bar baz"), vec!["foo bar", "baz"]);
    }

    #[test]
    fn backslash_space_inside_quotes_stays_literal() {
        // Inside quotes, `\space` is NOT a recognised escape, so the
        // backslash is kept literal and the space is taken as a
        // normal quoted character.
        assert_eq!(split(r#""foo\ bar""#), vec![r"foo\ bar"]);
    }

    #[test]
    fn backslash_backslash_consumed() {
        assert_eq!(split(r"foo\\bar"), vec![r"foo\bar"]);
    }

    #[test]
    fn backslash_double_quote_inside_double_quote() {
        assert_eq!(split(r#""a\"b""#), vec![r#"a"b"#]);
    }

    #[test]
    fn backslash_single_quote_inside_single_quote() {
        assert_eq!(split(r"'a\'b'"), vec!["a'b"]);
    }

    #[test]
    fn unrecognised_escape_keeps_backslash() {
        assert_eq!(split(r"foo\xbar"), vec![r"foo\xbar"]);
    }

    #[test]
    fn trailing_backslash_kept_literal() {
        assert_eq!(split(r"foo\"), vec![r"foo\"]);
    }

    #[test]
    fn comment_terminates_with_flag() {
        assert_eq!(
            argv_split("foo bar # baz qux", true).unwrap(),
            vec!["foo", "bar"]
        );
    }

    #[test]
    fn comment_not_terminator_without_flag() {
        assert_eq!(
            argv_split("foo bar # baz", false).unwrap(),
            vec!["foo", "bar", "#", "baz"]
        );
    }

    #[test]
    fn comment_inside_quotes_is_literal() {
        assert_eq!(
            argv_split(r#""foo # bar" baz"#, true).unwrap(),
            vec!["foo # bar", "baz"]
        );
    }

    #[test]
    fn unbalanced_double_quote_errors() {
        assert_eq!(
            argv_split(r#""unclosed"#, true),
            Err(TokenizeError::UnbalancedQuote)
        );
    }

    #[test]
    fn unbalanced_single_quote_errors() {
        assert_eq!(
            argv_split(r"'unclosed", true),
            Err(TokenizeError::UnbalancedQuote)
        );
    }

    #[test]
    fn realistic_identity_file_quoted_space() {
        // Matches the ssh -G ground truth for:
        //   IdentityFile "/home/me/.ssh/path with space/id_rsa"
        assert_eq!(
            split(r#""/home/me/.ssh/path with space/id_rsa""#),
            vec!["/home/me/.ssh/path with space/id_rsa"]
        );
    }

    #[test]
    fn realistic_identity_file_escaped_space() {
        // Matches the ssh -G ground truth for:
        //   IdentityFile /home/me/.ssh/path\ with\ space/id_rsa
        assert_eq!(
            split(r"/home/me/.ssh/path\ with\ space/id_rsa"),
            vec!["/home/me/.ssh/path with space/id_rsa"]
        );
    }

    #[test]
    fn realistic_identity_file_two_tokens_one_line() {
        // `IdentityFile /a /b` tokenises to two tokens; the caller
        // (the config parser) is responsible for rejecting extra
        // arguments for directives that only accept one.
        assert_eq!(
            split("/home/me/.ssh/first /home/me/.ssh/second"),
            vec!["/home/me/.ssh/first", "/home/me/.ssh/second"]
        );
    }

    #[test]
    fn utf8_content_preserved() {
        // Non-ASCII bytes outside of quoting punctuation pass through
        // transparently and round-trip as valid UTF-8 `String`s.
        assert_eq!(split("föö bär"), vec!["föö", "bär"]);
        assert_eq!(split(r#""日本 語""#), vec!["日本 語"]);
    }
}
