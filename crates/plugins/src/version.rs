//! Versions as agent description files compare them (docs/research/multica.md §2.9,
//! docs/DESIGN.md §9.4).
//!
//! Two questions are asked of a CLI's version:
//!
//! - is it at least the description's `min_version`? A machine with an older CLI does not
//!   register the agent at all.
//! - is it in one of the `verified` series, the upstream versions the driver was checked
//!   against? Outside them it still runs, and the GUI says it is unverified.
//!
//! CLIs print their version in their own words (`2.1.317 (Claude Code)`, `codex-cli
//! 0.158.0`, `v0.31.0`), so a version is read as the first run of numbers in the text.

/// A version, read from what a CLI prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    /// `2.1.317` → `[2, 1, 317]`.
    numbers: Vec<u64>,
    /// A `-beta.1` / `+build` suffix on the numbers: `2.0.0-beta` is older than `2.0.0`.
    prerelease: bool,
}

impl Version {
    /// The first run of numbers in `text` (`1.2.3`, `v1.2`, `2.1.317 (Claude Code)`), or
    /// `None` when there is none (`unknown`).
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        let start = bytes.iter().position(u8::is_ascii_digit)?;
        let mut numbers = Vec::new();
        let mut number = 0u64;
        let mut digits = 0usize;
        let mut i = start;
        while i < bytes.len() {
            let byte = bytes[i];
            if byte.is_ascii_digit() {
                number = number
                    .saturating_mul(10)
                    .saturating_add(u64::from(byte - b'0'));
                digits += 1;
            } else if byte == b'.' && digits > 0 {
                numbers.push(number);
                number = 0;
                digits = 0;
            } else {
                break;
            }
            i += 1;
        }
        if digits > 0 {
            numbers.push(number);
        }
        if numbers.is_empty() {
            return None;
        }
        // The suffix decides nothing but "is this a pre-release": `2.0.0-beta` sorts below
        // `2.0.0`, `2.0.0+build` does not.
        let rest = &text[i..];
        Some(Self {
            numbers,
            prerelease: rest.starts_with('-'),
        })
    }

    /// The number at `index`, `0` when the version has fewer (`1.2` read as `1.2.0`).
    fn at(&self, index: usize) -> u64 {
        self.numbers.get(index).copied().unwrap_or(0)
    }

    /// Whether this version is at least `other`.
    pub fn at_least(&self, other: &Self) -> bool {
        let count = self.numbers.len().max(other.numbers.len());
        for index in 0..count {
            match self.at(index).cmp(&other.at(index)) {
                std::cmp::Ordering::Less => return false,
                std::cmp::Ordering::Greater => return true,
                std::cmp::Ordering::Equal => {}
            }
        }
        // Same numbers: a pre-release is below the release it leads to.
        !self.prerelease || other.prerelease
    }

    /// Whether this version falls in `series`, the leading components of a version:
    /// `"2.1"` holds `2.1.317`, `"2"` holds every 2.x. The series' own pre-release flag is
    /// ignored, since it names a range rather than a build.
    pub fn in_series(&self, series: &Self) -> bool {
        series
            .numbers
            .iter()
            .enumerate()
            .all(|(index, number)| self.at(index) == *number)
    }
}

/// Whether `version` is at least `minimum`; `None` when either cannot be read (an answer
/// like `unknown`, or nothing asked yet).
pub fn is_at_least(version: &str, minimum: &str) -> Option<bool> {
    Some(Version::parse(version)?.at_least(&Version::parse(minimum)?))
}

/// Whether `version` is in `series` (`"2.1"`); `None` when either cannot be read.
pub fn is_in_series(version: &str, series: &str) -> Option<bool> {
    Some(Version::parse(version)?.in_series(&Version::parse(series)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_numbers_out_of_what_a_cli_prints() {
        let v = Version::parse("2.1.317 (Claude Code)").unwrap();
        assert_eq!(v.numbers, [2, 1, 317]);
        assert!(!v.prerelease);
        assert_eq!(
            Version::parse("codex-cli 0.158.0").unwrap().numbers,
            [0, 158, 0]
        );
        assert_eq!(Version::parse("v0.31.0").unwrap().numbers, [0, 31, 0]);
        assert_eq!(Version::parse("2.173.0").unwrap().numbers, [2, 173, 0]);
        assert!(Version::parse("unknown").is_none());
        assert!(Version::parse("").is_none());
    }

    #[test]
    fn one_dot_two_is_one_dot_two_dot_zero() {
        assert!(is_at_least("1.2", "1.2.0").unwrap());
        assert!(is_at_least("1.2.0", "1.2").unwrap());
        assert!(!is_at_least("1.2", "1.2.1").unwrap());
    }

    #[test]
    fn numbers_compare_numerically_not_as_text() {
        // The reason this is not a string comparison: "0.99" > "0.100" as text.
        assert!(!is_at_least("0.99.0", "0.100.0").unwrap());
        assert!(is_at_least("0.100.0", "0.100.0").unwrap());
        assert!(is_at_least("0.101.0", "0.100.0").unwrap());
        assert!(is_at_least("2.1.317", "2.0.0").unwrap());
        assert!(!is_at_least("2.1.317", "2.2.0").unwrap());
        assert!(is_at_least("10.0.0", "9.9.9").unwrap());
    }

    #[test]
    fn a_pre_release_is_below_its_release() {
        assert!(!is_at_least("2.0.0-beta.1", "2.0.0").unwrap());
        assert!(is_at_least("2.0.0", "2.0.0-beta.1").unwrap());
        // A build suffix does not move the version.
        assert!(is_at_least("2.0.0+build7", "2.0.0").unwrap());
    }

    #[test]
    fn a_series_holds_the_versions_under_it() {
        assert!(is_in_series("2.1.317", "2.1").unwrap());
        assert!(!is_in_series("2.2.0", "2.1").unwrap());
        assert!(is_in_series("0.158.3", "0.158").unwrap());
        assert!(!is_in_series("0.159.0", "0.158").unwrap());
        assert!(is_in_series("2.9.1", "2").unwrap());
        assert!(is_in_series("2.1", "2.1.0").unwrap());
        assert!(is_in_series("unknown", "2.1").is_none());
    }
}
