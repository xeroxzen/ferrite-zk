//! Device compatibility table: the record layouts and behaviour observed
//! on specific devices.
//!
//! [`COMPATIBILITY`] mirrors the table in `docs/COMPATIBILITY.md` row for
//! row; a test fails if they differ. To add a device, add the row to both.
//! Nothing else needs to change: [`user_format_for`] reads the table.

use zkteco_proto::records::options::keys;
use zkteco_proto::records::user::UserFormat;

use crate::client::Client;
use crate::error::Result;

/// Text recorded for a value the device would not report.
pub const UNAVAILABLE: &str = "unavailable";

/// Matches any value in an identifying column.
pub const ANY: &str = "*";

/// One device observed in the field. All columns are text, exactly as in
/// `docs/COMPATIBILITY.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompatRow {
    /// `~DeviceName` (identifying; `*` matches any).
    pub model: &'static str,
    /// Firmware version string (identifying; `*` matches any).
    pub firmware: &'static str,
    /// `~Platform` (identifying; `*` matches any).
    pub platform: &'static str,
    /// `~UserExtFmt` value, or `unavailable` (identifying; `*` matches any).
    pub user_ext_fmt: &'static str,
    /// `~ExtendFmt` value, or `unavailable` (identifying; `*` matches any).
    pub extend_fmt: &'static str,
    /// User record size in bytes, or `-` if not observed.
    pub user_record: &'static str,
    /// Attendance record size in bytes, or `-` if not observed.
    pub attendance_record: &'static str,
    /// How the device handles reply IDs (as `zkctl diagnose` reports it).
    pub reply_ids: &'static str,
    /// Transports that worked (`tcp`, `udp`, `tcp+udp`).
    pub transport: &'static str,
    /// Who verified the row, and how. Rows without it are ignored.
    pub verified_by: &'static str,
}

/// The table. Empty until hardware reports arrive.
pub const COMPATIBILITY: &[CompatRow] = &[];

/// What a device says about itself: the identifying columns of a
/// [`CompatRow`]. Values it would not report are [`UNAVAILABLE`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceFacts {
    /// `~DeviceName`.
    pub model: String,
    /// Firmware version.
    pub firmware: String,
    /// `~Platform`.
    pub platform: String,
    /// `~UserExtFmt`.
    pub user_ext_fmt: String,
    /// `~ExtendFmt`.
    pub extend_fmt: String,
}

impl CompatRow {
    /// Whether every identifying column matches `facts` (or is `*`).
    #[must_use]
    pub fn matches(&self, facts: &DeviceFacts) -> bool {
        let eq = |cell: &str, value: &str| cell == ANY || cell == value;
        eq(self.model, &facts.model)
            && eq(self.firmware, &facts.firmware)
            && eq(self.platform, &facts.platform)
            && eq(self.user_ext_fmt, &facts.user_ext_fmt)
            && eq(self.extend_fmt, &facts.extend_fmt)
    }
}

/// The user layout for a device, from the verified rows of `table` that
/// match it. `None` if no verified row with a known user record size
/// matches, or if matching rows disagree.
#[must_use]
pub fn user_format_for(table: &[CompatRow], facts: &DeviceFacts) -> Option<UserFormat> {
    let mut found = None;
    for row in table
        .iter()
        .filter(|r| !r.verified_by.trim().is_empty() && r.matches(facts))
    {
        let Some(format) = row.user_record.parse().ok().and_then(UserFormat::from_record_len) else {
            continue;
        };
        match found {
            None => found = Some(format),
            Some(f) if f == format => {}
            Some(_) => {
                tracing::warn!(?facts, "compatibility rows disagree on the user layout");
                return None;
            }
        }
    }
    found
}

impl Client {
    /// The device's identifying facts, as used by the compatibility table.
    /// Values the device refuses are recorded as [`UNAVAILABLE`].
    ///
    /// # Errors
    ///
    /// Only errors that close the connection; refusals are recorded.
    pub async fn device_facts(&mut self) -> Result<DeviceFacts> {
        let firmware = self.firmware_version().await;
        Ok(DeviceFacts {
            firmware: shown(firmware)?,
            model: self.option_or_unavailable(keys::DEVICE_NAME).await?,
            platform: self.option_or_unavailable(keys::PLATFORM).await?,
            user_ext_fmt: self.option_or_unavailable(keys::USER_EXT_FMT).await?,
            extend_fmt: self.option_or_unavailable(keys::EXTEND_FMT).await?,
        })
    }

    async fn option_or_unavailable(&mut self, key: &str) -> Result<String> {
        let value = self.option(key).await;
        shown(value)
    }
}

/// A queried value; fatal errors propagate, refusals become
/// [`UNAVAILABLE`].
fn shown(value: Result<String>) -> Result<String> {
    match value {
        Ok(v) => Ok(v),
        Err(e) if e.is_fatal() => Err(e),
        Err(e) => {
            tracing::debug!(%e, "value unavailable");
            Ok(UNAVAILABLE.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> DeviceFacts {
        DeviceFacts {
            model: "K40".into(),
            firmware: "Ver 6.60 Apr 28 2017".into(),
            platform: "ZMM220_TFT".into(),
            user_ext_fmt: "1".into(),
            extend_fmt: UNAVAILABLE.into(),
        }
    }

    const fn row(platform: &'static str, user_record: &'static str, verified_by: &'static str) -> CompatRow {
        CompatRow {
            model: ANY,
            firmware: ANY,
            platform,
            user_ext_fmt: "1",
            extend_fmt: UNAVAILABLE,
            user_record,
            attendance_record: "40",
            reply_ids: "echoed",
            transport: "tcp",
            verified_by,
        }
    }

    #[test]
    fn matching_verified_row_gives_the_layout() {
        let table = [row("ZMM220_TFT", "72", "maintainer, zkctl diagnose")];
        assert_eq!(user_format_for(&table, &facts()), Some(UserFormat::Standard72));
    }

    #[test]
    fn unverified_mismatched_unsized_or_conflicting_rows_give_nothing() {
        assert_eq!(user_format_for(&[], &facts()), None);
        assert_eq!(user_format_for(&[row("ZMM220_TFT", "72", " ")], &facts()), None);
        assert_eq!(user_format_for(&[row("ZMM100", "72", "x")], &facts()), None);
        assert_eq!(user_format_for(&[row("ZMM220_TFT", "-", "x")], &facts()), None);
        let conflict = [row("ZMM220_TFT", "72", "x"), row(ANY, "28", "y")];
        assert_eq!(user_format_for(&conflict, &facts()), None);
        let agree = [row("ZMM220_TFT", "72", "x"), row(ANY, "72", "y")];
        assert_eq!(user_format_for(&agree, &facts()), Some(UserFormat::Standard72));
    }

    /// `docs/COMPATIBILITY.md` and [`COMPATIBILITY`] hold the same rows.
    #[test]
    fn table_matches_the_document() {
        let doc = include_str!("../../../docs/COMPATIBILITY.md");
        let rows: Vec<Vec<&str>> = doc
            .lines()
            .skip_while(|l| *l != "## Table")
            .skip_while(|l| !l.starts_with("| Model"))
            .skip(2)
            .take_while(|l| l.starts_with('|'))
            .map(|l| l.trim().trim_matches('|').split('|').map(str::trim).collect())
            .collect();
        let ours: Vec<Vec<&str>> = COMPATIBILITY
            .iter()
            .map(|r| {
                vec![
                    r.model,
                    r.firmware,
                    r.platform,
                    r.user_ext_fmt,
                    r.extend_fmt,
                    r.user_record,
                    r.attendance_record,
                    r.reply_ids,
                    r.transport,
                    r.verified_by,
                ]
            })
            .collect();
        assert_eq!(rows, ours, "docs/COMPATIBILITY.md and compat.rs differ");
    }
}
