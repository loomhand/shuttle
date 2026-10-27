//! The model calls of the desk runtime over one OpenAI-compatible endpoint: chat completions and decisions, non-streaming, with the key kept here.

mod chat;
pub mod config;
mod decide;

use std::borrow::Cow;
use std::fmt::Write as _;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::chat::Part;
use crate::config::{Config, DecideApi, Secret};

const ASK_MAX_TOKENS: u32 = 400;
const GROUND_MAX_TOKENS: u32 = 100;
const GROUND_SCALE: f64 = 1000.0;

/// The models of one configuration, called through one HTTP client.
#[derive(Debug)]
pub struct Models {
    config: Config,
    key: Option<Secret>,
    chat_url: String,
    http: reqwest::Client,
}

impl Models {
    pub fn new(config: Config) -> Self {
        Self {
            key: config.key(),
            chat_url: format!("{}/chat/completions", config.base_url.trim_end_matches('/')),
            config,
            http: reqwest::Client::new(),
        }
    }

    /// `model_decide`: the questions about a state, from the `verify` model when the state holds an image, else `decide`.
    pub async fn decide(&self, args: DecideArgs) -> Result<Decided, String> {
        let roles = &self.config.models;
        let model = match &roles.verify {
            Some(verify) if args.image => verify,
            _ => &roles.decide,
        };
        let timeout = self.timeout(args.timeout_ms);
        let decided = match self.config.decide_api {
            DecideApi::Decisions => decide::decisions(self, model, &args, timeout).await,
            DecideApi::Chat => decide::over_chat(self, model, &args, timeout).await,
        }?;
        if let Some(pattern) = &self.config.decide_build {
            let build = decided.served.build.as_deref().unwrap_or_default();
            if !build_matches(build, pattern) {
                return Err(format!(
                    "{model} answered from build {build:?}, not one of {pattern}"
                ));
            }
        }
        Ok(decided)
    }

    /// `model_ask`: a vision model's answer about PNG images.
    pub async fn ask(&self, args: AskArgs) -> Result<Asked, String> {
        let mut parts = vec![Part::Text(Cow::Borrowed(&args.prompt))];
        parts.extend(args.images.iter().map(|png| Part::Png(png)));
        let model = &self.config.models.ask;
        let reply = chat::complete(self, model, &parts, ASK_MAX_TOKENS, self.timeout(None)).await?;
        Ok(Asked {
            text: reply.text,
            usage: reply.usage,
        })
    }

    /// `model_ground`: the point of a described element, in the desk coordinates of `area`, whatever the PNG's size.
    pub async fn ground(&self, args: GroundArgs) -> Result<Grounded, String> {
        let prompt = format!(
            "Locate the {} in this screenshot. Answer with one JSON object and nothing else: {{\"point_2d\": [x, y]}}, the centre of the element in coordinates from 0 to 1000 across and down the image.",
            args.target
        );
        let parts = [Part::Png(&args.png_b64), Part::Text(prompt.into())];
        let model = &self.config.models.ground;
        let reply =
            chat::complete(self, model, &parts, GROUND_MAX_TOKENS, self.timeout(None)).await?;
        let (x, y) = point(&reply.text, &args.target, &args.area)?;
        Ok(Grounded {
            x,
            y,
            usage: reply.usage,
        })
    }

    fn timeout(&self, cap_ms: Option<u64>) -> Duration {
        let ms = self.config.timeout_ms;
        Duration::from_millis(cap_ms.map_or(ms, |c| c.min(ms)))
    }

    async fn post<T: DeserializeOwned>(
        &self,
        url: &str,
        model: &str,
        body: &impl Serialize,
        timeout: Duration,
    ) -> Result<T, String> {
        let mut req = self.http.post(url).json(body).timeout(timeout);
        if let Some(k) = &self.key {
            req = req.bearer_auth(k.expose());
        }
        let res = req.send().await.map_err(|e| {
            let at = e
                .url()
                .map_or_else(|| model.to_owned(), |u| reached(model, u));
            failed(&at, e)
        })?;
        let at = reached(model, res.url());
        let status = res.status();
        let text = res.text().await.map_err(|e| failed(&at, e))?;
        if !status.is_success() {
            return Err(format!("{at} returned {status}: {}", snippet(&text, 300)));
        }
        serde_json::from_str(&text).map_err(|e| format!("{model}: {e}: {}", snippet(&text, 120)))
    }
}

/// The arguments of `model_decide`; `questions` maps keys to `{type: noul|choice, instructions, criteria?}`, and `timeout_ms` caps the configured timeout.
#[derive(Debug, Deserialize)]
pub struct DecideArgs {
    pub state: Value,
    pub questions: Map<String, Value>,
    #[serde(default)]
    pub image: bool,
    pub timeout_ms: Option<u64>,
}

/// The answer of `model_decide`.
#[derive(Debug, Serialize)]
pub struct Decided {
    pub answers: Map<String, Value>,
    pub served: Served,
    pub usage: Usage,
}

/// The model a judgment was asked of, and the build and generation id that answered when the endpoint names them.
#[derive(Debug, Serialize)]
pub struct Served {
    pub model: String,
    pub build: Option<String>,
    pub id: Option<String>,
}

/// The arguments of `model_ask`: a prompt and PNGs as base64.
#[derive(Debug, Deserialize)]
pub struct AskArgs {
    pub prompt: String,
    #[serde(default)]
    pub images: Vec<String>,
}

/// The answer of `model_ask`.
#[derive(Debug, Serialize)]
pub struct Asked {
    pub text: String,
    pub usage: Usage,
}

/// The arguments of `model_ground`: a PNG as base64, the target, and the desk rectangle the PNG shows.
#[derive(Debug, Deserialize)]
pub struct GroundArgs {
    pub png_b64: String,
    pub target: String,
    pub area: Area,
}

/// A rectangle in desk coordinates, the space `desk_click` and `desk_at` take.
#[derive(Debug, Deserialize)]
pub struct Area {
    #[serde(default)]
    pub left: f64,
    #[serde(default)]
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

/// The answer of `model_ground`, in desk coordinates.
#[derive(Debug, Serialize)]
pub struct Grounded {
    pub x: i64,
    pub y: i64,
    pub usage: Usage,
}

/// The tokens one call used.
#[derive(Debug, Serialize)]
pub struct Usage {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
}

impl Usage {
    /// The usage block of a reply, in the chat or the decisions field names.
    pub(crate) fn of(model: &str, u: &Value) -> Self {
        let n = |keys: &[&str]| keys.iter().find_map(|k| u[*k].as_u64()).unwrap_or(0);
        Self {
            model: model.to_owned(),
            input_tokens: n(&["prompt_tokens", "input_tokens"]),
            output_tokens: n(&["completion_tokens", "output_tokens"]),
            cached_tokens: u["prompt_tokens_details"]["cached_tokens"]
                .as_u64()
                .unwrap_or(0),
        }
    }
}

/// The first `x, y` pair of a grounding reply, 0 to 1000 across and down the image, mapped onto `area` and kept inside it.
fn point(text: &str, target: &str, area: &Area) -> Result<(i64, i64), String> {
    let spans = numbers(text);
    let (px, py) = spans
        .windows(2)
        .find(|w| text[w[0].1..w[1].0].trim() == ",")
        .map(|w| (w[0].2, w[1].2))
        .ok_or_else(|| {
            format!(
                "grounding found no point for {target}: {}",
                snippet(text, 120)
            )
        })?;
    let scale = 0.0..=GROUND_SCALE;
    if !scale.contains(&px) || !scale.contains(&py) {
        return Err(format!(
            "grounding put {target} at ({px}, {py}), outside 0 to 1000"
        ));
    }
    Ok((
        place(area.left, area.width, px),
        place(area.top, area.height, py),
    ))
}

fn place(start: f64, len: f64, at: f64) -> i64 {
    let last = (start + len).round() - 1.0;
    (start + at / GROUND_SCALE * len)
        .round()
        .min(last)
        .max(start.round()) as i64
}

fn numbers(text: &str) -> Vec<(usize, usize, f64)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        if b[i] == b'-' && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if !b[i].is_ascii_digit()
            || (start > 0 && (b[start - 1].is_ascii_alphanumeric() || b[start - 1] == b'_'))
        {
            i = start + 1;
            continue;
        }
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
        if let Ok(v) = text[start..i].parse() {
            out.push((start, i, v));
        }
    }
    out
}

fn snippet(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

fn reached(model: &str, url: &reqwest::Url) -> String {
    let mut u = url.clone();
    let _ = u.set_username("");
    let _ = u.set_password(None);
    u.set_query(None);
    u.set_fragment(None);
    format!("{model} at {u}")
}

// a reqwest error prints only its kind; its sources name the cause, and without_url keeps any key in a query out
fn failed(model: &str, e: reqwest::Error) -> String {
    let e = e.without_url();
    let mut out = format!("{model}: {e}");
    let mut source = std::error::Error::source(&e);
    while let Some(s) = source {
        let _ = write!(out, ": {s}");
        source = s.source();
    }
    out
}

/// Whether `build` fits `pattern`, where `#` stands for one ASCII digit and every other character for itself.
fn build_matches(build: &str, pattern: &str) -> bool {
    build.len() == pattern.len()
        && build.bytes().zip(pattern.bytes()).all(|(b, p)| {
            if p == b'#' {
                b.is_ascii_digit()
            } else {
                b == p
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_pattern_takes_any_digits_where_it_says_hash_and_nothing_else() {
        let p = "typesafe/jev-1.14-########";
        assert!(build_matches("typesafe/jev-1.14-20261014", p));
        assert!(build_matches("typesafe/jev-1.14-20270101", p));
        for other in [
            "typesafe/jev-1.14",
            "typesafe/jev-1.13-20260917",
            "typesafe/jev-preview",
            "typesafe/jev-1.14-2026",
            "typesafe/jev-1.141-20261014",
            "typesafe/jev-1.14-2026101",
            "typesafe/jev-1.14-202610140",
            "typesafe/jev-1.14-2026101x",
            "~typesafe/jev-latest",
            "",
        ] {
            assert!(!build_matches(other, p), "{other}");
        }
    }

    #[test]
    fn an_error_names_the_endpoint_without_its_credentials_query_or_fragment() {
        let url =
            reqwest::Url::parse("https://user:secret@gw.example/v1/chat/completions?key=sk-1#k")
                .unwrap();
        assert_eq!(
            reached("m", &url),
            "m at https://gw.example/v1/chat/completions"
        );
    }

    fn area(left: f64, top: f64, width: f64, height: f64) -> Area {
        Area {
            left,
            top,
            width,
            height,
        }
    }

    #[test]
    fn a_point_lands_in_desk_coordinates_whatever_the_screenshot_size() {
        let screen = area(0.0, 0.0, 1280.0, 800.0);
        assert_eq!(
            point("{\"point_2d\": [500, 250]}", "t", &screen),
            Ok((640, 200))
        );
        let crop = area(320.0, 200.0, 640.0, 400.0);
        assert_eq!(point("[250.5, 750]", "t", &crop), Ok((480, 500)));
        assert_eq!(
            point("point_2d 7: (300, 900)", "t", &screen),
            Ok((384, 720))
        );
        assert_eq!(
            point("no point", "OK", &screen),
            Err("grounding found no point for OK: no point".to_owned())
        );
    }

    #[test]
    fn the_1000_edge_is_the_last_pixel_and_beyond_1000_is_refused() {
        let screen = area(0.0, 0.0, 1280.0, 800.0);
        assert_eq!(point("[1000, 1000]", "t", &screen), Ok((1279, 799)));
        assert_eq!(point("[0, 0]", "t", &screen), Ok((0, 0)));
        let crop = area(320.0, 200.0, 640.0, 400.0);
        assert_eq!(point("[1000, 0]", "t", &crop), Ok((959, 200)));
        assert_eq!(
            point("[1001, 5]", "OK", &screen),
            Err("grounding put OK at (1001, 5), outside 0 to 1000".to_owned())
        );
        assert!(point("[-3, 5]", "OK", &screen).is_err());
    }
}
