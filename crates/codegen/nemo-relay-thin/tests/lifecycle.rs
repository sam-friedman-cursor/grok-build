use nemo_relay::api::subscriber::{deregister_subscriber, flush_subscribers, register_subscriber};
use nemo_relay_thin::{SessionScope, TurnScope, initialize, managed_llm_call, managed_tool_call};
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn emits_content_stripped_agent_turn_tool_and_llm_lifecycle() {
    assert!(initialize());
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&events);
    register_subscriber(
        "grok.nemo-relay.lifecycle-test",
        Arc::new(move |event| {
            if matches!(
                event.name(),
                "grok.session" | "grok.turn" | "grok.tool" | "grok.llm"
            ) {
                captured
                    .lock()
                    .expect("capture lock")
                    .push(event.to_json_value());
            }
        }),
    )
    .expect("test subscriber should register");

    let session = SessionScope::start("session-test", None).expect("session scope");
    let turn = TurnScope::start("session-test").expect("turn scope");
    let tool_result =
        managed_tool_call("session-test", async { Ok::<_, &'static str>(7_u8) }).await;
    let llm_result = managed_llm_call("session-test", async { Some(11_u8) }, Option::is_some).await;
    assert_eq!(tool_result, Ok(7));
    assert_eq!(llm_result, Some(11));
    drop(turn);
    drop(session);
    flush_subscribers().expect("subscriber flush");

    let events = events.lock().expect("capture lock");
    let names = events
        .iter()
        .map(|event| event["name"].as_str().expect("event name"))
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "grok.session",
            "grok.turn",
            "grok.tool",
            "grok.tool",
            "grok.llm",
            "grok.llm",
            "grok.turn",
            "grok.session",
        ]
    );
    for event in events.iter() {
        assert!(event.get("data").is_none_or(serde_json::Value::is_null));
        assert!(
            event
                .get("category_profile")
                .is_none_or(serde_json::Value::is_null)
        );
        assert!(event.get("metadata").is_none_or(serde_json::Value::is_null));
    }
    assert_eq!(events[1]["parent_uuid"], events[0]["uuid"]);
    assert_eq!(events[2]["parent_uuid"], events[1]["uuid"]);
    assert_eq!(events[4]["parent_uuid"], events[1]["uuid"]);

    deregister_subscriber("grok.nemo-relay.lifecycle-test")
        .expect("test subscriber should deregister");
}
