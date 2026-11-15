//! `zkctl live` in human mode: one status line per connection change, per
//! device, instead of the client's logs.

use std::time::{Duration, Instant};

use zkteco_client::CaptureStats;

/// What the status lines say, from the command line.
#[derive(Debug, Clone, Copy)]
pub(super) struct Wording {
    /// `-v`: also report each failed reconnect attempt.
    pub(super) verbose: bool,
    /// `--reconnect`: a lost connection is retried.
    pub(super) reconnect: bool,
    /// `--backfill`: a reconnect reports how many events it backfilled.
    pub(super) backfill: bool,
}

/// One device's connection, as last reported.
#[derive(Debug, Default)]
pub(super) struct Link {
    down: Option<Down>,
}

#[derive(Debug)]
struct Down {
    since: Instant,
    /// `CaptureStats::backfilled` when the connection was lost.
    backfilled: u64,
    /// Failed attempts already reported.
    failures: u32,
}

impl Link {
    /// The status line for a change since the last call, if any.
    pub(super) fn change(&mut self, stats: &CaptureStats, w: Wording) -> Option<String> {
        match (&self.down, stats.connected) {
            (None, false) => {
                self.down = Some(Down {
                    since: Instant::now(),
                    backfilled: stats.backfilled,
                    failures: 0,
                });
                Some(if w.reconnect {
                    "disconnected, reconnecting\u{2026}".into()
                } else {
                    "disconnected".into()
                })
            }
            (Some(down), true) => {
                let backfilled = stats.backfilled.saturating_sub(down.backfilled);
                let line = reconnected(down.since.elapsed(), backfilled, w);
                self.down = None;
                Some(line)
            }
            _ => None,
        }
    }

    /// The status line for an error from the stream while the connection
    /// is down: a failed reconnect attempt, reported only with `-v`. The
    /// error that reported the loss itself says nothing more.
    pub(super) fn failed_attempt(
        &mut self,
        stats: &CaptureStats,
        error: &dyn std::fmt::Display,
        w: Wording,
    ) -> Option<String> {
        let down = self.down.as_mut()?;
        if stats.reconnect_failures <= down.failures {
            return None;
        }
        down.failures = stats.reconnect_failures;
        w.verbose
            .then(|| format!("reconnect failed (attempt {}): {error}", stats.reconnect_failures))
    }
}

/// The status line for the first connection failing: the first failure,
/// then each later attempt only with `-v`. `failures` counts the earlier
/// ones.
pub(super) fn connect_failed(failures: u32, error: &dyn std::fmt::Display, w: Wording) -> Option<String> {
    if failures == 0 {
        return Some(format!("cannot connect ({error}), retrying\u{2026}"));
    }
    w.verbose
        .then(|| format!("connect failed (attempt {}): {error}", failures + 1))
}

/// The status line once the first connection works after failing.
pub(super) fn connected_after(after: Duration) -> String {
    format!("connected after {:.1} s", after.as_secs_f64())
}

fn reconnected(after: Duration, backfilled: u64, w: Wording) -> String {
    let after = after.as_secs_f64();
    if !w.backfill {
        return format!("reconnected after {after:.1} s");
    }
    let s = if backfilled == 1 { "" } else { "s" };
    format!("reconnected after {after:.1} s, backfilled {backfilled} event{s}")
}

/// Whether human output is coloured: standard output is a terminal and
/// `NO_COLOR` is unset or empty (<https://no-color.org>).
pub(super) fn use_color() -> bool {
    use std::io::IsTerminal;
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    !no_color && std::io::stdout().is_terminal()
}

/// `text` dimmed, when colouring.
pub(super) fn dim(text: &str, color: bool) -> String {
    if color {
        format!("\x1b[2m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}

/// An event's source tag, padded; `backfill` in yellow when colouring.
pub(super) fn source_tag(source: &str, color: bool) -> String {
    let padded = format!("{source:<8}");
    if color && source == "backfill" {
        format!("\x1b[33m{padded}\x1b[0m")
    } else {
        padded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: Wording = Wording {
        verbose: false,
        reconnect: true,
        backfill: true,
    };

    fn stats(connected: bool, backfilled: u64, reconnect_failures: u32) -> CaptureStats {
        CaptureStats {
            connected,
            backfilled,
            reconnect_failures,
            ..CaptureStats::default()
        }
    }

    #[test]
    fn one_line_per_change() {
        let mut link = Link::default();
        assert_eq!(link.change(&stats(true, 0, 0), W), None);
        let down = link.change(&stats(false, 4, 0), W);
        assert_eq!(down.as_deref(), Some("disconnected, reconnecting\u{2026}"));
        assert_eq!(link.change(&stats(false, 4, 1), W), None, "no repeat");
        let up = link.change(&stats(true, 7, 0), W).unwrap();
        assert!(up.starts_with("reconnected after 0.0 s"), "{up}");
        assert!(up.ends_with(", backfilled 3 events"), "{up}");
        assert_eq!(link.change(&stats(true, 7, 0), W), None);
    }

    #[test]
    fn wording_follows_the_options() {
        let plain = Wording {
            reconnect: false,
            backfill: false,
            ..W
        };
        let mut link = Link::default();
        assert_eq!(
            link.change(&stats(false, 0, 0), plain).as_deref(),
            Some("disconnected")
        );
        let up = link.change(&stats(true, 0, 0), plain).unwrap();
        assert!(up.starts_with("reconnected after ") && up.ends_with(" s"), "{up}");
        assert_eq!(
            reconnected(Duration::from_millis(6200), 1, W),
            "reconnected after 6.2 s, backfilled 1 event"
        );
    }

    #[test]
    fn failed_attempts_only_with_verbose() {
        let verbose = Wording { verbose: true, ..W };
        let mut link = Link::default();
        assert_eq!(
            link.failed_attempt(&stats(true, 0, 0), &"x", verbose),
            None,
            "not down"
        );
        link.change(&stats(false, 0, 0), verbose);
        assert_eq!(
            link.failed_attempt(&stats(false, 0, 0), &"lost", verbose),
            None,
            "the loss itself"
        );
        assert_eq!(
            link.failed_attempt(&stats(false, 0, 2), &"connection refused", verbose)
                .as_deref(),
            Some("reconnect failed (attempt 2): connection refused")
        );
        assert_eq!(link.failed_attempt(&stats(false, 0, 2), &"again", verbose), None);
        assert_eq!(
            link.failed_attempt(&stats(false, 0, 3), &"refused", W),
            None,
            "quiet by default"
        );
    }

    #[test]
    fn first_connection() {
        let verbose = Wording { verbose: true, ..W };
        assert_eq!(
            connect_failed(0, &"refused", W).as_deref(),
            Some("cannot connect (refused), retrying\u{2026}")
        );
        assert_eq!(connect_failed(1, &"refused", W), None);
        assert_eq!(
            connect_failed(1, &"refused", verbose).as_deref(),
            Some("connect failed (attempt 2): refused")
        );
        assert_eq!(
            connected_after(Duration::from_millis(3050)),
            "connected after 3.0 s"
        );
    }

    #[test]
    fn colour_only_when_asked() {
        assert_eq!(source_tag("live", true), "live    ");
        assert_eq!(source_tag("backfill", false), "backfill");
        assert_eq!(source_tag("backfill", true), "\x1b[33mbackfill\x1b[0m");
        assert_eq!(dim("x", false), "x");
        assert_eq!(dim("x", true), "\x1b[2mx\x1b[0m");
    }
}
