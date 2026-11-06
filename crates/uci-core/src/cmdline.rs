//! Splitting a typed or configured argument string.

/// Splits on whitespace, keeping "double quoted" groups together.
pub fn split_args(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut has_token = false;
    for c in text.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                has_token = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if has_token {
                    args.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            c => {
                current.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        args.push(current);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_whitespace_and_keeps_quoted_groups() {
        assert_eq!(
            split_args(r#"--weights "C:\Program Files\net.pb" -t 4"#),
            vec!["--weights", r"C:\Program Files\net.pb", "-t", "4"]
        );
        assert!(split_args("   ").is_empty());
        assert_eq!(split_args(r#""" x"#), vec!["", "x"]);
    }
}
