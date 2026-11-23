//! Inert end-to-end tests for the Sign in with ChatGPT connector.
//!
//! A local axum fixture plays OpenAI's issuer (discovery, JWKS, token,
//! revocation) and API (`/v1/models`, `/v1/responses` SSE), all on
//! 127.0.0.1. A scripted "browser" follows the one-time start redirect and
//! calls the loopback callback. Nothing here contacts OpenAI, runs a model
//! CLI, or reads any real credentials. The signing key is generated per run.

mod common;

use std::time::Duration;

use common::*;
use metro_connector::openai_oauth as oa;
use serde_json::{Value, json};

/// A stored profile file as text on Unix (mode 0600 is its protection). On
/// Windows it must be DPAPI-sealed, with no token or client ID readable in
/// it; its contents aren't returned.
fn stored(p: &std::path::Path) -> Option<String> {
    let bytes = std::fs::read(p).unwrap();
    if cfg!(windows) {
        assert!(
            bytes.starts_with(b"metro-dpapi-v1\n"),
            "{} is not sealed",
            p.display()
        );
        let text = String::from_utf8_lossy(&bytes);
        for plain in [
            "rt-",
            "access_token",
            "refresh_token",
            ISSUED,
            "urn:uuid:",
            "person@example.test",
        ] {
            assert!(
                !text.contains(plain),
                "{} holds {plain} in plain text",
                p.display()
            );
        }
        None
    } else {
        Some(String::from_utf8(bytes).unwrap())
    }
}

// -------------------------------------------------------------------- tests

#[tokio::test]
async fn sign_in_list_models_tool_round_and_sign_out() {
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;
    oa::login_with(&f.profile, opts(&f)).await.expect("login");

    // Authorize request: first-time dynamic registration.
    let q = f.s.lock().unwrap().authorize[0].clone();
    assert_eq!(q["client_id"], "dynamic_agent_client");
    assert_eq!(q["agent_name_hint"], "Metro");
    assert!(q["ext_agent_host_id"].starts_with("urn:uuid:"));
    assert_eq!(q["response_type"], "code");
    assert_eq!(
        q["scope"],
        "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct"
    );
    assert_eq!(q["resource"], RESOURCE);
    assert_eq!(q["code_challenge_method"], "S256");
    assert!(q["state"].len() >= 43 && q["nonce"].len() >= 43);
    assert!(
        q["redirect_uri"].starts_with("http://127.0.0.1:")
            && q["redirect_uri"].ends_with("/auth/callback")
    );
    assert!(!q.contains_key("id_token_hint") && !q.contains_key("prompt"));

    // Storage: Unix 0700 dir and 0600 files; Windows DPAPI-sealed files.
    // Nothing secret in account.json either way.
    #[cfg(unix)]
    {
        assert_eq!(mode(&f.profile), 0o700);
        for file in ["host.json", "account.json", "tokens.json"] {
            assert_eq!(mode(&f.profile.join(file)), 0o600, "{file}");
        }
    }
    for file in ["host.json", "account.json", "tokens.json"] {
        stored(&f.profile.join(file));
    }
    if let Some(account) = stored(&f.profile.join("account.json")) {
        assert!(
            account.contains(ISSUED)
                && !account.contains("dynamic_agent_client")
                && !account.contains("rt-0")
        );
    }

    let st = oa::status(&f.profile).await.unwrap();
    assert!(st.signed_in && st.registered && st.plan_usage_enabled);
    assert_eq!(st.subject.as_deref(), Some("user-1"));
    let host_id = st.ext_agent_host_id.clone().unwrap();
    assert!(!format!("{st:?}").contains("rt-0"));

    // Only models the account lists for display, in server order.
    assert_eq!(
        oa::models_with(&f.profile, &f.ep).await.unwrap(),
        vec!["gpt-fixture-a", "gpt-fixture-b"]
    );

    // Round 1: the model calls two tools.
    let r1 = oa::complete_with(&f.profile, &f.ep, tool_request())
        .await
        .unwrap();
    assert_eq!(r1["object"], "chat.completion");
    assert_eq!(r1["choices"][0]["finish_reason"], "tool_calls");
    let calls = r1["choices"][0]["message"]["tool_calls"]
        .as_array()
        .unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["id"], "call_A");
    assert_eq!(calls[0]["function"]["name"], "workspace_list");
    assert_eq!(calls[0]["function"]["arguments"], "{\"path\":\".\"}");
    assert_eq!(calls[1]["id"], "call_B");
    assert_eq!(r1["usage"]["total_tokens"], 15);

    let sent = f.s.lock().unwrap().responses_bodies[0].clone();
    assert_eq!(sent["store"], false);
    assert_eq!(sent["stream"], true);
    assert_eq!(
        sent["input"][0]["role"], "developer",
        "system becomes developer"
    );
    assert_eq!(sent["tools"][0]["type"], "namespace");
    assert_eq!(sent["tools"][0]["name"], "metro");
    assert_eq!(sent["tools"][0]["tools"][0]["name"], "workspace_list");
    assert!(sent.get("temperature").is_none() && sent.get("max_output_tokens").is_none());

    // Round 2: history includes the calls and their results under the same IDs.
    let mut req = tool_request();
    let msgs = req["messages"].as_array_mut().unwrap();
    msgs.push(r1["choices"][0]["message"].clone());
    msgs.push(json!({"role": "tool", "tool_call_id": "call_A", "content": "README.md"}));
    msgs.push(json!({"role": "tool", "tool_call_id": "call_B", "content": [{"type": "text", "text": "# Metro"}]}));
    let r2 = oa::complete_with(&f.profile, &f.ep, req).await.unwrap();
    assert_eq!(r2["choices"][0]["message"]["content"], "It is sunny.");
    assert_eq!(r2["choices"][0]["finish_reason"], "stop");
    assert!(
        !r2.to_string().contains("SECRET-REASONING"),
        "reasoning never forwarded"
    );
    let sent = f.s.lock().unwrap().responses_bodies[1].clone();
    let input = sent["input"].as_array().unwrap();
    let fc: Vec<&Value> = input
        .iter()
        .filter(|i| i["type"] == "function_call")
        .collect();
    assert_eq!(fc[0]["call_id"], "call_A");
    assert_eq!(fc[0]["namespace"], "metro");
    assert_eq!(fc[0]["name"], "workspace_list");
    let out: Vec<&Value> = input
        .iter()
        .filter(|i| i["type"] == "function_call_output")
        .collect();
    assert_eq!(out[0]["call_id"], "call_A");
    assert_eq!(out[1]["output"], "# Metro");

    // Sign-out revokes the refresh token with the issued client ID.
    let o = oa::logout_outcome(&f.profile, &f.ep).await.unwrap();
    assert!(o.signed_out && o.remote_revoked);
    let rev = f.s.lock().unwrap().revoked[0].clone();
    assert_eq!(rev["token"], "rt-0");
    assert_eq!(rev["token_type_hint"], "refresh_token");
    assert_eq!(rev["client_id"], ISSUED);
    let st = oa::status(&f.profile).await.unwrap();
    assert!(!st.signed_in && st.registered);
    assert_eq!(
        st.ext_agent_host_id.as_deref(),
        Some(host_id.as_str()),
        "host id kept"
    );
    assert!(!f.profile.join("tokens.json").exists());
    let e = oa::complete_with(&f.profile, &f.ep, tool_request())
        .await
        .unwrap_err();
    assert!(safe(&e).contains("Sign in again"), "{e}");

    // Re-sign-in reuses the issued client and host. There's no name hint and
    // no id_token_hint after sign-out, but login_hint is sent.
    oa::login_with(&f.profile, opts(&f))
        .await
        .expect("re-login");
    let q2 = f.s.lock().unwrap().authorize[1].clone();
    assert_eq!(q2["client_id"], ISSUED);
    assert_eq!(q2["ext_agent_host_id"], host_id);
    assert!(!q2.contains_key("agent_name_hint"));
    assert!(!q2.contains_key("id_token_hint"));
    assert_eq!(q2["login_hint"], "person@example.test");
    assert_ne!(q2["state"], q["state"]);
    assert_ne!(q2["code_challenge"], q["code_challenge"]);

    // Reauthorizing while tokens are retained sends id_token_hint.
    oa::login_with(&f.profile, opts(&f)).await.expect("reauth");
    let q3 = f.s.lock().unwrap().authorize[2].clone();
    assert!(q3["id_token_hint"].split('.').count() == 3);
}

#[tokio::test]
async fn expiry_refresh_is_serialized_and_rotates() {
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;
    f.s.lock().unwrap().first_expires_in = 1;
    oa::login_with(&f.profile, opts(&f)).await.unwrap();

    // Eight concurrent callers against an expired token produce exactly one
    // refresh. The others reuse the rotated token after taking the lock.
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let (p, ep) = (f.profile.clone(), f.ep.clone());
        tasks.push(tokio::spawn(async move { oa::models_with(&p, &ep).await }));
    }
    for t in tasks {
        assert_eq!(t.await.unwrap().unwrap().len(), 2);
    }
    let fx = f.s.lock().unwrap();
    assert_eq!(fx.refresh_count, 1);
    assert_eq!(fx.refresh.as_deref(), Some("rt-1"));
    drop(fx);
    if let Some(tokens) = stored(&f.profile.join("tokens.json")) {
        assert!(tokens.contains("rt-1") && !tokens.contains("\"rt-0\""));
    }
}

#[tokio::test]
async fn unusable_refresh_clears_tokens_but_keeps_registration() {
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;
    f.s.lock().unwrap().first_expires_in = 1;
    oa::login_with(&f.profile, opts(&f)).await.unwrap();
    f.s.lock().unwrap().refresh_error = Some("invalid_grant");
    let e = oa::models_with(&f.profile, &f.ep).await.unwrap_err();
    assert!(safe(&e).contains("Sign in again"), "{e:#}");
    let st = oa::status(&f.profile).await.unwrap();
    assert!(!st.signed_in && st.registered);
}

#[tokio::test]
async fn transient_refresh_failure_keeps_tokens() {
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;
    f.s.lock().unwrap().first_expires_in = 1;
    oa::login_with(&f.profile, opts(&f)).await.unwrap();
    f.s.lock().unwrap().refresh_error = Some("temporarily_unavailable");
    let e = oa::models_with(&f.profile, &f.ep).await.unwrap_err();
    assert!(safe(&e).contains("temporarily unavailable"), "{e:#}");
    assert!(oa::status(&f.profile).await.unwrap().signed_in);
}

#[tokio::test]
async fn forged_state_is_ignored_then_real_callback_completes() {
    let f = fixture(Browser::ForgedStateFirst).await;
    oa::login_with(&f.profile, opts(&f))
        .await
        .expect("login despite forged callback");
    if let Some(account) = stored(&f.profile.join("account.json")) {
        assert!(!account.contains("oaiapp_evil"));
    }
}

#[tokio::test]
async fn declined_consent_stores_nothing() {
    let f = fixture(Browser::Deny).await;
    let e = oa::login_with(&f.profile, opts(&f)).await.unwrap_err();
    assert!(format!("{e:#}").contains("declined"), "{e:#}");
    assert_eq!(oa::sign_in_stage(&e), Some(oa::SignInStage::Response));
    assert!(safe(&e).contains("declined"), "{e:#}");
    assert!(!f.profile.join("tokens.json").exists());
    assert!(!f.profile.join("account.json").exists());
}

#[tokio::test]
async fn bad_nonce_is_rejected() {
    let f = fixture(Browser::BadNonce).await;
    let e = oa::login_with(&f.profile, opts(&f)).await.unwrap_err();
    assert!(format!("{e:#}").contains("nonce"), "{e:#}");
    assert_eq!(oa::sign_in_stage(&e), Some(oa::SignInStage::Exchange));
    // The person sees what to do, not the verifier's detail.
    assert!(
        !safe(&e).contains("nonce") && safe(&e).contains("Start the sign-in again"),
        "{e:#}"
    );
    assert!(!f.profile.join("tokens.json").exists());
}

#[tokio::test]
async fn registration_without_issued_client_id_is_incomplete() {
    let f = fixture(Browser::NoClientId).await;
    let e = oa::login_with(&f.profile, opts(&f)).await.unwrap_err();
    assert!(
        format!("{e:#}").contains("registration incomplete"),
        "{e:#}"
    );
}

#[tokio::test]
async fn missing_plan_scope_signs_in_but_never_calls_inference() {
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: "email offline_access openid profile",
    })
    .await;
    let e = oa::login_with(&f.profile, opts(&f)).await.unwrap_err();
    assert!(
        format!("{e:#}").contains("chatgpt.tokens.use.direct"),
        "{e:#}"
    );
    assert!(safe(&e).contains("plan usage wasn't granted"), "{e:#}");
    let st = oa::status(&f.profile).await.unwrap();
    assert!(st.signed_in && !st.plan_usage_enabled);
    assert!(
        oa::complete_with(&f.profile, &f.ep, tool_request())
            .await
            .is_err()
    );
    assert!(
        f.s.lock().unwrap().responses_bodies.is_empty(),
        "no inference without plan scope"
    );
    // The next sign-in asks for consent with the full scope set.
    f.s.lock().unwrap().browser = Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    };
    oa::login_with(&f.profile, opts(&f)).await.unwrap();
    let q = f.s.lock().unwrap().authorize[1].clone();
    assert_eq!(q["prompt"], "consent");
    assert!(q["scope"].contains("chatgpt.tokens.use.direct"));
    assert!(oa::status(&f.profile).await.unwrap().plan_usage_enabled);
}

#[tokio::test]
async fn different_account_cannot_overwrite_registration() {
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;
    oa::login_with(&f.profile, opts(&f)).await.unwrap();
    f.s.lock().unwrap().browser = Browser::Approve {
        sub: "user-2",
        scope: PLAN_SCOPES,
    };
    let e = oa::login_with(&f.profile, opts(&f)).await.unwrap_err();
    assert!(
        format!("{e:#}").contains("different ChatGPT account"),
        "{e:#}"
    );
    assert!(safe(&e).contains("different ChatGPT account"), "{e:#}");
    assert_eq!(
        oa::status(&f.profile).await.unwrap().subject.as_deref(),
        Some("user-1")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn profile_must_be_private_and_endpoints_bound() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;
    std::fs::create_dir(&f.profile).unwrap();
    std::fs::set_permissions(&f.profile, std::fs::Permissions::from_mode(0o755)).unwrap();
    let e = oa::login_with(&f.profile, opts(&f)).await.unwrap_err();
    assert!(format!("{e:#}").contains("mode 700"), "{e:#}");
    // Before any browser: no start link was ever handed out.
    assert_eq!(oa::sign_in_stage(&e), Some(oa::SignInStage::Storage));
    assert!(safe(&e).contains("so no browser was opened"), "{e:#}");
    assert!(f.s.lock().unwrap().authorize.is_empty());
    std::fs::set_permissions(&f.profile, std::fs::Permissions::from_mode(0o700)).unwrap();
    oa::login_with(&f.profile, opts(&f)).await.unwrap();
    // A signed-in fixture profile refuses production endpoints, and the other
    // way round.
    let e = oa::models(&f.profile).await.unwrap_err();
    assert!(safe(&e).contains("registration"), "{e}");
}

#[tokio::test]
async fn loopback_rejects_foreign_host_header() {
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    let mut o = opts(&f);
    o.timeout = Duration::from_secs(3);
    o.open_browser = Box::new(move |url: &str| {
        let _ = tx.send(url.to_owned());
        Ok(())
    });
    let login = tokio::spawn({
        let p = f.profile.clone();
        async move { oa::login_with(&p, o).await }
    });
    let start = rx.await.unwrap();
    let u = url::Url::parse(&start).unwrap();
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", u.port().unwrap()))
        .await
        .unwrap();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let req = format!(
        "GET {}?{} HTTP/1.1\r\nHost: evil.example:80\r\n\r\n",
        u.path(),
        u.query().unwrap()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = String::new();
    s.read_to_string(&mut buf).await.unwrap();
    assert!(buf.starts_with("HTTP/1.1 421"), "{buf}");
    let e = login.await.unwrap().unwrap_err();
    assert!(format!("{e:#}").contains("timed out"), "{e:#}");
    assert_eq!(oa::sign_in_stage(&e), Some(oa::SignInStage::Waiting));
}

#[test]
fn translation_is_strict() {
    for (field, v) in [
        ("temperature", json!(0.2)),
        ("max_tokens", json!(10)),
        ("stop", json!(["x"])),
        ("n", json!(2)),
        ("stream", json!(true)),
        ("user", json!("u")),
    ] {
        let mut r = tool_request();
        r[field] = v;
        let e = oa::chat_to_responses(&r).unwrap_err().to_string();
        assert!(e.contains(field), "{field}: {e}");
    }
    let mut r = tool_request();
    r["n"] = json!(1);
    r["temperature"] = Value::Null;
    assert!(oa::chat_to_responses(&r).is_ok());

    let mut r = tool_request();
    r["tools"] = json!([{"type": "web_search"}]);
    assert!(
        oa::chat_to_responses(&r)
            .unwrap_err()
            .to_string()
            .contains("web_search")
    );

    let mut r = tool_request();
    r["tool_choice"] = json!({"type": "function", "function": {"name": "workspace_list"}});
    assert!(oa::chat_to_responses(&r).is_err());

    let mut r = tool_request();
    r["messages"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role": "tool", "tool_call_id": "nope", "content": "x"}));
    assert!(
        oa::chat_to_responses(&r)
            .unwrap_err()
            .to_string()
            .contains("unknown call id")
    );

    let mut r = tool_request();
    r["messages"][1]["content"] = json!([{"type": "input_audio", "input_audio": {}}]);
    assert!(
        oa::chat_to_responses(&r)
            .unwrap_err()
            .to_string()
            .contains("audio")
    );

    // Images, json_schema, reasoning effort and custom tools translate.
    let r = json!({
        "model": "m",
        "reasoning_effort": "low",
        "response_format": {"type": "json_schema", "json_schema": {"name": "out", "schema": {"type": "object"}, "strict": true}},
        "tools": [{"type": "custom", "custom": {"name": "apply_patch", "description": "patch"}}],
        "messages": [
            {"role": "developer", "content": "d"},
            {"role": "user", "content": [{"type": "text", "text": "see"}, {"type": "image_url", "image_url": {"url": "data:image/png;base64,AA", "detail": "low"}}]},
            {"role": "assistant", "content": null, "reasoning_content": "hidden", "tool_calls": [{"id": "c1", "type": "custom", "custom": {"name": "apply_patch", "input": "*** patch"}}]},
            {"role": "tool", "tool_call_id": "c1", "content": "ok"},
        ],
    });
    let b = oa::chat_to_responses(&r).unwrap();
    assert_eq!(b["reasoning"]["effort"], "low");
    assert_eq!(b["text"]["format"]["type"], "json_schema");
    assert_eq!(b["text"]["format"]["name"], "out");
    assert_eq!(b["tools"][0]["tools"][0]["type"], "custom");
    assert_eq!(b["input"][1]["content"][1]["type"], "input_image");
    assert_eq!(b["input"][2]["type"], "custom_tool_call");
    assert_eq!(b["input"][3]["type"], "custom_tool_call_output");
    assert!(!b.to_string().contains("hidden"));

    // Normalizing back: custom calls, tools outside the namespace, status.
    let c = oa::responses_to_chat(&json!({"status": "completed", "output": [{"type": "custom_tool_call", "call_id": "c2", "namespace": "metro", "name": "apply_patch", "input": "x"}]})).unwrap();
    assert_eq!(
        c["choices"][0]["message"]["tool_calls"][0]["custom"]["name"],
        "apply_patch"
    );
    assert!(oa::responses_to_chat(&json!({"status": "completed", "output": [{"type": "function_call", "call_id": "c", "namespace": "other", "name": "x"}]})).is_err());
    assert!(oa::responses_to_chat(&json!({"status": "incomplete", "output": []})).is_err());
}

#[test]
fn eligibility_is_explicit() {
    let e = oa::eligibility();
    assert!(e.eligible.contains("open-source") && e.not_covered.contains("remotely hosted"));
    assert!(
        e.docs
            .iter()
            .all(|d| d.starts_with("https://developers.openai.com/siwc/"))
    );
}

/// What `run.rs` sends to Metro for an error, and what it logs locally.
fn metro_and_local(e: &anyhow::Error) -> (String, String) {
    (
        metro_connector::secret::metro_error_text(e),
        metro_connector::secret::log_text(e, &[]),
    )
}

#[tokio::test]
async fn metro_sees_only_fixed_error_categories() {
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;

    // Not signed in: sign-in category, before any network call.
    let e = oa::complete_with(&f.profile, &f.ep, tool_request())
        .await
        .unwrap_err();
    let (metro, _) = metro_and_local(&e);
    assert!(metro.contains("Sign in again on the device"), "{metro}");

    oa::login_with(&f.profile, opts(&f)).await.unwrap();

    // Unsupported option: fixed text. The request's own text never appears.
    let mut r = tool_request();
    r["temperature"] = json!(0.3);
    r["messages"][1]["content"] = json!("SENSITIVE-USER-TEXT");
    let e = oa::complete_with(&f.profile, &f.ep, r).await.unwrap_err();
    let (metro, local) = metro_and_local(&e);
    assert!(metro.contains("doesn't support"), "{metro}");
    let d = oa::local_failure(&e).unwrap();
    assert_eq!(
        (d.operation(), d.phase(), d.http_status(), d.code()),
        (
            oa::Operation::Inference,
            oa::Phase::Translation,
            None,
            "chatgpt_unsupported"
        )
    );
    assert_eq!(
        local,
        format!("{metro}: {}", d.summary()),
        "only the diagnostic is added locally"
    );

    use oa::Phase::{Http, Stream, Translation};
    // Mode, Metro text, then the local diagnostic: phase, status, provider
    // code, provider param, outcome token.
    type Want = (
        &'static str,
        &'static str,
        oa::Phase,
        Option<u16>,
        Option<&'static str>,
        Option<&'static str>,
        &'static str,
    );
    let cases: [Want; 16] = [
        (
            "limit",
            "usage limit reached",
            Stream,
            None,
            Some("subscription_sharing_usage_limit_exceeded"),
            None,
            "chatgpt_usage_limit",
        ),
        (
            "capability",
            "doesn't support",
            Http,
            Some(400),
            Some("subscription_sharing_unsupported_capability"),
            Some(oa::UNRECOGNIZED),
            "chatgpt_unsupported",
        ),
        (
            "reasoning",
            "rejected the reasoning effort",
            Http,
            Some(400),
            Some("unsupported_value"),
            Some("reasoning.effort"),
            "chatgpt_reasoning_effort",
        ),
        (
            "model_missing",
            "Refresh the model list",
            Http,
            Some(404),
            Some("model_not_found"),
            Some("model"),
            "chatgpt_model",
        ),
        (
            "http503",
            "direct plan routing isn't enabled",
            Http,
            Some(503),
            None,
            None,
            "chatgpt_service_or_routing_unavailable",
        ),
        (
            "http403",
            "isn't available for this connection",
            Http,
            Some(403),
            None,
            None,
            "chatgpt_plan_not_available",
        ),
        (
            "http401",
            "Sign in again on the device",
            Http,
            Some(401),
            None,
            None,
            "chatgpt_sign_in",
        ),
        (
            "truncated",
            "ended early or was interrupted",
            Stream,
            None,
            None,
            None,
            "chatgpt_interrupted",
        ),
        (
            "http400",
            "rejected the ChatGPT request",
            Http,
            Some(400),
            Some(oa::UNRECOGNIZED),
            None,
            "chatgpt_rejected",
        ),
        (
            "flat_error",
            "context limit",
            Stream,
            None,
            Some("context_length_exceeded"),
            Some("input"),
            "chatgpt_context_length",
        ),
        (
            "incomplete",
            "shorter answer",
            Stream,
            None,
            None,
            None,
            "chatgpt_incomplete",
        ),
        (
            "filtered",
            "content filter",
            Stream,
            None,
            None,
            None,
            "chatgpt_content_filter",
        ),
        (
            "bad_completion",
            "unexpected response",
            Stream,
            None,
            None,
            None,
            "chatgpt_unexpected_response",
        ),
        (
            "malformed",
            "unexpected response",
            Stream,
            None,
            None,
            None,
            "chatgpt_unexpected_response",
        ),
        (
            "non_sse",
            "unexpected response",
            Http,
            Some(200),
            None,
            None,
            "chatgpt_unexpected_response",
        ),
        (
            "hosted",
            "unexpected response",
            Translation,
            None,
            None,
            None,
            "chatgpt_unexpected_response",
        ),
    ];
    for (mode, want, phase, status, code, param, outcome) in cases {
        f.s.lock().unwrap().sse_mode = mode;
        let e = oa::complete_with(&f.profile, &f.ep, tool_request())
            .await
            .unwrap_err();
        let (metro, local) = metro_and_local(&e);
        assert!(metro.contains(want), "{mode}: {metro}");
        let d = oa::local_failure(&e).unwrap();
        assert_eq!(
            (
                d.operation(),
                d.phase(),
                d.http_status(),
                d.provider_code(),
                d.provider_param(),
                d.code()
            ),
            (
                oa::Operation::Inference,
                phase,
                status,
                code,
                param,
                outcome
            ),
            "{mode}"
        );
        assert_eq!(local, format!("{metro}: {}", d.summary()), "{mode}");
        let stored = serde_json::to_string(&d).unwrap();
        for leak in [
            "SENSITIVE",
            "https://",
            "example.test",
            "req_",
            "resp_",
            "weird_code",
            "\"",
        ] {
            assert!(!local.contains(leak), "{mode}: {leak:?} logged: {local}");
            if leak != "\"" {
                assert!(!stored.contains(leak), "{mode}: {leak:?} stored: {stored}");
            }
        }
        for leak in [
            "SENSITIVE",
            "https://",
            "example.test",
            "req_",
            "resp_",
            "weird_code",
            "HTTP",
            "{",
            "\"",
        ] {
            assert!(
                !metro.contains(leak),
                "{mode}: {leak:?} reached Metro: {metro}"
            );
        }
        assert!(metro.chars().count() <= metro_connector::secret::MAX_SAFE_ERROR);
    }

    // Model listing uses the same boundary.
    f.s.lock().unwrap().sse_mode = "";
    let mut o = f.ep.clone();
    o.api_base = format!("{}/missing", f.ep.issuer);
    let e = oa::models_with(&f.profile, &o).await.unwrap_err();
    assert!(!metro_and_local(&e).0.contains("missing"));
    let d = oa::local_failure(&e).unwrap();
    assert_eq!(
        (d.operation(), d.phase(), d.http_status()),
        // A profile refuses endpoints it didn't sign in with.
        (oa::Operation::Catalog, oa::Phase::Session, None)
    );
}

/// The relay body for each failure is exactly `{"ok":false,"error":<fixed
/// text>}`, as before this diagnostic existed. The one deliberate change is
/// an inference 503 without a documented code, which no longer claims the
/// device's network is at fault.
#[tokio::test]
async fn relay_error_bodies_are_unchanged_and_carry_no_diagnostic() {
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;
    oa::login_with(&f.profile, opts(&f)).await.unwrap();
    const REJECTED: &str = "OpenAI rejected the ChatGPT request. Try a new chat with default model settings; if it keeps failing, update Metro.";
    const UNEXPECTED: &str = "OpenAI returned an unexpected response. Try again; if it keeps failing, update Metro or select another ChatGPT model.";
    const INTERRUPTED: &str = "The OpenAI response stream ended early or was interrupted. Check this device's connection and try again.";
    const UNAVAILABLE: &str = "OpenAI couldn't be reached from the device or is temporarily unavailable. Try again later.";
    const PLAN: &str = "ChatGPT plan usage isn't available for this connection (not approved, or not allowed for the account, workspace or region). Check your ChatGPT account and workspace settings.";
    const SIGN_IN: &str = "The ChatGPT sign-in for this connection ended or was not accepted. Sign in again on the device.";
    const LIMIT: &str = "ChatGPT plan usage limit reached. Review it in ChatGPT Settings under Usage. Metro doesn't switch to another billing path.";
    const ROUTING: &str = "OpenAI's ChatGPT plan service is unavailable, or direct plan routing isn't enabled for this connection. Try again later; if it continues, check ChatGPT plan usage.";
    for (mode, text) in [
        ("http400", REJECTED),
        ("http401", SIGN_IN),
        ("http403", PLAN),
        ("limit", LIMIT),
        ("truncated", INTERRUPTED),
        ("malformed", UNEXPECTED),
        ("http503", ROUTING),
        ("drop", UNAVAILABLE),
    ] {
        f.s.lock().unwrap().sse_mode = mode;
        let e = oa::complete_with(&f.profile, &f.ep, tool_request())
            .await
            .unwrap_err();
        // What `run::handle_job` posts for an error.
        let body = json!({"ok": false, "error": metro_connector::secret::metro_error_text(&e)});
        assert_eq!(body, json!({"ok": false, "error": text}), "{mode}");
        let wire = body.to_string();
        for token in [
            "phase",
            "operation",
            "http_status",
            "unrecognized",
            "chatgpt_",
            "503",
            "400",
        ] {
            assert!(!wire.contains(token), "{mode}: {token:?} in {wire}");
        }
    }
}

/// Transport, unrecognized 400 and unrecognized stream failures, and an
/// uncoded 503, all give distinct local diagnostics.
#[tokio::test]
async fn transport_503_and_unrecognized_failures_are_distinct() {
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;
    oa::login_with(&f.profile, opts(&f)).await.unwrap();
    let mut seen = Vec::new();
    for mode in ["http400", "http503"] {
        f.s.lock().unwrap().sse_mode = mode;
        let e = oa::complete_with(&f.profile, &f.ep, tool_request())
            .await
            .unwrap_err();
        seen.push(oa::local_failure(&e).unwrap());
    }
    // An unknown mid-stream error code.
    f.s.lock().unwrap().sse_mode = "stream_unknown";
    let e = oa::complete_with(&f.profile, &f.ep, tool_request())
        .await
        .unwrap_err();
    seen.push(oa::local_failure(&e).unwrap());
    // Transport: the API stops answering after sign-in.
    f.s.lock().unwrap().sse_mode = "drop";
    let e = oa::complete_with(&f.profile, &f.ep, tool_request())
        .await
        .unwrap_err();
    seen.push(oa::local_failure(&e).unwrap());

    let shape: Vec<_> = seen
        .iter()
        .map(|d| (d.phase(), d.http_status(), d.provider_code(), d.code()))
        .collect();
    assert_eq!(
        shape,
        [
            (
                oa::Phase::Http,
                Some(400),
                Some(oa::UNRECOGNIZED),
                "chatgpt_rejected"
            ),
            (
                oa::Phase::Http,
                Some(503),
                None,
                "chatgpt_service_or_routing_unavailable"
            ),
            (
                oa::Phase::Stream,
                None,
                Some(oa::UNRECOGNIZED),
                "chatgpt_rejected"
            ),
            (oa::Phase::Transport, None, None, "chatgpt_unavailable"),
        ]
    );
    for d in &seen {
        assert!(!d.summary().contains("SENSITIVE"));
    }
}

/// The worker hands each failed ChatGPT job's diagnostic to the observer
/// exactly once, and never for a request the local policy refused.
#[tokio::test]
async fn worker_observer_sees_each_chatgpt_failure_once() {
    use metro_connector::metro::Job;
    use metro_connector::profile::{Kind, Profile, ProfileConfig};
    use metro_connector::provider::Provider;
    use metro_connector::run::{FailureObserver, handle_job, handle_job_observed};
    use std::sync::{Arc, Mutex};

    let dir = tempfile::tempdir().unwrap();
    let config = ProfileConfig {
        version: 1,
        kind: Kind::OpenAiOAuth,
        models: vec!["gpt-6.2-sol".into()],
        server_url: "http://127.0.0.1:9".into(),
        base_url: None,
        request_timeout_secs: 5,
    };
    let profile = Profile::open(dir.path(), "unsigned").unwrap();
    let provider = Provider {
        config: &config,
        profile: &profile,
    };
    let seen = Arc::new(Mutex::new(Vec::new()));
    let observer: FailureObserver = {
        let seen = Arc::clone(&seen);
        Arc::new(move |f| seen.lock().unwrap().push(f))
    };
    let chat =
        json!({"model": "gpt-6.2-sol", "messages": [{"role": "user", "content": "SENSITIVE-hi"}]});
    let mut unsupported = chat.clone();
    unsupported["temperature"] = json!(0.2);
    let mut refused = chat.clone();
    refused["model"] = json!("not-approved");
    let jobs = [
        ("complete", chat),
        ("complete", unsupported),
        ("models", json!({})),
        ("complete", refused),
    ];
    let mut bodies = Vec::new();
    for (i, (operation, request)) in jobs.into_iter().enumerate() {
        let job = Job {
            id: format!("j{i}"),
            operation: operation.into(),
            request,
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let observed = handle_job_observed(&provider, &job, deadline, Some(&observer)).await;
        let plain = handle_job(&provider, &job, deadline).await;
        assert_eq!(observed, plain, "the observer never changes the result");
        assert_eq!(observed["ok"], false);
        assert_eq!(observed.as_object().unwrap().len(), 2);
        bodies.push(observed.to_string());
    }
    let seen = seen.lock().unwrap();
    let shape: Vec<_> = seen
        .iter()
        .map(|d| (d.operation(), d.phase(), d.code()))
        .collect();
    assert_eq!(
        shape,
        [
            (
                oa::Operation::Inference,
                oa::Phase::Session,
                "chatgpt_sign_in"
            ),
            (
                oa::Operation::Inference,
                oa::Phase::Translation,
                "chatgpt_unsupported"
            ),
            (
                oa::Operation::Catalog,
                oa::Phase::Session,
                "chatgpt_sign_in"
            ),
        ]
    );
    for b in bodies {
        for token in ["phase", "chatgpt_", "SENSITIVE", "operation="] {
            assert!(!b.contains(token), "{token:?} in {b}");
        }
    }
}

#[tokio::test]
async fn first_rig_chat_through_relay_wire_and_streaming_oauth() {
    use axum::{Json, Router, extract::Path, http::StatusCode, routing::post};
    use metro_connector::metro::{Claim, MetroClient};
    use metro_connector::policy::{sanitize_complete_for_kind, sanitize_response};
    use metro_connector::profile::{Kind, Profile, ProfileConfig};
    use metro_connector::{provider::Provider, run::handle_job};
    use std::sync::{Arc, Mutex};

    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;
    oa::login_with(&f.profile, opts(&f)).await.unwrap();
    let available = oa::models_with(&f.profile, &f.ep).await.unwrap();
    let model = &available[0];
    // Captured Rig 0.47 device agent turn, after the relay removes
    // `stream`/`stream_options` and sets the selected model.
    let turns: Value = serde_json::from_str(include_str!(
        "../src/claude_code/fixtures/rig_0_47_agent_turns.json"
    ))
    .unwrap();
    let mut request = turns[0].clone();
    request["model"] = json!(model);
    request["reasoning_effort"] = json!("max");
    let mut first = request.clone();
    first["messages"].as_array_mut().unwrap().drain(1..3);
    let mut malformed = first.clone();
    malformed["reasoning_effort"] = json!("private@example.test");
    let jobs = Arc::new(Mutex::new([first, request, malformed].into_iter()));
    let result = Arc::new(Mutex::new(None));
    let relay = Router::new()
        .route(
            "/v1/device/claim",
            post(move || async move {
                let request = jobs.lock().unwrap().next().unwrap();
                Json(json!({"id": "first-chat", "operation": "complete", "request": request}))
            }),
        )
        .route(
            "/v1/device/jobs/{id}/result",
            post({
                let result = Arc::clone(&result);
                move |Path(id): Path<String>, Json(body): Json<Value>| async move {
                    *result.lock().unwrap() = Some((id, body));
                    StatusCode::NO_CONTENT
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, relay).await.unwrap() });
    let client = MetroClient::new(
        &format!("http://{addr}"),
        metro_connector::Secret::new("metro_pc_fixturetoken-no-reuse"),
        Duration::from_secs(5),
    )
    .unwrap();
    for mode in ["first_chat", "eof_completed"] {
        let Claim::Job(job) = client.claim().await.unwrap() else {
            panic!("expected the first chat claim");
        };
        assert_eq!(job.operation, "complete");
        let clean =
            sanitize_complete_for_kind(&job.request, &available, Kind::OpenAiOAuth).unwrap();
        f.s.lock().unwrap().sse_mode = mode;
        let raw = oa::complete_with(&f.profile, &f.ep, clean).await.unwrap();
        let response = sanitize_response(&raw, model).unwrap();
        assert_eq!(
            response["choices"][0]["message"]["content"],
            "Hello, world!"
        );
        assert_eq!(response["usage"]["total_tokens"], 15);
        let sent = f.s.lock().unwrap().responses_bodies.last().unwrap().clone();
        assert_eq!(sent["input"][0]["role"], "developer");
        if mode == "first_chat" {
            assert_eq!(sent["input"].as_array().unwrap().len(), 2);
            assert_eq!(sent["input"][1]["role"], "user");
        } else {
            assert_eq!(
                sent["input"][2],
                json!({"role": "assistant", "content": "earlier reply"})
            );
        }
        assert_eq!(sent["tools"][0]["tools"][0]["strict"], false);
        assert_eq!(sent["reasoning"]["effort"], "max");
        assert_eq!(sent["stream"], true);
        assert_eq!(sent["store"], false);
        assert!(sent.get("previous_response_id").is_none());
        let body = json!({"ok": true, "response": response});
        client
            .post_result(&job.id, &body, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(*result.lock().unwrap(), Some((job.id, body)));
    }
    // The production worker must refuse a malformed setting before it even
    // tries to load the vendor credentials or contact OpenAI.
    let Claim::Job(job) = client.claim().await.unwrap() else {
        panic!("expected the malformed chat claim");
    };
    let config = ProfileConfig {
        version: 1,
        kind: Kind::OpenAiOAuth,
        models: available,
        server_url: format!("http://{addr}"),
        base_url: None,
        request_timeout_secs: 5,
    };
    let profile = Profile::open(&f._dir.0, "unsigned").unwrap();
    let provider = Provider {
        config: &config,
        profile: &profile,
    };
    let error = handle_job(
        &provider,
        &job,
        std::time::Instant::now() + Duration::from_secs(5),
    )
    .await;
    assert_eq!(error["ok"], false);
    let text = error["error"].as_str().unwrap();
    assert!(text.contains("'reasoning_effort' must be"), "{text}");
    assert!(!text.contains("private@example.test"), "{text}");
    client
        .post_result(&job.id, &error, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(*result.lock().unwrap(), Some((job.id, error)));
}

#[tokio::test]
async fn refresh_scope_change_stops_waiting_calls_before_inference() {
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;
    f.s.lock().unwrap().first_expires_in = 1;
    oa::login_with(&f.profile, opts(&f)).await.unwrap();
    f.s.lock().unwrap().refresh_scope = Some("openid profile offline_access");
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let (p, ep) = (f.profile.clone(), f.ep.clone());
        tasks.push(tokio::spawn(async move {
            oa::complete_with(&p, &ep, tool_request()).await
        }));
    }
    for task in tasks {
        let e = task.await.unwrap().unwrap_err();
        assert!(safe(&e).contains("approve plan usage"), "{e}");
    }
    assert!(!oa::status(&f.profile).await.unwrap().plan_usage_enabled);
    let fx = f.s.lock().unwrap();
    assert_eq!(fx.refresh_count, 1);
    assert!(fx.responses_bodies.is_empty());
}
