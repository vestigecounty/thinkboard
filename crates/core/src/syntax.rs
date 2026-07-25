//! A tiny, dependency-free syntax highlighter for fenced code blocks.
//!
//! It is a generic lexical tokenizer, not a real parser: it recognizes
//! strings, numbers, comments, and per-language keyword/type sets, plus a
//! `name(` → function heuristic. That is plenty for the small code previews
//! rendered inside node cards, and it stays deterministic and testable
//! without pulling in a heavyweight grammar engine.

/// The lexical class of a code run, mapped to a color by the rasterizer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    /// Ordinary code (operators, punctuation, plain identifiers).
    Plain,
    Keyword,
    Type,
    String,
    Number,
    Comment,
    Function,
}

/// Per-language lexical rules.
struct Lang {
    line_comment: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    keywords: &'static [&'static str],
    types: &'static [&'static str],
}

/// Splits `code` into `(text, kind)` runs. The concatenation of the returned
/// texts is exactly `code`. Unknown languages still get strings, numbers, and
/// C-style comments highlighted generically.
pub fn highlight(code: &str, lang: &str) -> Vec<(String, Syntax)> {
    let cfg = Lang::for_tag(lang);
    let n = code.len();
    let mut runs: Vec<(usize, usize, Syntax)> = Vec::new();
    let mut i = 0;

    while i < n {
        let rest = &code[i..];

        if cfg.line_comment.iter().any(|m| rest.starts_with(m)) {
            let end = rest.find('\n').map_or(n, |p| i + p);
            runs.push((i, end, Syntax::Comment));
            i = end;
            continue;
        }
        if let Some((open, close)) = cfg.block_comment.filter(|(open, _)| rest.starts_with(open)) {
            let after = i + open.len();
            let end = code[after..].find(close).map_or(n, |p| after + p + close.len());
            runs.push((i, end, Syntax::Comment));
            i = end;
            continue;
        }

        let c = rest.chars().next().unwrap();
        if c == '"' || c == '\'' || c == '`' {
            let end = scan_string(code, i, c);
            runs.push((i, end, Syntax::String));
            i = end;
        } else if c.is_ascii_digit() {
            let end = scan_while(code, i, |c| {
                c.is_ascii_alphanumeric() || c == '.' || c == '_'
            });
            runs.push((i, end, Syntax::Number));
            i = end;
        } else if c == '_' || c.is_alphabetic() {
            let end = scan_while(code, i, |c| c == '_' || c.is_alphanumeric());
            let word = &code[i..end];
            runs.push((i, end, classify(word, &code[end..], &cfg)));
            i = end;
        } else {
            let len = c.len_utf8();
            runs.push((i, i + len, Syntax::Plain));
            i += len;
        }
    }

    coalesce(code, runs)
}

/// Classifies an identifier: keyword / type / function-call / plain.
fn classify(word: &str, after: &str, cfg: &Lang) -> Syntax {
    if cfg.keywords.contains(&word) {
        Syntax::Keyword
    } else if cfg.types.contains(&word) {
        Syntax::Type
    } else if after.trim_start().starts_with('(') {
        Syntax::Function
    } else {
        Syntax::Plain
    }
}

/// Index just past a string literal opened by `quote` at `start`, honoring
/// backslash escapes; runs to end-of-input if unterminated.
fn scan_string(code: &str, start: usize, quote: char) -> usize {
    let mut chars = code[start..].char_indices();
    chars.next(); // opening quote
    let mut escaped = false;
    for (offset, c) in chars {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == quote {
            return start + offset + c.len_utf8();
        }
    }
    code.len()
}

/// Index just past the run of characters matching `pred`, starting at `start`.
fn scan_while(code: &str, start: usize, pred: impl Fn(char) -> bool) -> usize {
    for (offset, c) in code[start..].char_indices() {
        if !pred(c) {
            return start + offset;
        }
    }
    code.len()
}

/// Merges adjacent same-kind runs and materializes their text.
fn coalesce(code: &str, runs: Vec<(usize, usize, Syntax)>) -> Vec<(String, Syntax)> {
    let mut out: Vec<(String, Syntax)> = Vec::new();
    for (start, end, kind) in runs {
        match out.last_mut() {
            Some((text, last)) if *last == kind => text.push_str(&code[start..end]),
            _ => out.push((code[start..end].to_owned(), kind)),
        }
    }
    out
}

impl Lang {
    fn for_tag(tag: &str) -> Lang {
        let tag = tag.trim().to_ascii_lowercase();
        match tag.as_str() {
            "rust" | "rs" => Lang {
                line_comment: &["//"],
                block_comment: Some(("/*", "*/")),
                keywords: &[
                    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else",
                    "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop",
                    "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static",
                    "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while",
                ],
                types: &[
                    "bool", "char", "str", "String", "Vec", "Option", "Result", "Box", "usize",
                    "isize", "u8", "u16", "u32", "u64", "u128", "i8", "i16", "i32", "i64", "i128",
                    "f32", "f64",
                ],
            },
            "python" | "py" => Lang {
                line_comment: &["#"],
                block_comment: None,
                keywords: &[
                    "and", "as", "assert", "async", "await", "break", "class", "continue", "def",
                    "del", "elif", "else", "except", "finally", "for", "from", "global", "if",
                    "import", "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass",
                    "raise", "return", "True", "False", "try", "while", "with", "yield",
                ],
                types: &["int", "float", "str", "bool", "bytes", "list", "dict", "set", "tuple"],
            },
            "javascript" | "js" | "typescript" | "ts" | "jsx" | "tsx" => Lang {
                line_comment: &["//"],
                block_comment: Some(("/*", "*/")),
                keywords: &[
                    "async", "await", "break", "case", "catch", "class", "const", "continue",
                    "default", "delete", "do", "else", "export", "extends", "false", "finally",
                    "for", "function", "if", "import", "in", "instanceof", "let", "new", "null",
                    "of", "return", "super", "switch", "this", "throw", "true", "try", "typeof",
                    "undefined", "var", "void", "while", "yield",
                ],
                types: &[
                    "number", "string", "boolean", "object", "any", "unknown", "never", "void",
                    "Array", "Promise",
                ],
            },
            "go" => Lang {
                line_comment: &["//"],
                block_comment: Some(("/*", "*/")),
                keywords: &[
                    "break", "case", "chan", "const", "continue", "default", "defer", "else",
                    "fallthrough", "for", "func", "go", "goto", "if", "import", "interface", "map",
                    "package", "range", "return", "select", "struct", "switch", "type", "var",
                    "nil", "true", "false",
                ],
                types: &[
                    "bool", "string", "int", "int8", "int16", "int32", "int64", "uint", "byte",
                    "rune", "float32", "float64", "error",
                ],
            },
            "c" | "cpp" | "c++" | "h" | "hpp" | "java" => Lang {
                line_comment: &["//"],
                block_comment: Some(("/*", "*/")),
                keywords: &[
                    "auto", "break", "case", "class", "const", "continue", "default", "do", "else",
                    "enum", "extern", "for", "if", "namespace", "new", "nullptr", "private",
                    "protected", "public", "return", "sizeof", "static", "struct", "switch",
                    "template", "this", "typedef", "using", "virtual", "void", "volatile", "while",
                    "true", "false",
                ],
                types: &[
                    "bool", "char", "double", "float", "int", "long", "short", "signed",
                    "unsigned", "size_t",
                ],
            },
            "sh" | "bash" | "shell" | "zsh" => Lang {
                line_comment: &["#"],
                block_comment: None,
                keywords: &[
                    "if", "then", "else", "elif", "fi", "for", "in", "do", "done", "while",
                    "case", "esac", "function", "return", "export", "local", "echo", "cd",
                ],
                types: &[],
            },
            _ => Lang {
                line_comment: &["//", "#"],
                block_comment: Some(("/*", "*/")),
                keywords: &[
                    "if", "else", "for", "while", "return", "function", "class", "true", "false",
                    "null", "import", "export", "const", "let", "var",
                ],
                types: &[],
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(code: &str, lang: &str) -> Vec<(String, Syntax)> {
        highlight(code, lang)
    }

    fn find(runs: &[(String, Syntax)], text: &str) -> Syntax {
        runs.iter().find(|(t, _)| t == text).map(|(_, k)| *k).unwrap()
    }

    #[test]
    fn concatenation_is_lossless() {
        let code = "fn main() {\n    let x = 42; // hi\n}";
        let joined: String = highlight(code, "rust").into_iter().map(|(t, _)| t).collect();
        assert_eq!(joined, code);
    }

    #[test]
    fn classifies_rust_tokens() {
        // The line comment runs up to (but not including) the newline.
        let runs = kinds("let n: u32 = 0xFF; // note\n", "rust");
        assert_eq!(find(&runs, "let"), Syntax::Keyword);
        assert_eq!(find(&runs, "u32"), Syntax::Type);
        assert_eq!(find(&runs, "0xFF"), Syntax::Number);
        assert_eq!(find(&runs, "// note"), Syntax::Comment);
    }

    #[test]
    fn strings_swallow_delimiters_and_escapes() {
        let runs = kinds(r#"print("a \"b\" c")"#, "python");
        assert_eq!(find(&runs, "print"), Syntax::Function);
        assert_eq!(find(&runs, r#""a \"b\" c""#), Syntax::String);
    }

    #[test]
    fn unterminated_string_runs_to_end() {
        let runs = highlight("x = \"oops", "python");
        assert_eq!(runs.last().unwrap(), &("\"oops".to_owned(), Syntax::String));
    }

    #[test]
    fn unknown_language_still_finds_strings_and_numbers() {
        let runs = highlight("val = 3.14 \"hi\"", "made-up");
        assert_eq!(find(&runs, "3.14"), Syntax::Number);
        assert_eq!(find(&runs, "\"hi\""), Syntax::String);
    }
}
