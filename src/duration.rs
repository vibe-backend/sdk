pub fn parse_duration_ms(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if let Some(n) = raw.strip_suffix("ms") {
        return n.trim().parse().ok();
    }
    if let Some(n) = raw.strip_suffix('s') {
        return n
            .trim()
            .parse::<u64>()
            .ok()
            .map(|sec| sec.saturating_mul(1000));
    }
    if let Some(n) = raw.strip_suffix('m') {
        return n
            .trim()
            .parse::<u64>()
            .ok()
            .map(|min| min.saturating_mul(60_000));
    }
    if let Some(n) = raw.strip_suffix('h') {
        return n
            .trim()
            .parse::<u64>()
            .ok()
            .map(|hour| hour.saturating_mul(3_600_000));
    }
    if let Some(n) = raw.strip_suffix('d') {
        return n
            .trim()
            .parse::<u64>()
            .ok()
            .map(|day| day.saturating_mul(86_400_000));
    }
    raw.parse::<u64>().ok().map(|sec| sec.saturating_mul(1000))
}
