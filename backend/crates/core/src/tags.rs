//! Tag validation shared by the tags PUT (API) and the admin import
//! manifest (worker): identical rules, one implementation.

/// One normalized label: 1..=32 chars of [a-z0-9_-] (already lowercased).
pub fn valid_tag(t: &str) -> bool {
    t.len() <= 32
        && t.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Tag rules: 1..=32 chars of `[a-z0-9_-]` after trim+lowercase, at most
/// 8 per placement, duplicates dropped. Returns the message of the first
/// violation so both callers map it to a 400.
pub fn normalize_tags(raw: Vec<String>) -> Result<Vec<String>, &'static str> {
    let mut out: Vec<String> = Vec::with_capacity(raw.len().min(8));
    for t in raw {
        let t = t.trim().to_ascii_lowercase();
        if t.is_empty() {
            continue;
        }
        if !valid_tag(&t) {
            return Err("tags must be 1..=32 chars of [a-z0-9_-]");
        }
        if !out.contains(&t) {
            out.push(t);
        }
    }
    if out.len() > 8 {
        return Err("at most 8 tags per placement");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::normalize_tags as norm;

    #[test]
    fn normalizes_dedupes_and_caps() {
        assert_eq!(
            norm(vec![" A ".into(), "a".into(), "b-c_d".into()]).unwrap(),
            vec!["a".to_string(), "b-c_d".to_string()]
        );
        // Validation runs after trim+lowercase, so mixed case is fine.
        assert_eq!(norm(vec!["UP".into()]).unwrap(), vec!["up".to_string()]);
        assert!(norm(vec!["x".repeat(33)]).is_err());
        assert!(norm((0..9).map(|i| format!("t{i}")).collect()).is_err());
        assert_eq!(
            norm(vec![String::new(), "  ".into()]).unwrap(),
            Vec::<String>::new()
        );
    }
}
