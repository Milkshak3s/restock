use regex::Regex;

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

/// Capture group 1 of `re` in `text`, e.g. the version inside a tag or URL.
pub fn capture(re: &Regex, text: &str) -> Option<String> {
    Some(re.captures(text)?.get(1)?.as_str().to_owned())
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

    #[test]
    fn captures_versions() {
        let brave = Regex::new(r"^\d+\.(\d+\.\d+\.\d+)$").unwrap();
        assert_eq!(capture(&brave, "156.1.98.60").as_deref(), Some("1.98.60"));
        let discord = Regex::new(r"/x64/([\d.]+)/").unwrap();
        let url =
            "https://stable.dl2.discordapp.net/distro/app/stable/win/x64/1.0.9265/DiscordSetup.exe";
        assert_eq!(capture(&discord, url).as_deref(), Some("1.0.9265"));
        assert_eq!(capture(&discord, "https://discord.com/api/download"), None);
    }
}
