//! Judgments over a runtime state: from the decisions endpoint, or from a chat model asked for the same answers as JSON.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::chat::{self, Part};
use crate::{DecideArgs, Decided, Models, Served, Usage};

const MAX_TOKENS: u32 = 800;
const INSTRUCTIONS: &str = "You judge the state of a computer screen. Answer each question below and reply with one JSON object and nothing else, keyed by question. A \"noul\" question is a yes/no question: answer {\"noul\": P}, P from 0 to 1 that the answer is yes. A \"choice\" question picks one of its criteria keys: answer {\"choice\": KEY, \"probabilities\": {KEY: P, ...}} over all its keys, summing to 1.";

#[derive(Serialize)]
struct Ask<'a> {
    model: &'a str,
    state: &'a Value,
    questions: &'a Map<String, Value>,
}

#[derive(Deserialize)]
struct Judged {
    id: Option<String>,
    model: Option<String>,
    #[serde(default)]
    answers: Map<String, Value>,
    #[serde(default)]
    usage: Value,
}

/// Asks the decisions endpoint `model` the questions of `args`.
pub(crate) async fn decisions(
    m: &Models,
    model: &str,
    args: &DecideArgs,
    timeout: Duration,
) -> Result<Decided, String> {
    let ask = Ask {
        model,
        state: &args.state,
        questions: &args.questions,
    };
    let r: Judged = m
        .post(&m.config.decisions_url, model, &ask, timeout)
        .await?;
    Ok(Decided {
        answers: answers(model, r.answers, &args.questions)?,
        served: Served {
            model: model.to_owned(),
            build: r.model,
            id: r.id,
        },
        usage: Usage::of(model, &r.usage),
    })
}

/// Asks the chat model `model` the questions of `args` and reads its JSON answers.
pub(crate) async fn over_chat(
    m: &Models,
    model: &str,
    args: &DecideArgs,
    timeout: Duration,
) -> Result<Decided, String> {
    let asked =
        serde_json::to_string(&args.questions).map_err(|e| format!("encode questions: {e}"))?;
    let mut parts = vec![
        Part::Text(format!("{INSTRUCTIONS}\nQuestions: {asked}").into()),
        Part::Text("State:".into()),
    ];
    parts.extend(Part::from_state(&args.state));
    let reply = chat::complete(m, model, &parts, MAX_TOKENS, timeout).await?;
    Ok(Decided {
        answers: answers(model, json_object(model, &reply.text)?, &args.questions)?,
        served: Served {
            model: model.to_owned(),
            build: reply.build,
            id: reply.id,
        },
        usage: reply.usage,
    })
}

fn json_object(model: &str, text: &str) -> Result<Map<String, Value>, String> {
    let body = match (text.find('{'), text.rfind('}')) {
        (Some(s), Some(e)) if s < e => &text[s..=e],
        _ => {
            return Err(format!(
                "{model}: no JSON object in the reply: {}",
                crate::snippet(text, 120)
            ));
        }
    };
    serde_json::from_str(body).map_err(|e| format!("{model}: {e}: {}", crate::snippet(text, 120)))
}

/// One answer per question: `{probability}` for a yes question, `{choice, confidence, probabilities}` for a choice; a missing or partial answer is an error.
fn answers(
    model: &str,
    mut given: Map<String, Value>,
    questions: &Map<String, Value>,
) -> Result<Map<String, Value>, String> {
    questions
        .iter()
        .map(|(k, q)| {
            let Some(Value::Object(mut a)) = given.remove(k) else {
                return Err(format!("{model}: no answer for {k}"));
            };
            if q["type"] == "noul" {
                let p = a
                    .get("noul")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| format!("{model}: {k} has no noul"))?;
                return Ok((k.clone(), json!({"probability": p.clamp(0.0, 1.0)})));
            }
            let Some(Value::String(choice)) = a.remove("choice") else {
                return Err(format!("{model}: {k} has no choice"));
            };
            let Some(Value::Object(probabilities)) = a.remove("probabilities") else {
                return Err(format!("{model}: {k} has no probabilities"));
            };
            let confidence = a
                .get("confidence")
                .and_then(Value::as_f64)
                .or_else(|| probabilities.get(&choice).and_then(Value::as_f64))
                .unwrap_or(0.0);
            let answer = Map::from_iter([
                ("choice".to_owned(), Value::String(choice)),
                ("confidence".to_owned(), confidence.into()),
                ("probabilities".to_owned(), Value::Object(probabilities)),
            ]);
            Ok((k.clone(), Value::Object(answer)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn questions() -> Map<String, Value> {
        json!({"on": {"type": "noul", "instructions": "Is it on?"}, "pick": {"type": "choice", "instructions": "Which?", "criteria": {"r1": "a", "none": "b"}}})
            .as_object()
            .unwrap()
            .clone()
    }

    #[test]
    fn a_chat_reply_becomes_one_answer_per_question() {
        let text = "Sure.\n{\"on\": {\"noul\": 1.4}, \"pick\": {\"choice\": \"r1\", \"probabilities\": {\"r1\": 0.8, \"none\": 0.2}}}";
        let answers = answers("m", json_object("m", text).unwrap(), &questions()).unwrap();
        assert_eq!(answers["on"], json!({"probability": 1.0}));
        assert_eq!(
            answers["pick"],
            json!({"choice": "r1", "confidence": 0.8, "probabilities": {"r1": 0.8, "none": 0.2}})
        );
    }

    #[test]
    fn a_missing_or_partial_answer_is_an_error() {
        let only_on = json!({"on": {"noul": 0.2}}).as_object().unwrap().clone();
        assert_eq!(
            answers("m", only_on, &questions()).unwrap_err(),
            "m: no answer for pick"
        );
        let no_probabilities =
            json!({"on": {"noul": 0.2}, "pick": {"type": "choice", "choice": "r1"}})
                .as_object()
                .unwrap()
                .clone();
        assert_eq!(
            answers("m", no_probabilities, &questions()).unwrap_err(),
            "m: pick has no probabilities"
        );
        let no_noul = json!({"on": {"type": "noul"}}).as_object().unwrap().clone();
        assert_eq!(
            answers("m", no_noul, &questions()).unwrap_err(),
            "m: on has no noul"
        );
        assert!(
            json_object("m", "no json here")
                .unwrap_err()
                .starts_with("m: no JSON object")
        );
    }
}
