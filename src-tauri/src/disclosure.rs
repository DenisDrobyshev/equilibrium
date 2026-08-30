//! When the app must tell the user it is not a human.
//!
//! Every rule here is a legal requirement, not a design preference, and they
//! come from three different places:
//!
//! * **Before the first interaction** — EU AI Act art. 50(1): the information
//!   must reach the user no later than the first interaction.
//! * **On return after more than 7 days** — Utah HB 452.
//! * **Every 3 hours of continuous use** — New York GBL art. 47, which carries
//!   penalties up to $15,000 per day.
//!
//! They are collected in one module on purpose: this is the file to hand to a
//! lawyer, and the file to re-check when a jurisdiction changes its mind.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// Utah HB 452.
const RETURN_GAP_DAYS: i64 = 7;
/// New York GBL art. 47.
const PERIODIC_INTERVAL_HOURS: i64 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Disclosure {
    FirstRun,
    ReturnGap,
    Periodic,
}

impl Disclosure {
    /// Matches the CHECK constraint on `disclosures.kind`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Disclosure::FirstRun => "first_run",
            Disclosure::ReturnGap => "return_gap",
            Disclosure::Periodic => "periodic",
        }
    }
}

/// What the caller knows about the history of disclosures and use.
#[derive(Debug, Clone, Default)]
pub struct History {
    /// Any disclosure ever shown. `None` means this is a first run.
    pub any_shown_at: Option<DateTime<Utc>>,
    /// End of the previous practice, for the return-gap rule.
    pub last_practice_at: Option<DateTime<Utc>>,
    /// Start of the current continuous stretch of use.
    pub current_session_started_at: Option<DateTime<Utc>>,
    /// Last periodic reminder within the current stretch.
    pub last_periodic_at: Option<DateTime<Utc>>,
}

/// Which disclosure, if any, is owed right now.
///
/// Checked in order of legal weight: a first run outranks everything, and a
/// long absence outranks the periodic reminder.
pub fn due(now: DateTime<Utc>, history: &History) -> Option<Disclosure> {
    if history.any_shown_at.is_none() {
        return Some(Disclosure::FirstRun);
    }

    if let Some(last_practice) = history.last_practice_at {
        if now - last_practice > Duration::days(RETURN_GAP_DAYS) {
            return Some(Disclosure::ReturnGap);
        }
    }

    if let Some(session_start) = history.current_session_started_at {
        // The periodic clock runs from the last reminder, or from the start of
        // the stretch if none has been shown yet.
        let since = history.last_periodic_at.unwrap_or(session_start);
        if now - since >= Duration::hours(PERIODIC_INTERVAL_HOURS) {
            return Some(Disclosure::Periodic);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(iso: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(iso).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn first_run_is_owed_before_anything_else() {
        let history = History::default();
        assert_eq!(due(t("2026-08-29T10:00:00Z"), &history), Some(Disclosure::FirstRun));
    }

    #[test]
    fn returning_after_more_than_seven_days_owes_a_disclosure() {
        let history = History {
            any_shown_at: Some(t("2026-08-01T10:00:00Z")),
            last_practice_at: Some(t("2026-08-01T10:00:00Z")),
            ..Default::default()
        };
        // Eight days later.
        assert_eq!(due(t("2026-08-09T11:00:00Z"), &history), Some(Disclosure::ReturnGap));
    }

    #[test]
    fn returning_within_seven_days_owes_nothing() {
        let history = History {
            any_shown_at: Some(t("2026-08-01T10:00:00Z")),
            last_practice_at: Some(t("2026-08-01T10:00:00Z")),
            ..Default::default()
        };
        assert_eq!(due(t("2026-08-07T10:00:00Z"), &history), None);
    }

    #[test]
    fn three_hours_of_continuous_use_owes_a_reminder() {
        let history = History {
            any_shown_at: Some(t("2026-08-29T09:00:00Z")),
            last_practice_at: Some(t("2026-08-29T09:00:00Z")),
            current_session_started_at: Some(t("2026-08-29T09:00:00Z")),
            last_periodic_at: None,
        };
        assert_eq!(due(t("2026-08-29T12:00:00Z"), &history), Some(Disclosure::Periodic));
        // One minute short of three hours is not yet owed.
        assert_eq!(due(t("2026-08-29T11:59:00Z"), &history), None);
    }

    #[test]
    fn the_periodic_clock_restarts_after_each_reminder() {
        let history = History {
            any_shown_at: Some(t("2026-08-29T09:00:00Z")),
            last_practice_at: Some(t("2026-08-29T09:00:00Z")),
            current_session_started_at: Some(t("2026-08-29T09:00:00Z")),
            last_periodic_at: Some(t("2026-08-29T12:00:00Z")),
        };
        // Four hours into the session, but only one since the last reminder.
        assert_eq!(due(t("2026-08-29T13:00:00Z"), &history), None);
        assert_eq!(due(t("2026-08-29T15:00:00Z"), &history), Some(Disclosure::Periodic));
    }

    #[test]
    fn a_long_absence_outranks_the_periodic_reminder() {
        let history = History {
            any_shown_at: Some(t("2026-08-01T09:00:00Z")),
            last_practice_at: Some(t("2026-08-01T09:00:00Z")),
            current_session_started_at: Some(t("2026-08-20T09:00:00Z")),
            last_periodic_at: None,
        };
        assert_eq!(due(t("2026-08-20T13:00:00Z"), &history), Some(Disclosure::ReturnGap));
    }

    #[test]
    fn kind_strings_match_the_database_constraint() {
        assert_eq!(Disclosure::FirstRun.as_str(), "first_run");
        assert_eq!(Disclosure::ReturnGap.as_str(), "return_gap");
        assert_eq!(Disclosure::Periodic.as_str(), "periodic");
    }
}
