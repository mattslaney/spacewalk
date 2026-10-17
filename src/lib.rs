pub mod scan;

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Prevent control characters in filenames from reaching the terminal.
pub fn display_name(name: &std::ffi::OsStr) -> String {
    name.to_string_lossy()
        .chars()
        .map(|c| if c.is_control() { '�' } else { c })
        .collect()
}
