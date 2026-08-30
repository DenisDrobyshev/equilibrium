import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

type Line = { role: string; content: string };
type Resource = { name: string; contact: string };
type View = {
  state: string;
  hint: string;
  transcript: Line[];
  crisis: { resources: Resource[] } | null;
  finished: boolean;
  missing: string[];
  problems: string[];
};
type VaultStatus = {
  resumed: boolean;
  needs_onboarding: boolean;
  model_available: boolean;
};

/// Step labels. The person should always be able to see where they are —
/// an opaque chat that decides things invisibly is exactly what this is not.
const STEP_LABEL: Record<string, string> = {
  Disclosure: "Прежде чем начать",
  ProblemsIntake: "С чем работаем",
  GoalIntake: "Цель",
  Baseline: "Первая отметка",
  Psychoeducation: "Как это работает",
  Opening: "Что вышло с задуманным",
  ReviewPlanned: "Разбор",
  Agenda: "Чем займёмся",
  Pattern: "Разбор ситуации",
  SelectAction: "Выбор действия",
  ConcretePlan: "Конкретика",
  Close: "Записано",
  Crisis: "Пауза",
  Ended: "Практика завершена",
};

export default function App() {
  const [passphrase, setPassphrase] = useState("");
  const [status, setStatus] = useState<VaultStatus | null>(null);
  const [view, setView] = useState<View | null>(null);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const bottom = useRef<HTMLDivElement>(null);

  useEffect(() => {
    bottom.current?.scrollIntoView({ behavior: "smooth" });
  }, [view?.transcript.length, busy]);

  async function run<T>(fn: () => Promise<T>): Promise<T | null> {
    setBusy(true);
    setError(null);
    try {
      return await fn();
    } catch (e) {
      setError(String(e));
      return null;
    } finally {
      setBusy(false);
    }
  }

  async function unlock() {
    const result = await run(() =>
      invoke<VaultStatus>("open_vault", { passphrase }),
    );
    if (result) setStatus(result);
  }

  async function begin(onboarding: boolean) {
    const result = await run(() =>
      invoke<View>("begin_practice", { passphrase, onboarding }),
    );
    if (result) setView(result);
  }

  async function send() {
    const text = input.trim();
    if (!text) return;
    setInput("");
    const result = await run(() => invoke<View>("send_message", { text }));
    if (result) setView(result);
  }

  async function recordProblem() {
    const text = input.trim();
    if (!text) return;
    setInput("");
    const result = await run(() => invoke<View>("record_problem", { text }));
    if (result) setView(result);
  }

  async function advance(event: string, payload?: unknown) {
    const result = await run(() =>
      invoke<View>("advance", { event, payload: payload ?? null }),
    );
    if (result) setView(result);
  }

  // --- Vault ---
  if (!status) {
    return (
      <main className="gate">
        <h1>Equilibrium</h1>
        <p className="lede">
          Направляемые упражнения для самостоятельной работы. Всё, что вы
          напишете, остаётся на этом компьютере в зашифрованном виде.
        </p>
        <p className="warn">
          Пароль нигде не хранится. Если его забыть, записи не восстановить —
          это сделано намеренно.
        </p>
        <input
          type="password"
          value={passphrase}
          placeholder="Пароль"
          onChange={(e) => setPassphrase(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && unlock()}
          autoFocus
        />
        <button onClick={unlock} disabled={busy || passphrase.length < 4}>
          {busy ? "Открываю…" : "Открыть"}
        </button>
        {error && <p className="error">{error}</p>}
      </main>
    );
  }

  // --- Between sessions ---
  if (!view) {
    return (
      <main className="gate">
        <h1>Equilibrium</h1>
        {!status.model_available && (
          <p className="warn">
            Локальная модель не отвечает. Запустите Ollama, иначе диалога не
            будет.
          </p>
        )}
        {status.needs_onboarding ? (
          <>
            <p className="lede">
              Начнём с того, что вы своими словами назовёте то, с чем хотите
              работать. Это займёт несколько минут.
            </p>
            <button onClick={() => begin(true)} disabled={busy}>
              Начать
            </button>
          </>
        ) : (
          <>
            <p className="lede">Готовы к практике.</p>
            <button onClick={() => begin(false)} disabled={busy}>
              Начать практику
            </button>
          </>
        )}
        {error && <p className="error">{error}</p>}
      </main>
    );
  }

  // --- Crisis: static content, no model, no continuation ---
  if (view.crisis) {
    return (
      <main className="crisis">
        <h2>Остановимся здесь</h2>
        <p>
          Это программа, и она не может помочь с тем, что вы сейчас описали.
          С этим нужен живой человек.
        </p>
        <ul>
          {view.crisis.resources.map((r) => (
            <li key={r.contact}>
              <strong>{r.name}</strong>
              <span>{r.contact}</span>
            </li>
          ))}
        </ul>
        <p className="note">
          Если есть непосредственная опасность — вызовите экстренные службы.
        </p>
        <button onClick={() => advance("crisis_acknowledged")} disabled={busy}>
          Я прочитал
        </button>
      </main>
    );
  }

  // --- Disclosure ---
  if (view.state === "Disclosure") {
    return (
      <main className="gate">
        <h2>Прежде чем начать</h2>
        <ul className="disclosure">
          <li>
            Вы разговариваете с программой, а не с человеком. Это не терапия и
            не замена помощи специалиста.
          </li>
          <li>
            Программа не предназначена для состояний, требующих помощи: острых
            мыслей о самоповреждении, мании, психоза.
          </li>
          <li>Только для совершеннолетних.</li>
          <li>
            Записи хранятся на этом компьютере в зашифрованном виде и удаляются
            одной кнопкой.
          </li>
        </ul>
        <div className="row">
          <button onClick={() => advance("disclosure_accepted")} disabled={busy}>
            Мне есть 18, продолжить
          </button>
          <button
            className="ghost"
            onClick={() => advance("disclosure_declined")}
            disabled={busy}
          >
            Выйти
          </button>
        </div>
        {error && <p className="error">{error}</p>}
      </main>
    );
  }

  if (view.finished) {
    return (
      <main className="gate">
        <h2>Практика завершена</h2>
        <p className="lede">Записи сохранены.</p>
        <button
          onClick={() => {
            setView(null);
          }}
          disabled={busy}
        >
          Хорошо
        </button>
      </main>
    );
  }

  return (
    <main className="practice">
      <header>
        <span className="step">{STEP_LABEL[view.state] ?? view.state}</span>
        <button className="ghost small" onClick={() => advance("user_left")}>
          Закончить
        </button>
      </header>

      {view.hint && <p className="hint">{view.hint}</p>}

      {view.problems.length > 0 && (
        <ol className="recorded">
          {view.problems.map((problem, i) => (
            <li key={i}>{problem}</li>
          ))}
        </ol>
      )}

      <div className="transcript">
        {view.transcript
          .filter((line) => line.content.trim().length > 0)
          .map((line, i) => (
            <div key={i} className={`line ${line.role}`}>
              {line.content}
            </div>
          ))}
        {busy && <div className="line assistant pending">Думает…</div>}
        <div ref={bottom} />
      </div>

      <StepControls
        state={view.state}
        missing={view.missing}
        problems={view.problems}
        busy={busy}
        hasInput={input.trim().length > 0}
        advance={advance}
        recordProblem={recordProblem}
      />

      <div className="composer">
        <textarea
          value={input}
          placeholder="Напишите здесь"
          rows={3}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) send();
          }}
          disabled={busy}
        />
        <button onClick={send} disabled={busy || !input.trim()}>
          Отправить
        </button>
      </div>
      {error && <p className="error">{error}</p>}
    </main>
  );
}

function StepControls({
  state,
  missing,
  problems,
  busy,
  hasInput,
  advance,
  recordProblem,
}: {
  state: string;
  missing: string[];
  problems: string[];
  busy: boolean;
  hasInput: boolean;
  advance: (event: string, payload?: unknown) => void;
  recordProblem: () => void;
}) {
  const [plan, setPlan] = useState({
    description: "",
    scheduled_at: "",
    place: "",
  });

  const button = (label: string, event: string, payload?: unknown) => (
    <button key={event} onClick={() => advance(event, payload)} disabled={busy}>
      {label}
    </button>
  );

  switch (state) {
    case "ProblemsIntake":
      return (
        <div className="controls">
          <button onClick={recordProblem} disabled={busy || !hasInput}>
            Записать как трудность
          </button>
          <button
            onClick={() => advance("problems_finished")}
            disabled={busy || problems.length < 2}
          >
            {problems.length < 2
              ? `Дальше (нужно ещё ${2 - problems.length})`
              : "Дальше"}
          </button>
        </div>
      );
    case "GoalIntake":
      return <div className="controls">{button("Цель записана", "goal_set")}</div>;
    case "Baseline":
      return (
        <div className="controls">
          {button("Отметка сделана", "baseline_recorded")}
        </div>
      );
    case "Psychoeducation":
      return (
        <div className="controls">{button("Понятно", "psychoeducation_seen")}</div>
      );
    case "Opening":
      return (
        <div className="controls">{button("Дальше", "opening_acknowledged")}</div>
      );
    case "ReviewPlanned":
      return (
        <div className="controls">
          {button("Сделал", "reviewed_done")}
          {button("Частично", "reviewed_partial")}
          {button("Не сделал", "reviewed_not_done")}
        </div>
      );
    case "Agenda":
      return (
        <div className="controls">
          {button("Разобрать ситуацию", "focus_situation")}
          {button("Запланировать действие", "focus_planning")}
          {button("Пересмотреть формулировки", "focus_revise")}
        </div>
      );
    case "Pattern":
      return (
        <div className="controls">
          {button("Всё верно, дальше", "situation_confirmed")}
        </div>
      );
    case "SelectAction":
      return (
        <div className="controls">
          {button("Это действие подходит", "action_proposed")}
        </div>
      );
    case "ConcretePlan":
      return (
        <div className="controls plan">
          <input
            placeholder="Что именно"
            value={plan.description}
            onChange={(e) => setPlan({ ...plan, description: e.target.value })}
          />
          <input
            placeholder="Когда"
            value={plan.scheduled_at}
            onChange={(e) => setPlan({ ...plan, scheduled_at: e.target.value })}
          />
          <input
            placeholder="Где"
            value={plan.place}
            onChange={(e) => setPlan({ ...plan, place: e.target.value })}
          />
          <button
            onClick={() =>
              advance("plan_updated", {
                description: plan.description,
                scheduled_at: plan.scheduled_at || null,
                place: plan.place || null,
                duration_min: null,
                obstacle: null,
                plan_b: null,
                stimulus_prep: null,
              })
            }
            disabled={busy}
          >
            Записать план
          </button>
          {missing.length > 0 && (
            <span className="missing">Не хватает: {missing.join(", ")}</span>
          )}
        </div>
      );
    case "Close":
      return (
        <div className="controls">{button("Завершить", "practice_closed")}</div>
      );
    default:
      return null;
  }
}
