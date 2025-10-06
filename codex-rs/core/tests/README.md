# codex-core 测试说明

本文档介绍 `codex-rs/core/tests` 目录下的测试分类、用途、执行方式，以及如何新增测试用例，帮助你快速上手并扩展测试覆盖率。

## 目录结构与聚合方式

- 集成测试统一聚合为一个测试二进制，入口文件：<mcfile name="all.rs" path="/Users/bytedance/work/tools/git/codex/codex-rs/core/tests/all.rs"></mcfile>
- 具体测试模块按主题组织：
  - 单体/主题测试模块位于 `suite/` 目录，通过聚合模块文件注册：<mcfile name="mod.rs" path="/Users/bytedance/work/tools/git/codex/codex-rs/core/tests/suite/mod.rs"></mcfile>
  - 集成测试模块位于 `integration/` 目录（新的约定）：`core/tests/integration/*.rs`

常见模块示例：
- <mcfile name="mcp_pipeline.rs" path="/Users/bytedance/work/tools/git/codex/codex-rs/core/tests/integration/mcp_pipeline.rs"></mcfile>：MCP 工具调用管线（握手、工具发现、成功/失败/超时事件映射）
- <mcfile name="plan_tool.rs" path="/Users/bytedance/work/tools/git/codex/codex-rs/core/tests/integration/plan_tool.rs"></mcfile>：`update_plan` 语义、状态机与事件（`PlanUpdate`）
- <mcfile name="apply_patch_tool.rs" path="/Users/bytedance/work/tools/git/codex/codex-rs/core/tests/integration/apply_patch_tool.rs"></mcfile>：`apply_patch` 从模型工具调用到本地补丁应用（`PatchApplyBegin`/`PatchApplyEnd`）
- 其他：`client.rs`、`prompt_caching.rs`、`exec_stream_events.rs`、`compact.rs`、`subagents.rs` 等主题模块

## 测试分类与用途

1. 单元测试（Unit Tests）
   - 位置：源码文件内部的 `#[cfg(test)] mod tests`（例如 `core/src/*.rs`）
   - 用途：验证单个模块/函数的最小行为单位（纯逻辑、数据结构、解析函数等）

2. 集成测试（Integration Tests）
   - 位置：`core/tests/integration/*.rs`（新的统一位置）
   - 用途：验证跨模块的端到端行为（包括 HTTP 流、工具调用管线、事件生命周期等）
   - 特点：大量用到 `wiremock`、`core_test_support` 和异步事件流断言

3. 端到端（E2E）工具链测试（Feature/E2E）
   - 用途：覆盖从“模型返回工具调用”到“Codex 内部执行器处理”再到“事件发出”的完整链路
   - 典型场景：`update_plan`、`apply_patch`、MCP `tools/call` 调用（含错误与超时路径）

## 依赖与测试基建

- 推荐使用的测试工具/辅助方法（来自 `core_test_support`）：
  - `load_default_config_for_test`：生成可控的默认配置（含临时 `CODEX_HOME`）
  - `start_mock_server`/`mount_sse_once`：启动 `wiremock` 并挂载一次性 SSE 响应
  - `responses::{sse, ev_function_call, ev_completed}`：快速构造流式响应事件
  - `wait_for_event`/`wait_for_event_with_timeout`：等待并断言异步事件
- 典型执行器/会话管理：
  - <mcfile name="conversation_manager.rs" path="/Users/bytedance/work/tools/git/codex/codex-rs/core/src/conversation_manager.rs"></mcfile> 提供 `ConversationManager` 创建与管理会话
  - <mcfile name="codex.rs" path="/Users/bytedance/work/tools/git/codex/codex-rs/core/src/codex.rs"></mcfile> 内部负责提交用户输入与消费事件流

## 如何执行测试

- 构建当前 crate：
  - `cargo build -p codex-core`
- 运行所有集成测试：
  - `cargo test -p codex-core --tests`
- 运行某个模块（按名称过滤）：
  - `cargo test -p codex-core --tests plan_tool`
  - `cargo test -p codex-core --tests mcp_pipeline`
- 运行某个具体用例（按函数名或子串过滤）：
  - `cargo test -p codex-core update_plan_emits_plan_update_event_and_succeeds`
  - `cargo test -p codex-core mcp_codex_tool_argument_parse_error_maps_to_function_output_error`
- 显示标准输出/日志：
  - `cargo test -p codex-core --tests -- --nocapture`
  - 设置日志：`RUST_LOG=info cargo test -p codex-core --tests -- --nocapture`

> 提示：测试名过滤是按子串进行的，尽量使用唯一且可读的测试函数名。

## 如何新增测试

1. 在 `core/tests/integration/` 下新建测试文件（例如：`my_feature.rs`），并使用 Tokio 异步测试：
   - `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]`
   - 使用 `core_test_support` 与 `wiremock` 生成稳定、可复现的输入
2. 若为主题/单体测试，放在 `suite/` 并在聚合文件中注册模块：
   - 编辑 <mcfile name="mod.rs" path="/Users/bytedance/work/tools/git/codex/codex-rs/core/tests/suite/mod.rs"></mcfile>，追加一行：`mod my_feature;`
3. 遵循以下约定以保持测试稳定：
   - 不依赖外部网络，全部用 `wiremock`/SSE fixture 模拟
   - 使用 `TempDir` 管理临时文件与 `CODEX_HOME`，避免污染工作目录
   - 为事件等待设置合理的超时（如 3~5s），避免假死或长时间阻塞
   - 对于需要文件读写的测试（如 `apply_patch`），在临时目录中创建与校验

## 编写测试的最佳实践

- 使用 `ConversationManager` 创建会话，随后通过 `CodexConversation::submit` 发送 `Op::UserInput`
- 对工具调用类场景：先构造 SSE 流中的 `function_call` 项（如 `update_plan`、`apply_patch` 或 `codex__codex`），再断言 Codex 发出的事件
- 错误路径需覆盖：参数解析失败、工具执行失败、超时与重试（如 MCP 管线）
- 命名规范：测试函数名应体现场景与预期（如 `..._emits_begin_and_end_events`、`..._returns_error`）

## 常见主题与对应示例

- MCP 工具调用管线：<mcfile name="mcp_pipeline.rs" path="/Users/bytedance/work/tools/git/codex/codex-rs/core/tests/integration/mcp_pipeline.rs"></mcfile>
- 计划工具：<mcfile name="plan_tool.rs" path="/Users/bytedance/work/tools/git/codex/codex-rs/core/tests/integration/plan_tool.rs"></mcfile>
- 补丁应用：<mcfile name="apply_patch_tool.rs" path="/Users/bytedance/work/tools/git/codex/codex-rs/core/tests/integration/apply_patch_tool.rs"></mcfile>
- 客户端与提示缓存：<mcfile name="client.rs" path="/Users/bytedance/work/tools/git/codex/codex-rs/core/tests/suite/client.rs"></mcfile>、<mcfile name="prompt_caching.rs" path="/Users/bytedance/work/tools/git/codex/codex-rs/core/tests/suite/prompt_caching.rs"></mcfile>
- 执行流与并发输出：<mcfile name="exec_stream_events.rs" path="/Users/bytedance/work/tools/git/codex/codex-rs/core/tests/suite/exec_stream_events.rs"></mcfile>

## 注意事项

- 部分测试涉及 MCP Server 工具发现与调用，需保证工作区可构建 `codex-mcp-server`（`cargo build -p codex-mcp-server`）
- 若本地环境或 CI 无法访问外网，请确保所有测试均使用 mock 响应，不要向真实服务发起请求
- 避免使用过长的超时与随机等待，优先事件驱动与有限重试，确保测试可重复与稳定

如需进一步扩展某个主题的测试或添加新的工具管线验证，建议参考上述模块的写法并保持一致的模式与约定。