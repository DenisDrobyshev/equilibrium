//! Post-generation review.
//!
//! The standing prompt tells the model not to validate, not to advise and not
//! to offer ready-made options. A 7B model does not reliably obey negative
//! instructions — both of these came out of qwen2.5:7b with the full prompt in
//! place:
//!
//! > «Понятно, что вы чувствуете себя так. Что может помешать вам принять
//! > себя таким, какой вы есть?»
//!
//! > «Попробуйте выбрать одно простое действие… Например, выпейте стакан воды
//! > или сделайте несколько глубоких дыхательных вдохов.»
//!
//! The first agrees with an avoidance conclusion and helps entrench it. The
//! second hands out advice the protocol has no room for. Both are in the tests
//! below, verbatim, so the guard cannot regress.
//!
//! So the prompt is a request, and this module is the enforcement. A reply
//! that fails review is regenerated with the objection attached; if it fails
//! again, the step falls back to a fixed question. A plain fixed question is a
//! better product than a fluent violation.

use crate::protocol::State;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Violation {
    /// Agreement, reassurance, praise — the thing that entrenches rumination.
    Sycophancy,
    /// Ready-made options where the person's own words are needed.
    OffersOptions,
    /// Advice the protocol did not ask for.
    GivesAdvice,
    /// Claims to feel or understand. It is a program and has said so.
    ClaimsFeelings,
    /// Metaphor, uplift, therapeutic grandeur. The register of a poster, not
    /// of an experienced practitioner.
    Pompous,
    /// Officialese. Makes the reply read as a form rather than a conversation.
    Bureaucratic,
    /// Longer than a step warrants.
    TooLong,
    /// More than one question at a time.
    MultipleQuestions,
}

impl Violation {
    /// Phrased as an instruction, because it is fed back to the model.
    pub fn objection(&self) -> &'static str {
        match self {
            Violation::Sycophancy => {
                "Ты согласился или начал утешать. Убери одобрение, сочувствие и поддакивание — они закрепляют то, ради чего человек пришёл. Если человек сделал вывод, который не следует из сказанного, спокойно назови это."
            }
            Violation::OffersOptions => {
                "Ты предложил готовые варианты. Здесь нужны слова самого человека. Задай вопрос, а не список."
            }
            Violation::GivesAdvice => {
                "Ты дал совет. Это не твоя работа: шаг ведёт программа, а человек сам выбирает действие."
            }
            Violation::ClaimsFeelings => {
                "Ты приписал чувства себе. Чувства принадлежат человеку: назови его словами то, что почувствовал он, и убери всё про себя."
            }
            Violation::Pompous => {
                "Ты ушёл в образы и торжественность. Убери метафоры про путь, рост и работу над собой, скажи то же самое обычными словами."
            }
            Violation::Bureaucratic => {
                "Это канцелярит. Скажи как в разговоре: короче и обычными словами."
            }
            Violation::TooLong => "Слишком длинно. Две-четыре фразы.",
            Violation::MultipleQuestions => "Задай ровно один вопрос.",
        }
    }
}

const SYCOPHANCY: &[&str] = &[
    "понятно, что вы",
    "понимаю, что вы",
    "я понимаю вас",
    "это нормально",
    "это совершенно нормально",
    "вы молодец",
    "молодец",
    "отличная работа",
    "хорошо, что вы",
    "вы справляетесь",
    "вы справитесь",
    "не переживайте",
    "не расстраивайтесь",
    "всё будет хорошо",
    "все будет хорошо",
    "вы не одиноки",
    "это смелый шаг",
    "спасибо, что поделились",
];

const ADVICE: &[&str] = &[
    "попробуйте",
    "советую",
    "рекомендую",
    "стоит попробовать",
    "вам поможет",
    "помогает",
    "дыхательн",
    "медитац",
    "глубоких вдох",
    "выпейте",
    "прогуляйтесь",
];

const OPTIONS: &[&str] = &["например", "к примеру", "вот несколько", "варианты:", "можно так"];

const FEELINGS: &[&str] = &["мне жаль", "я чувствую", "мне приятно", "я рад", "сочувствую"];

/// Poster language. An experienced practitioner does not talk like this, and
/// in a program it reads as a stock phrase rather than attention.
const POMPOUS: &[&str] = &[
    "путь к себе",
    "ваш путь",
    "путешествие",
    "личностный рост",
    "работа над собой",
    "внутренняя сила",
    "внутренний ребенок",
    "принять себя",
    "полюбить себя",
    "исцелен",
    "гармони",
    "обрести",
    "свет в конце",
    "маленькие шаги ведут",
];

const BUREAUCRATIC: &[&str] = &[
    "осуществля",
    "в рамках данн",
    "на данный момент времени",
    "в связи с вышеизложенн",
    "следует отметить",
    "необходимо понимать",
    "с целью улучшения",
];

/// Maximum sentences in a reply. The protocol moves in small steps.
const MAX_SENTENCES: usize = 5;
const MAX_CHARS: usize = 700;

pub fn review(text: &str) -> Vec<Violation> {
    let lower = text.to_lowercase().replace('ё', "е");
    let mut found = Vec::new();

    if SYCOPHANCY.iter().any(|p| lower.contains(p)) {
        found.push(Violation::Sycophancy);
    }
    if OPTIONS.iter().any(|p| lower.contains(p)) || has_bullet_list(text) {
        found.push(Violation::OffersOptions);
    }
    if ADVICE.iter().any(|p| lower.contains(p)) {
        found.push(Violation::GivesAdvice);
    }
    if FEELINGS.iter().any(|p| lower.contains(p)) {
        found.push(Violation::ClaimsFeelings);
    }
    if POMPOUS.iter().any(|p| lower.contains(p)) {
        found.push(Violation::Pompous);
    }
    if BUREAUCRATIC.iter().any(|p| lower.contains(p)) {
        found.push(Violation::Bureaucratic);
    }
    if text.chars().count() > MAX_CHARS || count_sentences(text) > MAX_SENTENCES {
        found.push(Violation::TooLong);
    }
    if text.matches('?').count() > 1 {
        found.push(Violation::MultipleQuestions);
    }

    found
}

fn has_bullet_list(text: &str) -> bool {
    text.lines()
        .filter(|line| {
            let t = line.trim_start();
            t.starts_with("- ") || t.starts_with("• ") || t.starts_with("* ")
                || t.starts_with("1. ") || t.starts_with("2. ")
        })
        .count()
        >= 2
}

fn count_sentences(text: &str) -> usize {
    text.split(['.', '!', '?'])
        .filter(|part| !part.trim().is_empty())
        .count()
}

/// What the step asks when the model cannot produce an acceptable reply.
///
/// These are the product's floor. They are plain on purpose — a fixed, honest
/// question beats a fluent answer that breaks the protocol.
pub fn fallback(state: &State) -> &'static str {
    match state {
        State::ProblemsIntake { .. } => {
            "Опишите одну ситуацию, в которой это проявляется. Что именно происходит?"
        }
        State::GoalIntake => {
            "Назовите одно действие, которое вы сможете сделать, когда получится. По чему поймёте, что вышло?"
        }
        State::Psychoeducation => {
            "Обычно настроение идёт следом за действием, а не наоборот: сначала человек что-то делает, и только потом меняется состояние. Поэтому здесь мы работаем с действиями, а не с настроением напрямую."
        }
        State::Opening => "В прошлый раз было запланировано действие. Что с ним вышло?",
        State::ReviewPlanned => "Что встало на пути?",
        State::Agenda => "Чем займёмся сегодня?",
        State::Pattern { .. } => "Что было прямо перед этим?",
        State::SelectAction { .. } => {
            "Какое одно действие вы могли бы сделать до следующего раза?"
        }
        State::ConcretePlan { .. } => "Когда именно вы это сделаете?",
        State::Close => "Записано. Посмотреть и поправить записи можно в любой момент.",
        State::Disclosure | State::Baseline | State::Crisis { .. } | State::Ended { .. } => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::PlanDraft;

    /// Real replies from qwen2.5:7b that broke the protocol while the full
    /// prompt was in place. If these ever pass review, the guard is broken.
    #[test]
    fn catches_the_replies_that_actually_slipped_through() {
        let sycophantic =
            "Понятно, что вы чувствуете себя так. Что может помешать вам принять себя таким, какой вы есть?";
        assert!(
            review(sycophantic).contains(&Violation::Sycophancy),
            "missed agreement with an avoidance conclusion"
        );

        let advisory = "Давайте не будем говорить о том, что у вас депрессия. Важно сосредоточиться на том, что вы можете сделать сейчас. Попробуйте выбрать одно простое действие, которое вы сможете выполнить без особых усилий. Например, выпейте стакан воды или сделайте несколько глубоких дыхательных вдохов. Какое из этих действий вы решите сделать первым?";
        let violations = review(advisory);
        assert!(violations.contains(&Violation::GivesAdvice), "missed advice");
        assert!(
            violations.contains(&Violation::OffersOptions),
            "missed ready-made options"
        );
    }

    #[test]
    fn accepts_a_reply_that_does_its_job() {
        let good = "Что помешало выйти вчера?";
        assert!(review(good).is_empty(), "{:?}", review(good));

        let also_good =
            "Вы говорите, что ничего никогда не выходит, но на прошлой неделе прогулка состоялась. Что было по-другому в тот день?";
        assert!(review(also_good).is_empty(), "{:?}", review(also_good));
    }

    #[test]
    fn catches_bullet_lists() {
        let listy = "Варианты действий:\n- выйти на балкон\n- позвонить другу\n- сходить в магазин";
        assert!(review(listy).contains(&Violation::OffersOptions));
    }

    #[test]
    fn catches_length_and_multiple_questions() {
        let long = "Одно. Два. Три. Четыре. Пять. Шесть.";
        assert!(review(long).contains(&Violation::TooLong));

        let questions = "Что помешало? И как вы себя чувствовали?";
        assert!(review(questions).contains(&Violation::MultipleQuestions));
    }

    #[test]
    fn catches_performed_feelings() {
        assert!(review("Мне жаль, что так вышло.").contains(&Violation::ClaimsFeelings));
    }

    #[test]
    fn catches_poster_language_and_officialese() {
        assert!(review("Это ваш путь к себе.").contains(&Violation::Pompous));
        assert!(
            review("Главное — принять себя таким, какой вы есть.").contains(&Violation::Pompous)
        );
        assert!(
            review("В рамках данного этапа необходимо понимать динамику.")
                .contains(&Violation::Bureaucratic)
        );
    }

    /// Reflection is what makes the reply sound like a person rather than a
    /// form, so the review must not stand in its way.
    #[test]
    fn lets_a_plain_reflection_through() {
        let reflections = [
            "Пришли с работы, легли — и вечер закончился. Что было в голове в тот момент?",
            "Значит, вы оттягивали до последнего, а потом стало поздно. Что мешало начать раньше?",
            "Вы говорите про стеснение, а описываете, как молчали весь вечер. Так было и в этот раз?",
        ];
        for reply in reflections {
            assert!(
                review(reply).is_empty(),
                "review rejected a plain reflection {reply:?}: {:?}",
                review(reply)
            );
        }
    }

    #[test]
    fn every_generating_state_has_a_usable_fallback() {
        let states = [
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
        for state in states {
            let text = fallback(&state);
            assert!(!text.is_empty(), "{} has no fallback", state.name());
            // The floor must not itself break the rules.
            assert!(
                review(text).is_empty(),
                "fallback for {} fails review: {:?}",
                state.name(),
                review(text)
            );
        }
    }

    #[test]
    fn objections_are_phrased_as_instructions() {
        for violation in [
            Violation::Sycophancy,
            Violation::OffersOptions,
            Violation::GivesAdvice,
            Violation::ClaimsFeelings,
            Violation::TooLong,
            Violation::MultipleQuestions,
        ] {
            assert!(!violation.objection().is_empty());
        }
    }
}
