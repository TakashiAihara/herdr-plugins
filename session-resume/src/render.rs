// Single quotes are the one quoting both sh and zsh read the same way, whatever the text holds.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// One left-to-right pass over the template: text that came from a value is never scanned again,
// so a directory holding `{id}` stays literal.
pub fn render(template: &str, id: &str, cwd: &str) -> String {
    let values = [("{id}", id), ("{cwd}", cwd)];
    let mut out = String::new();
    let mut rest = template;
    while let Some((at, name, value)) = values
        .iter()
        .filter_map(|(name, value)| rest.find(name).map(|at| (at, *name, *value)))
        .min_by_key(|(at, _, _)| *at)
    {
        out.push_str(&rest[..at]);
        out.push_str(&quote(value));
        rest = &rest[at + name.len()..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_wraps_in_single_quotes_and_escapes_a_single_quote() {
        assert_eq!(quote("do x"), "'do x'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote("fix $HOME `x`"), "'fix $HOME `x`'");
    }

    #[test]
    fn the_default_template_gets_the_id_as_one_word() {
        assert_eq!(
            render("claude --resume {id}", "9c1f", "/w"),
            "claude --resume '9c1f'"
        );
    }

    #[test]
    fn id_and_cwd_are_each_one_quoted_word_in_either_order() {
        assert_eq!(
            render("go {cwd} {id}", "a b", "/it's here"),
            r"go '/it'\''s here' 'a b'"
        );
        assert_eq!(render("go {id} {cwd}", "a b", "/w"), "go 'a b' '/w'");
    }

    #[test]
    fn text_that_came_from_a_value_is_not_scanned_again() {
        assert_eq!(
            render("x {cwd} {id}", "i$&", "/w/{id}"),
            "x '/w/{id}' 'i$&'"
        );
    }

    #[test]
    fn a_placeholder_may_repeat_and_a_template_without_one_is_kept_as_is() {
        assert_eq!(render("{id} {id}", "a", "/w"), "'a' 'a'");
        assert_eq!(render("make", "a", "/w"), "make");
        assert_eq!(render("", "a", "/w"), "");
    }
}
