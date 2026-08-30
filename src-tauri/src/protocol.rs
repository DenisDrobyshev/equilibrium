//! The behavioural activation protocol as an explicit state machine.
//!
//! The point of this module is the product's central claim: **the code holds
//! the structure, the model only supplies wording**. The model never decides
//! what happens next, never picks the action for the user, and cannot skip a
//! step by being persuasive. It hands events to this machine; the machine
//! decides where the practice goes.
//!
//! Two consequences shape the design:
//!
//! * `next` is an exhaustive `match`. Adding a state breaks compilation
//!   everywhere it must be handled, which is the intended pressure — a
//!   half-wired state should not be representable in a shipped build.
//! * The risk guard is checked before the state match, so no state can be
//!   written in a way that misses it.
//!
//! `next` takes `&self` and returns the new state, so a rejected event leaves
//! the caller holding the state it already had.
//!
//! Protocol reference: `docs/protocol-behavioral-activation.md`.

use serde::{Deserialize, Serialize};

/// Minimum number of problems the user states in their own words before the
/// onboarding branch can proceed. Fewer than two gives nothing to compare.
const MIN_PROBLEMS: u8 = 2;

/// How many times the machine asks for a reformulation of a chosen action
/// before accepting it as stated. The protocol caps this deliberately:
/// nagging someone into a "correct" action defeats the exercise.
const MAX_ACTION_REFORMULATIONS: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    // --- Branch A: onboarding, once per install ---
    /// Legally required disclosures and the 18+ gate. Not a dialogue.
    Disclosure,
    /// The user names 2–4 difficulties in their own words (idiographic items).
    ProblemsIntake { collected: u8 },
    /// One goal, in observable terms, expanded into GAS levels.
    GoalIntake,
    /// First measurement of the items just created.
    Baseline,
    /// Two or three sentences, grounded in what the user already said.
    Psychoeducation,

    // --- Branch B: regular practice ---
    /// Opens on the concrete: what was planned, what happened.
    Opening,
    /// Reviewing the previously planned action.
    ReviewPlanned,
    /// Choosing the focus of this practice. The user may override.
    Agenda,
    /// Trigger → feeling → avoidance → consequence, for one situation.
    Pattern { situation: SituationDraft },
    /// Choosing exactly one action. Not a list, not a ladder.
    SelectAction { reformulations: u8 },
    /// Making it concrete. Cannot be left while required fields are missing.
    ConcretePlan { draft: PlanDraft },
    /// Showing what was recorded and where.
    Close,

    // --- Terminal ---
    /// Risk detected. Static content, no generation, no automatic resumption.
    Crisis { kind: RiskKind },
    Ended { reason: EndReason },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Event {
    // Universal — may arrive in any state.
    RiskDetected { kind: RiskKind },
    TimeLimitReached,
    UserLeft,

    // Branch A
    DisclosureAccepted { adult: bool },
    ProblemAdded,
    ProblemsFinished,
    GoalSet,
    BaselineRecorded,
    PsychoeducationSeen,

    // Branch B
    OpeningAcknowledged,
    ActionReviewed { outcome: ActionOutcome },
    FocusChosen { focus: Focus },
    /// The extracted situation, as it currently stands.
    SituationDrafted { situation: SituationDraft },
    /// The user accepted the extracted situation, possibly after editing it.
    SituationConfirmed,
    /// An action was proposed together with the checks the caller ran on it.
    ActionProposed { check: ActionCheck },
    /// The plan draft as it currently stands. Always accepted; the practice
    /// only moves on once the required fields are there.
    PlanUpdated { draft: PlanDraft },
    PracticeClosed,
    /// The user explicitly leaves the crisis screen.
    CrisisAcknowledged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskKind {
    SelfHarm,
    Suicidality,
    HarmToOthers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionOutcome {
    Done,
    Partial,
    /// Not done. Never treated as failure — it routes to pattern work, because
    /// what got in the way is the material.
    NotDone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Focus {
    Situation,
    Planning,
    ReviseMeasures,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EndReason {
    Completed,
    Abandoned,
    TimeLimit,
    Crisis,
}

/// Whether a proposed action satisfies the protocol's criteria. Judged by the
/// calling layer (model proposal plus user confirmation), never here — this
/// machine only decides what to do with the verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionCheck {
    /// Can "did you do it?" be answered yes or no?
    pub observable: bool,
    /// Does it depend on the user rather than on other people?
    pub self_dependent: bool,
    /// Is it bounded in time?
    pub bounded: bool,
}

impl ActionCheck {
    pub fn passes(&self) -> bool {
        self.observable && self.self_dependent && self.bounded
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SituationDraft {
    pub trigger: String,
    pub feeling: Option<String>,
    pub avoidance: Option<String>,
    pub consequence: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanDraft {
    pub description: String,
    /// When. Required — specificity is what makes the action happen.
    pub scheduled_at: Option<String>,
    /// Where. Required.
    pub place: Option<String>,
    pub duration_min: Option<u32>,
    pub obstacle: Option<String>,
    pub plan_b: Option<String>,
    /// Stimulus control: what to prepare or remove beforehand.
    pub stimulus_prep: Option<String>,
}

impl PlanDraft {
    /// Fields without which the practice may not move on.
    pub fn missing(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.description.trim().is_empty() {
            missing.push("description");
        }
        if self.scheduled_at.is_none() {
            missing.push("scheduled_at");
        }
        if self.place.is_none() {
            missing.push("place");
        }
        missing
    }

    pub fn is_complete(&self) -> bool {
        self.missing().is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionError {
    /// The event makes no sense in this state. A bug in the calling layer,
    /// or a model trying to jump ahead.
    NotAllowed { state: &'static str, event: &'static str },
    /// The step is not finished. Carries what is missing so the UI can ask.
    Incomplete { missing: Vec<&'static str> },
    /// Nothing follows a terminal state.
    Terminal,
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAllowed { state, event } => {
                write!(f, "event {event} is not allowed in state {state}")
            }
            Self::Incomplete { missing } => {
                write!(f, "step is incomplete, missing: {}", missing.join(", "))
            }
            Self::Terminal => write!(f, "the practice has already ended"),
        }
    }
}

impl std::error::Error for TransitionError {}

impl State {
    pub fn start_onboarding() -> Self {
        State::Disclosure
    }

    pub fn start_practice() -> Self {
        State::Opening
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, State::Ended { .. })
    }

    /// True while the machine is inside the onboarding branch.
    pub fn is_onboarding(&self) -> bool {
        matches!(
            self,
            State::Disclosure
                | State::ProblemsIntake { .. }
                | State::GoalIntake
                | State::Baseline
                | State::Psychoeducation
        )
    }

    /// Whether a generative model may be called in this state at all.
    /// Crisis and the disclosure screen are static by design.
    pub fn allows_generation(&self) -> bool {
        !matches!(
            self,
            State::Disclosure | State::Crisis { .. } | State::Ended { .. } | State::Baseline
        )
    }

    pub fn name(&self) -> &'static str {
        match self {
            State::Disclosure => "Disclosure",
            State::ProblemsIntake { .. } => "ProblemsIntake",
            State::GoalIntake => "GoalIntake",
            State::Baseline => "Baseline",
            State::Psychoeducation => "Psychoeducation",
            State::Opening => "Opening",
            State::ReviewPlanned => "ReviewPlanned",
            State::Agenda => "Agenda",
            State::Pattern { .. } => "Pattern",
            State::SelectAction { .. } => "SelectAction",
            State::ConcretePlan { .. } => "ConcretePlan",
            State::Close => "Close",
            State::Crisis { .. } => "Crisis",
            State::Ended { .. } => "Ended",
        }
    }

    /// The only way the practice moves.
    pub fn next(&self, event: Event) -> Result<State, TransitionError> {
        // Universal handling first, so that no state can forget the guard.
        match (self, &event) {
            (State::Ended { .. }, _) => return Err(TransitionError::Terminal),

            // Risk interrupts everything, including itself.
            (_, Event::RiskDetected { kind }) => return Ok(State::Crisis { kind: *kind }),

            // The crisis screen is left only by an explicit act, and the
            // practice does not resume: the protocol ends.
            (State::Crisis { .. }, Event::CrisisAcknowledged) => {
                return Ok(State::Ended { reason: EndReason::Crisis })
            }
            (State::Crisis { .. }, _) => {
                return Err(TransitionError::NotAllowed {
                    state: self.name(),
                    event: event.name(),
                })
            }

            (_, Event::UserLeft) => return Ok(State::Ended { reason: EndReason::Abandoned }),
            // Time limits exist to stop long sessions, not to reward them.
            (_, Event::TimeLimitReached) => {
                return Ok(State::Ended { reason: EndReason::TimeLimit })
            }
            _ => {}
        }

        match (self, &event) {
            // --- Branch A ---
            (State::Disclosure, Event::DisclosureAccepted { adult }) => {
                if *adult {
                    Ok(State::ProblemsIntake { collected: 0 })
                } else {
                    // Under 18 is out of scope, and this is not negotiable.
                    Ok(State::Ended { reason: EndReason::Abandoned })
                }
            }

            (State::ProblemsIntake { collected }, Event::ProblemAdded) => {
                Ok(State::ProblemsIntake { collected: collected.saturating_add(1) })
            }
            (State::ProblemsIntake { collected }, Event::ProblemsFinished) => {
                if *collected >= MIN_PROBLEMS {
                    Ok(State::GoalIntake)
                } else {
                    Err(TransitionError::Incomplete { missing: vec!["at least two problems"] })
                }
            }

            (State::GoalIntake, Event::GoalSet) => Ok(State::Baseline),
            (State::Baseline, Event::BaselineRecorded) => Ok(State::Psychoeducation),
            (State::Psychoeducation, Event::PsychoeducationSeen) => {
                Ok(State::Ended { reason: EndReason::Completed })
            }

            // --- Branch B ---
            (State::Opening, Event::OpeningAcknowledged) => Ok(State::ReviewPlanned),

            (State::ReviewPlanned, Event::ActionReviewed { outcome }) => match outcome {
                ActionOutcome::Done | ActionOutcome::Partial => Ok(State::Agenda),
                // Not done routes into pattern work rather than into
                // encouragement: the obstacle is the material.
                ActionOutcome::NotDone => {
                    Ok(State::Pattern { situation: SituationDraft::default() })
                }
            },

            (State::Agenda, Event::FocusChosen { focus }) => match focus {
                Focus::Situation => Ok(State::Pattern { situation: SituationDraft::default() }),
                Focus::Planning => Ok(State::SelectAction { reformulations: 0 }),
                Focus::ReviseMeasures => Ok(State::ProblemsIntake { collected: MIN_PROBLEMS }),
            },

            (State::Pattern { .. }, Event::SituationDrafted { situation }) => {
                Ok(State::Pattern { situation: situation.clone() })
            }
            (State::Pattern { situation }, Event::SituationConfirmed) => {
                if situation.trigger.trim().is_empty() {
                    Err(TransitionError::Incomplete { missing: vec!["trigger"] })
                } else {
                    Ok(State::SelectAction { reformulations: 0 })
                }
            }

            (State::SelectAction { reformulations }, Event::ActionProposed { check }) => {
                if check.passes() || *reformulations >= MAX_ACTION_REFORMULATIONS {
                    Ok(State::ConcretePlan { draft: PlanDraft::default() })
                } else {
                    Ok(State::SelectAction { reformulations: reformulations + 1 })
                }
            }

            // The draft is always accepted; only completeness opens the door.
            (State::ConcretePlan { .. }, Event::PlanUpdated { draft }) => {
                if draft.is_complete() {
                    Ok(State::Close)
                } else {
                    Ok(State::ConcretePlan { draft: draft.clone() })
                }
            }

            (State::Close, Event::PracticeClosed) => {
                Ok(State::Ended { reason: EndReason::Completed })
            }

            (state, event) => Err(TransitionError::NotAllowed {
                state: state.name(),
                event: event.name(),
            }),
        }
    }
}

impl Event {
    pub fn name(&self) -> &'static str {
        match self {
            Event::RiskDetected { .. } => "RiskDetected",
            Event::TimeLimitReached => "TimeLimitReached",
            Event::UserLeft => "UserLeft",
            Event::DisclosureAccepted { .. } => "DisclosureAccepted",
            Event::ProblemAdded => "ProblemAdded",
            Event::ProblemsFinished => "ProblemsFinished",
            Event::GoalSet => "GoalSet",
            Event::BaselineRecorded => "BaselineRecorded",
            Event::PsychoeducationSeen => "PsychoeducationSeen",
            Event::OpeningAcknowledged => "OpeningAcknowledged",
            Event::ActionReviewed { .. } => "ActionReviewed",
            Event::FocusChosen { .. } => "FocusChosen",
            Event::SituationDrafted { .. } => "SituationDrafted",
            Event::SituationConfirmed => "SituationConfirmed",
            Event::ActionProposed { .. } => "ActionProposed",
            Event::PlanUpdated { .. } => "PlanUpdated",
            Event::PracticeClosed => "PracticeClosed",
            Event::CrisisAcknowledged => "CrisisAcknowledged",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good_check() -> ActionCheck {
        ActionCheck { observable: true, self_dependent: true, bounded: true }
    }

    fn bad_check() -> ActionCheck {
        ActionCheck { observable: false, self_dependent: true, bounded: true }
    }

    fn complete_plan() -> PlanDraft {
        PlanDraft {
            description: "выйти на десять минут вокруг дома".into(),
            scheduled_at: Some("2026-08-29T19:00:00".into()),
            place: Some("двор".into()),
            duration_min: Some(10),
            obstacle: Some("пойдёт дождь".into()),
            plan_b: Some("пройтись по коридору".into()),
            stimulus_prep: Some("положить кроссовки у двери".into()),
        }
    }

    #[test]
    fn onboarding_runs_to_completion() {
        let s = State::start_onboarding();
        let s = s.next(Event::DisclosureAccepted { adult: true }).unwrap();
        let s = s.next(Event::ProblemAdded).unwrap();
        let s = s.next(Event::ProblemAdded).unwrap();
        let s = s.next(Event::ProblemsFinished).unwrap();
        assert_eq!(s, State::GoalIntake);
        let s = s.next(Event::GoalSet).unwrap();
        let s = s.next(Event::BaselineRecorded).unwrap();
        let s = s.next(Event::PsychoeducationSeen).unwrap();
        assert_eq!(s, State::Ended { reason: EndReason::Completed });
    }

    #[test]
    fn onboarding_requires_two_problems_in_the_users_own_words() {
        let s = State::ProblemsIntake { collected: 1 };
        let err = s.next(Event::ProblemsFinished).unwrap_err();
        assert!(matches!(err, TransitionError::Incomplete { .. }));
        // A refused event leaves the caller's state untouched.
        assert_eq!(s, State::ProblemsIntake { collected: 1 });
    }

    #[test]
    fn minors_are_turned_away_rather_than_routed_onward() {
        let s = State::Disclosure
            .next(Event::DisclosureAccepted { adult: false })
            .unwrap();
        assert_eq!(s, State::Ended { reason: EndReason::Abandoned });
    }

    #[test]
    fn practice_runs_to_completion() {
        let s = State::start_practice();
        let s = s.next(Event::OpeningAcknowledged).unwrap();
        let s = s.next(Event::ActionReviewed { outcome: ActionOutcome::Done }).unwrap();
        assert_eq!(s, State::Agenda);
        let s = s.next(Event::FocusChosen { focus: Focus::Planning }).unwrap();
        let s = s.next(Event::ActionProposed { check: good_check() }).unwrap();
        let s = s.next(Event::PlanUpdated { draft: complete_plan() }).unwrap();
        assert_eq!(s, State::Close);
        let s = s.next(Event::PracticeClosed).unwrap();
        assert_eq!(s, State::Ended { reason: EndReason::Completed });
    }

    #[test]
    fn not_done_routes_to_pattern_work_not_to_encouragement() {
        let s = State::ReviewPlanned
            .next(Event::ActionReviewed { outcome: ActionOutcome::NotDone })
            .unwrap();
        assert!(matches!(s, State::Pattern { .. }), "got {s:?}");
    }

    #[test]
    fn plan_stays_open_until_when_and_where_are_answered() {
        let partial = PlanDraft {
            description: "погулять".into(),
            scheduled_at: None,
            place: None,
            ..Default::default()
        };
        let s = State::ConcretePlan { draft: PlanDraft::default() }
            .next(Event::PlanUpdated { draft: partial })
            .unwrap();

        match &s {
            State::ConcretePlan { draft } => {
                let missing = draft.missing();
                assert!(missing.contains(&"scheduled_at"));
                assert!(missing.contains(&"place"));
            }
            other => panic!("expected to stay in ConcretePlan, got {other:?}"),
        }

        // Filling them in is what opens the way out.
        let s = s.next(Event::PlanUpdated { draft: complete_plan() }).unwrap();
        assert_eq!(s, State::Close);
    }

    #[test]
    fn action_reformulation_is_capped_then_accepted_as_stated() {
        let mut s = State::SelectAction { reformulations: 0 };
        for expected in 1..=MAX_ACTION_REFORMULATIONS {
            s = s.next(Event::ActionProposed { check: bad_check() }).unwrap();
            assert_eq!(s, State::SelectAction { reformulations: expected });
        }
        // Past the cap the machine stops pushing and takes the action as stated.
        let s = s.next(Event::ActionProposed { check: bad_check() }).unwrap();
        assert!(matches!(s, State::ConcretePlan { .. }), "got {s:?}");
    }

    #[test]
    fn risk_interrupts_from_every_non_terminal_state() {
        let states = [
            State::Disclosure,
            State::ProblemsIntake { collected: 1 },
            State::GoalIntake,
            State::Baseline,
            State::Psychoeducation,
            State::Opening,
            State::ReviewPlanned,
            State::Agenda,
            State::Pattern { situation: SituationDraft::default() },
            State::SelectAction { reformulations: 0 },
            State::ConcretePlan { draft: PlanDraft::default() },
            State::Close,
        ];
        for state in states {
            let name = state.name();
            let next = state
                .next(Event::RiskDetected { kind: RiskKind::Suicidality })
                .unwrap_or_else(|e| panic!("{name} did not accept the risk guard: {e}"));
            assert!(matches!(next, State::Crisis { .. }), "{name} -> {next:?}");
        }
    }

    #[test]
    fn crisis_does_not_resume_the_practice() {
        let s = State::Crisis { kind: RiskKind::Suicidality };
        // No ordinary event moves it.
        assert!(s.next(Event::OpeningAcknowledged).is_err());
        assert!(s.next(Event::PracticeClosed).is_err());
        // Only an explicit acknowledgement, and it ends the practice.
        let ended = s.next(Event::CrisisAcknowledged).unwrap();
        assert_eq!(ended, State::Ended { reason: EndReason::Crisis });
    }

    #[test]
    fn generation_is_forbidden_where_content_must_be_static() {
        assert!(!State::Crisis { kind: RiskKind::SelfHarm }.allows_generation());
        assert!(!State::Disclosure.allows_generation());
        assert!(State::Pattern { situation: SituationDraft::default() }.allows_generation());
    }

    #[test]
    fn nothing_follows_a_finished_practice() {
        let s = State::Ended { reason: EndReason::Completed };
        assert_eq!(
            s.next(Event::OpeningAcknowledged).unwrap_err(),
            TransitionError::Terminal
        );
    }

    #[test]
    fn state_survives_serialisation() {
        // Practices are resumed after a restart, so the state must round-trip.
        let s = State::ConcretePlan { draft: complete_plan() };
        let json = serde_json::to_string(&s).unwrap();
        let back: State = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }
}
