use nemo_relay_thin::{SessionScope, TurnScope, flush, managed_llm_call, managed_tool_call};

#[tokio::main]
async fn main() {
    let session = SessionScope::start("headless-session", None).expect("start Agent scope");
    let turn = TurnScope::start("headless-session").expect("start Function scope");

    managed_tool_call("headless-session", async { Ok::<_, &'static str>(()) })
        .await
        .expect("managed tool call");
    managed_llm_call("headless-session", async { true }, |result| *result).await;

    drop(turn);
    drop(session);
    flush();
}
