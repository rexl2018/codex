# Codex Core Tests

本目录包含 Codex Core 的测试套件，按照功能和测试类型进行组织。

## 目录结构

```
tests/
├── README.md                    # 本文档
├── all.rs                      # 聚合测试入口（用于 suite 目录）
├── common/                     # 测试支持库
│   ├── Cargo.toml
│   ├── lib.rs
│   └── responses.rs
├── fixtures/                   # 测试数据文件
│   ├── completed_template.json
│   └── incomplete_sse.json
├── integration/                # 集成测试
│   ├── apply_patch_tool.rs     # apply_patch 工具全链路测试
│   ├── chat_completions_payload.rs  # Chat Completions 请求负载测试
│   ├── chat_completions_sse.rs      # Chat Completions SSE 流测试
│   ├── mcp_pipeline.rs         # MCP 工具调用管线测试
│   ├── plan_tool.rs           # update_plan 工具端到端测试
│   ├── state_management_integration_test.rs  # Agent 状态管理测试
│   └── subagent_tests.rs      # Subagent 系统集成测试
└── suite/                     # 主题测试模块
    ├── mod.rs                 # 模块声明
    ├── cli_stream.rs          # CLI 流处理测试
    ├── client.rs              # 客户端测试
    ├── compact.rs             # 压缩功能测试
    ├── exec.rs                # 执行功能测试
    ├── fork_conversation.rs   # 对话分叉测试
    ├── live_cli.rs            # 实时 CLI 测试
    ├── model_overrides.rs     # 模型覆盖测试
    ├── prompt_caching.rs      # 提示缓存测试
    ├── review.rs              # 代码审查测试
    ├── stream_*.rs            # 流处理相关测试
    └── subagents.rs           # Subagent 功能测试
```

## 测试分类

### 集成测试 (`integration/`)
这些测试验证多个模块之间的交互和端到端功能：

- **MCP 工具调用管线** (`mcp_pipeline.rs`) - 测试 MCP 工具的完整调用流程
- **计划工具** (`plan_tool.rs`) - 测试 `update_plan` 工具的端到端功能
- **补丁应用** (`apply_patch_tool.rs`) - 测试 `apply_patch` 工具的全链路验证
- **Chat Completions** (`chat_completions_*.rs`) - 测试与 OpenAI 风格 API 的集成
- **状态管理** (`state_management_integration_test.rs`) - 测试 Agent 状态机的统一管理
- **Subagent 系统** (`subagent_tests.rs`) - 测试 Subagent 管理器、上下文存储和多代理协调器的集成

### 主题测试 (`suite/`)
这些测试专注于特定功能模块的测试：

- **CLI 相关** - 命令行界面和流处理
- **客户端功能** - HTTP 客户端和模型交互
- **执行引擎** - 代码执行和环境管理
- **对话管理** - 对话分叉、历史管理等
- **流处理** - 各种流式数据处理场景

## 运行测试

### 运行所有测试
```bash
cargo test -p codex-core --tests
```

### 运行特定集成测试
```bash
# MCP 管线测试
cargo test -p codex-core --tests mcp_pipeline

# 计划工具测试
cargo test -p codex-core --tests plan_tool

# 补丁应用测试
cargo test -p codex-core --tests apply_patch_tool

# 状态管理测试
cargo test -p codex-core --tests state_management_integration_test

# Subagent 系统测试
cargo test -p codex-core --tests subagent_tests
```

### 运行主题测试套件
```bash
# 运行所有 suite 测试
cargo test -p codex-core --tests all

# 运行特定主题测试
cargo test -p codex-core --tests client
cargo test -p codex-core --tests exec
```

### 运行特定测试用例
```bash
# 运行具体的测试函数
cargo test -p codex-core update_plan_emits_plan_update_event_and_succeeds
cargo test -p codex-core mcp_codex_tool_success_emits_begin_and_end_events
cargo test -p codex-core test_unified_state_management
```

## 添加新测试

### 添加集成测试
1. 在 `integration/` 目录下创建新的 `.rs` 文件
2. 使用 `codex_core::` 前缀导入需要的模块
3. 编写测试函数，使用 `#[tokio::test]` 或 `#[test]` 标注
4. 测试文件会被 Cargo 自动发现和编译

### 添加主题测试
1. 在 `suite/` 目录下创建新的 `.rs` 文件
2. 在 `suite/mod.rs` 中添加模块声明：`mod your_new_test;`
3. 使用 `crate::` 前缀导入内部模块
4. 编写测试函数并包装在 `#[cfg(test)] mod tests { ... }` 中

## 测试最佳实践

### 集成测试
- 测试真实的用户场景和工作流
- 验证模块间的正确交互
- 使用真实的配置和数据结构
- 测试错误处理和边界情况

### 主题测试
- 专注于单个模块或功能
- 使用模拟对象隔离依赖
- 测试内部逻辑和算法
- 保持测试快速和独立

### 通用原则
- 使用描述性的测试名称
- 包含必要的断言和验证
- 清理测试资源（如临时文件）
- 使用适当的异步测试标注

## 常见测试主题

- **事件驱动测试** - 验证事件的发送和接收
- **状态管理** - 测试状态转换和约束
- **工具调用** - 验证工具的注册、发现和执行
- **错误处理** - 测试各种错误场景的处理
- **并发安全** - 验证多线程环境下的正确性

## 示例文件位置

- 集成测试示例：`integration/mcp_pipeline.rs`
- 主题测试示例：`suite/client.rs`
- 测试支持代码：`common/lib.rs`
- 测试数据：`fixtures/completed_template.json`

## 重要注意事项

- 集成测试文件不需要 `#[cfg(test)]` 包装
- 主题测试需要在 `suite/mod.rs` 中声明模块
- 使用 `codex_core::` 导入集成测试中的模块
- 使用 `crate::` 导入主题测试中的内部模块
- 测试应该是确定性的和可重复的