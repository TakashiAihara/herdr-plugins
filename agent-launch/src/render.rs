// Single quotes are the one quoting both sh and zsh read the same way, whatever the text holds.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// One left-to-right pass over the template: text that came from a value is never scanned again,
// so a prompt holding `{repo}` or `$&` stays literal. An empty repo leaves its placeholder out.
pub fn render(template: &str, prompt: &str, repo: &str) -> String {
    const PROMPT: &str = "{prompt}";
    const REPO: &str = "{repo}";
    let mut out = String::new();
    let mut rest = template;
    loop {
        let p = rest.find(PROMPT);
        let r = rest.find(REPO);
        match (p, r) {
            (Some(i), r) if r.map_or(true, |j| i < j) => {
                out.push_str(&rest[..i]);
                out.push_str(&quote(prompt));
                rest = &rest[i + PROMPT.len()..];
            }
            (_, Some(j)) => {
                out.push_str(&rest[..j]);
                if !repo.is_empty() {
                    out.push_str(&quote(repo));
                }
                rest = &rest[j + REPO.len()..];
            }
            _ => {
                out.push_str(rest);
                return out;
            }
        }
    }
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
    fn the_default_template_gets_the_prompt_as_one_word() {
        assert_eq!(
            render("claude -- {prompt}", "fix it's $HOME", ""),
            r"claude -- 'fix it'\''s $HOME'"
        );
    }

    #[test]
    fn repo_is_its_own_word_and_an_empty_repo_leaves_the_placeholder_out() {
        assert_eq!(
            render("ccxn {repo} {prompt}", "do x", "o/r"),
            "ccxn 'o/r' 'do x'"
        );
        assert_eq!(render("ccxn {repo} {prompt}", "do x", ""), "ccxn  'do x'");
    }

    #[test]
    fn placeholders_work_in_either_order_and_a_repo_may_hold_a_quote() {
        assert_eq!(
            render("go {prompt} --in {repo}", "do x", "o/it's"),
            r"go 'do x' --in 'o/it'\''s'"
        );
    }

    #[test]
    fn text_that_came_from_a_value_is_not_scanned_again() {
        assert_eq!(
            render("ccxn {repo} {prompt}", "fix A&B in {repo}", "o/{prompt}"),
            "ccxn 'o/{prompt}' 'fix A&B in {repo}'"
        );
        assert_eq!(
            render("x {prompt}", "$& $1 {prompt}", ""),
            "x '$& $1 {prompt}'"
        );
    }

    #[test]
    fn a_placeholder_may_repeat_and_a_template_without_one_is_kept_as_is() {
        assert_eq!(render("{prompt} {prompt}", "a", ""), "'a' 'a'");
        assert_eq!(render("make", "a", "r"), "make");
        assert_eq!(render("{repo}", "a", ""), "");
    }
}
