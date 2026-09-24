use super::theme::{Theme, Token};

pub(crate) fn compact_identity(theme: Theme, label: &str) -> String {
    format!(
        "{} · {}",
        theme.paint(theme.brand_compact(), Token::Primary),
        theme.paint(&title_case(label), Token::Primary)
    )
}

fn title_case(value: &str) -> String {
    if value != value.to_ascii_uppercase() {
        return value.to_owned();
    }
    value
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                format!("{}{}", first.to_ascii_uppercase(), chars.as_str().to_ascii_lowercase())
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_compact_and_readable() {
        let mut theme = Theme::test(80);
        theme.unicode = true;
        let text = compact_identity(theme, "Software");
        assert_eq!(text, "◈ Orbis · Software");
        assert!(text.lines().all(|line| line.chars().count() < 80));
    }

    #[test]
    fn ascii_identity_is_safe() {
        let mut theme = Theme::test(80);
        theme.unicode = false;
        let text = compact_identity(theme, "Software");
        assert_eq!(text, "* Orbis · Software");
        assert!(!text.contains('╭'));
    }

    #[test]
    fn identity_title_is_human_readable() {
        assert_eq!(compact_identity(Theme::test(80), "SELF UPDATE"), "* Orbis · Self Update");
    }
}
