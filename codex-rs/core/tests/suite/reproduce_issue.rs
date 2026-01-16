use std::time::Duration;

use codex_core::protocol::EventMsg;
use codex_core::protocol::Op;
use codex_protocol::items::TurnItem;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_reasoning_item_added;
use core_test_support::responses::ev_reasoning_text_delta;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_reasoning_records_history() {
    let reasoning_text = "I am thinking about the problem...";

    let server = start_mock_server().await;

    // Mount a mock that sends reasoning but keeps the stream open
    use wiremock::Mock;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path_regex;

    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(|_req: &wiremock::Request| {
            let body = sse(vec![
                ev_response_created("resp-reasoning"),
                ev_reasoning_item_added("reasoning-1", &[]),
                ev_reasoning_text_delta(reasoning_text),
            ]);

            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(body, "text/event-stream")
        })
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let fixture = test_codex()
        .with_model("gpt-5.1")
        .build(&server)
        .await
        .unwrap();
    let codex = fixture.codex;

    // Kick off a turn
    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "Think about it".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
        .unwrap();

    // Wait until we receive the reasoning delta (proving the stream started)
    wait_for_event(&codex, |ev| {
        matches!(ev, EventMsg::ReasoningRawContentDelta(_))
    })
    .await;

    // Give the core a moment to fully process the delta into active_item
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Interrupt immediately after
    codex.submit(Op::Interrupt).await.unwrap();

    // Consume events until TurnAborted, looking for ItemCompleted with reasoning
    let mut found_reasoning = false;

    wait_for_event(&codex, |ev| {
        if let EventMsg::ItemCompleted(item) = ev
            && let TurnItem::Reasoning(reasoning) = &item.item
            && !reasoning.raw_content.is_empty()
        {
            found_reasoning = true;
        }
        matches!(ev, EventMsg::TurnAborted(_) | EventMsg::TurnComplete(_))
    })
    .await;

    assert!(
        found_reasoning,
        "Partial reasoning should be recorded in history after interruption"
    );
}
