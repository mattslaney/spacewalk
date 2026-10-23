//! View-only age/size criteria; disk totals and filesystem operations stay real.
use std::time::{Duration, SystemTime};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ViewFilters {
    pub min_size: Option<u64>,
    pub min_age_days: Option<u64>,
}
impl ViewFilters {
    pub fn active(self) -> bool {
        self.min_size.is_some() || self.min_age_days.is_some()
    }
    pub fn cutoff(self, now: SystemTime) -> Option<SystemTime> {
        now.checked_sub(Duration::from_secs(self.min_age_days?.checked_mul(86400)?))
    }
    pub fn parse(size: &str, days: &str, now: SystemTime) -> Result<Self, String> {
        let min_size = parse_size(size)?;
        let min_age_days = if days.trim().is_empty() {
            None
        } else {
            let value = days
                .trim()
                .parse::<u64>()
                .map_err(|_| "Age must be a whole number of days")?;
            (value != 0).then_some(value)
        };
        let result = Self {
            min_size,
            min_age_days,
        };
        if min_age_days.is_some() && result.cutoff(now).is_none() {
            return Err("Age is too large".into());
        }
        Ok(result)
    }
    pub fn label(self) -> String {
        let mut parts = Vec::new();
        if let Some(size) = self.min_size {
            parts.push(format!("size ≥ {}", crate::human_size(size)));
        }
        if let Some(days) = self.min_age_days {
            parts.push(format!("modified ≥ {days} days ago"));
        }
        parts.join(" · ")
    }
}
fn parse_size(text: &str) -> Result<Option<u64>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let split = text
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(text.len());
    let number = &text[..split];
    let suffix = text[split..].trim().to_ascii_uppercase();
    let factor: u128 = match suffix.as_str() {
        "" | "B" => 1,
        "KB" => 1000,
        "MB" => 1000_u128.pow(2),
        "GB" => 1000_u128.pow(3),
        "TB" => 1000_u128.pow(4),
        "K" | "KIB" => 1024,
        "M" | "MIB" => 1024_u128.pow(2),
        "G" | "GIB" => 1024_u128.pow(3),
        "T" | "TIB" => 1024_u128.pow(4),
        _ => return Err("Use bytes, KB/MB/GB/TB, or KiB/MiB/GiB/TiB".into()),
    };
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() > 9
        || number.ends_with('.')
    {
        return Err("Size must be a non-negative number, for example 500MiB or 1.5GB".into());
    }
    let digits = format!("{whole}{fraction}")
        .parse::<u128>()
        .map_err(|_| "Size is too large")?;
    let bytes = digits
        .checked_mul(factor)
        .ok_or("Size is too large")?
        .div_ceil(10_u128.pow(fraction.len() as u32));
    let bytes = u64::try_from(bytes).map_err(|_| "Size is too large")?;
    if bytes == 0 && digits != 0 {
        return Err("Size must be at least one byte".into());
    }
    Ok((bytes != 0).then_some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_units_decimals_and_optional_criteria_without_overflow() {
        let now = SystemTime::now();
        for (text, bytes) in [
            ("1.5GB", 1_500_000_000),
            ("2 GiB", 2 * 1024_u64.pow(3)),
            ("500MiB", 500 * 1024_u64.pow(2)),
            ("42", 42),
            ("1.5B", 2),
        ] {
            assert_eq!(
                ViewFilters::parse(text, "90", now).unwrap().min_size,
                Some(bytes)
            );
        }
        assert_eq!(
            ViewFilters::parse("", "", now).unwrap(),
            ViewFilters::default()
        );
        assert!(!ViewFilters::parse("0", "0", now).unwrap().active());
        for text in [
            "-1GiB",
            "NaN",
            "1..5GB",
            "1.",
            "1e9",
            "1XB",
            "999999999999999999999999999999999999GiB",
        ] {
            assert!(ViewFilters::parse(text, "", now).is_err(), "{text}");
        }
        for text in ["-1", "1.5", "90days", "18446744073709551615"] {
            assert!(ViewFilters::parse("", text, now).is_err(), "{text}");
        }
    }
}
