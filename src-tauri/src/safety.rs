//! The risk guard.
//!
//! This is deliberately **not** a risk predictor. Fifty years of research
//! (Franklin et al., Psychological Bulletin 2017, 365 studies) put prediction
//! of suicidal behaviour barely above chance, and PPV of machine-learning
//! models at 6–17% in-sample. A model that claims to predict risk is either
//! uselessly unspecific or dangerously insensitive.
//!
//! What this does instead: detect *explicit statements* and route
//! unconditionally. It never scores, never estimates likelihood, and never
//! reports a "low risk, carry on" verdict — the only outcomes are `Clear`
//! and `Flagged`.
//!
//! Why it does not ask the main model: McBain et al. (Psychiatric Services
//! 2025) ran 30 suicide-related prompts past ChatGPT, Claude and Gemini a
//! hundred times each. Very low and very high risk were handled consistently;
//! the middle band was not. "У меня суицидальные мысли" — the single most
//! likely thing this product will receive — sits in that middle band. So the
//! guard is deterministic code that runs before any generation.
//!
//! Sensitivity is preferred over specificity, following ASQ (sensitivity 100%,
//! specificity 89%). But not blindly: Russian is full of idioms built on
//! "убить" and "умирать", and a guard that fires on "умираю с голоду" trains
//! people to dismiss the crisis screen. That is alarm fatigue, and it costs
//! lives in exactly the case the guard exists for. Hence two tiers.

use crate::protocol::RiskKind;

/// Written into `safety_events.guard_version`. Bump on every change to the
/// patterns — the stored history has to say which logic actually ran.
pub const GUARD_VERSION: &str = "rules-1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Assessment {
    Clear,
    Flagged {
        kind: RiskKind,
        /// Which pattern fired. Kept for auditing the guard itself, never
        /// stored alongside the message and never shown to the user.
        matched: &'static str,
        guard_version: &'static str,
    },
}

impl Assessment {
    pub fn is_flagged(&self) -> bool {
        matches!(self, Assessment::Flagged { .. })
    }

    pub fn kind(&self) -> Option<RiskKind> {
        match self {
            Assessment::Flagged { kind, .. } => Some(*kind),
            Assessment::Clear => None,
        }
    }
}

pub trait RiskDetector {
    fn assess(&self, text: &str) -> Assessment;
}

/// Statements that fire regardless of surrounding context. No idiom in either
/// language produces these by accident.
const HARD_PATTERNS: &[(&str, RiskKind)] = &[
    // --- Russian: suicidality ---
    ("покончить с собой", RiskKind::Suicidality),
    ("покончу с собой", RiskKind::Suicidality),
    ("свести счеты с жизнью", RiskKind::Suicidality),
    ("суицид", RiskKind::Suicidality),
    ("суицидальн", RiskKind::Suicidality),
    ("убить себя", RiskKind::Suicidality),
    ("убью себя", RiskKind::Suicidality),
    ("не хочу жить", RiskKind::Suicidality),
    ("не хочется жить", RiskKind::Suicidality),
    ("нет смысла жить", RiskKind::Suicidality),
    ("не вижу смысла жить", RiskKind::Suicidality),
    ("лучше бы я не родил", RiskKind::Suicidality),
    ("хочу умереть", RiskKind::Suicidality),
    ("хочу сдохнуть", RiskKind::Suicidality),
    ("заснуть и не проснуться", RiskKind::Suicidality),
    ("повеситься", RiskKind::Suicidality),
    ("повешусь", RiskKind::Suicidality),
    ("выпрыгнуть из окна", RiskKind::Suicidality),
    ("шагнуть с крыши", RiskKind::Suicidality),
    ("наглотаться таблеток", RiskKind::Suicidality),
    ("передозировк", RiskKind::Suicidality),
    // --- Russian: self-harm ---
    ("резать себя", RiskKind::SelfHarm),
    ("режу себя", RiskKind::SelfHarm),
    ("порезать себя", RiskKind::SelfHarm),
    ("порезал себя", RiskKind::SelfHarm),
    ("причинить себе вред", RiskKind::SelfHarm),
    ("навредить себе", RiskKind::SelfHarm),
    ("selfharm", RiskKind::SelfHarm),
    ("селфхарм", RiskKind::SelfHarm),
    // --- Russian: harm to others ---
    ("убью его", RiskKind::HarmToOthers),
    ("убью ее", RiskKind::HarmToOthers),
    ("убью их", RiskKind::HarmToOthers),
    ("хочу убить", RiskKind::HarmToOthers),
    // --- English: suicidality ---
    ("kill myself", RiskKind::Suicidality),
    ("killing myself", RiskKind::Suicidality),
    ("end my life", RiskKind::Suicidality),
    ("take my own life", RiskKind::Suicidality),
    ("commit suicide", RiskKind::Suicidality),
    ("suicidal", RiskKind::Suicidality),
    ("want to die", RiskKind::Suicidality),
    ("better off dead", RiskKind::Suicidality),
    ("dont want to be alive", RiskKind::Suicidality),
    ("dont want to live", RiskKind::Suicidality),
    ("hang myself", RiskKind::Suicidality),
    ("overdose", RiskKind::Suicidality),
    // --- English: self-harm ---
    ("cut myself", RiskKind::SelfHarm),
    ("cutting myself", RiskKind::SelfHarm),
    ("hurt myself", RiskKind::SelfHarm),
    ("harm myself", RiskKind::SelfHarm),
    ("self harm", RiskKind::SelfHarm),
    // --- English: harm to others ---
    ("kill him", RiskKind::HarmToOthers),
    ("kill her", RiskKind::HarmToOthers),
    ("kill them", RiskKind::HarmToOthers),
];

/// Statements that fire unless an idiom explains them. Each entry lists the
/// idioms that neutralise it — kept narrow on purpose.
const SOFT_PATTERNS: &[(&str, RiskKind, &[&str])] = &[
    (
        "умереть",
        RiskKind::Suicidality,
        &["умереть со смеху", "умереть от скуки", "хоть умри"],
    ),
    (
        "умираю",
        RiskKind::Suicidality,
        &["умираю с голоду", "умираю от смеха", "умираю от скуки", "умираю как хочу"],
    ),
    (
        "сдохнуть",
        RiskKind::Suicidality,
        &["сдохнуть от скуки", "сдохнуть со смеху"],
    ),
    (
        "все закончилось",
        RiskKind::Suicidality,
        &["все закончилось хорошо", "все закончилось нормально", "все закончилось благополучно"],
    ),
    (
        "исчезнуть",
        RiskKind::Suicidality,
        &["исчезнуть из чата", "исчезнуть с радаров"],
    ),
];

/// Idioms that must never be read as risk. Checked before soft patterns.
///
/// This list is a deliberate trade of sensitivity for usability: each entry is
/// a phrase common enough that firing on it would teach users to dismiss the
/// crisis screen. Hard patterns are unaffected by it.
const IDIOMS: &[&str] = &[
    "убил на это",
    "убил кучу времени",
    "убить время",
    "убивает время",
    "меня это убивает",
    "убийственно",
    "до смерти надоел",
    "до смерти устал",
    "смертельно устал",
    "смертельно скучно",
    "мертвый сезон",
    "убил день",
    "killing time",
    "this is killing me",
    "dead tired",
    "dead line",
    "deadline",
    "dying to know",
    "dying to see",
];

pub struct RuleBasedDetector;

impl RiskDetector for RuleBasedDetector {
    fn assess(&self, text: &str) -> Assessment {
        let normalised = normalise(text);

        // Hard patterns bypass idiom handling entirely.
        for (pattern, kind) in HARD_PATTERNS {
            if normalised.contains(pattern) {
                return Assessment::Flagged {
                    kind: *kind,
                    matched: pattern,
                    guard_version: GUARD_VERSION,
                };
            }
        }

        let idiomatic = IDIOMS.iter().any(|idiom| normalised.contains(idiom));

        for (pattern, kind, exceptions) in SOFT_PATTERNS {
            if !normalised.contains(pattern) {
                continue;
            }
            if idiomatic || exceptions.iter().any(|e| normalised.contains(e)) {
                continue;
            }
            return Assessment::Flagged {
                kind: *kind,
                matched: pattern,
                guard_version: GUARD_VERSION,
            };
        }

        Assessment::Clear
    }
}

/// Combines detectors so that a flag from any one of them wins.
///
/// This is the seam for adding a small classifier model later: it joins the
/// rules rather than replacing them, because a model that silently stops
/// firing is worse than rules that fire too often.
pub struct AnyOf(pub Vec<Box<dyn RiskDetector + Send + Sync>>);

impl RiskDetector for AnyOf {
    fn assess(&self, text: &str) -> Assessment {
        for detector in &self.0 {
            let verdict = detector.assess(text);
            if verdict.is_flagged() {
                return verdict;
            }
        }
        Assessment::Clear
    }
}

/// Lowercases, folds ё to е, strips punctuation and collapses whitespace, so
/// that "Не хочу жить..." and "не  хочу  жить" both match.
fn normalise(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_space = true;

    for ch in text.chars() {
        let ch = match ch.to_lowercase().next().unwrap_or(ch) {
            'ё' => 'е',
            'й' => 'й',
            other => other,
        };

        if ch.is_alphanumeric() {
            out.push(ch);
            last_was_space = false;
        } else if matches!(ch, '\'' | '\u{2019}' | '`') {
            // Apostrophes are dropped, not turned into a separator: otherwise
            // "don't want to live" normalises to "don t want to live" and slips
            // past the pattern. A missed explicit statement is the one failure
            // mode this module exists to prevent.
        } else if !last_was_space {
            out.push(' ');
            last_was_space = true;
        }
    }

    out.trim_end().to_string()
}

/// Crisis resources shown when the guard fires. Static and verified — never
/// generated, because a hallucinated hotline number is worse than none.
///
/// NOTE BEFORE RELEASE: every number here must be re-verified against its
/// official source, and the Russian and CIS entries are still missing because
/// I would not add a number I could not confirm. A wrong crisis line is a
/// safety defect, not a content gap.
pub struct CrisisResource {
    pub locale: &'static str,
    pub name: &'static str,
    pub contact: &'static str,
}

const RESOURCES: &[CrisisResource] = &[
    CrisisResource {
        locale: "en-US",
        name: "988 Suicide & Crisis Lifeline",
        contact: "988",
    },
    CrisisResource {
        locale: "en-GB",
        name: "Samaritans",
        contact: "116 123",
    },
];

/// Resources for a locale, falling back to everything known when the locale
/// has no entry — showing too many is acceptable, showing none is not.
pub fn resources_for(locale: &str) -> Vec<&'static CrisisResource> {
    let exact: Vec<_> = RESOURCES.iter().filter(|r| r.locale == locale).collect();
    if exact.is_empty() {
        RESOURCES.iter().collect()
    } else {
        exact
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assess(text: &str) -> Assessment {
        RuleBasedDetector.assess(text)
    }

    /// The phrases the guard exists for. A failure here is a safety defect.
    #[test]
    fn flags_explicit_statements() {
        let must_flag = [
            // The middle-band phrase that LLMs handle inconsistently.
            "у меня суицидальные мысли",
            "иногда думаю покончить с собой",
            "не хочу жить",
            "я не вижу смысла жить дальше",
            "хочу умереть",
            "хочу просто заснуть и не проснуться",
            "думаю о том чтобы навредить себе",
            "вчера порезал себя",
            "я убью его если он ещё раз так сделает",
            "i have suicidal thoughts",
            "i want to kill myself",
            "i think i'd be better off dead",
            "i've been cutting myself again",
            "I DON'T WANT TO LIVE",
            "Не хочу жить...",
            "не   хочу   жить",
        ];

        for text in must_flag {
            assert!(
                assess(text).is_flagged(),
                "guard missed an explicit statement: {text:?}"
            );
        }
    }

    #[test]
    fn assigns_the_right_kind() {
        assert_eq!(assess("хочу покончить с собой").kind(), Some(RiskKind::Suicidality));
        assert_eq!(assess("я режу себя").kind(), Some(RiskKind::SelfHarm));
        assert_eq!(assess("я хочу убить соседа").kind(), Some(RiskKind::HarmToOthers));
    }

    /// Idioms that must not fire. Every false alarm here teaches the user to
    /// dismiss the crisis screen, which defeats the guard entirely.
    #[test]
    fn does_not_fire_on_idioms() {
        let must_not_flag = [
            "умираю с голоду, пойду поем",
            "убил на это весь день и ничего не вышло",
            "эта задача меня убивает",
            "до смерти устал за неделю",
            "смертельно скучно на работе",
            "дедлайн горит",
            "умираю от смеха",
            "все закончилось хорошо",
            "i was killing time before the meeting",
            "this deadline is killing me",
            "i'm dying to know what happened",
            "dead tired after the trip",
            // Ordinary practice content must pass through untouched.
            "вчера не смог заставить себя выйти из дома",
            "поругался с начальником и весь вечер прокручивал это в голове",
            "запланировал прогулку на завтра в семь",
        ];

        for text in must_not_flag {
            let verdict = assess(text);
            assert!(
                !verdict.is_flagged(),
                "guard produced a false alarm on {text:?}: {verdict:?}"
            );
        }
    }

    #[test]
    fn hard_patterns_ignore_idiomatic_context() {
        // An idiom elsewhere in the message must not suppress an explicit statement.
        let text = "убил на это весь день, и вообще я не хочу жить";
        assert!(assess(text).is_flagged(), "idiom suppressed a hard pattern");
    }

    #[test]
    fn reports_the_guard_version_for_the_audit_trail() {
        match assess("хочу умереть") {
            Assessment::Flagged { guard_version, matched, .. } => {
                assert_eq!(guard_version, GUARD_VERSION);
                assert!(!matched.is_empty());
            }
            Assessment::Clear => panic!("expected a flag"),
        }
    }

    #[test]
    fn there_is_no_middle_verdict() {
        // The type has exactly two shapes. If a "low risk" variant is ever
        // added, this test is where the argument should happen first.
        let clear = assess("сегодня спокойный день");
        assert_eq!(clear, Assessment::Clear);
        assert!(clear.kind().is_none());
    }

    #[test]
    fn a_flag_from_any_detector_wins() {
        struct NeverFlags;
        impl RiskDetector for NeverFlags {
            fn assess(&self, _: &str) -> Assessment {
                Assessment::Clear
            }
        }

        let combined = AnyOf(vec![Box::new(NeverFlags), Box::new(RuleBasedDetector)]);
        assert!(combined.assess("не хочу жить").is_flagged());
        assert!(!combined.assess("обычный день").is_flagged());
    }

    #[test]
    fn unknown_locale_still_gets_resources() {
        let resources = resources_for("ru-RU");
        assert!(
            !resources.is_empty(),
            "a locale without its own entry must still see something"
        );
    }

    #[test]
    fn normalisation_handles_case_yo_and_punctuation() {
        assert_eq!(normalise("Всё — ПЛОХО!!!"), "все плохо");
        assert_eq!(normalise("не   хочу\n\nжить"), "не хочу жить");
    }

    #[test]
    fn apostrophes_do_not_split_words() {
        // Regression: an apostrophe turned into a space let "don't want to
        // live" slip past the pattern list.
        assert_eq!(normalise("I don't want to live"), "i dont want to live");
        assert_eq!(normalise("I don\u{2019}t want to live"), "i dont want to live");
        assert!(assess("I don't want to live").is_flagged());
        assert!(assess("i don\u{2019}t want to be alive anymore").is_flagged());
    }
}
