# QQ 机器人 / Pi 主 Agent：执行记录接入指南

本页交给负责机器人与 Pi 调度代码的 Agent 使用。服务端执行记录 API 已实现；机器人代码不在本仓库，尚未接入或验证真实 QQ/Pi hooks。请根据实际机器人框架与 Pi 版本寻找消息接收、任务下发、上下文压缩/退出和结果回传接口，不假定某个事件名或插件 API 一定存在。

Harness 存储状态与检查点，不执行任务、不主动启动 Agent。首版用单主调度器；CAS 保证同一任务同时只能接受一份最新写入，不提供租约、心跳监听、自动故障转移或业务动作恰好一次保证。

## 连接与通用约定

- 本机默认 origin 为 `http://localhost:8080`；跨机器使用显式 HTTPS origin。所有下面接口需要 `Authorization: Bearer <设备凭据>`。通过机器人真实运行环境注入凭据，不写入提示词、配置示例、日志或任务正文。
- POST/PUT 需要独立 `Idempotency-Key`，1–128位 ASCII 字母数字、`-` 或 `_`。同一次网络重试使用同 key、路径和 JSON；修改请求用新 key。超时表示结果未知，不能当作没有保存。响应 `idempotent=true` 是原始回执，可能早于任务当前状态；恢复业务动作前再 GET 最新记录。
- ID 为1–128位小写字母数字或 `-`、首位字母数字。为消息、调度会话和子任务使用稳定匿名映射；不要直接保存 QQ 账号、群号或完整消息。`source_key` 在整个服务中唯一，应包含平台/机器人实例/会话/消息的命名空间摘要；拆分多个任务时加稳定子任务键。同一消息的文本修改或重拆分不能静默覆盖原委托。
- 写入成功返回最新任务 `sha256`；后续同任务写入必须提供 `expected_sha256`。409 后读取最新任务，核验 attempt_id、状态、取消请求和已发生的动作，合并后用新 key 重试。不要只替换哈希并盲目重跑。
- 状态与检查点原子提交、幂等回执随重启保留；正文/元数据扫描疑似凭据。只保存脱敏摘要与受控产物引用，调用方仍负责避免个人信息和秘密。

## API

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| POST | `/v1/executions` | 接受任务，持久化来源去重映射 |
| GET | `/v1/executions` | 按会话/项目/状态列举任务摘要 |
| GET | `/v1/executions/{task_id}` | 当前任务与最新执行尝试/检查点 |
| POST | `/v1/executions/{task_id}/attempts` | 原子领取并建立执行尝试，转为 DISPATCHING |
| GET | `/v1/executions/{task_id}/attempts` | 分页列举历史尝试摘要，序号倒序 |
| GET | `/v1/executions/{task_id}/attempts/{attempt_id}` | 读取指定尝试的最后检查点 |
| POST | `/v1/executions/{task_id}/checkpoint` | 条件更新执行状态与完整检查点 |
| POST | `/v1/executions/{task_id}/cancel` | 请求取消；未下发任务可立即取消 |
| POST | `/v1/executions/{task_id}/delivery` | 独立记录结果回复的投递状态 |
| PUT | `/v1/execution-contexts/{conversation_id}` | 条件保存主 Agent 的调度摘要 |
| GET | `/v1/execution-contexts/{conversation_id}` | 读取调度摘要 |

当前 Rust CLI 没有新增执行记录子命令，机器人直接调用 HTTP。通用 documents PUT 禁止修改这些记录；知识 list/route 不返回执行记录。

## 1. 接受任务

收到消息后、下发前发送 JSON：

```json
{
  "task_id": "fix-print-01",
  "conversation_id": "qq-conversation-a",
  "source_key": "qq-source-hash-child-1",
  "project": "demo",
  "title": "修复打印接口",
  "goal": "修复接口并保存可续接的执行进度",
  "acceptance": "验证受影响接口，记录实际结果"
}
```

`task_id` 可省略，服务生成 UUID；`parent_task_id` 可选，父任务须属于同一调度会话。goal/acceptance各≤600字，title≤100字。project 使用稳定项目ID，不传本机绝对路径；`daily` 返回 SKIP，不保存或计统计。

响应含 `task`、`sha256`、`latest_attempt`、`created`、`deduplicated`。同 `source_key` 与同规范化请求，即使用不同 HTTP key 也返回原 task_id 和当前状态；同来源绑定不同需求返回409。不要为重复消息再次拆分任务或重新派发。分解键和 task_id 映射应在机器人持久 inbox 保存，不能只存在 LLM 上下文。

记录失败时不下发，并保留可重放消息或本地 inbox。平台回调已确认但记录尚未保存，仍可能丢任务；Harness 无法替代消息入口的持久化。

## 2. 建立执行尝试，再下发

POST attempts：

```json
{"expected_sha256": "<任务当前SHA>", "agent": "pi"}
```

可带已知 `native_session_id`。响应 `task.current_attempt_id` 为 attempt_id；以它作为执行器启动的去重/查询标识，并持久化它与 Pi 原生会话 ID 的映射。此时状态是 DISPATCHING，不能表示已启动。

只有 QUEUED 或明确 FAILED、且未请求取消的任务可新建尝试。并发领取同一任务只有一份成功。下发失去确认时先查询执行器，不直接新开 Agent；明确未启动才记录 FAILED，无法确认则记录 RECONCILING。成功启动后保存 RUNNING 检查点。

## 3. 保存检查点与结果

POST checkpoint：

```json
{
  "expected_sha256": "<任务当前SHA>",
  "attempt_id": "<当前attempt_id>",
  "native_session_id": "<实际Pi原生会话ID>",
  "status": "RUNNING",
  "checkpoint": {
    "current": "已定位接口，准备验证",
    "completed_steps": ["已确认字段来源"],
    "next_steps": ["运行受影响验证"],
    "blockers": [],
    "verification": [],
    "artifacts": ["branch:fix-print", "commit:<已存在的提交>"]
  }
}
```

这是**完整检查点替换**，省略的数组会变为空；先读取并保留仍需要续接的信息。current≤400字，所有文本合计≤1,200字，不自动截断。completed_steps最多8条；next_steps/blockers/verification各最多5条，每条≤120字；artifacts最多8条、每条≤200字。原生会话可在启动确认时首次绑定，之后不能替换；重试使用新 Attempt。

阶段完成、产生重要产物、等待用户、外部关键动作边界、上下文压缩或退出前自动保存，不每条消息/每次工具调用都写。压缩 hook 存在时优先保存；即使没有退出 hook，已完成的阶段检查点仍可恢复。

允许状态路径：

- DISPATCHING → RUNNING / RECONCILING / FAILED / CANCELLED。
- RUNNING → WAITING_USER / RECONCILING / SUCCEEDED / FAILED / CANCELLED。
- WAITING_USER → RUNNING / RECONCILING / FAILED / CANCELLED。
- RECONCILING → RUNNING / SUCCEEDED / FAILED / CANCELLED，先核验事实。
- 上述非终态可保持原状态更新检查点；终态检查点不再更改。同 key 重试可以重放原回执。

真实完成使用 SUCCEEDED，明确失败使用 FAILED；保存实际结果、验证范围和产物。新 Attempt 会保留旧尝试的失败检查点；旧 attempt_id 即使拿到新 SHA 也不能更新当前任务。

cancel API 接收 `{"expected_sha256":"<SHA>"}`。QUEUED 可直接转 CANCELLED；已经下发的任务只设置 `cancel_requested=true`，由适配器停止实际进程并确认后保存 CANCELLED。主 Agent 不把取消请求当作已停止执行。

## 4. 保存主 Agent 摘要

PUT execution-contexts：

```json
{
  "title": "QQ 主 Agent 当前调度",
  "goal": "完成当前两项开发任务",
  "constraints": ["使用既有项目目录和分支", "仅按影响运行验证"],
  "open_questions": ["等待用户确认发布窗口"],
  "task_ids": ["fix-print-01"]
}
```

首次创建省略 expected_sha256；后续替换必带当前 SHA。goal≤400字、title≤100字；constraints/open_questions各最多8条、每条≤200字；摘要文本合计≤1,200字。task_ids最多20个、必须已存在且属于当前会话；更多任务用列表查询，不塞入摘要。

摘要与任务接受是独立调用，最新任务列表是已接受工作的依据：摘要更新失败时不能遗忘已接受的任务。摘要不是完整聊天记录；保留当前目标、约束、等待事项即可。

## 5. 重启或清空上下文后恢复

1. GET 调度摘要。404表示没有保存过摘要，继续查询任务列表，不能假定没有任务。
2. GET `/v1/executions?conversation_id=<id>&active_only=true&limit=20&offset=0`，按 remaining 分页。支持 project、status过滤；每页最多50条，按 task_id 排序，不是最近时间排序。
3. 按需 GET 将要处理的 task_id，只返回最新尝试的检查点。执行器仍存活时续接原实例；DISPATCHING / RECONCILING / RUNNING失联时先核验执行器与副作用，超时不等于未执行。
4. 另查询终态结果的待回复事项：列表所有任务再检查 delivery_status，或按 SUCCEEDED/FAILED/CANCELLED 分别过滤；active_only不包含这些结果，不可只查询未完成任务。
5. 结合项目记忆、项目本机根映射与 Pi 原生会话恢复实际执行。需要知识时另调用 route；不全量读取旧尝试、聊天或全部记忆。

## 6. 记录结果回复

终态结果自动具有 `delivery_status=PENDING` 和稳定 `delivery_key`，每个新执行尝试使用不同 key。调用 delivery 时带 `expected_sha256`、当前 `delivery_key`、`status` 和可选 `summary`。

发送前保存 SENDING；发送确认后保存 SENT，结果未知时保存 UNKNOWN。允许 PENDING→SENDING，SENDING→SENT/UNKNOWN；UNKNOWN→SENT/PENDING需要非空核验说明。重启看到 SENDING同样先查询投递回执，再记录 UNKNOWN或SENT，不能盲目重发。请求的旧 delivery_key不能标记新尝试的结果已经投递。

Harness 持久化回复意图和状态，不负责发送 QQ 消息。适配器使用 delivery_key 去重，并尽可能查询平台回执；不支持查询/去重且结果不明时保留待核验。记录与网络发送之间仍存在崩溃窗口，不承诺恰好一次；业务动作同样要使用实际系统的操作ID和核验机制。

## 运行与升级边界

执行记录保存在同一持久卷的完整修订中，复用 writer锁、原子HEAD、事务回滚和持久幂等；只做低频快照，没有高频事件流或自动清理。全部记忆加执行文档受4,096份/32MiB总文档限制，完整历史修订和回执没有GC，需监控卷容量。备份整个store；记录增加后旧版本无法识别新路径，降级需恢复对应旧备份，不能直接用旧二进制打开更新后的store。

验收时模拟重复消息、两个并发任务、主Agent清空上下文、进程重启、下发确认丢失、旧执行器回传、保存失败和完成但回复未确认。服务端回归通过不代替真实QQ/Pi接入验证。不要自动切换现有全局Harness入口或双写；调用新能力与知识迁移分别配置。

## v0.3 namespace接入

机器人请求带`X-Harness-Namespace`和可选`X-Harness-Agent-Id`，凭据绑定的真实身份及读写范围按[身份指南](AGENT-NAMESPACES.md)配置。Task/Attempt/Context不跨namespace继承；恢复、领取、检查点和回复状态始终使用原空间。主Agent与执行Agent要协作同一任务记录时，使用双方获准的工作流namespace，身份及具体Attempt仍分别记录；不要在各自专属空间创建同ID后当作同一任务。服务不自动分发或移动任务。

空间先由管理员在`/admin`创建并授权。一个Agent可读写多个空间，但Task/Attempt/Context API一次只选择一个；机器人需保存每个任务的原namespace，默认空间改变不能移动执行记录。route/start的多空间读取预算与执行调度隔离，见接入指南。
