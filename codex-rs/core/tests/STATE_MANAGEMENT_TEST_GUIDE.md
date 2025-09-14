# 状态管理综合测试指南

## 概述

这个测试套件验证了主agent的状态管理系统，确保在subagent被强制完成后，系统能够正确阻止相应类型的subagent创建。

## 测试文件

- `state_management_integration_test.rs` - 主要的集成测试文件

## 测试场景

### 主要测试：`test_comprehensive_state_management`

这是一个完整的端到端测试，验证以下场景：

#### 阶段1：初始状态验证
- **目标**：验证系统初始状态
- **期望**：
  - 当前状态：`Initialization`
  - Explorer创建：✅ 允许
  - Coder创建：✅ 允许

#### 阶段2：Explorer强制完成测试
- **操作**：
  1. 创建Explorer subagent
  2. 强制其达到最大turn数
  3. 验证状态变化
- **期望**：
  - 当前状态：`ExplorerForcedCompletion`
  - Explorer创建：❌ 阻止
  - Coder创建：✅ 允许
  - 尝试创建Explorer被阻止
  - 尝试创建Coder成功

#### 阶段3：Coder强制完成测试
- **操作**：
  1. 创建Coder subagent
  2. 强制其达到最大turn数
  3. 验证状态变化
- **期望**：
  - 当前状态：`CoderForcedCompletion`
  - Explorer创建：❌ 阻止
  - Coder创建：❌ 阻止
  - 尝试创建任何subagent都被阻止

#### 阶段4：Prompt注入验证
- **目标**：验证状态信息正确注入到prompt中
- **期望**：
  - Prompt包含"Current Agent State"
  - Prompt包含"Subagent Creation Constraints"
  - 正确显示Explorer和Coder的状态

#### 阶段5：替代建议验证
- **目标**：验证系统提供适当的替代方案
- **期望**：
  - 提供非空的替代建议列表
  - 建议包含"available information"等关键词

### 辅助测试

#### `test_explorer_normal_completion_allows_coder`
- **目标**：验证Explorer正常完成不影响Coder创建
- **期望**：正常完成后状态保持`Initialization`

#### `test_state_transition_sequence`
- **目标**：验证所有状态转换的逻辑正确性
- **期望**：每个状态的约束规则正确

## 运行测试

### 运行单个测试
```bash
cd codex-rs
cargo test test_comprehensive_state_management -- --nocapture
```

### 运行所有状态管理测试
```bash
cd codex-rs
cargo test state_management -- --nocapture
```

### 运行完整测试套件
```bash
cd codex-rs
cargo test --package codex-core --test state_management_integration_test -- --nocapture
```

## 测试输出示例

成功运行时，你应该看到类似以下的输出：

```
🧪 Starting comprehensive state management test...

📋 Phase 1: Initial State Verification
✅ Initial state: Both Explorer and Coder creation allowed

📋 Phase 2: Explorer Forced Completion Test
✅ After Explorer forced completion: Explorer blocked, Coder allowed
✅ Explorer creation correctly blocked
✅ Coder creation still allowed

📋 Phase 3: Coder Forced Completion Test
✅ After Coder forced completion: Both Explorer and Coder blocked
✅ Both Explorer and Coder creation correctly blocked

📋 Phase 4: Prompt Injection Verification
✅ State information correctly injected into prompts

📋 Phase 5: Alternative Suggestion Verification
✅ Appropriate alternatives suggested when subagents blocked

🎉 Comprehensive state management test completed successfully!
```

## 故障排除

### 常见问题

1. **状态检测失败**
   - 检查`detect_agent_state`函数是否正确实现
   - 验证subagent历史记录是否正确更新

2. **Prompt注入失败**
   - 检查`agent_state_info`字段是否正确设置
   - 验证`get_full_instructions`方法是否包含状态信息

3. **约束检查失败**
   - 检查`can_create_explorer`和`can_create_coder`方法
   - 验证状态转换逻辑

### 调试技巧

1. **启用详细日志**：
   ```bash
   RUST_LOG=debug cargo test test_comprehensive_state_management -- --nocapture
   ```

2. **单步调试**：
   - 在关键点添加`println!`语句
   - 使用断点调试器

3. **检查状态变化**：
   - 在每个阶段后打印当前状态
   - 验证subagent历史记录

## 扩展测试

### 添加新的测试场景

1. **并发subagent测试**：
   ```rust
   #[tokio::test]
   async fn test_concurrent_subagent_creation() {
       // 测试同时创建多个subagent的情况
   }
   ```

2. **状态恢复测试**：
   ```rust
   #[tokio::test]
   async fn test_state_recovery_after_restart() {
       // 测试系统重启后状态恢复
   }
   ```

3. **边界条件测试**：
   ```rust
   #[tokio::test]
   async fn test_edge_cases() {
       // 测试各种边界条件
   }
   ```

## 集成到CI/CD

将以下命令添加到你的CI/CD流水线：

```yaml
- name: Run State Management Tests
  run: |
    cd codex-rs
    cargo test --package codex-core --test state_management_integration_test
```

## 性能考虑

- 测试使用mock实现，运行速度快
- 每个测试独立，可以并行运行
- 内存使用量低，适合CI环境

## 维护指南

1. **更新测试**：当状态管理逻辑变化时，更新相应的测试用例
2. **添加覆盖**：为新的状态或约束添加测试覆盖
3. **性能监控**：定期检查测试运行时间，确保效率