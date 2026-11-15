//! Durations on the command line: `500ms`, `30s`, `15m`, `24h`, `7d`, or a
//! bare number of seconds.

use std::time::Duration;

/// Parse a duration.
///
/// # Errors
///
/// A message naming the accepted forms.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let split = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    let (digits, unit) = text.split_at(split);
    let n: u64 = digits
        .parse()
        .map_err(|_| format!("{text:?} is not a duration (examples: 500ms, 30s, 15m, 24h, 7d)"))?;
    let secs = |mult: u64| n.checked_mul(mult).map(Duration::from_secs);
    let d = match unit {
        "ms" => Some(Duration::from_millis(n)),
        "" | "s" => secs(1),
        "m" => secs(60),
        "h" => secs(3600),
        "d" => secs(86_400),
        _ => None,
    };
    d.ok_or_else(|| format!("{text:?} is not a duration (examples: 500ms, 30s, 15m, 24h, 7d)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms() {
        assert_eq!(parse_duration("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_duration("30"), Ok(Duration::from_secs(30)));
        assert_eq!(parse_duration("15m"), Ok(Duration::from_secs(900)));
        assert_eq!(parse_duration("24h"), Ok(Duration::from_secs(86_400)));
        assert_eq!(parse_duration("7d"), Ok(Duration::from_secs(604_800)));
        for bad in ["", "m", "5x", "-1s", "1.5h"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }
}
