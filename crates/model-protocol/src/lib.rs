//! Metro's model wire types, shared by the server lanes that call models.
//!
//! - [`Capability`]: what one model connection is set up to do. A connection
//!   serves exactly one capability and is never used for another.
//! - System One: the typed decision API of TypeSafe
//!   (`POST /v1/systemone`, models such as `jev-latest`). A request sends a
//!   `state` and a map of typed questions ([`Question`]: Noul, Choice,
//!   Score); the answer to each comes back under the same id ([`Answer`]).
//!   [`parse_answers`] checks a response strictly against the questions that
//!   were asked, so callers can treat any mismatch as "no decision" instead
//!   of guessing.
//!
//! The types do no I/O. The `client` module adds the HTTP calls
//! ([`decide`], [`embed`], [`list_models`]) and their strict request and
//! response checks, shared by the server and `metro-connector`.

use std::collections::BTreeMap;

mod client;
mod media;
pub mod web_privacy;
pub use client::*;
pub use media::*;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What a model connection is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Answers and tool turns.
    Chat,
    /// Typed System One decisions.
    SystemOne,
    /// Text embeddings.
    Embeddings,
}

impl Capability {
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Chat => "chat",
            Capability::SystemOne => "system_one",
            Capability::Embeddings => "embeddings",
        }
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Most questions Metro puts in one System One request.
pub const MAX_QUESTIONS: usize = 64;
/// Longest question id.
pub const MAX_QUESTION_ID: usize = 64;
/// Most options of one Choice (the API accepts up to 255).
pub const MAX_CHOICE_OPTIONS: usize = 255;
/// Fewest and most levels of one Score.
pub const MIN_SCORE_LEVELS: usize = 2;
pub const MAX_SCORE_LEVELS: usize = 10;

/// The two sides of a Noul: what counts as a yes and what counts as a no.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoulCriteria {
    #[serde(rename = "true")]
    pub yes: Value,
    #[serde(rename = "false")]
    pub no: Value,
}

/// One typed System One question. `instructions` and criteria may be a
/// string, an object or an array, as the API allows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Question {
    /// A yes/no question; the answer is the probability of yes.
    Noul {
        instructions: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
    /// One option of a fixed set, keyed by option name.
    Choice {
        instructions: Value,
        criteria: BTreeMap<String, Value>,
    },
    /// A position on ordered levels, low to high.
    Score {
        instructions: Value,
        criteria: Vec<Value>,
    },
}

impl Question {
    pub fn noul(instructions: impl Into<Value>) -> Self {
        Question::Noul {
            instructions: instructions.into(),
            criteria: None,
        }
    }

    pub fn noul_with(
        instructions: impl Into<Value>,
        yes: impl Into<Value>,
        no: impl Into<Value>,
    ) -> Self {
        Question::Noul {
            instructions: instructions.into(),
            criteria: Some(NoulCriteria {
                yes: yes.into(),
                no: no.into(),
            }),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Question::Noul { .. } => "noul",
            Question::Choice { .. } => "choice",
            Question::Score { .. } => "score",
        }
    }
}

/// The questions of one request, by id. The ids are not shown to the model.
pub type Questions = BTreeMap<String, Question>;

/// Check a question set before it is sent: ids, counts and option limits.
pub fn validate_questions(questions: &Questions) -> Result<(), ProtocolError> {
    if questions.is_empty() || questions.len() > MAX_QUESTIONS {
        return Err(ProtocolError::new(format!(
            "a request asks 1..={MAX_QUESTIONS} questions"
        )));
    }
    for (id, q) in questions {
        if !valid_key(id, MAX_QUESTION_ID) {
            return Err(ProtocolError::new("a question id is malformed"));
        }
        match q {
            Question::Noul { .. } => {}
            Question::Choice { criteria, .. } => {
                if criteria.is_empty() || criteria.len() > MAX_CHOICE_OPTIONS {
                    return Err(ProtocolError::new(format!(
                        "a choice has 1..={MAX_CHOICE_OPTIONS} options"
                    )));
                }
                if criteria
                    .keys()
                    .any(|k| k.is_empty() || k.chars().count() > 128)
                {
                    return Err(ProtocolError::new("a choice option name is malformed"));
                }
            }
            Question::Score { criteria, .. } => {
                if !(MIN_SCORE_LEVELS..=MAX_SCORE_LEVELS).contains(&criteria.len()) {
                    return Err(ProtocolError::new(format!(
                        "a score has {MIN_SCORE_LEVELS}..={MAX_SCORE_LEVELS} levels"
                    )));
                }
            }
        }
    }
    Ok(())
}

/// The body of `POST /v1/systemone`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SystemOneRequest {
    pub model: String,
    pub state: Value,
    pub questions: Questions,
}

/// One validated answer.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    /// Probability that the answer is yes, 0..=1.
    Noul { noul: f64 },
    Choice {
        /// One of the options that were asked.
        choice: String,
        confidence: Option<f64>,
        /// Probability of each option returned; every key was asked.
        probabilities: BTreeMap<String, f64>,
    },
    Score {
        /// 0..=top level.
        score: f64,
        confidence: Option<f64>,
        probabilities: BTreeMap<String, f64>,
    },
}

impl Answer {
    pub fn as_noul(&self) -> Option<f64> {
        match self {
            Answer::Noul { noul } => Some(*noul),
            _ => None,
        }
    }

    /// The chosen option and its probability (`None` when the response
    /// gave no probability for it).
    pub fn as_choice(&self) -> Option<(&str, Option<f64>)> {
        match self {
            Answer::Choice {
                choice,
                probabilities,
                ..
            } => Some((choice.as_str(), probabilities.get(choice).copied())),
            _ => None,
        }
    }

    pub fn as_score(&self) -> Option<f64> {
        match self {
            Answer::Score { score, .. } => Some(*score),
            _ => None,
        }
    }
}

/// Validated answers by question id: exactly the ids that were asked.
pub type Answers = BTreeMap<String, Answer>;

/// A System One response that does not fit the questions asked. The message
/// names the problem, never the content of the request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolError(String);

impl ProtocolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProtocolError {}

/// Parse `response` (the whole JSON body of `POST /v1/systemone`) against
/// `questions`. Every question must be answered once, with its own type;
/// no answer may be for a question that was not asked; every number must be
/// finite and in range; a Choice must name an option that was offered.
/// Other fields (`model`, `usage`, `legend`, ...) are ignored.
pub fn parse_answers(questions: &Questions, response: &Value) -> Result<Answers, ProtocolError> {
    let answers = response
        .get("answers")
        .and_then(Value::as_object)
        .ok_or_else(|| ProtocolError::new("the response has no answers object"))?;
    if let Some(extra) = answers.keys().find(|k| !questions.contains_key(*k)) {
        let _ = extra;
        return Err(ProtocolError::new(
            "the response answers a question that was not asked",
        ));
    }
    let mut out = Answers::new();
    for (id, question) in questions {
        let answer = answers
            .get(id)
            .and_then(Value::as_object)
            .ok_or_else(|| ProtocolError::new("the response is missing an answer"))?;
        let kind = answer.get("type").and_then(Value::as_str);
        if kind != Some(question.kind()) {
            return Err(ProtocolError::new("an answer has the wrong type"));
        }
        let parsed = match question {
            Question::Noul { .. } => Answer::Noul {
                noul: unit(answer.get("noul"))?,
            },
            Question::Choice { criteria, .. } => {
                let choice = answer
                    .get("choice")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ProtocolError::new("a choice answer has no choice"))?;
                if !criteria.contains_key(choice) {
                    return Err(ProtocolError::new(
                        "a choice answer names an option that was not offered",
                    ));
                }
                let probabilities =
                    probabilities(answer.get("probabilities"), |k| criteria.contains_key(k))?;
                Answer::Choice {
                    choice: choice.to_owned(),
                    confidence: optional_unit(answer.get("confidence"))?,
                    probabilities,
                }
            }
            Question::Score { criteria, .. } => {
                let top = (criteria.len() - 1) as f64;
                let score = answer
                    .get("score")
                    .and_then(Value::as_f64)
                    .filter(|s| s.is_finite() && (0.0..=top).contains(s))
                    .ok_or_else(|| ProtocolError::new("a score answer is out of range"))?;
                let levels = criteria.len();
                let probabilities = probabilities(answer.get("probabilities"), |k| {
                    k.parse::<usize>().is_ok_and(|n| n < levels)
                })?;
                Answer::Score {
                    score,
                    confidence: optional_unit(answer.get("confidence"))?,
                    probabilities,
                }
            }
        };
        out.insert(id.clone(), parsed);
    }
    Ok(out)
}

fn unit(v: Option<&Value>) -> Result<f64, ProtocolError> {
    v.and_then(Value::as_f64)
        .filter(|p| p.is_finite() && (0.0..=1.0).contains(p))
        .ok_or_else(|| ProtocolError::new("a probability is missing or out of range"))
}

fn optional_unit(v: Option<&Value>) -> Result<Option<f64>, ProtocolError> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(v) => unit(Some(v)).map(Some),
    }
}

fn probabilities(
    v: Option<&Value>,
    allowed: impl Fn(&str) -> bool,
) -> Result<BTreeMap<String, f64>, ProtocolError> {
    let Some(v) = v else {
        return Ok(BTreeMap::new());
    };
    let map = v
        .as_object()
        .ok_or_else(|| ProtocolError::new("probabilities must be an object"))?;
    let mut out = BTreeMap::new();
    for (k, p) in map {
        if !allowed(k) {
            return Err(ProtocolError::new(
                "probabilities name an option that was not offered",
            ));
        }
        out.insert(k.clone(), unit(Some(p))?);
    }
    Ok(out)
}

/// `[a-z0-9_]`, starting with a letter, 1..=`max` bytes: the shape of
/// question ids and of keys Metro puts in typed choices.
pub fn valid_key(key: &str, max: usize) -> bool {
    !key.is_empty()
        && key.len() <= max
        && key.starts_with(|c: char| c.is_ascii_lowercase())
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn asked() -> Questions {
        let mut q = Questions::new();
        q.insert(
            "directed".into(),
            Question::noul("Is it for the assistant?"),
        );
        q.insert(
            "use_case".into(),
            Question::Choice {
                instructions: json!("Which use case?"),
                criteria: [
                    ("code".to_owned(), json!("Programming")),
                    ("writing".to_owned(), json!("Prose")),
                ]
                .into_iter()
                .collect(),
            },
        );
        q
    }

    #[test]
    fn serializes_like_the_api() {
        let q = asked();
        let v = serde_json::to_value(&q).unwrap();
        assert_eq!(
            v["directed"],
            json!({"type":"noul","instructions":"Is it for the assistant?"})
        );
        assert_eq!(v["use_case"]["type"], "choice");
        assert_eq!(v["use_case"]["criteria"]["code"], "Programming");
        let n = Question::noul_with("q", "yes means", "no means");
        assert_eq!(
            serde_json::to_value(n).unwrap()["criteria"],
            json!({"true":"yes means","false":"no means"})
        );
        validate_questions(&q).unwrap();
    }

    #[test]
    fn accepts_a_well_formed_response() {
        let r = json!({"model":"jev-1.14.0","answers":{
            "directed":{"type":"noul","noul":0.91},
            "use_case":{"type":"choice","choice":"code","confidence":0.8,
                        "probabilities":{"code":0.9,"writing":0.1}}},
            "usage":{"input_tokens":1,"output_tokens":1}});
        let a = parse_answers(&asked(), &r).unwrap();
        assert_eq!(a["directed"].as_noul(), Some(0.91));
        assert_eq!(a["use_case"].as_choice(), Some(("code", Some(0.9))));
    }

    #[test]
    fn rejects_malformed_responses() {
        let ok_choice = json!({"type":"choice","choice":"code","probabilities":{"code":1.0}});
        let bad = [
            json!({}),
            json!({"answers": []}),
            json!({"answers": {"directed": {"type":"noul","noul":0.9}}}),
            json!({"answers": {"directed": {"type":"noul","noul":1.5}, "use_case": ok_choice}}),
            json!({"answers": {"directed": {"type":"noul","noul":"0.9"}, "use_case": ok_choice}}),
            json!({"answers": {"directed": {"type":"choice","noul":0.9}, "use_case": ok_choice}}),
            json!({"answers": {"directed": {"type":"noul","noul":0.9},
                               "use_case": {"type":"choice","choice":"pc_evil"}}}),
            json!({"answers": {"directed": {"type":"noul","noul":0.9},
                               "use_case": {"type":"choice","choice":"code",
                                            "probabilities":{"other":0.2}}}}),
            json!({"answers": {"directed": {"type":"noul","noul":0.9}, "use_case": ok_choice,
                               "extra": {"type":"noul","noul":0.1}}}),
        ];
        for r in bad {
            assert!(parse_answers(&asked(), &r).is_err(), "accepted {r}");
        }
    }

    #[test]
    fn checks_scores_and_keys() {
        let mut q = Questions::new();
        q.insert(
            "relevance".into(),
            Question::Score {
                instructions: json!("How relevant?"),
                criteria: vec![json!("no"), json!("some"), json!("very")],
            },
        );
        let good = json!({"answers":{"relevance":{"type":"score","score":1.5,
            "probabilities":{"0":0.0,"1":0.5,"2":0.5}}}});
        assert_eq!(
            parse_answers(&q, &good).unwrap()["relevance"].as_score(),
            Some(1.5)
        );
        let high = json!({"answers":{"relevance":{"type":"score","score":2.5}}});
        assert!(parse_answers(&q, &high).is_err());
        assert!(valid_key("code_review", 32));
        assert!(!valid_key("Code", 32));
        assert!(!valid_key("1st", 32));
        assert!(!valid_key("", 32));
    }
}
