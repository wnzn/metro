//! The System One rerank request and how its answer is read.
//!
//! Pointwise, as in TypeSafe's re-ranking pattern: one request per query
//! and candidate pair, `state` holding exactly that pair, and one `noul`
//! question with the same instructions and criteria for every pair, so
//! every noul is on the same scale and can be sorted together. No request
//! ever holds two candidates.
//!
//! Request (`state`, `questions` as passed to `providers::decide`):
//!
//! ```json
//! { "state": { "search_query": "…", "candidate_message": "…" },
//!   "questions": { "relevant": { "type": "noul", "instructions": "…",
//!                                "criteria": { "true": "…", "false": "…" } } } }
//! ```
//!
//! Answer: `{"answers": {"relevant": {"type": "noul", "noul": 0.87}}, …}`
//! (the whole System One response) or just its `answers` object.

use serde_json::{Value, json};

/// The question name.
pub const QUESTION: &str = "relevant";

const INSTRUCTIONS: &str = "A person searched their own chat history with the search query. \
Is the candidate message relevant to what they are looking for: does it contain, discuss or \
directly answer what the query asks about?";
const CRITERIA_TRUE: &str = "The candidate message is about what the query asks for, or \
contains the information, decision or answer the query is looking for.";
const CRITERIA_FALSE: &str = "The candidate message is unrelated, or shares only words or a \
general topic with the query without being what the person is looking for.";

/// `state` for one pair.
pub fn state(query: &str, candidate: &str) -> Value {
    json!({
        "search_query": query,
        "candidate_message": candidate,
    })
}

/// `questions`: the same for every pair.
pub fn questions() -> Value {
    json!({
        QUESTION: {
            "type": "noul",
            "instructions": INSTRUCTIONS,
            "criteria": { "true": CRITERIA_TRUE, "false": CRITERIA_FALSE },
        }
    })
}

/// The noul of a System One answer, when it is a well-formed `noul` answer
/// to [`QUESTION`] with a finite value in 0..=1. Anything else is `None`:
/// such a candidate is not reranked, and nothing is guessed.
pub fn read_noul(answer: &Value) -> Option<f64> {
    let answers = answer.get("answers").unwrap_or(answer);
    let a = answers.get(QUESTION)?;
    if a.get("type").and_then(Value::as_str) != Some("noul") {
        return None;
    }
    let n = a.get("noul")?.as_f64()?;
    (n.is_finite() && (0.0..=1.0).contains(&n)).then_some(n)
}
