我已经收集了足够的信息来详细分析codex-api和codex-core的差异，现在将整理这些信息，给出一个完整的分析报告。

## 详细分析 codex-api 和 codex-core 的差异

### 1. codex-api 变更

#### 1.1 common.rs
- 新增 `include_instructions` 字段到 `Prompt` 结构体，用于控制是否在 API 请求中包含系统指令
- `ResponsesApiRequest` 结构体中的多个字段变为可选（使用 `Option` 类型）：
  - `instructions` - 系统指令
  - `tools` - 工具定义
  - `tool_choice` - 工具选择策略
  - `parallel_tool_calls` - 是否允许并行工具调用
- 新增 `Caching` 和 `CachingType` 结构体，用于配置请求的缓存策略
- 新增 `max_output_tokens` 字段，用于限制模型输出的最大令牌数
- 新增 `previous_response_id` 字段，用于指定前一个响应的 ID

#### 1.2 requests/chat.rs
- 新增对 Google Gemini 模型的支持，包括：
  - 处理 `thought_signature` 字段，用于提供工具调用的上下文信息
  - 为 Gemini 模型添加特殊的工具调用格式，包含 `extra_content.google` 字段
  - 新增 `ensure_tool_results_balanced` 函数，用于确保工具调用和结果的平衡
  - 新增 `coalesce_messages` 函数，用于合并连续的助手消息

### 2. codex-core 变更

#### 2.1 codex.rs
- 新增 `ManageHistory` 操作处理，用于管理对话历史
- 新增 `set_last_response_id` 方法和相关测试，用于跟踪和设置最后一个响应的 ID
- 增强了会话状态管理，支持处理中断的对话

#### 2.2 compact.rs
- 新增 `run_compact_task_on_range` 函数，支持对指定范围的对话历史进行压缩
- 增强了 `run_compact_task_inner` 函数，支持接收和处理范围参数
- 改进了压缩任务的实现，支持更灵活的历史处理方式

#### 2.3 thread_manager.rs
- 重命名了多个方法，使用更清晰的命名：
  - `resume_conversation_from_rollout` 改为 `resume_thread_from_rollout`
  - `resume_conversation_with_history` 改为 `resume_thread_with_history`
  - `remove_conversation` 改为 `remove_thread`
  - `fork_conversation` 改为 `fork_thread`
- 新增 `get_conversation` 方法，用于获取会话
- 新增 `sanitize_history` 和 `sanitize_rollout_items` 函数，用于清理历史记录中的加密内容，防止服务器端上下文过期导致的错误

#### 2.4 tools/spec.rs
- 新增 `freeform_as_function` 字段到 `ToolsConfig` 结构体，用于控制是否将自由格式的工具调用转换为函数格式
- 增强了 `build_specs` 函数，根据配置决定使用自由格式还是函数格式的工具
- 新增了两个测试函数：
  - `test_freeform_as_function_conversion`
  - `test_freeform_without_conversion`

#### 2.5 context_manager/history.rs (新增文件)
- 新增上下文管理器，用于管理对话历史
- 实现了历史记录的添加、删除、查询等功能

#### 2.6 context_manager/history_tests.rs (新增文件)
- 包含了对历史管理功能的测试用例

#### 2.7 util.rs
- 优化了退避算法，将初始延迟从 200ms 增加到 500ms，并设置最大延迟为 60 秒

### 3. 主要功能改进

#### 3.1 API 增强
- 更加灵活的请求配置，多个字段变为可选
- 新增缓存策略配置
- 支持限制输出令牌数
- 新增对前一个响应的引用支持

#### 3.2 模型支持
- 新增对 Google Gemini 模型的全面支持
- 特殊处理 Gemini 模型的工具调用格式

#### 3.3 历史管理
- 新增对话历史管理功能
- 支持对指定范围的历史进行压缩
- 增强了历史记录的清理功能

#### 3.4 工具调用优化
- 支持将自由格式的工具调用转换为函数格式
- 改进了工具调用的平衡检查

#### 3.5 会话管理
- 更清晰的方法命名
- 新增会话查询功能
- 增强了会话状态管理

### 4. 架构改进

#### 4.1 代码组织
- 新增上下文管理器模块，更好地组织历史管理代码
- 分离了不同功能的代码，提高了可维护性

#### 4.2 错误处理
- 增强了对服务器端上下文过期的处理
- 改进了错误报告和处理机制

### 5. 总结

这些变更表明代码库正在进行重大的改进和扩展，特别是：
1. 增强了 API 的灵活性和可配置性
2. 新增了对 Google Gemini 模型的支持
3. 改进了对话历史管理功能
4. 优化了工具调用和会话管理
5. 提高了代码的组织性和可维护性

这些改进为构建更强大、更灵活的 AI 应用程序提供了更好的基础。