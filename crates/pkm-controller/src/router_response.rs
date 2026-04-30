use std::collections::HashMap;

/// Picks an agent name from the system agent's free-text response.
///
/// Trims and lowercases the response. If it matches a known agent name (and is
/// not the reserved "router" keyword), use it. Otherwise, fall back to `current`.
pub fn parse(response_text: &str, prompts: &HashMap<String, String>, current: &str) -> String {
    let trimmed = response_text.trim().to_lowercase();

    if trimmed.is_empty() {
        tracing::warn!(
            current = %current,
            "free-text response empty; keeping current agent"
        );
        return current.to_string();
    }

    if trimmed == "router" || !prompts.contains_key(&trimmed) {
        tracing::warn!(
            chosen = %trimmed,
            current = %current,
            "free-text response not a known agent; keeping current"
        );
        return current.to_string();
    }

    tracing::info!(agent = %trimmed, "picked agent via free-text");
    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_prompts() -> HashMap<String, String> {
        HashMap::from([
            ("research".into(), "prompt".into()),
            ("writer".into(), "prompt".into()),
            ("router".into(), "prompt".into()),
        ])
    }

    #[test]
    fn valid_agent() {
        let prompts = make_prompts();
        assert_eq!(parse("research", &prompts, "writer"), "research");
    }

    #[test]
    fn trims_and_lowercases() {
        let prompts = make_prompts();
        assert_eq!(parse("  Research \n", &prompts, "writer"), "research");
    }

    #[test]
    fn unknown_keeps_current() {
        let prompts = make_prompts();
        assert_eq!(parse("nonexistent", &prompts, "research"), "research");
    }

    #[test]
    fn rejects_router_keyword() {
        let prompts = make_prompts();
        assert_eq!(parse("router", &prompts, "research"), "research");
    }

    #[test]
    fn empty_keeps_current() {
        let prompts = make_prompts();
        assert_eq!(parse("", &prompts, "research"), "research");
    }
}
