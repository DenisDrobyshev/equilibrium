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
use crate::protocol::{ActionCheck, ActionOutcome, Event, Focus, PlanDraft, State};
use crate::safety::{self, RuleBasedDetector};
use crate::session::{Branch, Practice, Turn};

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
    pub transcript: Vec<Line>,
    pub crisis: Option<CrisisView>,
    pub finished: bool,
    /// What the step still needs before the practice can move on.
    pub missing: Vec<String>,
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
        Turn::Continue { .. } => {
            generate_reply(&state).await?;
            view(&state)
        }
    }
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
        transcript,
        crisis,
        finished: protocol_state.is_terminal(),
        missing,
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
