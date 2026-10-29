#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::{Map, Value, json};
    use shuttle_models::config::{Config, DecideApi, Roles};
    use shuttle_models::{AskArgs, DecideArgs, GroundArgs, Models};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[derive(Debug)]
    struct Seen {
        path: String,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl Seen {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        }

        fn json(&self) -> Value {
            serde_json::from_str(&self.body).unwrap()
        }
    }

    async fn fake(reply: Value) -> (String, Arc<Mutex<Vec<Seen>>>) {
        let text = reply.to_string();
        serve(move |_| {
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                text.len()
            )
        })
        .await
    }

    async fn serve(
        respond: impl Fn(&str) -> String + Send + 'static,
    ) -> (String, Arc<Mutex<Vec<Seen>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head, body) = loop {
                    let n = s.read(&mut chunk).await.unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..end]).into_owned();
                        let len = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        while buf.len() < end + 4 + len {
                            let n = s.read(&mut chunk).await.unwrap();
                            buf.extend_from_slice(&chunk[..n]);
                        }
                        break (head, buf[end + 4..end + 4 + len].to_vec());
                    }
                };
                let mut lines = head.lines();
                let path = lines.next().unwrap().split(' ').nth(1).unwrap().to_owned();
                let headers = lines
                    .filter_map(|l| {
                        l.split_once(':')
                            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
                    })
                    .collect();
                let res = respond(&path);
                log.lock().unwrap().push(Seen {
                    path,
                    headers,
                    body: String::from_utf8(body).unwrap(),
                });
                s.write_all(res.as_bytes()).await.unwrap();
            }
        });
        (base, seen)
    }

    fn gateway(base: &str) -> Config {
        Config {
            base_url: format!("{base}/v1"),
            decisions_url: format!("{base}/v1/decisions"),
            api_key: Some("gw-key".to_owned().into()),
            ..Config::default()
        }
    }

    fn questions(v: Value) -> Map<String, Value> {
        serde_json::from_value(v).unwrap()
    }

    #[tokio::test]
    async fn request_bodies_match_what_desk_mts_sends() {
        let reply = json!({
            "choices": [{"message": {"content": "{\"point_2d\": [500, 250]}"}}],
            "answers": {"pick": {"type": "choice", "choice": "r1", "confidence": 0.9, "probabilities": {"r1": 0.9, "none": 0.1}}}
        });
        let (base, seen) = fake(reply).await;
        let m = Models::new(gateway(&base));
        let state = json!(["the list", {"type": "image_url", "image_url": {"url": "data:image/png;base64,UE5H"}}]);
        m.decide(DecideArgs {
            state,
            questions: questions(
                json!({"pick": {"type": "choice", "instructions": "Which?", "criteria": {"r1": "a", "none": "b"}}}),
            ),
            image: true,
            timeout_ms: Some(5000),
        })
        .await
        .unwrap();
        m.ask(AskArgs {
            prompt: "What is it?".into(),
            images: vec!["UE5H".into()],
        })
        .await
        .unwrap();
        m.ground(GroundArgs {
            png_b64: "UE5H".into(),
            target: "OK button".into(),
            area: serde_json::from_value(json!({"width": 1280, "height": 800})).unwrap(),
        })
        .await
        .unwrap();
        let seen = seen.lock().unwrap();
        let calls: Vec<(&str, &str, Option<&str>)> = seen
            .iter()
            .map(|s| (s.path.as_str(), s.body.as_str(), s.header("authorization")))
            .collect();
        assert_eq!(
            calls,
            [
                (
                    "/v1/decisions",
                    r#"{"model":"typesafe/jev-1.14","state":["the list",{"type":"image_url","image_url":{"url":"data:image/png;base64,UE5H"}}],"questions":{"pick":{"type":"choice","instructions":"Which?","criteria":{"r1":"a","none":"b"}}}}"#,
                    Some("Bearer gw-key")
                ),
                (
                    "/v1/chat/completions",
                    r#"{"model":"anthropic/claude-sonnet-5","max_tokens":400,"messages":[{"role":"user","content":[{"type":"text","text":"What is it?"},{"type":"image_url","image_url":{"url":"data:image/png;base64,UE5H"}}]}]}"#,
                    Some("Bearer gw-key")
                ),
                (
                    "/v1/chat/completions",
                    r#"{"model":"qwen/qwen3-vl-235b-a22b-instruct","max_tokens":100,"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,UE5H"}},{"type":"text","text":"Locate the OK button in this screenshot. Answer with one JSON object and nothing else: {\"point_2d\": [x, y]}, the centre of the element in coordinates from 0 to 1000 across and down the image."}]}]}"#,
                    Some("Bearer gw-key")
                ),
            ]
        );
    }

    #[tokio::test]
    async fn decide_on_the_decisions_endpoint_asks_verify_for_an_image_and_keeps_the_build() {
        let reply = json!({"model": "jev-1.14-20261014", "id": "gen-7", "answers": {"on": {"type": "noul", "noul": 0.9}, "pick": {"type": "choice", "choice": "r2", "confidence": 0.7, "probabilities": {"r2": 0.7, "none": 0.3}}}, "usage": {"input_tokens": 284, "output_tokens": 21, "cost": 0.0000119}});
        let (base, seen) = fake(reply).await;
        let m = Models::new(Config {
            models: Roles {
                verify: Some("jev-verify".to_owned()),
                ..Roles::default()
            },
            ..gateway(&base)
        });
        let out = m
            .decide(DecideArgs {
                state: json!("a window"),
                questions: questions(
                    json!({"on": {"type": "noul", "instructions": "On?"}, "pick": {"type": "choice", "instructions": "Which?", "criteria": {"r2": "x", "none": "y"}}}),
                ),
                image: true,
                timeout_ms: None,
            })
            .await
            .unwrap();
        assert_eq!(out.answers["on"], json!({"probability": 0.9}));
        assert_eq!(
            out.answers["pick"],
            json!({"choice": "r2", "confidence": 0.7, "probabilities": {"r2": 0.7, "none": 0.3}})
        );
        assert_eq!(
            (
                out.served.model.as_str(),
                out.served.build.as_deref(),
                out.served.id.as_deref()
            ),
            ("jev-verify", Some("jev-1.14-20261014"), Some("gen-7"))
        );
        assert_eq!((out.usage.input_tokens, out.usage.output_tokens), (284, 21));
        assert_eq!(seen.lock().unwrap()[0].json()["model"], "jev-verify");
    }

    #[tokio::test]
    async fn with_a_decide_build_pattern_only_a_matching_build_answers() {
        let ask = || DecideArgs {
            state: json!("a window"),
            questions: questions(json!({"on": {"type": "noul", "instructions": "On?"}})),
            image: false,
            timeout_ms: None,
        };
        let answer = |model: Option<&str>| {
            let mut r = json!({"id": "gen-1", "answers": {"on": {"type": "noul", "noul": 0.9}}});
            if let Some(m) = model {
                r["model"] = m.into();
            }
            r
        };
        let pinned = |base: &str| Config {
            decide_build: Some("typesafe/jev-1.14-########".to_owned()),
            ..gateway(base)
        };
        let (base, _) = fake(answer(Some("typesafe/jev-1.14-20261014"))).await;
        let out = Models::new(pinned(&base)).decide(ask()).await.unwrap();
        assert_eq!(
            out.served.build.as_deref(),
            Some("typesafe/jev-1.14-20261014")
        );
        for served in [
            Some("typesafe/jev-1.13-20260917"),
            Some("typesafe/jev-1.14"),
            None,
        ] {
            let (base, _) = fake(answer(served)).await;
            let err = Models::new(pinned(&base)).decide(ask()).await.unwrap_err();
            assert!(
                err.contains(&format!("{:?}", served.unwrap_or_default()))
                    && err.contains("########"),
                "{err}"
            );
            assert!(Models::new(gateway(&base)).decide(ask()).await.is_ok());
        }
    }

    #[tokio::test]
    async fn decide_over_chat_asks_for_the_same_json_without_a_key() {
        let (base, seen) =
            fake(json!({"model": "local-vl", "choices": [{"message": {"content": "{\"on\": {\"noul\": 0.2}}"}}]}))
                .await;
        let m = Models::new(Config {
            base_url: format!("{base}/v1/"),
            api_key_env: "SHUTTLE_TEST_UNSET_KEY".to_owned(),
            decide_api: DecideApi::Chat,
            ..Config::default()
        });
        let state = json!(["the list", {"type": "image_url", "image_url": {"url": "data:image/png;base64,UE5H"}}]);
        let out = m
            .decide(DecideArgs {
                state,
                questions: questions(json!({"on": {"type": "noul", "instructions": "On?"}})),
                image: false,
                timeout_ms: None,
            })
            .await
            .unwrap();
        assert_eq!(out.answers["on"], json!({"probability": 0.2}));
        assert_eq!(out.served.build.as_deref(), Some("local-vl"));
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].path, "/v1/chat/completions");
        assert_eq!(seen[0].header("authorization"), None);
        let content = seen[0].json()["messages"][0]["content"].take();
        assert!(
            content[0]["text"].as_str().unwrap().contains("\"on\""),
            "{content}"
        );
        assert_eq!(content[3]["image_url"]["url"], "data:image/png;base64,UE5H");
    }

    #[tokio::test]
    async fn ground_maps_the_point_onto_the_desk_area_of_a_2x_screenshot_up_to_its_last_pixel() {
        let png_2560x1600 = "iVBORw0KGgoAAAANSUhEUgAACgAAAAZAAQAAAADg";
        for (said, want) in [("[500, 250]", (640, 200)), ("[1000, 1000]", (1279, 799))] {
            let content = format!("{{\"point_2d\": {said}}}");
            let (base, _) = fake(json!({"choices": [{"message": {"content": content}}]})).await;
            let out = Models::new(gateway(&base))
                .ground(GroundArgs {
                    png_b64: png_2560x1600.into(),
                    target: "OK button".into(),
                    area: serde_json::from_value(json!({"width": 1280, "height": 800})).unwrap(),
                })
                .await
                .unwrap();
            assert_eq!((out.x, out.y), want, "{said}");
        }
    }

    #[tokio::test]
    async fn an_error_status_names_the_endpoint_that_answered_it_after_a_redirect() {
        let (base, seen) = serve(|path| {
            if path == "/v1/chat/completions" {
                "HTTP/1.1 308 Permanent Redirect\r\nlocation: /moved?key=sk-1\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_owned()
            } else {
                "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 4\r\nconnection: close\r\n\r\noops".to_owned()
            }
        })
        .await;
        let err = Models::new(gateway(&base))
            .ask(AskArgs {
                prompt: "x".into(),
                images: vec![],
            })
            .await
            .unwrap_err();
        assert_eq!(
            err,
            format!(
                "anthropic/claude-sonnet-5 at {base}/moved returned 500 Internal Server Error: oops"
            )
        );
        assert_eq!(seen.lock().unwrap()[1].path, "/moved?key=sk-1");
    }

    #[tokio::test]
    async fn a_402_from_decide_ground_or_ask_says_the_model_credits_are_used_up() {
        for (body, why) in [
            (
                r#"{"error":"daily credit exhausted"}"#,
                r#": {"error":"daily credit exhausted"}"#,
            ),
            ("", ""),
        ] {
            let (base, seen) = serve(move |_| {
                format!(
                    "HTTP/1.1 402 Payment Required\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
            })
            .await;
            let m = Models::new(gateway(&base));
            let decided = m
                .decide(DecideArgs {
                    state: json!("x"),
                    questions: questions(json!({"on": {"type": "noul", "instructions": "On?"}})),
                    image: false,
                    timeout_ms: None,
                })
                .await
                .unwrap_err();
            let grounded = m
                .ground(GroundArgs {
                    png_b64: "UE5H".into(),
                    target: "OK button".into(),
                    area: serde_json::from_value(json!({"width": 1280, "height": 800})).unwrap(),
                })
                .await
                .unwrap_err();
            let asked = m
                .ask(AskArgs {
                    prompt: "x".into(),
                    images: vec![],
                })
                .await
                .unwrap_err();
            let spent = |model: &str, path: &str| {
                format!(
                    "model credits are used up: {model} at {base}{path} returned 402 Payment Required{why}; add credit or upgrade the plan, then try again"
                )
            };
            assert_eq!(decided, spent("typesafe/jev-1.14", "/v1/decisions"));
            assert_eq!(
                grounded,
                spent("qwen/qwen3-vl-235b-a22b-instruct", "/v1/chat/completions")
            );
            assert_eq!(
                asked,
                spent("anthropic/claude-sonnet-5", "/v1/chat/completions")
            );
            assert_eq!(seen.lock().unwrap().len(), 3);
        }
    }

    #[tokio::test]
    async fn a_failed_send_names_the_url_it_requested_not_where_a_redirect_led() {
        let (base, _) = serve(|_| {
            "HTTP/1.1 308 Permanent Redirect\r\nlocation: http://127.0.0.1:9/elsewhere\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_owned()
        })
        .await;
        let err = Models::new(gateway(&base))
            .ask(AskArgs {
                prompt: "x".into(),
                images: vec![],
            })
            .await
            .unwrap_err();
        assert!(
            err.starts_with(&format!(
                "anthropic/claude-sonnet-5 request to {base}/v1/chat/completions: "
            )),
            "{err}"
        );
    }

    #[tokio::test]
    async fn an_empty_or_partial_reply_is_an_error() {
        let (base, _) = fake(json!({"choices": []})).await;
        let err = Models::new(gateway(&base))
            .ask(AskArgs {
                prompt: "x".into(),
                images: vec![],
            })
            .await
            .unwrap_err();
        assert_eq!(
            err,
            "anthropic/claude-sonnet-5: the reply holds no message text"
        );
        let (base, _) = fake(json!({"answers": {}})).await;
        let args = DecideArgs {
            state: json!("x"),
            questions: questions(json!({"on": {"type": "noul", "instructions": "On?"}})),
            image: false,
            timeout_ms: None,
        };
        assert_eq!(
            Models::new(gateway(&base)).decide(args).await.unwrap_err(),
            "typesafe/jev-1.14: no answer for on"
        );
        let (base, _) = fake(json!(["not", "an", "object"])).await;
        let args = DecideArgs {
            state: json!("x"),
            questions: Map::new(),
            image: false,
            timeout_ms: None,
        };
        let err = Models::new(gateway(&base)).decide(args).await.unwrap_err();
        assert!(err.starts_with("typesafe/jev-1.14: invalid type"), "{err}");
    }
}
