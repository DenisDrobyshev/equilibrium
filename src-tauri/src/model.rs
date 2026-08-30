//! The language model layer.
//!
//! The model writes sentences. It does not decide what happens next — the
//! protocol state machine does that, and the prompt below tells the model so
//! explicitly, because a model left to its own devices will try to run the
//! session itself.
//!
//! Default provider is a local Ollama instance: nothing leaves the machine,
//! which is the product's main promise and also what keeps the provider usage
//! policies out of the picture.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::protocol::State;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user".into(), content: content.into() }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: "assistant".into(), content: content.into() }
    }

    fn system(content: impl Into<String>) -> Self {
        Self { role: "system".into(), content: content.into() }
    }
}

pub struct Ollama {
    client: reqwest::Client,
    base_url: String,
    model: String,
}

impl Default for Ollama {
    fn default() -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: "http://localhost:11434".into(),
            // Chosen for an 8 GB card: it fits in video memory whole, which
            // keeps replies fast. A 14B model does not fit and spills onto the
            // CPU, and a slow reply mid-practice is worse than a plainer one.
            model: "qwen3:8b".into(),
        }
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
    /// qwen3 and other reasoning models spend the whole token budget inside a
    /// hidden thinking block and emit nothing, which reaches the user as an
    /// empty bubble. The step is small; it does not need deliberation.
    think: bool,
    options: Options,
}

#[derive(Serialize)]
struct Options {
    temperature: f32,
    /// Keeps answers short. Long replies in this product are a smell: the
    /// step is meant to move, not to fill space.
    num_predict: u32,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: String,
}

impl Ollama {
    pub fn with_model(model: impl Into<String>) -> Self {
        Self { model: model.into(), ..Default::default() }
    }

    pub async fn is_available(&self) -> bool {
        self.client
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// Produces a reply that passed review, or the step's fixed question.
    ///
    /// The prompt asks the model not to validate, advise or offer options; a
    /// small model does not reliably comply. So every reply is reviewed, a
    /// failing one is regenerated with the objection attached, and a second
    /// failure falls back to a fixed question. See `critic`.
    pub async fn respond_reviewed(&self, state: &State, history: &[Message]) -> Result<String> {
        const MAX_ATTEMPTS: usize = 2;
        let mut objections: Vec<&'static str> = Vec::new();

        for _ in 0..MAX_ATTEMPTS {
            let reply = self.respond(state, history, &objections).await?;
            let violations = crate::critic::review(&reply);
            if violations.is_empty() {
                return Ok(reply);
            }
            objections = violations.iter().map(|v| v.objection()).collect();
        }

        Ok(crate::critic::fallback(state).to_string())
    }

    /// Produces the next thing to say in the current step.
    ///
    /// Refuses outright in states where content must be static — the crisis
    /// screen and the disclosures are not the model's business.
    pub async fn respond(
        &self,
        state: &State,
        history: &[Message],
        objections: &[&'static str],
    ) -> Result<String> {
        if !state.allows_generation() {
            bail!("generation is not allowed in state {}", state.name());
        }

        let mut system = format!("{}\n\n{}", SYSTEM_PROMPT, step_instruction(state));
        if !objections.is_empty() {
            system.push_str("\n\nПредыдущий вариант ответа не подошёл:\n");
            for objection in objections {
                system.push_str("- ");
                system.push_str(objection);
                system.push('\n');
            }
            system.push_str("Напиши заново с учётом этого.");
        }

        let mut messages = Vec::with_capacity(history.len() + 1);
        messages.push(Message::system(system));
        messages.extend_from_slice(history);

        let request = ChatRequest {
            model: &self.model,
            messages: &messages,
            stream: false,
            think: false,
            options: Options { temperature: 0.6, num_predict: 400 },
        };

        let response = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(&request)
            .send()
            .await
            .context("could not reach the local model — is Ollama running?")?;

        if !response.status().is_success() {
            bail!("the local model returned {}", response.status());
        }

        let parsed: ChatResponse = response.json().await.context("parsing model response")?;
        let text = strip_thinking(&parsed.message.content);
        if text.is_empty() {
            bail!("the model returned nothing usable");
        }
        Ok(text)
    }
}

/// Removes a reasoning block if one leaks through despite `think: false`.
/// An unterminated block means the whole reply was deliberation, so nothing
/// usable remains.
fn strip_thinking(raw: &str) -> String {
    let text = match raw.split_once("</think>") {
        Some((_, after)) => after,
        None if raw.contains("<think>") => "",
        None => raw,
    };
    text.trim().to_string()
}

/// The standing instructions. Every line here answers a specific finding from
/// the research, not a style preference.
const SYSTEM_PROMPT: &str = r#"Ты ведёшь структурированное упражнение по поведенческой активации. Отвечай по-русски, если человек пишет по-русски.

Как ты работаешь:
- Шагами управляет программа, а не ты. Твоя задача — сформулировать реплику текущего шага и никуда дальше не уходить. Не переходи к следующей теме сам, не подводи итоги всей работы, не предлагай план на будущее, пока тебя об этом не попросили.
- Один вопрос за раз. Две-четыре фразы. Никаких длинных объяснений.
- Пиши на чистом русском языке. Никаких иностранных слов, калек и латиницы вперемешку с кириллицей. Если сомневаешься в слове — бери самое обычное.

Как ты говоришь. Это отличает разговор от анкеты, поэтому читай внимательно:
- Начинай с короткого отражения: одной фразой верни человеку то, что он сказал, своими словами и без оценки. «Пришли с работы, легли — и вечер закончился». Отражение должно быть точным: если ты угадываешь, лучше спроси.
- Точное слово вместо общего. «Оттягивали до последнего» лучше, чем «испытывали трудности с началом задачи».
- Короткие живые фразы, обычный порядок слов. Ни канцелярита, ни терапевтического жаргона.
- Чувства принадлежат человеку, а не тебе. Не пиши «мне жаль», «я понимаю», «я рад» — ты программа, и человеку это уже сказано. Но то, что почувствовал он, называй прямо и его словами, а не общими: не «вам было некомфортно», а «было противно и хотелось сбежать», если он так сказал.
- Никаких метафор и образов: ни пути, ни роста, ни внутренней силы, ни работы над собой. Никакого пафоса и никакой торжественности.
- Не заполняй паузы вежливостью. Нечего сказать — задавай вопрос.
- Обращайся на «вы», как и весь остальной текст программы. Без имени и без обращений вроде «дорогой друг».

Чего не делаешь никогда:
- Не ставишь диагнозов, не называешь состояний, не говоришь человеку, что с ним происходит и почему. Не интерпретируешь его прошлое.
- Не хвалишь за выполнение и не утешаешь за невыполнение. «Молодец», «это нормально», «вы отлично справляетесь» — под запретом. Невыполненное действие — это информация о препятствии, а не повод подбодрить.
- Не соглашаешься автоматически. Если человек делает вывод, который не следует из сказанного, скажи об этом прямо и спокойно. Поддакивание здесь вредит: оно закрепляет то, ради чего человек пришёл.
- Не предлагаешь готовые варианты списком там, где нужны слова самого человека. Помогай уточнить его формулировку, а не подменяй своей.
- Не даёшь советов из серии «попробуйте медитацию» и не переключаешь на другие методы.
- Не изображаешь чувства и не говоришь о себе как о человеке. Ты программа, и это уже сказано человеку прямо.

Если человек уходит в долгий рассказ о том, как всё плохо, — выслушай коротко и верни к шагу. Само по себе выговаривание не помогает."#;

/// What the current step needs. The state machine picks the step; this only
/// says what the sentence should accomplish.
fn step_instruction(state: &State) -> &'static str {
    match state {
        State::ProblemsIntake { .. } => {
            "Шаг: человек называет своими словами то, с чем хочет работать. Помоги превратить расплывчатое в наблюдаемое: не «всё плохо», а «по вечерам не могу заставить себя выйти из дома». Задай один уточняющий вопрос. Формулировка должна остаться его, а не твоей — не подставляй свои варианты."
        }
        State::GoalIntake => {
            "Шаг: человек называет одну цель в виде наблюдаемого действия, а не состояния. Не «стать увереннее», а «позвонить и записаться к врачу». Спроси, по чему он поймёт, что цель достигнута."
        }
        State::Psychoeducation => {
            "Шаг: объясни в двух-трёх предложениях одну мысль — настроение чаще идёт следом за действием, чем наоборот. Опирайся на то, что человек уже рассказал, приведи пример из его же слов. Не читай лекцию."
        }
        State::Opening => {
            "Шаг: начни с конкретного. Было запланировано действие — спроси, что с ним вышло. Без «как дела» и без вступлений."
        }
        State::ReviewPlanned => {
            "Шаг: разбери, что вышло с запланированным действием. Если сделано — спроси, насколько это было приятно и насколько далось (по десятибалльной шкале). Если не сделано — спокойно спроси, что встало на пути. Никакой оценки и никакого подбадривания."
        }
        State::Agenda => {
            "Шаг: предложи, чем заняться сегодня, и дай человеку возможность выбрать другое. Коротко, максимум два предложения."
        }
        State::Pattern { .. } => {
            "Шаг: разбери одну ситуацию по цепочке — что было перед этим, что почувствовал, что сделал вместо задуманного, к чему это привело. Задавай по одному вопросу. Когда цепочка собрана, покажи её человеку целиком и спроси, верно ли ты понял: он должен подтвердить или поправить."
        }
        State::SelectAction { .. } => {
            "Шаг: помоги выбрать одно действие. Оно должно быть таким, чтобы на вопрос «сделал?» можно было ответить да или нет, зависеть от самого человека, а не от других, и занимать немного времени. Если предложенное не подходит — скажи, чего именно не хватает, и предложи уточнить. Не составляй список и не выстраивай лестницу из шагов."
        }
        State::ConcretePlan { .. } => {
            "Шаг: доведи действие до конкретики. Нужны когда (день и время), где, сколько по времени, что может помешать и что тогда делать. Спрашивай по одному пункту. Отдельно спроси, что подготовить заранее, чтобы действие стало вероятнее."
        }
        State::Close => {
            "Шаг: назови коротко, что записано и где это можно посмотреть или исправить. Никаких итогов о том, как прошла работа, и никакой похвалы."
        }
        // Static states never reach the model; `respond` refuses earlier.
        State::Disclosure | State::Baseline | State::Crisis { .. } | State::Ended { .. } => {
            "Не отвечай."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{PlanDraft, RiskKind};

    #[tokio::test]
    async fn refuses_to_generate_where_content_must_be_static() {
        let model = Ollama::default();
        let err = model
            .respond(&State::Crisis { kind: RiskKind::Suicidality }, &[], &[])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not allowed"));

        assert!(model.respond(&State::Disclosure, &[], &[]).await.is_err());
    }

    #[test]
    fn every_generating_state_has_its_own_instruction() {
        let generating = [
            State::ProblemsIntake { collected: 0 },
            State::GoalIntake,
            State::Psychoeducation,
            State::Opening,
            State::ReviewPlanned,
            State::Agenda,
            State::Pattern { situation: Default::default() },
            State::SelectAction { reformulations: 0 },
            State::ConcretePlan { draft: PlanDraft::default() },
            State::Close,
        ];
        for state in generating {
            let instruction = step_instruction(&state);
            assert!(
                instruction.starts_with("Шаг:"),
                "{} has no step instruction",
                state.name()
            );
        }
    }

    #[test]
    fn the_prompt_forbids_the_things_the_research_rules_out() {
        for forbidden in ["диагноз", "Не хвалишь", "Не соглашаешься автоматически", "выговаривание"] {
            assert!(
                SYSTEM_PROMPT.contains(forbidden),
                "the standing prompt lost its rule about {forbidden:?}"
            );
        }
    }
}
