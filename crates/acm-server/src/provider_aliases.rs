pub(crate) fn canonical_provider_name(name: &str) -> &str {
    match name {
        "claude" | "claude_code" => "claude-code",
        _ => name,
    }
}

pub(crate) fn public_provider_names(provider_name: &str) -> Vec<&str> {
    let mut names = vec![provider_name];
    if provider_name == "claude-code" {
        names.push("claude");
    }
    names
}

#[cfg(test)]
mod tests {
    use super::{canonical_provider_name, public_provider_names};

    #[test]
    fn canonicalizes_claude_aliases() {
        assert_eq!(canonical_provider_name("claude"), "claude-code");
        assert_eq!(canonical_provider_name("claude_code"), "claude-code");
        assert_eq!(canonical_provider_name("claude-code"), "claude-code");
        assert_eq!(canonical_provider_name("codex"), "codex");
    }

    #[test]
    fn returns_public_aliases_for_claude_code() {
        assert_eq!(
            public_provider_names("claude-code"),
            vec!["claude-code", "claude"]
        );
        assert_eq!(public_provider_names("codex"), vec!["codex"]);
    }
}
