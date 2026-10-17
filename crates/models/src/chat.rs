//! One non-streaming chat completion: a single user message of text and PNG parts, answered with text.

use std::borrow::Cow;
use std::time::Duration;

use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Models, Usage};

const PNG_DATA_URL: &str = "data:image/png;base64,";

/// A part of the one user message: text, or a PNG as base64, borrowed where the caller holds it.
#[derive(Debug, PartialEq)]
pub(crate) enum Part<'a> {
    Text(Cow<'a, str>),
    Png(&'a str),
}

impl<'a> Part<'a> {
    /// The parts of a runtime state: strings are text, PNG data URLs are PNGs, anything else is its JSON.
    pub(crate) fn from_state(state: &'a Value) -> Vec<Part<'a>> {
        match state {
            Value::Array(items) => items.iter().map(Part::from_item).collect(),
            other => vec![Part::from_item(other)],
        }
    }

    fn from_item(v: &'a Value) -> Part<'a> {
        if let Some(s) = v.as_str() {
            return Part::Text(Cow::Borrowed(s));
        }
        match v["image_url"]["url"]
            .as_str()
            .and_then(|u| u.strip_prefix(PNG_DATA_URL))
        {
            Some(png) => Part::Png(png),
            None => Part::Text(Cow::Owned(v.to_string())),
        }
    }
}

impl Serialize for Part<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(2))?;
        match self {
            Part::Text(t) => {
                m.serialize_entry("type", "text")?;
                m.serialize_entry("text", t)?;
            }
            Part::Png(b64) => {
                m.serialize_entry("type", "image_url")?;
                m.serialize_entry("image_url", &ImageUrl { url: DataUrl(b64) })?;
            }
        }
        m.end()
    }
}

/// The text of a chat reply, with the build and generation id when the endpoint names them.
#[derive(Debug)]
pub(crate) struct Reply {
    pub text: String,
    pub build: Option<String>,
    pub id: Option<String>,
    pub usage: Usage,
}

/// Sends `parts` as one user message to `model` and returns its text; a reply without text is an error.
pub(crate) async fn complete(
    m: &Models,
    model: &str,
    parts: &[Part<'_>],
    max_tokens: u32,
    timeout: Duration,
) -> Result<Reply, String> {
    let body = Body {
        model,
        max_tokens,
        messages: [Message {
            role: "user",
            content: parts,
        }],
    };
    let c: Completion = m.post(&m.chat_url, model, &body, timeout).await?;
    let text = c
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.message.content)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| format!("{model}: the reply holds no message text"))?;
    Ok(Reply {
        text,
        build: c.model,
        id: c.id,
        usage: Usage::of(model, &c.usage),
    })
}

#[derive(Serialize)]
struct Body<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: [Message<'a>; 1],
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a [Part<'a>],
}

#[derive(Serialize)]
struct ImageUrl<'a> {
    url: DataUrl<'a>,
}

struct DataUrl<'a>(&'a str);

impl Serialize for DataUrl<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(&format_args!("{PNG_DATA_URL}{}", self.0))
    }
}

#[derive(Deserialize)]
struct Completion {
    id: Option<String>,
    model: Option<String>,
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Value,
}

#[derive(Deserialize)]
struct Choice {
    message: Said,
}

#[derive(Deserialize)]
struct Said {
    content: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_state_becomes_text_and_png_parts() {
        let state = json!(["the list", {"type": "image_url", "image_url": {"url": "data:image/png;base64,QUJD"}}, {"k": 1}]);
        assert_eq!(
            Part::from_state(&state),
            [
                Part::Text("the list".into()),
                Part::Png("QUJD"),
                Part::Text("{\"k\":1}".into())
            ]
        );
        let window = json!({"window_title": "x"});
        assert_eq!(
            Part::from_state(&window),
            [Part::Text("{\"window_title\":\"x\"}".into())]
        );
    }

    #[test]
    fn a_png_part_serializes_as_a_data_url() {
        assert_eq!(
            serde_json::to_string(&Part::Png("QUJD")).unwrap(),
            r#"{"type":"image_url","image_url":{"url":"data:image/png;base64,QUJD"}}"#
        );
    }
}
