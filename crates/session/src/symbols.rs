//! The symbols `/eval` content completes to, as the image itself answers them.
//!
//! There is no completion endpoint (see `docs/api-gaps.md`): what a half-typed
//! token could become is the running image's own knowledge — which package the
//! content reads in, which symbols are worth offering — and the one op that can
//! ask it is `eval`. So the popup evaluates a fixed form, and the form asks
//! `evo.eval:completions-for` for the token the reader is typing.
//!
//! Two things this module is careful about:
//!
//! * the **token** is the only thing the client puts into what the image
//!   evaluates, so it goes in as a string literal with its quote and backslash
//!   escaped, and never as code;
//! * the answer is read back as the op's **output** — one `name<TAB>description`
//!   line per candidate — because that is the channel that carries what the
//!   image printed without the op's own `⇒ value` decoration around it.
//!
//! An op that refuses, an image with no such function, a token that matches
//! nothing: all of them come back as no rows, and none of them is an error the
//! popup has to tell apart — the list simply has nothing in it.

use crate::OpRequest;

/// One symbol the image offered: the name as it will be typed, and what it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolOption {
    /// The whole replacement text, package qualifier and all, lower-cased the way
    /// the image spells its own names.
    pub name: String,
    /// What the symbol is — `function`, `variable`, `macro`, `generic function`,
    /// or a comma-separated list of those (`evo.eval:symbol-kind`).
    pub description: String,
}

/// The `eval` op that asks the image about TOKEN.
pub fn symbol_request(token: &str) -> OpRequest {
    OpRequest::eval(&completion_code(token))
}

/// The form the popup evaluates: the image's candidates for TOKEN, printed one
/// per line as `name<TAB>description`.
///
/// The token is a string literal here, and its own quote and backslash are
/// escaped: a token that is being typed is shaped like a symbol, but the client
/// does not get to decide that — the escaping is what makes the question safe
/// whatever was typed.
///
/// Each line's two halves have their tabs and newlines flattened to spaces
/// first: a tab inside a name would be a second column to the reader below.
pub fn completion_code(token: &str) -> String {
    let mut escaped = String::with_capacity(token.len());
    for c in token.chars() {
        if matches!(c, '\\' | '"') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    format!(
        "(labels ((clean (s)\n             (substitute #\\Space #\\Newline\n                         (substitute #\\Space #\\Tab (string s)))))\n  \
         (dolist (row (evo.eval:completions-for \"{escaped}\"))\n    \
         (format t \"~a~c~a~%\" (clean (car row)) #\\Tab (clean (cdr row)))))"
    )
}

/// The rows a completion reply's output holds, in the image's own order.
///
/// A line without a tab is not one of them: the op's output is whatever the
/// image printed while the form ran, and only the lines this form wrote are
/// candidates.
pub fn symbol_options(output: &str) -> Vec<SymbolOption> {
    output
        .lines()
        .filter_map(|line| {
            let (name, description) = line.split_once('\t')?;
            (!name.is_empty()).then(|| SymbolOption {
                name: name.to_string(),
                description: description.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_form_is_the_images_own_completer_and_asks_about_one_token() {
        let code = completion_code("evo:comp");
        assert!(
            code.contains("evo.eval:completions-for \"evo:comp\""),
            "{code}"
        );
        // The candidates come back on the op's output, one line each.
        assert!(code.contains("(format t "), "{code}");
    }

    #[test]
    fn the_token_goes_in_as_a_literal_and_never_as_code() {
        // A quote and a backslash cannot end the literal or escape out of it.
        let code = completion_code("a\"b");
        assert!(code.contains("completions-for \"a\\\"b\""), "{code}");
        let code = completion_code("a\\b");
        assert!(code.contains("completions-for \"a\\\\b\""), "{code}");
        let code = completion_code("(progn (evil)");
        assert!(code.contains("completions-for \"(progn (evil)\""), "{code}");
    }

    #[test]
    fn a_rows_line_is_a_name_and_what_it_is() {
        let rows = symbol_options("evo:eval\tfunction\ncar\tgeneric function\n*goal*\tvariable\n");
        assert_eq!(
            rows,
            vec![
                SymbolOption {
                    name: "evo:eval".into(),
                    description: "function".into()
                },
                SymbolOption {
                    name: "car".into(),
                    description: "generic function".into()
                },
                SymbolOption {
                    name: "*goal*".into(),
                    description: "variable".into()
                },
            ]
        );
    }

    #[test]
    fn anything_else_the_image_printed_is_not_a_candidate() {
        assert!(symbol_options("").is_empty());
        assert!(symbol_options("; compiling\n").is_empty());
        assert!(symbol_options("no tab on this line\n").is_empty());
        assert!(symbol_options("\tfunction\n").is_empty());
        // The tab that ends the name is the first one; the rest is description.
        assert_eq!(
            symbol_options("odd name\ta\ttab\n")[0].description,
            "a\ttab".to_string()
        );
    }
}
