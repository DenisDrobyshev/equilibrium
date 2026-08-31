//! The practice runtime: the one place where the protocol, the risk guard and
//! the store meet.
//!
//! Everything a user says passes through `submit_user_message`, and that
//! method runs the guard before anything else can happen. There is no other
//! way in. That is the point — a caller cannot accidentally reach the model
//! without the guard having run, because the caller never holds the model and
//! the state at the same time.
//!
//! The runtime also owns persistence: every accepted transition is written and
//! the vault is saved immediately. Losing a practice to a crash would mean
//! asking someone to tell a difficult story twice.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use rusqlite::params;

use crate::clock::Clock;
use crate::db::Store;
use crate::disclosure::{self, Disclosure, History};
use crate::protocol::{EndReason, Event, PlanDraft, RiskKind, SituationDraft, State};
use crate::safety::{Assessment, RiskDetector};

/// Hard cap on a single practice. Not a nudge — the machine ends the session.
///
/// This exists because engagement is the wrong objective here: longer daily
/// use is associated with more loneliness and dependence, not less (Fang et
/// al., MIT Media Lab / OpenAI RCT, n ≈ 981).
const MAX_PRACTICE_MINUTES: i64 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Turn {
    /// The guard is clear and the practice continues. Generation is allowed
    /// only if `state.allows_generation()`.
    Continue { state: State },
    /// The guard fired. Show static crisis content; do not generate.
    Crisis { kind: RiskKind },
    /// The practice is over.
    Ended { reason: EndReason },
}

pub struct Practice {
    store: Store,
    clock: Box<dyn Clock>,
    detector: Box<dyn RiskDetector + Send + Sync>,
    session_id: i64,
    started_at: DateTime<Utc>,
    state: State,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Branch {
    Onboarding,
    Regular,
}

impl Branch {
    fn as_str(&self) -> &'static str {
        match self {
            Branch::Onboarding => "onboarding",
            Branch::Regular => "regular",
        }
    }

    fn initial_state(&self) -> State {
        match self {
            Branch::Onboarding => State::start_onboarding(),
            Branch::Regular => State::start_practice(),
        }
    }
}

impl Practice {
    pub fn begin(
        store: Store,
        clock: Box<dyn Clock>,
        detector: Box<dyn RiskDetector + Send + Sync>,
        branch: Branch,
    ) -> Result<Self> {
        let now = clock.now();
        let state = branch.initial_state();

        store.connection().execute(
            "INSERT INTO practice_sessions (branch, started_at, current_state, states)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                branch.as_str(),
                now.to_rfc3339(),
                serde_json::to_string(&state)?,
                serde_json::to_string(&vec![state.name()])?,
            ],
        )?;
        let session_id = store.connection().last_insert_rowid();

        let practice = Practice {
            store,
            clock,
            detector,
            session_id,
            started_at: now,
            state,
        };
        practice.store.save()?;
        Ok(practice)
    }

    /// Picks up an unfinished practice, if there is one.
    pub fn resume(
        store: Store,
        clock: Box<dyn Clock>,
        detector: Box<dyn RiskDetector + Send + Sync>,
    ) -> Result<std::result::Result<Self, Store>> {
        let row = store
            .connection()
            .query_row(
                "SELECT id, started_at, current_state FROM practice_sessions
                 WHERE ended_at IS NULL ORDER BY started_at DESC LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .ok();

        let Some((session_id, started_at, current_state)) = row else {
            // Nothing to resume — hand the store back so the caller can begin.
            return Ok(Err(store));
        };

        let started_at = DateTime::parse_from_rfc3339(&started_at)
            .context("parsing session start time")?
            .with_timezone(&Utc);
        let state: State =
            serde_json::from_str(&current_state).context("restoring protocol state")?;

        Ok(Ok(Practice {
            store,
            clock,
            detector,
            session_id,
            started_at,
            state,
        }))
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn session_id(&self) -> i64 {
        self.session_id
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The only entry point for user input.
    ///
    /// Order matters: the message is recorded, the guard runs, and only then
    /// may the caller consider generating. A flagged message ends the practice
    /// in `Crisis` and no generation happens at all.
    pub fn submit_user_message(&mut self, text: &str) -> Result<Turn> {
        if self.state.is_terminal() {
            bail!("the practice has already ended");
        }

        let now = self.clock.now();
        self.record_message("user", text, now)?;

        match self.detector.assess(text) {
            Assessment::Flagged { kind, guard_version, .. } => {
                // Deliberately without the message text: enough to prove the
                // guard fired, not enough to become a store of sensitive
                // material in its own right.
                self.store.connection().execute(
                    "INSERT INTO safety_events (fired_at, trigger_kind, action_taken, guard_version)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        now.to_rfc3339(),
                        format!("{kind:?}"),
                        "practice interrupted, crisis resources shown",
                        guard_version,
                    ],
                )?;

                self.apply(Event::RiskDetected { kind })?;
                Ok(Turn::Crisis { kind })
            }
            Assessment::Clear => {
                if now - self.started_at > Duration::minutes(MAX_PRACTICE_MINUTES) {
                    self.apply(Event::TimeLimitReached)?;
                    return Ok(Turn::Ended { reason: EndReason::TimeLimit });
                }
                Ok(Turn::Continue { state: self.state.clone() })
            }
        }
    }

    /// Records something the model said. Refuses in states where content must
    /// be static, so a bug upstream cannot put generated text on the crisis
    /// screen.
    pub fn record_assistant_message(&mut self, text: &str) -> Result<()> {
        if !self.state.allows_generation() {
            bail!(
                "generated content is not allowed in state {}",
                self.state.name()
            );
        }
        let now = self.clock.now();
        self.record_message("assistant", text, now)?;
        self.store.save()
    }

    /// Applies a protocol event, persists the new state and saves the vault.
    pub fn apply(&mut self, event: Event) -> Result<State> {
        let next = self
            .state
            .next(event)
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        let now = self.clock.now();
        self.store.connection().execute(
            "UPDATE practice_sessions
             SET current_state = ?1,
                 states = json_insert(coalesce(states, '[]'), '$[#]', ?2)
             WHERE id = ?3",
            params![serde_json::to_string(&next)?, next.name(), self.session_id],
        )?;

        if let State::Ended { reason } = &next {
            self.store.connection().execute(
                "UPDATE practice_sessions SET ended_at = ?1, end_reason = ?2 WHERE id = ?3",
                params![now.to_rfc3339(), end_reason_str(reason), self.session_id],
            )?;
        }

        self.state = next.clone();
        self.store.save()?;
        Ok(next)
    }

    /// Which disclosure, if any, is owed right now.
    pub fn disclosure_due(&self) -> Result<Option<Disclosure>> {
        let conn = self.store.connection();

        let any_shown_at: Option<String> = conn
            .query_row("SELECT max(shown_at) FROM disclosures", [], |r| r.get(0))
            .unwrap_or(None);
        let last_practice_at: Option<String> = conn
            .query_row(
                "SELECT max(ended_at) FROM practice_sessions WHERE id != ?1",
                params![self.session_id],
                |r| r.get(0),
            )
            .unwrap_or(None);
        let last_periodic_at: Option<String> = conn
            .query_row(
                "SELECT max(shown_at) FROM disclosures
                 WHERE kind = 'periodic' AND shown_at >= ?1",
                params![self.started_at.to_rfc3339()],
                |r| r.get(0),
            )
            .unwrap_or(None);

        let history = History {
            any_shown_at: parse_opt(any_shown_at)?,
            last_practice_at: parse_opt(last_practice_at)?,
            current_session_started_at: Some(self.started_at),
            last_periodic_at: parse_opt(last_periodic_at)?,
        };

        Ok(disclosure::due(self.clock.now(), &history))
    }

    pub fn record_disclosure(&mut self, kind: Disclosure) -> Result<()> {
        let now = self.clock.now();
        self.store.connection().execute(
            "INSERT INTO disclosures (kind, shown_at) VALUES (?1, ?2)",
            params![kind.as_str(), now.to_rfc3339()],
        )?;
        self.store.save()
    }

    /// Stores a difficulty in the person's own words.
    ///
    /// This wording is the unit of measurement for everything that follows, so
    /// it is stored verbatim and never rewritten by the model.
    pub fn record_problem(&mut self, formulation: &str) -> Result<()> {
        let formulation = formulation.trim();
        if formulation.is_empty() {
            bail!("a difficulty cannot be empty");
        }

        let now = self.clock.now();
        let next_order: i64 = self
            .store
            .connection()
            .query_row(
                "SELECT coalesce(max(sort_order), -1) + 1 FROM problems",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);

        self.store.connection().execute(
            "INSERT INTO problems (formulation, created_at, sort_order) VALUES (?1, ?2, ?3)",
            params![formulation, now.to_rfc3339(), next_order],
        )?;
        self.store.save()
    }

    /// Stores the goal, in the person's own words.
    pub fn record_goal(&mut self, formulation: &str) -> Result<()> {
        let formulation = formulation.trim();
        if formulation.is_empty() {
            bail!("a goal cannot be empty");
        }
        let now = self.clock.now();
        self.store.connection().execute(
            "INSERT INTO goals (formulation, created_at) VALUES (?1, ?2)",
            params![formulation, now.to_rfc3339()],
        )?;
        self.store.save()
    }

    /// Stores one rating per difficulty. These single items are the measure of
    /// change — there are no clinical scales here by design.
    pub fn record_ratings(&mut self, ratings: &[(i64, i64)]) -> Result<()> {
        if ratings.is_empty() {
            bail!("nothing to record");
        }
        let now = self.clock.now().to_rfc3339();
        for (problem_id, value) in ratings {
            if !(0..=10).contains(value) {
                bail!("a rating must be between 0 and 10, got {value}");
            }
            self.store.connection().execute(
                "INSERT INTO problem_ratings (problem_id, value, rated_at) VALUES (?1, ?2, ?3)",
                params![problem_id, value, now],
            )?;
        }
        self.store.save()
    }

    /// Stores a situation once the person has confirmed the chain is right.
    pub fn record_situation(&mut self, situation: &SituationDraft) -> Result<()> {
        let now = self.clock.now();
        self.store.connection().execute(
            "INSERT INTO situations
             (session_id, trigger, feeling, avoidance, consequence, recorded_at, confirmed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)",
            params![
                self.session_id,
                situation.trigger,
                situation.feeling,
                situation.avoidance,
                situation.consequence,
                now.to_rfc3339(),
            ],
        )?;
        self.store.save()
    }

    /// Stores the planned action. Refuses an incomplete plan: "when" and
    /// "where" are what make the action happen.
    pub fn record_plan(&mut self, plan: &PlanDraft) -> Result<()> {
        let missing = plan.missing();
        if !missing.is_empty() {
            bail!("the plan is missing: {}", missing.join(", "));
        }
        let now = self.clock.now();
        self.store.connection().execute(
            "INSERT INTO planned_actions
             (session_id, description, scheduled_at, place, duration_min,
              obstacle, plan_b, stimulus_prep, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                self.session_id,
                plan.description,
                plan.scheduled_at,
                plan.place,
                plan.duration_min,
                plan.obstacle,
                plan.plan_b,
                plan.stimulus_prep,
                now.to_rfc3339(),
            ],
        )?;
        self.store.save()
    }

    /// Confirmed situations, newest first.
    pub fn recorded_situations(&self) -> Result<Vec<StoredSituation>> {
        let conn = self.store.connection();
        let mut stmt = conn.prepare(
            "SELECT trigger, feeling, avoidance, consequence, recorded_at
             FROM situations WHERE confirmed = 1 ORDER BY recorded_at DESC, id DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(StoredSituation {
                    trigger: r.get(0)?,
                    feeling: r.get(1)?,
                    avoidance: r.get(2)?,
                    consequence: r.get(3)?,
                    recorded_at: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// What has come up more than once across confirmed situations.
    ///
    /// Deliberately plain counting, not a network model: a stable idiographic
    /// network needs 75–100 observations and no more than six nodes, and with
    /// less than that a sparse graph says "not enough data", not "no link".
    /// Counting repetitions makes no claim it cannot support.
    pub fn repeated_elements(&self) -> Result<Vec<RepeatedElement>> {
        let situations = self.recorded_situations()?;
        let mut counts: std::collections::HashMap<(&'static str, String), usize> =
            std::collections::HashMap::new();

        for situation in &situations {
            let mut tally = |kind: &'static str, value: &Option<String>| {
                if let Some(text) = value {
                    let key = normalise_phrase(text);
                    if !key.is_empty() {
                        *counts.entry((kind, key)).or_insert(0) += 1;
                    }
                }
            };
            tally("trigger", &Some(situation.trigger.clone()));
            tally("avoidance", &situation.avoidance);
            tally("feeling", &situation.feeling);
        }

        let mut repeated: Vec<RepeatedElement> = counts
            .into_iter()
            .filter(|(_, count)| *count > 1)
            .map(|((kind, text), count)| RepeatedElement {
                kind: kind.to_string(),
                text,
                count,
            })
            .collect();
        repeated.sort_by(|a, b| b.count.cmp(&a.count).then(a.text.cmp(&b.text)));
        Ok(repeated)
    }

    /// Difficulties with their ids, for recording ratings against them.
    pub fn problems_with_ids(&self) -> Result<Vec<(i64, String)>> {
        let conn = self.store.connection();
        let mut stmt = conn.prepare(
            "SELECT id, formulation FROM problems WHERE retired_at IS NULL ORDER BY sort_order, id",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Difficulties recorded so far, in the order they were added.
    pub fn problems(&self) -> Result<Vec<String>> {
        let conn = self.store.connection();
        let mut stmt = conn.prepare(
            "SELECT formulation FROM problems WHERE retired_at IS NULL ORDER BY sort_order, id",
        )?;
        let rows = stmt
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The conversation so far, oldest first, as (role, content).
    pub fn transcript(&self) -> Result<Vec<(String, String)>> {
        let conn = self.store.connection();
        let mut stmt = conn.prepare(
            "SELECT role, content FROM messages
             WHERE session_id = ?1 ORDER BY said_at, id",
        )?;
        let rows = stmt
            .query_map(params![self.session_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn record_message(&self, role: &str, text: &str, at: DateTime<Utc>) -> Result<()> {
        self.store.connection().execute(
            "INSERT INTO messages (session_id, role, state, content, said_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                self.session_id,
                role,
                self.state.name(),
                text,
                at.to_rfc3339()
            ],
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct StoredSituation {
    pub trigger: String,
    pub feeling: Option<String>,
    pub avoidance: Option<String>,
    pub consequence: Option<String>,
    pub recorded_at: String,
}

#[derive(Debug, Clone)]
pub struct RepeatedElement {
    /// "trigger", "feeling" or "avoidance".
    pub kind: String,
    pub text: String,
    pub count: usize,
}

/// Case and punctuation folded, so "Промолчал." and "промолчал" count as one.
/// Nothing cleverer: stemming would merge phrases the person means to keep
/// apart, and this is their material, not the program's.
fn normalise_phrase(text: &str) -> String {
    text.to_lowercase()
        .replace('ё', "е")
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn end_reason_str(reason: &EndReason) -> &'static str {
    match reason {
        EndReason::Completed => "completed",
        EndReason::Abandoned => "abandoned",
        EndReason::TimeLimit => "time_limit",
        EndReason::Crisis => "crisis",
    }
}

fn parse_opt(value: Option<String>) -> Result<Option<DateTime<Utc>>> {
    match value {
        None => Ok(None),
        Some(raw) => Ok(Some(
            DateTime::parse_from_rfc3339(&raw)
                .with_context(|| format!("parsing timestamp {raw:?}"))?
                .with_timezone(&Utc),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::test_support::TestClock;
    use crate::protocol::ActionOutcome;
    use crate::safety::RuleBasedDetector;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "equilibrium-session-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    struct SharedClock(Arc<TestClock>);
    impl Clock for SharedClock {
        fn now(&self) -> DateTime<Utc> {
            self.0.now()
        }
    }

    fn practice(dir: &PathBuf, clock: Arc<TestClock>, branch: Branch) -> Practice {
        let store = Store::open(dir, "passphrase").unwrap();
        Practice::begin(
            store,
            Box::new(SharedClock(clock)),
            Box::new(RuleBasedDetector),
            branch,
        )
        .unwrap()
    }

    #[test]
    fn ordinary_input_continues_the_practice() {
        let dir = temp_dir("ordinary");
        let clock = Arc::new(TestClock::at("2026-08-29T10:00:00Z"));
        let mut p = practice(&dir, clock, Branch::Regular);

        let turn = p
            .submit_user_message("вчера не смог заставить себя выйти из дома")
            .unwrap();
        assert_eq!(turn, Turn::Continue { state: State::Opening });

        let messages: i64 = p
            .store()
            .connection()
            .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(messages, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_flagged_message_interrupts_and_leaves_no_content_in_the_audit_trail() {
        let dir = temp_dir("crisis");
        let clock = Arc::new(TestClock::at("2026-08-29T10:00:00Z"));
        let mut p = practice(&dir, clock, Branch::Regular);

        let turn = p.submit_user_message("я не хочу жить").unwrap();
        assert_eq!(turn, Turn::Crisis { kind: RiskKind::Suicidality });
        assert!(matches!(p.state(), State::Crisis { .. }));

        let (kind, action, version): (String, String, String) = p
            .store()
            .connection()
            .query_row(
                "SELECT trigger_kind, action_taken, guard_version FROM safety_events",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(kind, "Suicidality");
        assert!(!action.is_empty());
        assert_eq!(version, crate::safety::GUARD_VERSION);

        // The audit row must not carry what the person said.
        assert!(!kind.contains("жить") && !action.contains("жить"));

        // And generation must be refused while the crisis screen is up.
        assert!(p.record_assistant_message("что-нибудь утешительное").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_interrupted_practice_resumes_where_it_stopped() {
        let dir = temp_dir("resume");
        let clock = Arc::new(TestClock::at("2026-08-29T10:00:00Z"));

        {
            let mut p = practice(&dir, clock.clone(), Branch::Regular);
            p.apply(Event::OpeningAcknowledged).unwrap();
            p.apply(Event::ActionReviewed { outcome: ActionOutcome::Done })
                .unwrap();
            assert_eq!(p.state(), &State::Agenda);
        }

        let store = Store::open(&dir, "passphrase").unwrap();
        let resumed = Practice::resume(
            store,
            Box::new(SharedClock(clock)),
            Box::new(RuleBasedDetector),
        )
        .unwrap();

        match resumed {
            Ok(p) => assert_eq!(p.state(), &State::Agenda, "state was not restored"),
            Err(_) => panic!("an unfinished practice should have been found"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_finished_practice_is_not_resumed() {
        let dir = temp_dir("finished");
        let clock = Arc::new(TestClock::at("2026-08-29T10:00:00Z"));

        {
            let mut p = practice(&dir, clock.clone(), Branch::Regular);
            p.apply(Event::UserLeft).unwrap();
            assert!(p.state().is_terminal());
        }

        let store = Store::open(&dir, "passphrase").unwrap();
        let resumed = Practice::resume(
            store,
            Box::new(SharedClock(clock)),
            Box::new(RuleBasedDetector),
        )
        .unwrap();
        assert!(resumed.is_err(), "a finished practice must not be resumed");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_session_ends_at_the_time_cap_rather_than_running_on() {
        let dir = temp_dir("timecap");
        let clock = Arc::new(TestClock::at("2026-08-29T10:00:00Z"));
        let mut p = practice(&dir, clock.clone(), Branch::Regular);

        clock.advance(Duration::minutes(MAX_PRACTICE_MINUTES + 1));
        let turn = p.submit_user_message("ещё немного поговорим").unwrap();
        assert_eq!(turn, Turn::Ended { reason: EndReason::TimeLimit });

        let reason: String = p
            .store()
            .connection()
            .query_row(
                "SELECT end_reason FROM practice_sessions WHERE id = ?1",
                params![p.session_id()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(reason, "time_limit");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_first_run_owes_a_disclosure_and_recording_it_clears_the_debt() {
        let dir = temp_dir("disclosure");
        let clock = Arc::new(TestClock::at("2026-08-29T10:00:00Z"));
        let mut p = practice(&dir, clock.clone(), Branch::Onboarding);

        assert_eq!(p.disclosure_due().unwrap(), Some(Disclosure::FirstRun));
        p.record_disclosure(Disclosure::FirstRun).unwrap();
        assert_eq!(p.disclosure_due().unwrap(), None);

        // Three hours of continuous use owes the periodic reminder.
        clock.advance(Duration::hours(3));
        assert_eq!(p.disclosure_due().unwrap(), Some(Disclosure::Periodic));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A live walk through the whole stack against the local model.
    ///
    /// Ignored by default because it needs Ollama running. Run it with:
    ///   cargo test --lib -- --ignored --nocapture live_practice
    #[tokio::test]
    #[ignore]
    async fn live_practice() {
        use crate::critic;
        use crate::model::{Message, Ollama};

        let dir = temp_dir("live");
        let clock = Arc::new(TestClock::at("2026-08-30T19:00:00Z"));
        let model = Ollama::default();

        if !model.is_available().await {
            eprintln!("Ollama is not running — nothing to check");
            return;
        }

        let mut p = practice(&dir, clock, Branch::Regular);

        let script = [
            "не вышел вчера гулять как планировал. я вообще безнадёжен, у меня никогда ничего не получается",
            "было поздно и я устал после работы, просто лёг",
            "наверное мог бы выйти сразу как пришёл, а не ложиться",
        ];

        println!("\n===== практика =====");
        for line in script {
            println!("\n[шаг: {}]", p.state().name());
            println!("человек: {line}");

            match p.submit_user_message(line).unwrap() {
                Turn::Crisis { kind } => {
                    println!("<< гейт сработал: {kind:?}, практика прервана >>");
                    break;
                }
                Turn::Ended { reason } => {
                    println!("<< практика завершена: {reason:?} >>");
                    break;
                }
                Turn::Continue { state } => {
                    let history: Vec<Message> = p
                        .transcript()
                        .unwrap()
                        .into_iter()
                        .map(|(role, content)| Message { role, content })
                        .collect();
                    let reply = model
                        .respond_reviewed(&state, &history)
                        .await
                        .unwrap_or_else(|_| critic::fallback(&state).to_string());
                    println!("программа: {reply}");
                    assert!(
                        critic::review(&reply).is_empty(),
                        "a reply reached the user while failing review: {:?}",
                        critic::review(&reply)
                    );
                    p.record_assistant_message(&reply).unwrap();
                }
            }
        }

        // And the guard, on the same practice.
        println!("\n[шаг: {}]", p.state().name());
        println!("человек: иногда думаю что не хочу жить");
        let turn = p.submit_user_message("иногда думаю что не хочу жить").unwrap();
        println!("результат: {turn:?}");
        assert!(matches!(turn, Turn::Crisis { .. }));
        println!("===== конец =====\n");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Walks onboarding the way a person does and checks the data landed.
    ///
    /// Written after shipping a build where "record as a difficulty" advanced
    /// the step without storing anything: the state machine was right, the
    /// table was empty, and every existing test passed. Asserting on state
    /// transitions is not the same as asserting the product did its job.
    #[test]
    fn onboarding_actually_stores_what_the_person_wrote() {
        let dir = temp_dir("onboarding-data");
        let clock = Arc::new(TestClock::at("2026-08-30T10:00:00Z"));
        let mut p = practice(&dir, clock, Branch::Onboarding);

        p.apply(Event::DisclosureAccepted { adult: true }).unwrap();

        let first = "по вечерам не могу заставить себя выйти из дома";
        let second = "стесняюсь и побаиваюсь людей";

        p.record_problem(first).unwrap();
        p.apply(Event::ProblemAdded).unwrap();
        p.record_problem(second).unwrap();
        p.apply(Event::ProblemAdded).unwrap();

        // The wording is the unit of measurement: it must be stored verbatim.
        let stored = p.problems().unwrap();
        assert_eq!(stored, vec![first.to_string(), second.to_string()]);

        // And it must survive a restart, not just live in memory.
        drop(p);
        let store = Store::open(&dir, "passphrase").unwrap();
        let reopened = Practice::resume(
            store,
            Box::new(SharedClock(Arc::new(TestClock::at("2026-08-30T11:00:00Z")))),
            Box::new(RuleBasedDetector),
        )
        .unwrap();
        match reopened {
            Ok(p) => assert_eq!(p.problems().unwrap().len(), 2),
            Err(_) => panic!("the unfinished onboarding should resume"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every step that carries data must leave a row behind.
    ///
    /// The first build advanced through all of these while writing nothing:
    /// goal, ratings, situation and plan were all lost. One assertion per
    /// table, so a step that silently stops persisting fails here.
    #[test]
    fn every_step_that_carries_data_writes_a_row() {
        let dir = temp_dir("all-data");
        let clock = Arc::new(TestClock::at("2026-08-30T10:00:00Z"));
        let mut p = practice(&dir, clock, Branch::Onboarding);

        p.apply(Event::DisclosureAccepted { adult: true }).unwrap();
        p.record_problem("по вечерам не могу выйти из дома").unwrap();
        p.apply(Event::ProblemAdded).unwrap();
        p.record_problem("молчу на планёрках").unwrap();
        p.apply(Event::ProblemAdded).unwrap();
        p.apply(Event::ProblemsFinished).unwrap();

        p.record_goal("сказать на планёрке про сроки").unwrap();
        p.apply(Event::GoalSet).unwrap();

        let ids: Vec<i64> = p.problems_with_ids().unwrap().into_iter().map(|(id, _)| id).collect();
        p.record_ratings(&[(ids[0], 7), (ids[1], 4)]).unwrap();
        p.apply(Event::BaselineRecorded).unwrap();

        p.record_situation(&SituationDraft {
            trigger: "планёрка, зашла речь о сроках".into(),
            feeling: Some("сжалось внутри".into()),
            avoidance: Some("промолчал".into()),
            consequence: Some("весь день было противно".into()),
        })
        .unwrap();

        p.record_plan(&PlanDraft {
            description: "написать в чат про сроки".into(),
            scheduled_at: Some("2026-08-31T10:00".into()),
            place: Some("рабочий чат".into()),
            duration_min: Some(5),
            obstacle: Some("страшно, что переспросят".into()),
            plan_b: Some("написать одному человеку".into()),
            stimulus_prep: Some("заранее набросать текст".into()),
        })
        .unwrap();

        let conn = p.store().connection();
        let count = |table: &str| -> i64 {
            conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap()
        };

        assert_eq!(count("problems"), 2, "difficulties were not stored");
        assert_eq!(count("goals"), 1, "the goal was not stored");
        assert_eq!(count("problem_ratings"), 2, "the baseline was not stored");
        assert_eq!(count("situations"), 1, "the situation was not stored");
        assert_eq!(count("planned_actions"), 1, "the plan was not stored");

        // The plan's required fields are what make the action happen.
        let (when, place): (String, String) = conn
            .query_row(
                "SELECT scheduled_at, place FROM planned_actions",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(when, "2026-08-31T10:00");
        assert_eq!(place, "рабочий чат");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn counts_what_repeats_across_situations() {
        let dir = temp_dir("repeats");
        let clock = Arc::new(TestClock::at("2026-08-30T10:00:00Z"));
        let mut p = practice(&dir, clock, Branch::Regular);

        let silence = |trigger: &str| SituationDraft {
            trigger: trigger.into(),
            feeling: Some("сжалось внутри".into()),
            avoidance: Some("Промолчал.".into()),
            consequence: None,
        };

        p.record_situation(&silence("планёрка")).unwrap();
        p.record_situation(&silence("разговор с начальником")).unwrap();
        p.record_situation(&SituationDraft {
            trigger: "звонок маме".into(),
            feeling: None,
            avoidance: Some("не перезвонил".into()),
            consequence: None,
        })
        .unwrap();

        let repeated = p.repeated_elements().unwrap();

        // Case and punctuation must not split one behaviour into two.
        let avoidance = repeated
            .iter()
            .find(|r| r.kind == "avoidance" && r.text == "промолчал")
            .expect("the repeated avoidance should have been counted");
        assert_eq!(avoidance.count, 2);

        // Things seen once are not patterns and must not be listed.
        assert!(
            !repeated.iter().any(|r| r.text.contains("не перезвонил")),
            "a single occurrence was reported as a pattern"
        );
        assert!(!repeated.iter().any(|r| r.text == "планерка"));

        assert_eq!(p.recorded_situations().unwrap().len(), 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_incomplete_plan_is_refused() {
        let dir = temp_dir("incomplete-plan");
        let clock = Arc::new(TestClock::at("2026-08-30T10:00:00Z"));
        let mut p = practice(&dir, clock, Branch::Regular);

        let err = p
            .record_plan(&PlanDraft {
                description: "погулять".into(),
                ..Default::default()
            })
            .unwrap_err()
            .to_string();
        assert!(err.contains("scheduled_at"), "{err}");
        assert!(err.contains("place"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rating_outside_the_scale_is_refused() {
        let dir = temp_dir("bad-rating");
        let clock = Arc::new(TestClock::at("2026-08-30T10:00:00Z"));
        let mut p = practice(&dir, clock, Branch::Onboarding);
        p.record_problem("что-то").unwrap();
        let id = p.problems_with_ids().unwrap()[0].0;

        assert!(p.record_ratings(&[(id, 11)]).is_err());
        assert!(p.record_ratings(&[(id, -1)]).is_err());
        assert!(p.record_ratings(&[(id, 10)]).is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_difficulty_is_refused() {
        let dir = temp_dir("empty-problem");
        let clock = Arc::new(TestClock::at("2026-08-30T10:00:00Z"));
        let mut p = practice(&dir, clock, Branch::Onboarding);

        assert!(p.record_problem("   ").is_err());
        assert!(p.problems().unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_is_accepted_after_the_practice_ends() {
        let dir = temp_dir("after-end");
        let clock = Arc::new(TestClock::at("2026-08-29T10:00:00Z"));
        let mut p = practice(&dir, clock, Branch::Regular);

        p.apply(Event::UserLeft).unwrap();
        assert!(p.submit_user_message("ещё кое-что").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
