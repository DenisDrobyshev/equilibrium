//! Time, injected rather than read from the system.
//!
//! Several rules in this product are timing rules with legal force — the
//! three-hour disclosure, the seven-day return gap, the session length cap.
//! Testing them against the real clock is not possible, so nothing reads
//! `Utc::now()` directly outside this module.

use chrono::{DateTime, Utc};

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[cfg(test)]
pub mod test_support {
    use super::*;
    use std::sync::Mutex;

    /// A clock the test moves by hand.
    pub struct TestClock(Mutex<DateTime<Utc>>);

    impl TestClock {
        pub fn at(iso: &str) -> Self {
            let parsed = DateTime::parse_from_rfc3339(iso)
                .expect("valid RFC3339 timestamp")
                .with_timezone(&Utc);
            Self(Mutex::new(parsed))
        }

        pub fn advance(&self, duration: chrono::Duration) {
            let mut guard = self.0.lock().unwrap();
            *guard += duration;
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().unwrap()
        }
    }
}
