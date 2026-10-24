/// Extracts the leading numeric version (`v1.2.3-rc1` -> `[1, 2, 3]`), dropping
/// trailing zeros so `24.08` and `24.08.00.0` compare equal.
fn parts(v: &str) -> Vec<u64> {
    let start = v.find(|c: char| c.is_ascii_digit()).unwrap_or(v.len());
    let numeric: &str = &v[start..];
    let end = numeric
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(numeric.len());

    let mut parts: Vec<u64> = numeric[..end]
        .split('.')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().unwrap_or(u64::MAX))
        .collect();
    while parts.last() == Some(&0) {
        parts.pop();
    }
    parts
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    parts(candidate) > parts(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_numerically() {
        assert!(is_newer("1.10.0", "1.9.9"));
        assert!(is_newer("v8.7.1", "8.7"));
        assert!(!is_newer("24.08", "24.08.00.0"));
        assert!(!is_newer("1.2.3", "1.2.3"));
        assert!(!is_newer("1.2.3-rc1", "1.2.3"));
        assert!(is_newer("2.0", "unknown"));
    }
}
