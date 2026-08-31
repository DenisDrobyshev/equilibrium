//! Tauri commands — the boundary between the interface and the runtime.
//!
//! The UI never touches the protocol or the guard directly. It sends what the
//! person typed and gets back the new state; everything that decides anything
//! happens behind this line.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::Serialize;
use tauri::{Manager, State as TauriState};

use crate::clock::SystemClock;
use crate::db::Store;
use crate::model::{Message, Ollama};
use crate::protocol::{
    ActionCheck, ActionOutcome, Event, Focus, PlanDraft, SituationDraft, State,
};
use crate::safety::{self, RuleBasedDetector};
use crate::session::{Branch, Practice, Turn};
#[allow(unused_imports)]
use crate::session::{RepeatedElement, StoredSituation};

pub struct AppState {
    practice: Mutex<Option<Practice>>,
    model: Ollama,
    data_dir: Mutex<Option<PathBuf>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            practice: Mutex::new(None),
            model: Ollama::default(),
            data_dir: Mutex::new(None),
        }
    }
}

#[derive(Serialize)]
pub struct View {
    /// Name of the protocol state, for the UI to decide what controls to show.
    pub state: String,
    /// What to do on this step. Shown whether or not the model said anything.
    pub hint: String,
    pub transcript: Vec<Line>,
    pub crisis: Option<CrisisView>,
    pub finished: bool,
    /// What the step still needs before the practice can move on.
    pub missing: Vec<String>,
    /// Difficulties recorded so far, in the person's own words.
    pub problems: Vec<ProblemView>,
    /// The chain assembled so far in the pattern step, for the person to
    /// check and correct before anything is stored.
    pub situation: Option<SituationView>,
}

#[derive(Serialize)]
pub struct SituationView {
    pub trigger: String,
    pub feeling: Option<String>,
    pub avoidance: Option<String>,
    pub consequence: Option<String>,
    /// True once the chain has enough to be worth confirming.
    pub ready: bool,
}

#[derive(Serialize)]
pub struct ProblemView {
    pub id: i64,
    pub text: String,
}

#[derive(Serialize)]
pub struct Line {
    pub role: String,
    pub content: String,
}

#[derive(Serialize)]
pub struct CrisisView {
    pub resources: Vec<ResourceView>,
}

#[derive(Serialize)]
pub struct ResourceView {
    pub name: String,
    pub contact: String,
}

#[derive(Serialize)]
pub struct VaultStatus {
    pub resumed: bool,
    pub needs_onboarding: bool,
    pub model_available: bool,
}

type CmdResult<T> = std::result::Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Opens (or creates) the vault. The passphrase never leaves this process.
#[tauri::command]
pub async fn open_vault(
    app: tauri::AppHandle,
    state: TauriState<'_, AppState>,
    passphrase: String,
) -> CmdResult<VaultStatus> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(err)?
        .join("equilibrium");

    let store = Store::open(&data_dir, &passphrase).map_err(err)?;

    let onboarded: i64 = store
        .connection()
        .query_row("SELECT count(*) FROM problems", [], |r| r.get(0))
        .unwrap_or(0);

    let resumed = match Practice::resume(
        store,
        Box::new(SystemClock),
        Box::new(RuleBasedDetector),
    )
    .map_err(err)?
    {
        Ok(practice) => {
            *state.practice.lock().unwrap() = Some(practice);
            true
        }
        Err(_store) => false,
    };

    *state.data_dir.lock().unwrap() = Some(data_dir);

    Ok(VaultStatus {
        resumed,
        needs_onboarding: onboarded == 0,
        model_available: state.model.is_available().await,
    })
}

#[tauri::command]
pub async fn begin_practice(
    app: tauri::AppHandle,
    state: TauriState<'_, AppState>,
    passphrase: String,
    onboarding: bool,
) -> CmdResult<View> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(err)?
        .join("equilibrium");
    let store = Store::open(&data_dir, &passphrase).map_err(err)?;

    let branch = if onboarding { Branch::Onboarding } else { Branch::Regular };
    let practice = Practice::begin(
        store,
        Box::new(SystemClock),
        Box::new(RuleBasedDetector),
        branch,
    )
    .map_err(err)?;

    *state.practice.lock().unwrap() = Some(practice);
    drop(data_dir);

    // The disclosure branch starts static; the regular branch opens with the
    // model asking about the planned action.
    if onboarding {
        return view(&state);
    }
    generate_reply(&state).await?;
    view(&state)
}

/// Everything the person types goes through here.
#[tauri::command]
pub async fn send_message(
    state: TauriState<'_, AppState>,
    text: String,
) -> CmdResult<View> {
    let turn = {
        let mut guard = state.practice.lock().unwrap();
        let practice = guard.as_mut().ok_or("no practice is open")?;
        practice.submit_user_message(&text).map_err(err)?
    };

    match turn {
        // The guard fired or time ran out: no generation at all.
        Turn::Crisis { .. } | Turn::Ended { .. } => view(&state),
        Turn::Continue { state: protocol_state } => {
            // Telling the program what happened with the planned action *is*
            // acknowledging the opening. Making the person press "Next" after
            // answering leaves the practice stuck on one step, which is what it
            // did on first use.
            if matches!(protocol_state, State::Opening) {
                let mut guard = state.practice.lock().unwrap();
                let practice = guard.as_mut().ok_or("no practice is open")?;
                let _ = practice.apply(Event::OpeningAcknowledged);
            }

            generate_reply(&state).await?;
            extract_situation_if_relevant(&state).await?;
            view(&state)
        }
    }
}

/// In the pattern step, reads the chain out of the conversation so the person
/// can see and correct it. Nothing is stored until they confirm.
///
/// A failure here is not fatal: the practice continues with whatever the chain
/// already had, because losing an extraction is better than losing the turn.
async fn extract_situation_if_relevant(state: &TauriState<'_, AppState>) -> CmdResult<()> {
    let history = {
        let guard = state.practice.lock().unwrap();
        let practice = guard.as_ref().ok_or("no practice is open")?;
        if !matches!(practice.state(), State::Pattern { .. }) {
            return Ok(());
        }
        practice
            .transcript()
            .map_err(err)?
            .into_iter()
            .map(|(role, content)| Message { role, content })
            .collect::<Vec<_>>()
    };

    let Ok(extracted) = state.model.extract_situation(&history).await else {
        return Ok(());
    };

    let situation = SituationDraft {
        trigger: extracted.trigger.unwrap_or_default(),
        feeling: extracted.feeling,
        avoidance: extracted.avoidance,
        consequence: extracted.consequence,
    };

    let mut guard = state.practice.lock().unwrap();
    let practice = guard.as_mut().ok_or("no practice is open")?;
    let _ = practice.apply(Event::SituationDrafted { situation });
    Ok(())
}

/// Stores a difficulty the person wrote, then advances the step.
///
/// Kept separate from `advance` because the wording itself is the point: it is
/// what every later measurement compares against.
#[tauri::command]
pub async fn record_problem(
    state: TauriState<'_, AppState>,
    text: String,
) -> CmdResult<View> {
    {
        let mut guard = state.practice.lock().unwrap();
        let practice = guard.as_mut().ok_or("no practice is open")?;
        practice.record_problem(&text).map_err(err)?;
        practice.apply(Event::ProblemAdded).map_err(err)?;
    }
    generate_reply(&state).await?;
    view(&state)
}

#[derive(Serialize)]
pub struct HistoryView {
    pub situations: Vec<StoredSituationView>,
    pub repeated: Vec<RepeatedView>,
    pub problems: Vec<ProblemView>,
}

#[derive(Serialize)]
pub struct StoredSituationView {
    pub trigger: String,
    pub feeling: Option<String>,
    pub avoidance: Option<String>,
    pub consequence: Option<String>,
    pub recorded_at: String,
}

#[derive(Serialize)]
pub struct RepeatedView {
    pub kind: String,
    pub text: String,
    pub count: usize,
}

/// Everything recorded so far: the situations themselves and what repeats
/// across them. The person's own material, shown back to them.
#[tauri::command]
pub async fn history(state: TauriState<'_, AppState>) -> CmdResult<HistoryView> {
    let guard = state.practice.lock().unwrap();
    let practice = guard.as_ref().ok_or("no practice is open")?;

    Ok(HistoryView {
        situations: practice
            .recorded_situations()
            .map_err(err)?
            .into_iter()
            .map(|s| StoredSituationView {
                trigger: s.trigger,
                feeling: s.feeling,
                avoidance: s.avoidance,
                consequence: s.consequence,
                recorded_at: s.recorded_at,
            })
            .collect(),
        repeated: practice
            .repeated_elements()
            .map_err(err)?
            .into_iter()
            .map(|r| RepeatedView { kind: r.kind, text: r.text, count: r.count })
            .collect(),
        problems: practice
            .problems_with_ids()
            .map_err(err)?
            .into_iter()
            .map(|(id, text)| ProblemView { id, text })
            .collect(),
    })
}

/// Stores the goal, then advances the step.
#[tauri::command]
pub async fn record_goal(state: TauriState<'_, AppState>, text: String) -> CmdResult<View> {
    {
        let mut guard = state.practice.lock().unwrap();
        let practice = guard.as_mut().ok_or("no practice is open")?;
        practice.record_goal(&text).map_err(err)?;
        practice.apply(Event::GoalSet).map_err(err)?;
    }
    generate_reply(&state).await?;
    view(&state)
}

/// Stores one rating per difficulty, then advances the step.
#[tauri::command]
pub async fn record_ratings(
    state: TauriState<'_, AppState>,
    ratings: Vec<(i64, i64)>,
) -> CmdResult<View> {
    {
        let mut guard = state.practice.lock().unwrap();
        let practice = guard.as_mut().ok_or("no practice is open")?;
        practice.record_ratings(&ratings).map_err(err)?;
        practice.apply(Event::BaselineRecorded).map_err(err)?;
    }
    generate_reply(&state).await?;
    view(&state)
}

/// Applies a protocol event the UI raised (a button, not free text).
#[tauri::command]
pub async fn advance(
    state: TauriState<'_, AppState>,
    event: String,
    payload: Option<serde_json::Value>,
) -> CmdResult<View> {
    let event = parse_event(&event, payload)?;

    let allows_generation = {
        let mut guard = state.practice.lock().unwrap();
        let practice = guard.as_mut().ok_or("no practice is open")?;

        // Steps that carry data persist it before the state moves on;
        // otherwise the practice advances and the tables stay empty.
        match &event {
            Event::SituationConfirmed => {
                if let State::Pattern { situation } = practice.state() {
                    let situation = situation.clone();
                    practice.record_situation(&situation).map_err(err)?;
                }
            }
            Event::PlanUpdated { draft } if draft.is_complete() => {
                practice.record_plan(draft).map_err(err)?;
            }
            _ => {}
        }

        let next = practice.apply(event).map_err(err)?;
        next.allows_generation() && !next.is_terminal()
    };

    if allows_generation {
        generate_reply(&state).await?;
    }
    view(&state)
}

async fn generate_reply(state: &TauriState<'_, AppState>) -> CmdResult<()> {
    // The lock is released before the model call: generation is slow, and the
    // UI must stay responsive.
    let (protocol_state, history) = {
        let guard = state.practice.lock().unwrap();
        let practice = guard.as_ref().ok_or("no practice is open")?;
        let history = practice
            .transcript()
            .map_err(err)?
            .into_iter()
            .map(|(role, content)| Message { role, content })
            .collect::<Vec<_>>();
        (practice.state().clone(), history)
    };

    if !protocol_state.allows_generation() {
        return Ok(());
    }

    // If the model is unreachable the practice still runs on the step's fixed
    // question. A person mid-practice should not hit a dead end because a
    // background service stopped.
    let reply = match state.model.respond_reviewed(&protocol_state, &history).await {
        Ok(reply) => reply,
        Err(_) => crate::critic::fallback(&protocol_state).to_string(),
    };

    let mut guard = state.practice.lock().unwrap();
    let practice = guard.as_mut().ok_or("no practice is open")?;
    practice.record_assistant_message(&reply).map_err(err)?;
    Ok(())
}

fn view(state: &TauriState<'_, AppState>) -> CmdResult<View> {
    let guard = state.practice.lock().unwrap();
    let practice = guard.as_ref().ok_or("no practice is open")?;
    let protocol_state = practice.state();

    let transcript = practice
        .transcript()
        .map_err(err)?
        .into_iter()
        .map(|(role, content)| Line { role, content })
        .collect();

    let crisis = match protocol_state {
        State::Crisis { .. } => Some(CrisisView {
            resources: safety::resources_for("ru-RU")
                .into_iter()
                .map(|r| ResourceView {
                    name: r.name.to_string(),
                    contact: r.contact.to_string(),
                })
                .collect(),
        }),
        _ => None,
    };

    let missing = match protocol_state {
        State::ConcretePlan { draft } => {
            draft.missing().into_iter().map(String::from).collect()
        }
        _ => Vec::new(),
    };

    Ok(View {
        state: protocol_state.name().to_string(),
        hint: protocol_state.hint().to_string(),
        transcript,
        crisis,
        finished: protocol_state.is_terminal(),
        missing,
        problems: practice
            .problems_with_ids()
            .unwrap_or_default()
            .into_iter()
            .map(|(id, text)| ProblemView { id, text })
            .collect(),
        situation: match protocol_state {
            State::Pattern { situation } => Some(SituationView {
                trigger: situation.trigger.clone(),
                feeling: situation.feeling.clone(),
                avoidance: situation.avoidance.clone(),
                consequence: situation.consequence.clone(),
                ready: !situation.trigger.trim().is_empty(),
            }),
            _ => None,
        },
    })
}

fn parse_event(name: &str, payload: Option<serde_json::Value>) -> CmdResult<Event> {
    let event = match name {
        "disclosure_accepted" => Event::DisclosureAccepted { adult: true },
        "disclosure_declined" => Event::DisclosureAccepted { adult: false },
        "problem_added" => Event::ProblemAdded,
        "problems_finished" => Event::ProblemsFinished,
        "goal_set" => Event::GoalSet,
        "baseline_recorded" => Event::BaselineRecorded,
        "psychoeducation_seen" => Event::PsychoeducationSeen,
        "opening_acknowledged" => Event::OpeningAcknowledged,
        "reviewed_done" => Event::ActionReviewed { outcome: ActionOutcome::Done },
        "reviewed_partial" => Event::ActionReviewed { outcome: ActionOutcome::Partial },
        "reviewed_not_done" => Event::ActionReviewed { outcome: ActionOutcome::NotDone },
        "focus_situation" => Event::FocusChosen { focus: Focus::Situation },
        "focus_planning" => Event::FocusChosen { focus: Focus::Planning },
        "focus_revise" => Event::FocusChosen { focus: Focus::ReviseMeasures },
        "situation_confirmed" => Event::SituationConfirmed,
        "situation_drafted" => {
            let situation = serde_json::from_value(payload.ok_or("situation payload required")?)
                .map_err(err)?;
            Event::SituationDrafted { situation }
        }
        "action_proposed" => {
            let check: ActionCheck = payload
                .map(serde_json::from_value)
                .transpose()
                .map_err(err)?
                .unwrap_or(ActionCheck {
                    observable: true,
                    self_dependent: true,
                    bounded: true,
                });
            Event::ActionProposed { check }
        }
        "plan_updated" => {
            let draft: PlanDraft =
                serde_json::from_value(payload.ok_or("plan payload required")?).map_err(err)?;
            Event::PlanUpdated { draft }
        }
        "practice_closed" => Event::PracticeClosed,
        "crisis_acknowledged" => Event::CrisisAcknowledged,
        "user_left" => Event::UserLeft,
        other => return Err(format!("unknown event: {other}")),
    };
    Ok(event)
}
