use std::collections::HashSet;

use ratatui::style::{Modifier, Style};

use crate::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenType {
    Keyword,
    String,
    Number,
    Function,
    ConstantOrType,
    Comment,
    Punctuation,
    Text,
}

impl TokenType {
    pub fn style(self, theme: &Theme, is_current_line: bool) -> Style {
        let bg = if is_current_line {
            theme.cur_line
        } else {
            theme.canvas
        };
        let (fg, modifier) = match self {
            Self::Keyword => (theme.keyword, Modifier::BOLD),
            Self::String => (theme.string, Modifier::empty()),
            Self::Number => (theme.number, Modifier::empty()),
            Self::Function => (theme.function, Modifier::empty()),
            Self::ConstantOrType => (theme.constant, Modifier::empty()),
            Self::Comment => (theme.comment, Modifier::empty()),
            Self::Punctuation => (theme.punctuation, Modifier::empty()),
            Self::Text => (theme.text, Modifier::empty()),
        };
        Style::default().fg(fg).bg(bg).add_modifier(modifier)
    }
}

pub struct Token<'a> {
    pub token_type: TokenType,
    pub text: &'a str,
}

pub fn tokenize_line<'a>(line: &'a str, lang: &str) -> Vec<Token<'a>> {
    if line.is_empty() {
        return Vec::new();
    }

    if lang == "Plain Text" || lang.is_empty() {
        return vec![Token {
            token_type: TokenType::Text,
            text: line,
        }];
    }

    let mut tokens = Vec::new();
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    let keywords = get_keywords(lang);
    let uses_hash_comment = matches!(lang, "Python" | "Shell" | "YAML" | "TOML" | "Terraform");

    while i < len {
        let prev_i = i;
        // Line comments
        if !uses_hash_comment && i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            tokens.push(Token {
                token_type: TokenType::Comment,
                text: &line[i..],
            });
            break;
        }

        if uses_hash_comment && bytes[i] == b'#' {
            tokens.push(Token {
                token_type: TokenType::Comment,
                text: &line[i..],
            });
            break;
        }

        // Strings
        if bytes[i] == b'"'
            || bytes[i] == b'\''
            || (bytes[i] == b'`'
                && matches!(
                    lang,
                    "JavaScript" | "TypeScript" | "React JSX" | "React TSX" | "Markdown"
                ))
        {
            let quote = bytes[i];
            let start = i;
            i += 1;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else if bytes[i] == quote {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            tokens.push(Token {
                token_type: TokenType::String,
                text: &line[start..i],
            });
            continue;
        }

        // Whitespace
        if bytes[i].is_ascii_whitespace() {
            let start = i;
            while i < len && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            tokens.push(Token {
                token_type: TokenType::Text,
                text: &line[start..i],
            });
            continue;
        }

        // Numbers
        if bytes[i].is_ascii_digit() && (i == 0 || !is_ident_char(bytes[i - 1])) {
            let start = i;
            let mut is_hex = false;
            if i + 1 < len && bytes[i] == b'0' && (bytes[i + 1] == b'x' || bytes[i + 1] == b'X') {
                is_hex = true;
                i += 2;
            }
            while i < len {
                let b = bytes[i];
                let valid = if is_hex {
                    b.is_ascii_hexdigit()
                } else {
                    b.is_ascii_digit() || b == b'.' || b == b'_'
                };
                if valid {
                    i += 1;
                } else {
                    break;
                }
            }
            tokens.push(Token {
                token_type: TokenType::Number,
                text: &line[start..i],
            });
            continue;
        }

        // Identifiers / Words
        if is_ident_start(bytes[i]) {
            let start = i;
            while i < len && is_ident_char(bytes[i]) {
                i += 1;
            }
            let word = &line[start..i];

            // Look ahead for function call: word followed by '(' or '!('
            let mut lookahead = i;
            while lookahead < len && bytes[lookahead].is_ascii_whitespace() {
                lookahead += 1;
            }
            let is_fn_or_macro = (lookahead < len && bytes[lookahead] == b'(')
                || (lookahead + 1 < len
                    && bytes[lookahead] == b'!'
                    && bytes[lookahead + 1] == b'(');

            let token_type = if keywords.contains(word) {
                TokenType::Keyword
            } else if is_fn_or_macro && !matches!(word, "if" | "while" | "for" | "match") {
                TokenType::Function
            } else if is_constant_or_type(word) {
                TokenType::ConstantOrType
            } else {
                TokenType::Text
            };

            tokens.push(Token {
                token_type,
                text: word,
            });
            continue;
        }

        // Punctuation and operators
        let start = i;
        while i < len
            && !bytes[i].is_ascii_whitespace()
            && !is_ident_start(bytes[i])
            && !bytes[i].is_ascii_digit()
            && bytes[i] != b'"'
            && bytes[i] != b'\''
            && !(bytes[i] == b'`'
                && matches!(
                    lang,
                    "JavaScript" | "TypeScript" | "React JSX" | "React TSX" | "Markdown"
                ))
            && !(!uses_hash_comment && bytes[i] == b'/' && i + 1 < len && bytes[i + 1] == b'/')
            && !(uses_hash_comment && bytes[i] == b'#')
        {
            i += 1;
        }

        if start < i {
            tokens.push(Token {
                token_type: TokenType::Punctuation,
                text: &line[start..i],
            });
            continue;
        }

        // Guaranteed progress: advance at least one UTF-8 scalar so the loop never hangs
        if i == prev_i {
            let ch_len = line[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            let next_i = (i + ch_len).min(len);
            tokens.push(Token {
                token_type: TokenType::Punctuation,
                text: &line[i..next_i],
            });
            i = next_i;
        }
    }

    tokens
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$'
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

fn is_constant_or_type(word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let first = word.as_bytes()[0];
    if first.is_ascii_uppercase() {
        return true;
    }
    // UPPER_CASE constants
    word.len() > 1
        && word
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b == b'_' || b.is_ascii_digit())
}

fn get_keywords(lang: &str) -> HashSet<&'static str> {
    match lang {
        "Rust" => [
            "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
            "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
            "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super",
            "trait", "true", "type", "unsafe", "use", "where", "while",
        ]
        .into_iter()
        .collect(),
        "Python" => [
            "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
            "elif", "else", "except", "False", "finally", "for", "from", "global", "if", "import",
            "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return",
            "True", "try", "while", "with", "yield",
        ]
        .into_iter()
        .collect(),
        "JavaScript" | "TypeScript" | "React JSX" | "React TSX" => [
            "async",
            "await",
            "break",
            "case",
            "catch",
            "class",
            "const",
            "continue",
            "debugger",
            "default",
            "delete",
            "do",
            "else",
            "export",
            "extends",
            "false",
            "finally",
            "for",
            "function",
            "if",
            "import",
            "in",
            "instanceof",
            "interface",
            "let",
            "new",
            "null",
            "return",
            "super",
            "switch",
            "this",
            "throw",
            "true",
            "try",
            "type",
            "typeof",
            "undefined",
            "var",
            "void",
            "while",
            "with",
            "yield",
        ]
        .into_iter()
        .collect(),
        "Shell" => [
            "if", "then", "else", "elif", "fi", "case", "esac", "for", "while", "until", "do",
            "done", "in", "function", "select", "time", "return", "exit", "local", "export",
        ]
        .into_iter()
        .collect(),
        "Terraform" => [
            "terraform",
            "provider",
            "resource",
            "data",
            "module",
            "variable",
            "output",
            "locals",
            "backend",
            "required_providers",
            "for_each",
            "count",
            "depends_on",
            "lifecycle",
            "dynamic",
            "true",
            "false",
            "null",
        ]
        .into_iter()
        .collect(),
        "JSON" => ["true", "false", "null"].into_iter().collect(),
        _ => HashSet::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_rust_code_accurately() {
        let tokens = tokenize_line("let message = \"hello\"; // note", "Rust");
        assert_eq!(tokens[0].token_type, TokenType::Keyword);
        assert_eq!(tokens[0].text, "let");
        assert_eq!(tokens[1].text, " ");
        assert_eq!(tokens[2].token_type, TokenType::Text);
        assert_eq!(tokens[2].text, "message");
        assert_eq!(tokens[4].token_type, TokenType::Punctuation);
        assert_eq!(tokens[4].text, "=");
        assert_eq!(tokens[6].token_type, TokenType::String);
        assert_eq!(tokens[6].text, "\"hello\"");
        assert_eq!(tokens[9].token_type, TokenType::Comment);
        assert_eq!(tokens[9].text, "// note");
    }

    #[test]
    fn tokenizes_python_code() {
        let tokens = tokenize_line("def add(x, y): # comment", "Python");
        assert_eq!(tokens[0].token_type, TokenType::Keyword);
        assert_eq!(tokens[0].text, "def");
        assert_eq!(tokens[2].token_type, TokenType::Function);
        assert_eq!(tokens[2].text, "add");
        let comment = tokens.last().unwrap();
        assert_eq!(comment.token_type, TokenType::Comment);
        assert_eq!(comment.text, "# comment");
    }

    #[test]
    fn myn_e01_tokenizer_progress_guarantee_and_special_cases() {
        // Backtick in Plain Text
        let tokens = tokenize_line("`test`", "Plain Text");
        assert_eq!(tokens[0].text, "`test`");

        // Backtick in Rust
        let tokens = tokenize_line("`backtick` in rust", "Rust");
        let reconstructed: String = tokens.iter().map(|t| t.text).collect();
        assert_eq!(reconstructed, "`backtick` in rust");

        // Integer division // in Python
        let tokens = tokenize_line("x = 5 // 2", "Python");
        let reconstructed: String = tokens.iter().map(|t| t.text).collect();
        assert_eq!(reconstructed, "x = 5 // 2");

        // Unquoted // in Shell / YAML
        let tokens = tokenize_line("path = https://example.com", "YAML");
        let reconstructed: String = tokens.iter().map(|t| t.text).collect();
        assert_eq!(reconstructed, "path = https://example.com");

        // Property: full text is preserved byte-for-byte without hanging
        let weird = "` \\ / // # /* */ @@@ €€€ 汉字 🚀";
        for lang in &[
            "Rust",
            "Python",
            "JavaScript",
            "Shell",
            "JSON",
            "Plain Text",
        ] {
            let tokens = tokenize_line(weird, lang);
            let reconstructed: String = tokens.iter().map(|t| t.text).collect();
            assert_eq!(&reconstructed, weird, "Failed on lang {lang}");
        }
    }
}
