# 空间管理与多 namespace 接入（v0.3）

空间由服务端管理，Agent 只接入已有授权空间。每个 Agent 有稳定的 `agent_id`、多个读/写授权、最多 8 个默认读取空间，以及一个可选默认写入空间。适用于同一用户的可信设备与机器人；空间内没有项目级 ACL。

## 1. 服务端配置

使用 v0.3 服务、客户端和备份工具。复制 [身份模板](../examples/auth.example.json)，给每个长期 Agent 绑定不同的凭据环境变量与稳定 ID。新配置的读写列表留空；命名空间和授权在管理页面配置。客户端可先 `init --agent-id codex-example`，把这个非敏感 ID 提供给管理员绑定凭据。身份不能通过自报请求头冒用。

启动环境中另设独立 `HARNESS_ADMIN_TOKEN`（32–512 字、无空白，与所有 Agent token 不同）。实际值由私有部署环境注入，不填在 JSON、Git 或聊天里。身份文件只含环境变量名。

```sh
harness-server --data-dir /data serve --bind 127.0.0.1:8080 \
  --auth-config /etc/harness-server/auth.json
```

Docker 使用 [namespace 覆盖文件](../compose.namespaces.yaml)，私有 `.env` 分别注入 `HARNESS_ADMIN_TOKEN`、`HARNESS_CODEX_TOKEN`、`HARNESS_QQ_TOKEN`。复制模板到 `config/auth.json`，保证容器 UID10001 可读。保持已有卷、loopback 端口及 HTTPS 反向代理。基础 Compose 仍要求原 `HARNESS_TOKEN`；启用 auth-config 后，只有身份文件列出的 Agent 凭据有效。

```sh
# 使用 GitHub 构建发布的固定版本镜像；不在小型生产机编译。
docker compose -f compose.image.yaml -f compose.namespaces.yaml pull
docker compose -f compose.image.yaml -f compose.namespaces.yaml up -d
```

凭据绑定的新增、移除和轮换仍需修改部署环境并重启；**已有 Agent 的空间授权和默认范围通过页面即时更新，不需要重启**。未配置管理员 token 时，管理 API 返回 503，原业务 API 仍可使用。旧凭据模式继续仅访问 shared；要管理各 Agent 权限，需改用绑定身份的凭据配置。

## 2. 管理页面

打开同一服务地址的 `/admin`，例如 `https://your-harness.example/admin`，输入独立管理员 token。token 只留在当前页面内存；不放进 URL、Cookie、localStorage 或导出包，刷新或退出需要重新输入。

- **新建空间**：输入名称、说明；服务生成不可变 `ns-<UUID>`，客户端不能提交 ID。
- **Agent 管理**：给已有凭据绑定的身份分配多个读取/写入空间、默认读取范围及一个默认写入空间。写权限必须包含读权限；有读取权限时至少选一个默认读取空间。最多授权 16 个空间，默认检索最多 8 个。
- **内容**：分页查看空间内摘要和只读 Markdown；不继承 shared，不编辑正文。执行记录仍通过专用 API 浏览，页面只展示其数量。
- **成员与操作记录**：查看授权、最近 50 条管理审计；记录动作、时间和目标，不记录 token 或记忆正文。
- **归档**：保留文档、允许读取、停止写入，可恢复；shared 不可归档。归档不自动改 Agent 默认值，需要管理员按实际用途调整。

页面修改使用最新 SHA 和幂等键。409 后读取最新数据并重新审阅；网络超时或 5xx 保留原请求，显式重试沿用同一键和正文。退出页面会清除待重试请求；已发出的请求仍可能完成，重新连接后先查看服务状态。

## 3. 客户端接入

不同长期 Agent 使用独立配置文件。`init` 只保存自定义 ID 或一次生成的 `agent-<UUID>`，重复初始化复用，不生成 namespace、不注册服务、不读取凭据。显式指定旧 `--namespace` 仅保存未经核验的选择，推荐用 `connect`。

```sh
harness --config /path/to/client.json init --agent-id codex-example
harness --config /path/to/client.json identity
# 配好 URL/token_env 和凭据注入后，查询服务器已有授权。
harness --config /path/to/client.json namespaces
# 以下 ID 替换为管理页面实际创建、授权的 ID。
harness --config /path/to/client.json connect \
  --namespace ns-<实际UUID> --read-namespaces shared,ns-<实际UUID>
```

`connect` 查询服务目录，验证身份与每个读取空间，原子合并配置，不创建空间。未授权或不存在的选择不保存。只需服务默认值时保持 `namespace`、`read_namespaces` 为 null；配置了显式选择就以它为准，管理员改变默认值不会覆盖这些本机选择。

```json
{
  "url": "https://your-harness.example",
  "agent_id": "codex-example",
  "namespace": null,
  "read_namespaces": null,
  "token_env": "HARNESS_CODEX_TOKEN",
  "projects": {}
}
```

CLI 参数 / `HARNESS_AGENT_ID`、`HARNESS_NAMESPACE`、`HARNESS_READ_NAMESPACES` 优先于配置；未选择 namespace 时由服务使用 Agent 默认写入空间（只读 Agent 可用首个默认读取空间）。未绑定身份的旧模式默认 shared。`--agent codex/pi` 是 Session 前缀，与长期 `--agent-id` 含义不同。

## 4. 多空间读取与单空间写入

```sh
harness route --project demo --scenario coding --content
harness --read-namespaces shared,ns-<实际UUID> route --project demo --scenario coding --content
harness --namespace ns-<实际UUID> start --agent pi --native-session-id '<原生任务ID>' \
  --project demo --title '<任务>' --content
```

route/start 对读取范围**统一排序和统一预算**：最多 1 个项目入口、2 份流程、1 个专题，未命中仅返回最多 3 条候选摘要。项目入口按读取空间顺序选首个 ACTIVE 入口；流程与专题全局排名。同逻辑路径跨空间保持独立来源，不拼接正文或默认为覆盖关系；并列首选专题不自动选择。重复专题 ID 须用 `route --topic <id> --topic-namespace <namespace>` 明确来源。返回的正文、候选和 `memory_sources` 都有 `namespace/path`；多空间的引用字段使用 `{namespace,path}`，单空间保留逻辑路径字符串。

显式 `--namespace` 且不带读取列表时，兼容读取本空间及获准的 shared 背景；严格只读一个空间时同时指定 `--read-namespaces <同一ID>`。get/list/health 仍使用单空间视图，获准的 shared 项目/流程可被继承，本空间同路径覆盖；其他空间的 Session、Workstream、日志和执行记录不继承。

每次写入只落到一个空间。用继承记忆的 SHA 更新只创建本空间覆盖，不修改 shared；公共增量需明确 `--namespace shared finish --mode log ...`。不要把专属 Session 的 finish 切到 shared。

记录 start 返回的 `identity.namespace`、Session ID、Workstream SHA 和执行组。未显式传 namespace 时，start 恢复 / finish / abort 会在当前可读取空间中定位已有 Session，并在原空间执行；默认值改变不搬迁任务。多个空间有相同 Session ID 时拒绝猜测，显式带 start 返回的 namespace。原空间已撤权或归档时保存失败，不能改投其他空间；执行记录接口始终显式使用所选工作流空间。

## 5. HTTP、存储和升级

- `GET /v1/namespaces`：返回当前凭据身份、授权空间与默认值；零授权 Agent 也可查询。
- `X-Harness-Namespace`：单次操作空间；`X-Harness-Agent-Id`：必须与凭据一致；route/start 可带逗号分隔 `X-Harness-Read-Namespaces`。
- 管理接口为 `/admin/api/overview`、`/namespaces`、`/namespaces/{id}`、`/agents/{id}`、`/documents`、`/document`；只接受独立管理员凭据，写入需 Idempotency-Key，更新需 expected_sha256。没有跨域授权或通配符权限。
- 文档 API 拒绝 `control/...` 和 `namespaces/...` 物理路径；授权在存储锁内再次核验。历史回执含已撤权空间时禁止重放正文。
- v1 scoped客户端写入前只读核验health，拒绝忽略空间头的旧服务。v0.4 connect/setup确认scoped-v2后，使用独立`/v2/scoped/start|finish|abort|documents`写入口，一次完成身份/授权与事务；不兼容服务失败时不改投v1写入。保留原配置则继续v1。详细口径见[运行指南](OPERATIONS.md)。

shared 继续沿用原逻辑路径，其他文档物理路径为 `namespaces/<id>/<逻辑路径>`。管理事实为 `control/namespaces/*.md`、`control/agents/*.md`、`control/audit/YYYY-MM.md`，与记忆、持久回执同一次原子 HEAD 提交和回滚；不增加数据库。

首次升级导入已有空间/旧授权一次，保持已有 ID 和 shared 数据不变。目录建立后，部署配置不会覆盖页面授权；新增凭据身份默认零授权，不自动新建空间。所有空间与管理记录合计沿用 4096 文档 / 32MiB 容量上限；monthly audit ≤2MiB，达到容量限制会明确报错，不删除历史。

v0.3 在线备份包含空间目录、授权、审计、记忆和回执，**不包含身份凭据、部署身份文件和客户端配置**，这些需独立安全管理。恢复后部署同一批身份绑定才能访问旧授权。产生 control 数据后旧服务/备份工具不兼容，降级须恢复升级前备份；先更新备份工具并在隔离目录演练。

本机实现和验证不等于已发布、部署或切换全局入口；上线使用 GitHub 构建产物，不在小型业务机编译。
