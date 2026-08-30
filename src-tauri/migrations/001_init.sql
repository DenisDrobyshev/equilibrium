-- Equilibrium schema v1
--
-- Design rules that this schema enforces, from SPEC.md:
--   * The user's own wording is the unit of measurement. No normative scales.
--   * Anything the model inferred is stored unconfirmed until the user accepts it.
--   * safety_events never store message content.
--   * Everything is local and deletable. No server-side identity, no user table.

PRAGMA foreign_keys = ON;

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- ---------------------------------------------------------------------------
-- Idiographic measures: the user's own problems and goal, in their own words.
-- These replace PHQ-9 / GAD-7 and are measured with single items.
-- ---------------------------------------------------------------------------

CREATE TABLE problems (
    id          INTEGER PRIMARY KEY,
    formulation TEXT    NOT NULL,          -- verbatim from the user
    created_at  TEXT    NOT NULL,
    retired_at  TEXT,                      -- kept, never deleted on edit
    sort_order  INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE problem_ratings (
    id         INTEGER PRIMARY KEY,
    problem_id INTEGER NOT NULL REFERENCES problems(id) ON DELETE CASCADE,
    value      INTEGER NOT NULL CHECK (value BETWEEN 0 AND 10),
    rated_at   TEXT    NOT NULL,
    is_retest  INTEGER NOT NULL DEFAULT 0  -- duplicate probe for measuring noise
);

CREATE INDEX idx_problem_ratings_problem ON problem_ratings(problem_id, rated_at);

CREATE TABLE goals (
    id          INTEGER PRIMARY KEY,
    formulation TEXT    NOT NULL,
    created_at  TEXT    NOT NULL,
    retired_at  TEXT
);

-- Goal Attainment Scaling: five levels, -2 (much worse) .. +2 (much better).
CREATE TABLE goal_levels (
    goal_id     INTEGER NOT NULL REFERENCES goals(id) ON DELETE CASCADE,
    level       INTEGER NOT NULL CHECK (level BETWEEN -2 AND 2),
    description TEXT    NOT NULL,
    PRIMARY KEY (goal_id, level)
);

CREATE TABLE goal_ratings (
    id       INTEGER PRIMARY KEY,
    goal_id  INTEGER NOT NULL REFERENCES goals(id) ON DELETE CASCADE,
    level    INTEGER NOT NULL CHECK (level BETWEEN -2 AND 2),
    rated_at TEXT    NOT NULL
);

-- ---------------------------------------------------------------------------
-- Practice sessions and transcripts
-- ---------------------------------------------------------------------------

CREATE TABLE practice_sessions (
    id         INTEGER PRIMARY KEY,
    branch     TEXT    NOT NULL CHECK (branch IN ('onboarding', 'regular')),
    started_at TEXT    NOT NULL,
    ended_at   TEXT,
    end_reason TEXT CHECK (end_reason IN ('completed', 'abandoned', 'time_limit', 'crisis')),
    -- Serialized protocol state, so an interrupted practice resumes where it
    -- stopped instead of restarting and asking everything again.
    current_state TEXT NOT NULL,
    states     TEXT                        -- JSON array of visited state names
);

CREATE INDEX idx_practice_sessions_open ON practice_sessions(ended_at, started_at);

CREATE TABLE messages (
    id         INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES practice_sessions(id) ON DELETE CASCADE,
    role       TEXT    NOT NULL CHECK (role IN ('user', 'assistant')),
    state      TEXT    NOT NULL,           -- protocol state this was said in
    content    TEXT    NOT NULL,
    said_at    TEXT    NOT NULL
);

CREATE INDEX idx_messages_session ON messages(session_id, said_at);

-- ---------------------------------------------------------------------------
-- Behavioural activation working data
-- ---------------------------------------------------------------------------

-- Trigger -> feeling -> avoidance -> consequence. Extracted by the model,
-- then shown to the user; only stored as confirmed once they accept it.
CREATE TABLE situations (
    id           INTEGER PRIMARY KEY,
    session_id   INTEGER REFERENCES practice_sessions(id) ON DELETE SET NULL,
    trigger      TEXT NOT NULL,
    feeling      TEXT,
    avoidance    TEXT,
    consequence  TEXT,
    occurred_at  TEXT,
    recorded_at  TEXT NOT NULL,
    confirmed    INTEGER NOT NULL DEFAULT 0,
    edited_by_user INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE planned_actions (
    id           INTEGER PRIMARY KEY,
    session_id   INTEGER REFERENCES practice_sessions(id) ON DELETE SET NULL,
    goal_id      INTEGER REFERENCES goals(id) ON DELETE SET NULL,
    description  TEXT NOT NULL,
    scheduled_at TEXT NOT NULL,            -- when: required before leaving B6
    place        TEXT NOT NULL,            -- where: required
    duration_min INTEGER,
    obstacle     TEXT,                     -- what could get in the way
    plan_b       TEXT,                     -- and what then
    stimulus_prep TEXT,                    -- stimulus control: prepare/remove
    status       TEXT NOT NULL DEFAULT 'planned'
                 CHECK (status IN ('planned', 'done', 'partial', 'skipped')),
    created_at   TEXT NOT NULL
);

CREATE INDEX idx_planned_actions_status ON planned_actions(status, scheduled_at);

CREATE TABLE activity_log (
    id         INTEGER PRIMARY KEY,
    action_id  INTEGER REFERENCES planned_actions(id) ON DELETE CASCADE,
    done       INTEGER NOT NULL CHECK (done IN (0, 1, 2)),  -- 0 no, 1 yes, 2 partial
    pleasure   INTEGER CHECK (pleasure BETWEEN 0 AND 10),
    mastery    INTEGER CHECK (mastery BETWEEN 0 AND 10),
    obstacle_note TEXT,                    -- when not done: information, not failure
    logged_at  TEXT NOT NULL
);

-- Sleep and activity are context and planning targets only.
-- Never an assessment of the user's state.
CREATE TABLE context_log (
    id             INTEGER PRIMARY KEY,
    sleep_hours    REAL,
    activity_level INTEGER CHECK (activity_level BETWEEN 0 AND 10),
    logged_at      TEXT NOT NULL
);

-- ---------------------------------------------------------------------------
-- Situation map. Nodes and edges are hypotheses for the user to inspect,
-- not the app's decision mechanism. Never render an absent edge as "no link":
-- sparsity reflects insufficient data, not absence of a relation.
-- ---------------------------------------------------------------------------

CREATE TABLE map_nodes (
    id         INTEGER PRIMARY KEY,
    label      TEXT NOT NULL,
    kind       TEXT NOT NULL CHECK (kind IN ('trigger', 'response', 'avoidance', 'outcome', 'custom')),
    created_at TEXT NOT NULL,
    confirmed  INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE map_edges (
    id          INTEGER PRIMARY KEY,
    from_node   INTEGER NOT NULL REFERENCES map_nodes(id) ON DELETE CASCADE,
    to_node     INTEGER NOT NULL REFERENCES map_nodes(id) ON DELETE CASCADE,
    observations INTEGER NOT NULL DEFAULT 1,  -- how many times seen together
    created_at  TEXT NOT NULL,
    confirmed   INTEGER NOT NULL DEFAULT 0,
    UNIQUE (from_node, to_node)
);

-- ---------------------------------------------------------------------------
-- Safety. Content is deliberately absent: enough to show the guard fired,
-- not enough to become a store of sensitive material.
-- ---------------------------------------------------------------------------

CREATE TABLE safety_events (
    id           INTEGER PRIMARY KEY,
    fired_at     TEXT NOT NULL,
    trigger_kind TEXT NOT NULL,            -- classifier label, not the message
    action_taken TEXT NOT NULL,
    guard_version TEXT NOT NULL            -- which safety logic version ran
);

CREATE TABLE safety_plans (
    id         INTEGER PRIMARY KEY,
    content    TEXT NOT NULL,              -- Stanley & Brown template, user-authored
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- ---------------------------------------------------------------------------
-- Disclosure timing required by law: first run, return after >7 days,
-- and every 3 hours of continuous use.
-- ---------------------------------------------------------------------------

CREATE TABLE disclosures (
    id        INTEGER PRIMARY KEY,
    kind      TEXT NOT NULL CHECK (kind IN ('first_run', 'return_gap', 'periodic')),
    shown_at  TEXT NOT NULL
);

INSERT INTO meta (key, value) VALUES ('schema_version', '1');
