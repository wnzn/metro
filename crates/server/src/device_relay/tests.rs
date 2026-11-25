//! Device relay tests with an inert mock device: no vendor sign-in, no CLI,
//! no network.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rig_core::completion::{CompletionRequest, ToolDefinition};
use rig_core::message::{AssistantContent, ToolName};
use serde_json::{Value, json};

use super::*;

#[derive(Default)]
struct MockDirectory {
    conns: Mutex<HashMap<String, DeviceConnection>>,
    /// (connection, user, space) grants a share store would answer yes to.
    shares: Mutex<HashSet<(String, String, Option<String>)>>,
}

impl MockDirectory {
    fn add(&self, id: &str, owner: &str, provider: &str) {
        self.conns.lock().unwrap().insert(
            id.into(),
            DeviceConnection {
                id: id.into(),
                owner_id: owner.into(),
                provider: provider.into(),
            },
        );
    }
    fn remove(&self, id: &str) {
        self.conns.lock().unwrap().remove(id);
    }
    fn share(&self, id: &str, user: &str, space: Option<&str>) {
        self.shares
            .lock()
            .unwrap()
            .insert((id.into(), user.into(), space.map(str::to_owned)));
    }
    fn unshare(&self, id: &str, user: &str, space: Option<&str>) {
        self.shares
            .lock()
            .unwrap()
            .remove(&(id.into(), user.into(), space.map(str::to_owned)));
    }
}

impl ConnectionDirectory for MockDirectory {
    fn device_connection(
        &self,
        _conn: &Connection,
        id: &str,
    ) -> anyhow::Result<Option<DeviceConnection>> {
        Ok(self.conns.lock().unwrap().get(id).cloned())
    }

    fn shared_use_allowed(
        &self,
        _conn: &Connection,
        connection: &DeviceConnection,
        user: &str,
        space: Option<&str>,
    ) -> anyhow::Result<bool> {
        Ok(self.shares.lock().unwrap().contains(&(
            connection.id.clone(),
            user.to_owned(),
            space.map(str::to_owned),
        )))
    }
}

fn limits() -> DeviceLimits {
    DeviceLimits {
        queue_capacity: 2,
        claim_timeout: Duration::from_secs(5),
        run_timeout: Duration::from_secs(5),
        offline_after: Duration::from_secs(30),
        max_request_bytes: 64 * 1024,
        max_response_bytes: 64 * 1024,
        // One asset of up to ~150 KiB of base64. A 100 KiB asset is ~137 KiB
        // of JSON: one per person, two per connection, three in all.
        max_media_request_bytes: 160 * 1024,
        max_pending_media_bytes: 450 * 1024,
        max_media_bytes_per_caller: 150 * 1024,
        max_media_bytes_per_connection: 280 * 1024,
        max_media_dimensions: 768,
    }
}

struct Fixture {
    relay: DeviceRelay,
    dir: Arc<MockDirectory>,
    db: Db,
}

fn fixture_with(limits: DeviceLimits) -> Fixture {
    let db = Db::open_in_memory().unwrap();
    init(&db.lock()).unwrap();
    let dir = Arc::new(MockDirectory::default());
    dir.add("c1", "alice", "device-openai-oauth");
    dir.add("c2", "bob", "device-claude-code");
    dir.add("srv", "alice", "openai");
    dir.add("so", "alice", "device-systemone");
    dir.add("emb", "alice", "device-embeddings");
    dir.add("gem", "alice", "device-embeddinggemma-2");
    Fixture {
        relay: DeviceRelay::new(limits, dir.clone()),
        dir,
        db,
    }
}

fn fixture() -> Fixture {
    fixture_with(limits())
}

impl Fixture {
    async fn device(&self, token: &str) -> Option<Device> {
        self.relay.authenticate(&self.db, token).await.unwrap()
    }

    /// Issue a token for `owner`'s `conn` and bring its device online.
    async fn paired(&self, owner: &str, conn: &str) -> (String, Device) {
        let token = self.relay.issue_token(&self.db, owner, conn).await.unwrap();
        let device = self.device(&token).await.expect("token authenticates");
        assert!(self.relay.claim(&device).is_none());
        (token, device)
    }

    /// Poll until a job shows up (the dispatcher runs concurrently).
    async fn claim(&self, device: &Device) -> ClaimedDeviceJob {
        for _ in 0..200 {
            if let Some(job) = self.relay.claim(device) {
                return job;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("no job arrived");
    }
}

fn completion(text: &str) -> Value {
    json!({
        "id": "chatcmpl-1", "object": "chat.completion", "created": 1, "model": "m1",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": text},
                     "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}
    })
}

fn tool_call_completion() -> Value {
    json!({
        "id": "chatcmpl-2", "object": "chat.completion", "created": 1, "model": "m1",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": null,
            "tool_calls": [{"id": "call_1", "type": "function",
                "function": {"name": "workspace_read", "arguments": "{\"path\":\"README.md\"}"}}]},
            "finish_reason": "tool_calls"}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}
    })
}

#[test]
fn tokens_are_random_prefixed_and_hashed() {
    let a = new_device_token().unwrap();
    let b = new_device_token().unwrap();
    assert_ne!(a, b);
    assert!(a.starts_with("metro_pc_"));
    assert_eq!(a.len(), "metro_pc_".len() + 64);
    assert!(well_formed(&a));
    assert!(!well_formed("metro_rt_abc"));
    assert!(!well_formed("metro_pc_"));
    assert!(!well_formed("metro_pc_ab cd"));
    assert_eq!(device_token_hash(&a).len(), 32);
    assert_ne!(device_token_hash(&a), device_token_hash(&b));
    assert!(is_device_provider("device-openai-compatible"));
    assert!(!is_device_provider("openai"));
}

#[tokio::test]
async fn only_the_owner_issues_and_only_for_device_connections() {
    let f = fixture();
    assert!(matches!(
        f.relay.issue_token(&f.db, "bob", "c1").await,
        Err(IssueError::NotFound)
    ));
    assert!(matches!(
        f.relay.issue_token(&f.db, "alice", "nope").await,
        Err(IssueError::NotFound)
    ));
    assert!(matches!(
        f.relay.issue_token(&f.db, "alice", "srv").await,
        Err(IssueError::NotDevice)
    ));
    assert!(matches!(
        f.relay.revoke_token(&f.db, "bob", "c1").await,
        Err(IssueError::NotFound)
    ));
    let token = f.relay.issue_token(&f.db, "alice", "c1").await.unwrap();
    // Only the digest is stored.
    let stored: Vec<Vec<u8>> = {
        let conn = f.db.lock();
        let mut st = conn
            .prepare("SELECT token_hash FROM device_credentials")
            .unwrap();
        st.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(stored, vec![device_token_hash(&token)]);
    let device = f.device(&token).await.unwrap();
    assert_eq!(device.connection, "c1");
    assert_eq!(device.owner, "alice");
    assert_eq!(
        f.relay.paired(&f.db, "alice", "c1").await.unwrap(),
        Some(true)
    );
    assert_eq!(f.relay.paired(&f.db, "bob", "c1").await.unwrap(), None);
}

#[tokio::test]
async fn rotation_and_revocation_invalidate_the_old_token() {
    let f = fixture();
    let old = f.relay.issue_token(&f.db, "alice", "c1").await.unwrap();
    let new = f.relay.issue_token(&f.db, "alice", "c1").await.unwrap();
    assert!(f.device(&old).await.is_none());
    let device = f.device(&new).await.unwrap();
    f.relay.revoke_token(&f.db, "alice", "c1").await.unwrap();
    assert!(f.device(&new).await.is_none());
    // A device resolved before revocation can't claim under it.
    assert!(f.relay.claim(&device).is_none());
    let again = f.relay.issue_token(&f.db, "alice", "c1").await.unwrap();
    let fresh = f.device(&again).await.unwrap();
    assert!(fresh.generation > device.generation);
}

#[tokio::test]
async fn deleted_or_reassigned_connection_stops_authenticating() {
    let f = fixture();
    let token = f.relay.issue_token(&f.db, "alice", "c1").await.unwrap();
    f.dir.add("c1", "mallory", "device-openai-oauth");
    assert!(f.device(&token).await.is_none());
    assert_eq!(
        f.relay
            .dispatch(&f.db, "mallory", "c1", Operation::Models, json!({}))
            .await,
        Err(DeviceError::NotPaired)
    );
    f.dir.add("c1", "alice", "device-openai-oauth");
    assert!(f.device(&token).await.is_some());
    f.dir.remove("c1");
    assert!(f.device(&token).await.is_none());
    assert_eq!(
        f.relay
            .dispatch(&f.db, "alice", "c1", Operation::Models, json!({}))
            .await,
        Err(DeviceError::NotFound)
    );
}

#[tokio::test]
async fn dispatch_needs_the_owner_a_token_and_an_online_device() {
    let f = fixture();
    let models = || {
        f.relay
            .dispatch(&f.db, "alice", "c1", Operation::Models, json!({}))
    };
    assert_eq!(models().await, Err(DeviceError::NotPaired));
    let token = f.relay.issue_token(&f.db, "alice", "c1").await.unwrap();
    assert_eq!(models().await, Err(DeviceError::Offline));
    // Another user never reaches alice's device, and nothing falls back.
    assert_eq!(
        f.relay
            .dispatch(&f.db, "bob", "c1", Operation::Models, json!({}))
            .await,
        Err(DeviceError::NotFound)
    );
    let device = f.device(&token).await.unwrap();
    assert!(f.relay.claim(&device).is_none());
    assert!(f.relay.status("c1").online);
    assert!(!f.relay.status("c2").online);
}

#[tokio::test]
async fn models_and_complete_round_trip_and_results_are_idempotent() {
    let f = fixture();
    let (_, device) = f.paired("alice", "c1").await;
    let relay = f.relay.clone();
    let db = f.db.clone();
    let waiter = tokio::spawn(async move { relay.models(&db, "alice", "c1").await });
    let job = f.claim(&device).await;
    assert_eq!(job.operation, Operation::Models);
    assert_eq!(job.request, json!({}));
    assert_eq!(
        f.relay
            .complete_job(&device, &job.id, Ok(json!({"models": ["m1", "m2"]}))),
        Ok(true)
    );
    assert_eq!(waiter.await.unwrap().unwrap(), vec!["m1", "m2"]);
    // A retry of the same result is acknowledged and changes nothing.
    assert_eq!(
        f.relay
            .complete_job(&device, &job.id, Ok(json!({"models": []}))),
        Ok(false)
    );
    assert!(f.relay.claim(&device).is_none(), "never re-delivered");

    let relay = f.relay.clone();
    let db = f.db.clone();
    let request = json!({"model": "m1", "messages": [{"role": "user", "content": "hi"}],
                         "stream": true, "stream_options": {"include_usage": true}});
    let waiter = tokio::spawn(async move { relay.complete(&db, "alice", "c1", request).await });
    let job = f.claim(&device).await;
    assert_eq!(job.operation, Operation::Complete);
    assert!(job.request.get("stream").is_none());
    assert!(job.request.get("stream_options").is_none());
    f.relay
        .complete_job(&device, &job.id, Ok(completion("hello")))
        .unwrap();
    assert_eq!(waiter.await.unwrap().unwrap(), completion("hello"));
}

#[tokio::test]
async fn foreign_devices_cannot_claim_or_answer() {
    let f = fixture();
    let (_, alice) = f.paired("alice", "c1").await;
    let (_, bob) = f.paired("bob", "c2").await;
    let relay = f.relay.clone();
    let db = f.db.clone();
    let waiter = tokio::spawn(async move { relay.models(&db, "alice", "c1").await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(f.relay.claim(&bob).is_none());
    let job = f.claim(&alice).await;
    assert_eq!(
        f.relay
            .complete_job(&bob, &job.id, Ok(json!({"models": ["evil"]}))),
        Err(ResultError::Unknown)
    );
    f.relay
        .complete_job(&alice, &job.id, Ok(json!({"models": ["m1"]})))
        .unwrap();
    assert_eq!(waiter.await.unwrap().unwrap(), vec!["m1"]);
    assert_eq!(
        f.relay
            .complete_job(&bob, &job.id, Ok(json!({"models": []}))),
        Err(ResultError::Unknown)
    );
    assert_eq!(
        f.relay.complete_job(&alice, "no-such-job", Ok(json!({}))),
        Err(ResultError::Unknown)
    );
}

#[tokio::test]
async fn unclaimed_jobs_cannot_be_answered() {
    let f = fixture();
    let (_, device) = f.paired("alice", "c1").await;
    let relay = f.relay.clone();
    let db = f.db.clone();
    let waiter = tokio::spawn(async move { relay.models(&db, "alice", "c1").await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let id = f.relay.lock().jobs.keys().next().cloned().unwrap();
    assert_eq!(
        f.relay
            .complete_job(&device, &id, Ok(json!({"models": []}))),
        Err(ResultError::NotClaimed)
    );
    waiter.abort();
}

#[tokio::test]
async fn rotation_fails_pending_jobs() {
    let f = fixture();
    let (_, device) = f.paired("alice", "c1").await;
    let relay = f.relay.clone();
    let db = f.db.clone();
    let waiter = tokio::spawn(async move { relay.models(&db, "alice", "c1").await });
    let job = f.claim(&device).await;
    let _new = f.relay.issue_token(&f.db, "alice", "c1").await.unwrap();
    assert_eq!(waiter.await.unwrap(), Err(DeviceError::CredentialChanged));
    assert_eq!(
        f.relay
            .complete_job(&device, &job.id, Ok(json!({"models": []}))),
        Err(ResultError::Expired)
    );
    // The new token's device must poll before it counts as online.
    assert_eq!(
        f.relay.models(&f.db, "alice", "c1").await,
        Err(DeviceError::Offline)
    );
}

#[tokio::test]
async fn connection_removed_fails_pending_jobs() {
    let f = fixture();
    let (token, device) = f.paired("alice", "c1").await;
    let relay = f.relay.clone();
    let db = f.db.clone();
    let waiter = tokio::spawn(async move { relay.models(&db, "alice", "c1").await });
    f.claim(&device).await;
    f.dir.remove("c1");
    f.relay.connection_removed(&f.db, "c1").await.unwrap();
    assert_eq!(waiter.await.unwrap(), Err(DeviceError::CredentialChanged));
    f.dir.add("c1", "alice", "device-openai-oauth");
    assert!(f.device(&token).await.is_none(), "the hash row is gone");
}

#[tokio::test]
async fn timeouts_are_clear_and_late_results_are_refused() {
    let f = fixture_with(DeviceLimits {
        claim_timeout: Duration::from_millis(50),
        run_timeout: Duration::from_millis(50),
        ..limits()
    });
    let (_, device) = f.paired("alice", "c1").await;
    assert_eq!(
        f.relay.models(&f.db, "alice", "c1").await,
        Err(DeviceError::NotClaimed)
    );
    assert!(f.relay.claim(&device).is_none(), "withdrawn");
    let relay = f.relay.clone();
    let db = f.db.clone();
    let waiter = tokio::spawn(async move { relay.models(&db, "alice", "c1").await });
    let job = f.claim(&device).await;
    assert_eq!(waiter.await.unwrap(), Err(DeviceError::TimedOut));
    assert_eq!(
        f.relay
            .complete_job(&device, &job.id, Ok(json!({"models": []}))),
        Err(ResultError::Expired)
    );
}

#[tokio::test]
async fn abandoned_jobs_are_withdrawn() {
    let f = fixture();
    let (_, device) = f.paired("alice", "c1").await;
    let relay = f.relay.clone();
    let db = f.db.clone();
    let waiter = tokio::spawn(async move { relay.models(&db, "alice", "c1").await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    waiter.abort();
    let _ = waiter.await;
    assert!(f.relay.claim(&device).is_none());
    assert!(f.relay.lock().jobs.is_empty());
}

#[tokio::test]
async fn queue_and_size_are_bounded() {
    let f = fixture();
    let (_, _device) = f.paired("alice", "c1").await;
    let mut waiters = Vec::new();
    for _ in 0..2 {
        let relay = f.relay.clone();
        let db = f.db.clone();
        waiters.push(tokio::spawn(async move {
            relay.models(&db, "alice", "c1").await
        }));
    }
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        f.relay.models(&f.db, "alice", "c1").await,
        Err(DeviceError::QueueFull)
    );
    let big = json!({"messages": [{"role": "user", "content": "x".repeat(70 * 1024)}]});
    assert_eq!(
        f.relay.complete(&f.db, "alice", "c1", big).await,
        Err(DeviceError::TooLarge)
    );
    for w in waiters {
        w.abort();
    }
}

#[test]
fn response_shapes_are_checked() {
    assert!(validate_response(Operation::Models, &json!({"models": ["a"]})).is_ok());
    assert!(validate_response(Operation::Models, &json!({"models": [1]})).is_err());
    assert!(validate_response(Operation::Models, &json!({"models": [""]})).is_err());
    assert!(validate_response(Operation::Models, &json!({})).is_err());
    assert!(validate_response(Operation::Complete, &completion("x")).is_ok());
    assert!(validate_response(Operation::Complete, &tool_call_completion()).is_ok());
    assert!(validate_response(Operation::Complete, &json!({"choices": []})).is_err());
    assert!(validate_response(Operation::Complete, &json!({"models": []})).is_err());
    assert_eq!(sanitize_error("bad\nthing"), "bad thing");
    assert_eq!(sanitize_error("  "), "the request failed");
    assert!(sanitize_error(&"é".repeat(5000)).len() <= MAX_DEVICE_ERROR + 3);
}

#[test]
fn device_errors_are_clear_and_contain_no_payload() {
    assert!(DeviceError::Offline.to_string().contains("offline"));
    assert!(DeviceError::NotPaired.to_string().contains("device token"));
    assert!(DeviceError::TimedOut.to_string().contains("in time"));
    // The device's own text never reaches a message: it can echo anything.
    let reported = DeviceError::Device("quota".into()).to_string();
    assert!(!reported.contains("quota"), "{reported}");
    assert!(reported.contains("reported an error"), "{reported}");
    let malformed = DeviceError::BadResponse("missing choices".into()).to_string();
    assert!(!malformed.contains("missing choices"), "{malformed}");
    assert!(malformed.contains("invalid response"), "{malformed}");
}

fn read_tool() -> ToolDefinition {
    ToolDefinition::new(
        ToolName::new("workspace_read").unwrap(),
        "Read a file",
        json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
    )
}

/// Serve one `complete` job with `response`, returning what the device saw.
async fn serve_one(f: &Fixture, device: &Device, response: Value) -> Value {
    let job = f.claim(device).await;
    assert_eq!(job.operation, Operation::Complete);
    f.relay.complete_job(device, &job.id, Ok(response)).unwrap();
    job.request
}

#[tokio::test]
async fn rig_model_relays_openai_chat_and_parses_tool_calls() {
    let f = fixture();
    let (_, device) = f.paired("alice", "c1").await;
    let model = f.relay.rig_model(f.db.clone(), "alice", "c1", "m1");
    let request = CompletionRequest::new("read the readme").tool(read_tool());
    let call = tokio::spawn(async move { model.call(request).await });
    let seen = serve_one(&f, &device, tool_call_completion()).await;
    // Standard Chat Completions JSON, bound to the selected model.
    assert_eq!(seen["model"], "m1");
    assert_eq!(seen["messages"][0]["role"], "user");
    assert_eq!(seen["tools"][0]["function"]["name"], "workspace_read");
    assert!(seen.get("stream").is_none());
    let response = call.await.unwrap().expect("rig parses the reply");
    let calls: Vec<_> = response
        .choice
        .iter()
        .filter_map(|c| match c {
            AssistantContent::ToolCall(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].function.name.as_str(), "workspace_read");
    assert_eq!(calls[0].function.arguments["path"], "README.md");
}

#[tokio::test]
async fn rig_model_streams_from_a_whole_reply() {
    let f = fixture();
    let (_, device) = f.paired("alice", "c1").await;
    let model = f.relay.rig_model(f.db.clone(), "alice", "c1", "m1");
    let text = tokio::spawn(async move {
        model
            .stream(CompletionRequest::new("hi"))
            .expect("opens")
            .finish()
            .await
    });
    let seen = serve_one(&f, &device, completion("hello there")).await;
    assert!(
        seen.get("stream").is_none(),
        "the device wire never streams"
    );
    let response = text.await.unwrap().expect("the stream decodes");
    let joined: String = response
        .choice
        .iter()
        .filter_map(|c| match c {
            AssistantContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(joined, "hello there");

    let model = f.relay.rig_model(f.db.clone(), "alice", "c1", "m1");
    let calls = tokio::spawn(async move {
        model
            .stream(CompletionRequest::new("read").tool(read_tool()))
            .expect("opens")
            .finish()
            .await
    });
    serve_one(&f, &device, tool_call_completion()).await;
    let response = calls.await.unwrap().expect("the stream decodes");
    assert!(response.choice.iter().any(|c| matches!(
        c,
        AssistantContent::ToolCall(t) if t.function.name.as_str() == "workspace_read"
            && t.function.arguments["path"] == "README.md"
    )));
}

#[tokio::test]
async fn rig_model_reports_device_failures() {
    let f = fixture();
    let model = f.relay.rig_model(f.db.clone(), "alice", "c1", "m1");
    let err = model.call(CompletionRequest::new("hi")).await.unwrap_err();
    assert!(err.to_string().contains("isn't paired"), "{err}");
    let (_, device) = f.paired("alice", "c1").await;
    let model = f.relay.rig_model(f.db.clone(), "alice", "c1", "m1");
    let call = tokio::spawn(async move { model.call(CompletionRequest::new("hi")).await });
    let job = f.claim(&device).await;
    f.relay
        .complete_job(
            &device,
            &job.id,
            Err(DeviceError::Device("rate limited".into())),
        )
        .unwrap();
    let err = call.await.unwrap().unwrap_err();
    assert!(err.to_string().contains("reported an error"), "{err}");
    assert!(!err.to_string().contains("rate limited"), "{err}");
    // Bob's adapter never reaches alice's device.
    let model = f.relay.rig_model(f.db.clone(), "bob", "c1", "m1");
    let err = model.call(CompletionRequest::new("hi")).await.unwrap_err();
    assert!(err.to_string().contains("no such device"), "{err}");
}

// ---------------------------------------------------------------------------
// Typed operations and borrowed use.

fn decide_request() -> Value {
    json!({
        "model": "jev-latest",
        "state": "I was charged twice.",
        "questions": {
            "tone": {"type": "choice", "instructions": "Tone?", "criteria": {"angry": null, "calm": null}}
        }
    })
}

fn decide_answer() -> Value {
    json!({
        "model": "jev-latest",
        "answers": {
            "tone": {"type": "choice", "choice": "calm", "confidence": 0.4,
                     "probabilities": {"angry": 0.3, "calm": 0.7}}
        },
        "usage": {"input_tokens": 10, "output_tokens": 0}
    })
}

impl Fixture {
    /// Poll with the grant-checking claim until a job shows up.
    async fn claim_checked(&self, device: &Device) -> ClaimedDeviceJob {
        for _ in 0..200 {
            if let Some(job) = self.relay.claim_checked(&self.db, device).await {
                return job;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("no job arrived");
    }
}

#[test]
fn each_device_provider_serves_one_capability() {
    assert_eq!(DEVICE_PROVIDERS.len(), 6);
    for p in [
        "device-openai-oauth",
        "device-claude-code",
        "device-openai-compatible",
    ] {
        assert!(serves(p, Operation::Complete) && serves(p, Operation::Models));
        assert!(!serves(p, Operation::Decide) && !serves(p, Operation::Embed));
    }
    assert!(serves("device-systemone", Operation::Decide));
    assert!(!serves("device-systemone", Operation::Complete));
    assert!(!serves("device-systemone", Operation::Embed));
    assert!(serves("device-embeddings", Operation::Embed));
    assert!(!serves("device-embeddings", Operation::Complete));
    assert!(!serves("device-embeddings", Operation::Decide));
    assert!(serves("device-embeddinggemma-2", Operation::Embed));
    assert!(serves("device-embeddinggemma-2", Operation::MediaEmbed));
    assert!(serves("device-embeddinggemma-2", Operation::Models));
    assert!(!serves("device-embeddinggemma-2", Operation::Complete));
    assert!(!serves("device-embeddinggemma-2", Operation::Decide));
    for p in [
        "device-openai-oauth",
        "device-claude-code",
        "device-openai-compatible",
        "device-systemone",
        "device-embeddings",
        "embeddinggemma-2",
    ] {
        assert!(!serves(p, Operation::MediaEmbed), "{p}");
    }
    assert_eq!(
        serde_json::to_value(Operation::MediaEmbed).unwrap(),
        "media_embed"
    );
    assert!(!serves("openai", Operation::Models));
    assert_eq!(serde_json::to_value(Operation::Decide).unwrap(), "decide");
    assert_eq!(serde_json::to_value(Operation::Embed).unwrap(), "embed");
}

#[tokio::test]
async fn decide_round_trip_is_checked_both_ways() {
    let f = fixture();
    let (_, device) = f.paired("alice", "so").await;
    let alice = Caller::personal("alice");

    // A malformed request is refused before anything is queued.
    let mut bad = decide_request();
    bad["questions"] = json!({"where": {"type": "box", "instructions": "find"}});
    let mut two = decide_request();
    two["questions"]["billing"] = json!({"type": "noul", "instructions": "Billing?"});
    assert!(matches!(
        f.relay.decide(&f.db, &alice, "so", two).await,
        Err(DeviceError::InvalidRequest(_))
    ));
    assert!(matches!(
        f.relay.decide(&f.db, &alice, "so", bad).await,
        Err(DeviceError::InvalidRequest(_))
    ));
    assert!(f.relay.claim(&device).is_none());

    let relay = f.relay.clone();
    let db = f.db.clone();
    let who = alice.clone();
    let waiter = tokio::spawn(async move { relay.decide(&db, &who, "so", decide_request()).await });
    let job = f.claim(&device).await;
    assert_eq!(job.operation, Operation::Decide);
    assert_eq!(job.request["questions"]["tone"]["type"], "choice");
    let mut noisy = decide_answer();
    noisy["debug"] = json!({"secret": "x"});
    f.relay.complete_job(&device, &job.id, Ok(noisy)).unwrap();
    let out = waiter.await.unwrap().unwrap();
    assert_eq!(out, decide_answer());

    // Answers that don't fit the questions are an error, not a guess.
    let relay = f.relay.clone();
    let db = f.db.clone();
    let who = alice.clone();
    let waiter = tokio::spawn(async move { relay.decide(&db, &who, "so", decide_request()).await });
    let job = f.claim(&device).await;
    let mut wrong = decide_answer();
    wrong["answers"]["tone"]["choice"] = json!("excited");
    f.relay.complete_job(&device, &job.id, Ok(wrong)).unwrap();
    assert!(matches!(
        waiter.await.unwrap(),
        Err(DeviceError::BadResponse(_))
    ));

    // The wrong operation for the connection looks like no connection.
    assert_eq!(
        f.relay
            .complete(&f.db, "alice", "so", json!({"model": "m", "messages": []}))
            .await,
        Err(DeviceError::NotFound)
    );
    assert_eq!(
        f.relay
            .embed(&f.db, &alice, "so", json!({"model": "m", "input": ["x"]}))
            .await,
        Err(DeviceError::NotFound)
    );
    let (_, chat) = f.paired("alice", "c1").await;
    assert_eq!(
        f.relay.decide(&f.db, &alice, "c1", decide_request()).await,
        Err(DeviceError::NotFound)
    );
    assert!(f.relay.claim(&chat).is_none());
}

#[tokio::test]
async fn embed_round_trip_sorts_and_validates() {
    let f = fixture();
    let (_, device) = f.paired("alice", "emb").await;
    let relay = f.relay.clone();
    let db = f.db.clone();
    let waiter = tokio::spawn(async move {
        relay
            .embed(
                &db,
                &Caller::personal("alice"),
                "emb",
                json!({"model": "nomic", "input": ["a", "b"]}),
            )
            .await
    });
    let job = f.claim(&device).await;
    assert_eq!(job.operation, Operation::Embed);
    assert_eq!(job.request, json!({"model": "nomic", "input": ["a", "b"]}));
    f.relay
        .complete_job(
            &device,
            &job.id,
            Ok(json!({"data": [
                {"index": 1, "embedding": [0.0, 1.0]},
                {"index": 0, "embedding": [1.0, 0.0]}
            ]})),
        )
        .unwrap();
    let out = waiter.await.unwrap().unwrap();
    assert_eq!(out["data"][0]["embedding"], json!([1.0, 0.0]));
    assert_eq!(out["data"][1]["index"], 1);

    let relay = f.relay.clone();
    let db = f.db.clone();
    let waiter = tokio::spawn(async move {
        relay
            .embed(
                &db,
                &Caller::personal("alice"),
                "emb",
                json!({"model": "nomic", "input": ["a", "b"]}),
            )
            .await
    });
    let job = f.claim(&device).await;
    f.relay
        .complete_job(
            &device,
            &job.id,
            Ok(json!({"data": [{"index": 0, "embedding": [1.0]}, {"index": 0, "embedding": [1.0]}]})),
        )
        .unwrap();
    assert!(matches!(
        waiter.await.unwrap(),
        Err(DeviceError::BadResponse(_))
    ));
}

#[tokio::test]
async fn borrowing_needs_an_explicit_share_for_that_scope() {
    let f = fixture();
    let (_, device) = f.paired("alice", "so").await;
    let bob_s1 = Caller::in_space("bob", "s1");
    // No share: bob sees nothing, and nothing reaches alice's device.
    assert_eq!(
        f.relay.decide(&f.db, &bob_s1, "so", decide_request()).await,
        Err(DeviceError::NotFound)
    );
    f.dir.share("so", "bob", Some("s1"));
    // Another Space, or the personal Chat, is not covered by a Space share.
    for other in [Caller::in_space("bob", "s2"), Caller::personal("bob")] {
        assert_eq!(
            f.relay.decide(&f.db, &other, "so", decide_request()).await,
            Err(DeviceError::NotFound)
        );
    }
    // The owner-only entry points never borrow, share or not.
    assert_eq!(
        f.relay
            .dispatch(&f.db, "bob", "so", Operation::Models, json!({}))
            .await,
        Err(DeviceError::NotFound)
    );
    assert!(f.relay.claim(&device).is_none());

    let relay = f.relay.clone();
    let db = f.db.clone();
    let who = bob_s1.clone();
    let waiter = tokio::spawn(async move { relay.decide(&db, &who, "so", decide_request()).await });
    tokio::time::sleep(Duration::from_millis(30)).await;
    // The plain claim hands out only the owner's own jobs.
    assert!(f.relay.claim(&device).is_none());
    let job = f.claim_checked(&device).await;
    assert_eq!(job.operation, Operation::Decide);
    f.relay
        .complete_job(&device, &job.id, Ok(decide_answer()))
        .unwrap();
    assert_eq!(waiter.await.unwrap().unwrap(), decide_answer());

    // The subscription sign-in is never borrowed, even with a share.
    let (_, oauth) = f.paired("alice", "c1").await;
    f.dir.share("c1", "bob", Some("s1"));
    assert_eq!(
        f.relay
            .complete_as(
                &f.db,
                &bob_s1,
                "c1",
                json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]})
            )
            .await,
        Err(DeviceError::NotFound)
    );
    assert!(f.relay.claim_checked(&f.db, &oauth).await.is_none());
}

#[tokio::test]
async fn revoked_shares_stop_borrowed_work_at_every_step() {
    let f = fixture();
    let (_, device) = f.paired("alice", "so").await;
    let bob = Caller::in_space("bob", "s1");
    let spawn = |f: &Fixture| {
        let relay = f.relay.clone();
        let db = f.db.clone();
        let who = bob.clone();
        tokio::spawn(async move { relay.decide(&db, &who, "so", decide_request()).await })
    };

    // Revoked while queued: never handed to the device.
    f.dir.share("so", "bob", Some("s1"));
    let waiter = spawn(&f);
    tokio::time::sleep(Duration::from_millis(30)).await;
    f.dir.unshare("so", "bob", Some("s1"));
    assert!(f.relay.claim_checked(&f.db, &device).await.is_none());
    assert_eq!(waiter.await.unwrap(), Err(DeviceError::AccessRevoked));

    // Revoked while running, with `shares_changed`: cancelled at once, and
    // the device's late result is refused.
    f.dir.share("so", "bob", Some("s1"));
    let waiter = spawn(&f);
    let job = f.claim_checked(&device).await;
    f.dir.unshare("so", "bob", Some("s1"));
    f.relay.shares_changed(&f.db, Some("so")).await.unwrap();
    assert_eq!(waiter.await.unwrap(), Err(DeviceError::AccessRevoked));
    assert_eq!(
        f.relay.complete_job(&device, &job.id, Ok(decide_answer())),
        Err(ResultError::Expired)
    );

    // Revoked while running, without notice: the result is withheld.
    f.dir.share("so", "bob", Some("s1"));
    let waiter = spawn(&f);
    let job = f.claim_checked(&device).await;
    f.dir.unshare("so", "bob", Some("s1"));
    f.relay
        .complete_job(&device, &job.id, Ok(decide_answer()))
        .unwrap();
    assert_eq!(waiter.await.unwrap(), Err(DeviceError::AccessRevoked));

    // The owner's own work is untouched by share changes.
    let relay = f.relay.clone();
    let db = f.db.clone();
    let waiter = tokio::spawn(async move {
        relay
            .decide(&db, &Caller::personal("alice"), "so", decide_request())
            .await
    });
    let job = f.claim(&device).await;
    f.relay.shares_changed(&f.db, None).await.unwrap();
    f.relay
        .complete_job(&device, &job.id, Ok(decide_answer()))
        .unwrap();
    assert_eq!(waiter.await.unwrap().unwrap(), decide_answer());
}

#[tokio::test]
async fn borrowed_jobs_follow_the_owners_credential() {
    let f = fixture();
    let (_, device) = f.paired("alice", "so").await;
    f.dir.share("so", "bob", None);
    let relay = f.relay.clone();
    let db = f.db.clone();
    let waiter = tokio::spawn(async move {
        relay
            .decide(&db, &Caller::personal("bob"), "so", decide_request())
            .await
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    // Rotating alice's token fails the job; it never moves to a new device.
    let (_, rotated) = f.paired("alice", "so").await;
    assert_eq!(waiter.await.unwrap(), Err(DeviceError::CredentialChanged));
    assert!(f.relay.claim_checked(&f.db, &rotated).await.is_none());
    assert!(f.relay.claim_checked(&f.db, &device).await.is_none());
    // Bob can't manage alice's device token.
    assert!(matches!(
        f.relay.issue_token(&f.db, "bob", "so").await,
        Err(IssueError::NotFound)
    ));
    assert!(matches!(
        f.relay.revoke_token(&f.db, "bob", "so").await,
        Err(IssueError::NotFound)
    ));
}

#[test]
fn typed_response_shapes_are_checked_on_arrival() {
    assert!(validate_response(Operation::Decide, &json!({"answers": {}})).is_ok());
    assert!(validate_response(Operation::Decide, &json!({"answers": []})).is_err());
    assert!(validate_response(Operation::Embed, &json!({"data": []})).is_ok());
    assert!(validate_response(Operation::Embed, &json!({"vectors": []})).is_err());
    assert!(DeviceError::AccessRevoked.to_string().contains("revoked"));
}

// ---- Media embeddings ----

fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Not text: PNG signature, NUL, high bytes.
fn png_bytes(n: usize) -> Vec<u8> {
    let mut b = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0xff];
    b.extend((0..n).map(|i| (i % 256) as u8));
    b
}

fn media_asset(n: usize) -> Value {
    json!({"model": "google/embeddinggemma-2", "asset": {
        "name": "scan.png", "mime_type": "image/png", "data_base64": b64(&png_bytes(n))}})
}

fn media_answer() -> Value {
    json!({"model": "google/embeddinggemma-2", "partial": false, "data": [
        {"index": 1, "embedding": [0.0, 1.0], "locator": {"kind": "image"}},
        {"index": 0, "embedding": [1.0, 0.0], "locator": {"kind": "page", "page": 1},
         "excerpt": "caption"}
    ]})
}

#[tokio::test]
async fn media_round_trip_carries_bytes_and_checks_answers() {
    let f = fixture();
    let (_, device) = f.paired("alice", "gem").await;
    let alice = Caller::personal("alice");
    let relay = f.relay.clone();
    let db = f.db.clone();
    let who = alice.clone();
    // Larger than the text request bound: only media_embed may use it.
    let asset = media_asset(100 * 1024);
    let sent = asset.clone();
    let waiter = tokio::spawn(async move { relay.media_embed(&db, &who, "gem", sent).await });
    let job = f.claim(&device).await;
    assert_eq!(job.operation, Operation::MediaEmbed);
    assert_eq!(job.request, asset, "the bytes reach the device unchanged");
    let mut noisy = media_answer();
    noisy["debug"] = json!("dropped");
    f.relay.complete_job(&device, &job.id, Ok(noisy)).unwrap();
    let out = waiter.await.unwrap().unwrap();
    assert_eq!(
        out["data"][0]["locator"],
        json!({"kind": "page", "page": 1})
    );
    assert_eq!(out["data"][1]["locator"], json!({"kind": "image"}));
    assert!(out.get("debug").is_none());
    assert_eq!(f.relay.lock().media_bytes, 0, "released when done");

    // A query answers exactly one query vector.
    let relay = f.relay.clone();
    let db = f.db.clone();
    let who = alice.clone();
    let waiter = tokio::spawn(async move {
        relay
            .media_embed(
                &db,
                &who,
                "gem",
                json!({"model": "google/embeddinggemma-2", "query": "red bike"}),
            )
            .await
    });
    let job = f.claim(&device).await;
    f.relay
        .complete_job(&device, &job.id, Ok(media_answer()))
        .unwrap();
    assert!(matches!(
        waiter.await.unwrap(),
        Err(DeviceError::BadResponse(_))
    ));

    // Wider vectors than the relay's media bound are refused.
    let relay = f.relay.clone();
    let db = f.db.clone();
    let who = alice.clone();
    let waiter =
        tokio::spawn(async move { relay.media_embed(&db, &who, "gem", media_asset(10)).await });
    let job = f.claim(&device).await;
    let wide = json!({"partial": false, "data": [
        {"index": 0, "embedding": vec![0.5; 769], "locator": {"kind": "image"}}]});
    f.relay.complete_job(&device, &job.id, Ok(wide)).unwrap();
    assert!(matches!(
        waiter.await.unwrap(),
        Err(DeviceError::BadResponse(_))
    ));

    // Refused before anything is queued: URLs, paths, too large.
    for bad in [
        json!({"model": "m", "asset": {"name": "a.png", "mime_type": "image/png", "url": "https://x/a.png"}}),
        json!({"model": "m", "asset": {"name": "../a.png", "mime_type": "image/png", "data_base64": b64(b"x")}}),
        json!({"model": "m", "asset": {"name": "a.zip", "mime_type": "application/zip", "data_base64": b64(b"PK")}}),
    ] {
        assert!(matches!(
            f.relay.media_embed(&f.db, &alice, "gem", bad).await,
            Err(DeviceError::InvalidRequest(_))
        ));
    }
    assert_eq!(
        f.relay
            .media_embed(&f.db, &alice, "gem", media_asset(140 * 1024))
            .await,
        Err(DeviceError::TooLarge)
    );
    assert!(f.relay.claim(&device).is_none());
}

#[tokio::test]
async fn media_is_refused_on_text_connections_and_text_keeps_its_bound() {
    let f = fixture();
    let (_, emb) = f.paired("alice", "emb").await;
    let alice = Caller::personal("alice");
    // A text-only embeddings device never takes media.
    assert_eq!(
        f.relay
            .media_embed(&f.db, &alice, "emb", media_asset(10))
            .await,
        Err(DeviceError::NotFound)
    );
    assert!(f.relay.claim(&emb).is_none());
    // The media room is not open to other operations, on any connection.
    let (_, gem) = f.paired("alice", "gem").await;
    let big = json!({"model": "m", "input": ["x".repeat(30 * 1024), "y".repeat(30 * 1024), "z".repeat(10 * 1024)]});
    assert_eq!(
        f.relay
            .dispatch_as(&f.db, &alice, "gem", Operation::Embed, big)
            .await,
        Err(DeviceError::TooLarge)
    );
    assert!(f.relay.claim(&gem).is_none());
    // Text embeddings still run on the media device.
    let relay = f.relay.clone();
    let db = f.db.clone();
    let waiter = tokio::spawn(async move {
        relay
            .embed(
                &db,
                &Caller::personal("alice"),
                "gem",
                json!({"model": "google/embeddinggemma-2", "input": ["hi"]}),
            )
            .await
    });
    let job = f.claim(&gem).await;
    assert_eq!(job.operation, Operation::Embed);
    f.relay
        .complete_job(
            &gem,
            &job.id,
            Ok(json!({"data": [{"index": 0, "embedding": [1.0, 0.0]}]})),
        )
        .unwrap();
    assert!(waiter.await.unwrap().is_ok());
}

/// Wait until `n` jobs are pending.
async fn pending(f: &Fixture, n: usize) {
    for _ in 0..400 {
        if f.relay.lock().jobs.len() == n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!(
        "expected {n} pending jobs, have {}",
        f.relay.lock().jobs.len()
    );
}

fn spawn_media(
    f: &Fixture,
    caller: Caller,
    conn: &'static str,
) -> tokio::task::JoinHandle<Result<Value, DeviceError>> {
    let (relay, db) = (f.relay.clone(), f.db.clone());
    tokio::spawn(async move {
        relay
            .media_embed(&db, &caller, conn, media_asset(100 * 1024))
            .await
    })
}

#[tokio::test]
async fn media_bytes_are_shared_fairly_between_people_and_devices() {
    let f = fixture();
    f.dir.add("gem2", "carol", "device-embeddinggemma-2");
    let (_, gem) = f.paired("alice", "gem").await;
    let (_, gem2) = f.paired("carol", "gem2").await;
    let alice = Caller::personal("alice");
    let bob = Caller::in_space("bob", "s1");
    let carol = Caller::personal("carol");
    f.dir.share("gem", "bob", Some("s1"));
    f.dir.share("gem", "carol", None);

    // Alice holds her share: her next asset waits for none, it is refused,
    // on any connection.
    let a1 = spawn_media(&f, alice.clone(), "gem");
    pending(&f, 1).await;
    assert_eq!(
        f.relay
            .media_embed(&f.db, &alice, "gem", media_asset(100 * 1024))
            .await,
        Err(DeviceError::QueueFull)
    );
    // Claimed jobs leave the queue (capacity 2 per device here) but hold
    // their bytes until they end, so the byte budgets are what's tested.
    let a1_job = f.claim(&gem).await;
    // Text embeddings take no media budget at all.
    let text = {
        let (relay, db, who) = (f.relay.clone(), f.db.clone(), alice.clone());
        tokio::spawn(async move {
            relay
                .embed(&db, &who, "gem", json!({"model": "m", "input": ["hi"]}))
                .await
        })
    };
    let t = f.claim(&gem).await;
    assert_eq!(t.operation, Operation::Embed);
    f.relay
        .complete_job(
            &gem,
            &t.id,
            Ok(json!({"data": [{"index": 0, "embedding": [1.0]}]})),
        )
        .unwrap();
    assert!(text.await.unwrap().is_ok());
    // Bob, alice's saturation notwithstanding, gets the device's second share.
    let b1 = spawn_media(&f, bob.clone(), "gem");
    pending(&f, 2).await;
    // The device is now at its byte cap (its queue still has room): carol
    // is refused there...
    assert_eq!(
        f.relay
            .media_embed(&f.db, &carol, "gem", media_asset(100 * 1024))
            .await,
        Err(DeviceError::QueueFull)
    );
    // ...but her own device still has room in the relay-wide budget.
    let c1 = spawn_media(&f, carol.clone(), "gem2");
    pending(&f, 3).await;
    let held = f.relay.lock().media_bytes;
    assert!(held > 400 * 1024 && held <= 450 * 1024, "{held}");
    // Someone else's device stays out of reach whatever the budgets say.
    assert_eq!(
        f.relay
            .media_embed(&f.db, &alice, "gem2", media_asset(10))
            .await,
        Err(DeviceError::NotFound)
    );

    // Answer everything; every byte is given back.
    f.relay
        .complete_job(&gem, &a1_job.id, Ok(media_answer()))
        .unwrap();
    let job = f.claim_checked(&gem).await;
    f.relay
        .complete_job(&gem, &job.id, Ok(media_answer()))
        .unwrap();
    let job = f.claim(&gem2).await;
    f.relay
        .complete_job(&gem2, &job.id, Ok(media_answer()))
        .unwrap();
    for w in [a1, b1, c1] {
        assert!(w.await.unwrap().is_ok());
    }
    let inner = f.relay.lock();
    assert_eq!(inner.media_bytes, 0);
    assert!(inner.media_by_caller.is_empty() && inner.media_by_connection.is_empty());
}

#[tokio::test]
async fn media_bytes_come_back_on_cancel_revoke_timeout_and_rotation() {
    let f = fixture_with(DeviceLimits {
        claim_timeout: Duration::from_millis(300),
        ..limits()
    });
    let (_, gem) = f.paired("alice", "gem").await;
    let empty = |f: &Fixture| {
        let i = f.relay.lock();
        i.media_bytes == 0 && i.media_by_caller.is_empty() && i.media_by_connection.is_empty()
    };
    // The caller goes away (its future is dropped).
    let w = spawn_media(&f, Caller::personal("alice"), "gem");
    pending(&f, 1).await;
    w.abort();
    let _ = w.await;
    pending(&f, 0).await;
    assert!(empty(&f));
    // Nobody claims it in time.
    let w = spawn_media(&f, Caller::personal("alice"), "gem");
    assert_eq!(w.await.unwrap(), Err(DeviceError::NotClaimed));
    assert!(empty(&f));
    // A borrowed job whose share is revoked.
    f.dir.share("gem", "bob", Some("s1"));
    let w = spawn_media(&f, Caller::in_space("bob", "s1"), "gem");
    pending(&f, 1).await;
    f.dir.unshare("gem", "bob", Some("s1"));
    f.relay.shares_changed(&f.db, Some("gem")).await.unwrap();
    assert_eq!(w.await.unwrap(), Err(DeviceError::AccessRevoked));
    assert!(empty(&f));
    // The device token is rotated while the job is claimed.
    let w = spawn_media(&f, Caller::personal("alice"), "gem");
    let _job = f.claim(&gem).await;
    f.relay.issue_token(&f.db, "alice", "gem").await.unwrap();
    assert_eq!(w.await.unwrap(), Err(DeviceError::CredentialChanged));
    assert!(empty(&f));
}

#[tokio::test]
async fn borrowed_media_follows_the_share_and_its_space() {
    let f = fixture();
    let (_, gem) = f.paired("alice", "gem").await;
    let bob_s1 = Caller::in_space("bob", "s1");
    let bob_s2 = Caller::in_space("bob", "s2");
    // No share: nothing reaches alice's device.
    assert_eq!(
        f.relay
            .media_embed(&f.db, &bob_s1, "gem", media_asset(10))
            .await,
        Err(DeviceError::NotFound)
    );
    f.dir.share("gem", "bob", Some("s1"));
    // Another Space is not covered.
    assert_eq!(
        f.relay
            .media_embed(&f.db, &bob_s2, "gem", media_asset(10))
            .await,
        Err(DeviceError::NotFound)
    );
    assert!(f.relay.claim_checked(&f.db, &gem).await.is_none());

    // Shared in s1: it runs, and the answer comes back.
    let relay = f.relay.clone();
    let db = f.db.clone();
    let who = bob_s1.clone();
    let waiter =
        tokio::spawn(async move { relay.media_embed(&db, &who, "gem", media_asset(10)).await });
    let job = f.claim_checked(&gem).await;
    f.relay
        .complete_job(&gem, &job.id, Ok(media_answer()))
        .unwrap();
    assert!(waiter.await.unwrap().is_ok());

    // Revoked while pending: never claimed.
    let relay = f.relay.clone();
    let db = f.db.clone();
    let who = bob_s1.clone();
    let waiter =
        tokio::spawn(async move { relay.media_embed(&db, &who, "gem", media_asset(10)).await });
    for _ in 0..200 {
        if !f.relay.lock().jobs.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    f.dir.unshare("gem", "bob", Some("s1"));
    assert!(f.relay.claim_checked(&f.db, &gem).await.is_none());
    assert_eq!(waiter.await.unwrap(), Err(DeviceError::AccessRevoked));

    // Revoked after the claim: the answer is withheld.
    f.dir.share("gem", "bob", Some("s1"));
    let relay = f.relay.clone();
    let db = f.db.clone();
    let who = bob_s1.clone();
    let waiter =
        tokio::spawn(async move { relay.media_embed(&db, &who, "gem", media_asset(10)).await });
    let job = f.claim_checked(&gem).await;
    f.dir.unshare("gem", "bob", Some("s1"));
    f.relay
        .complete_job(&gem, &job.id, Ok(media_answer()))
        .unwrap();
    assert_eq!(waiter.await.unwrap(), Err(DeviceError::AccessRevoked));
    assert_eq!(f.relay.lock().media_bytes, 0);
}

#[test]
fn media_responses_need_data_and_partial() {
    assert!(
        validate_response(
            Operation::MediaEmbed,
            &json!({"data": [], "partial": false})
        )
        .is_ok()
    );
    assert!(validate_response(Operation::MediaEmbed, &json!({"data": []})).is_err());
    assert!(
        validate_response(
            Operation::MediaEmbed,
            &json!({"vectors": [], "partial": true})
        )
        .is_err()
    );
}
