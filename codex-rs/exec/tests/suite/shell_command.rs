#![allow(clippy::expect_used, clippy::unwrap_used)]

use core_test_support::responses;
use core_test_support::test_codex_exec::test_codex_exec;
use predicates::str::contains;
use wiremock::Mock;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bang_prefix_runs_shell_command_without_model_call() -> anyhow::Result<()> {
    let test = test_codex_exec();
    let server = responses::start_mock_server().await;

    Mock::given(method("POST"))
        .and(path_regex(r"/v1/responses"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    test.cmd_with_server(&server)
        .arg("--skip-git-repo-check")
        .arg("!echo codex-bang-test")
        .assert()
        .success()
        .stderr(contains("codex-bang-test"));

    Ok(())
}
