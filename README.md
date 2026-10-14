# Harness Server

Rust 原生 Harness HTTP 服务和客户端，当前源码包版本 **0.3.2**，edition **2024**，最低 Rust **1.88**，HTTP 框架为 axum **0.8**。服务二进制为 `harness-server`，客户端为 `harness`。本公开仓库从干净源码快照开始，不包含旧仓库的提交、发布标签或产物；安装包见 [Releases](https://github.com/YS1109/harness-server-public/releases)，镜像发布状态见 [Actions](https://github.com/YS1109/harness-server-public/actions/workflows/publish-image.yml)。

服务集中存储 Markdown，客户端通过 HTTP 路由记忆、维护 Session/Workstream 和提交结果。本版使用 Rust 引擎，不执行原 agent-home Python 引擎。**v1 是有限协议，不承诺兼容旧 Python 的全部 flags、字段或行为。**

QQ 机器人 / Pi 主 Agent 可通过独立执行记录API持久化调度摘要、任务/尝试状态和执行中检查点，支持来源消息去重、条件领取、重启读取及独立的结果回复状态。把 [Pi执行记录接入指南](docs/PI-EXECUTION-INTEGRATION.md) 交给机器人代码的Agent集成；设计与后续边界见 [执行记录设计](docs/EXECUTION-RECORDS.md)。服务不执行任务或自动重派，真实QQ/Pi接入尚未验证。

## Agent 身份与 namespace

v0.3 提供 `/admin` 管理工作台：服务生成空间 ID，管理员配置各 Agent 的多空间授权、默认读取范围和单一写入空间。`harness init --agent-id codex-example` 只建立本机身份；`harness namespaces` 查询授权目录，`harness connect` 核验接入已有空间。route/start 跨空间仍共用原读取预算并标注来源，已有 Session 在原空间续接和收尾。

客户端内置规则兼容v0.3.0服务，不要求同时升级现有服务。配置、管理页面、API、升级与回滚见 [空间管理与多 namespace 接入](docs/AGENT-NAMESPACES.md)。旧模式继续使用 shared；管理凭据独立于 Agent 凭据。授权和审计与记忆一起原子保存、一起备份，凭据不入存储。空间管理能力仍需支持v0.3的服务和实际授权，不自动切换现有设备。

## 让 AI 引导配置

v0.3.1 将通用记忆规则内置客户端：`harness instructions` 本地输出规则和本机选择，`--json` 返回结构化结果，`--bootstrap` 输出可复制到任意 Agent 全局提示词的短入口。无需联网或读取凭据。AI 判断共享/设备范围：可跨设备复用的稳定知识显式保存 shared；本机配置与交接保存已接入的设备空间。现有服务兼容，每次保存仍只写一个空间，不宣称跨空间原子提交；详见 [客户端规则](docs/CLIENT-RULES.md)。

把 [AI 配置引导](docs/AI-SETUP.md) 交给本机或云端 Agent，并说明“按这份文档帮我配置 Harness”。引导覆盖客户端安装、本地域名、凭据注入、项目目录、旧记忆迁移、全局入口及机器人多任务接入。

客户端默认地址为 **`http://localhost:8080`**，不需要修改 hosts。`localhost` 指当前客户端所在机器；连接云端时显式配置 HTTPS 地址。[客户端模板](examples/client.example.json) 不含凭据或个人目录。

## GitHub 构建镜像

[Publish image](.github/workflows/publish-image.yml) 在手动触发或推送 `v*` 标签时，先运行 CI，再在 amd64/arm64 原生 runner 上并行构建、检查并合并镜像，发布到 **`ghcr.io/ys1109/harness-server-public`**。GitHub 使用内置发布凭据，部署机无需安装 Rust。发布步骤、缓存、私有包登录和版本固定见 [镜像说明](docs/IMAGES.md)。

本仓库镜像首次发布成功后，在已配置 token 的仓库目录运行：

```sh
docker compose -f compose.image.yaml pull
docker compose -f compose.image.yaml up -d
docker compose -f compose.image.yaml exec harness-server harness health
```

镜像入口使用与源码入口一致的持久卷和 loopback 端口。下面的源码构建方式也继续可用。

## GitHub Release 客户端

[Release client](.github/workflows/release-client.yml) 原生构建 macOS Apple Silicon / Intel、Linux ARM64 / x64 和 Windows x64 客户端。推送匹配 Cargo 版本的 `v*` 标签后发布到 [Releases](https://github.com/YS1109/harness-server-public/releases)，使用方下载、校验 SHA-256 并解压即可运行，无需 Rust。公开Release可直接下载；Actions artifacts的访问按GitHub权限规则执行。选择平台、PATH 和连接配置见 [客户端安装](docs/CLIENT-INSTALL.md)，构建/发布见 [客户端 Release 说明](docs/CLIENT-RELEASES.md)。

手动运行 main 仅生成 Actions artifacts，用于验证，不发布 Release。安装包只含客户端、引导和无凭据模板；服务端使用上面的 Docker 镜像部署。

## Docker Compose 启动

需要 Docker Engine / Docker Desktop 和 Compose v2；从包含 `Cargo.lock` 的完整源码构建，Docker 使用 `--locked` 构建两个二进制。

```sh
git clone https://github.com/YS1109/harness-server-public.git
cd harness-server-public
cp .env.example .env
chmod 600 .env
# 将下面命令生成的值填入 .env 的 HARNESS_TOKEN，至少 32 字符。
openssl rand -hex 32
docker compose build
docker compose up -d
curl --fail http://127.0.0.1:8080/healthz
docker compose exec harness-server harness --url http://127.0.0.1:8080 health
```

`.env.example` 故意不含可用凭据。Compose 从 `.env` 注入 `HARNESS_TOKEN`，只在宿主机 **127.0.0.1:8080** 暴露服务，并将 named volume `harness_data` 挂载到 `/data`。容器内部监听 `0.0.0.0:8080`，与宿主机端口限制不冲突。公开 `/healthz` 仅表示进程存活；鉴权访问 `/v1/health` 才能检查 v1 接口。

Linux 服务器、HTTPS 反向代理、离线迁移、备份和升级详见 [部署文档](docs/DEPLOYMENT.md)。

在线备份使用独立Rust工具 `harness-backup`：固定不可变修订，服务继续处理请求；同一配置支持手动打包、OSS上传及每天systemd定时备份，失败保留本地副本并可重试。配置、凭据和恢复演练见 [在线备份与OSS](docs/BACKUP-OSS.md)。

## 本机运行

```sh
cargo build --release --locked --bin harness-server --bin harness
read -r -s HARNESS_TOKEN
export HARNESS_TOKEN
./target/release/harness-server --data-dir "$HOME/.local/share/harness-server" serve --bind 127.0.0.1:8080
```

`read` 时输入至少 32 字符的 token。另一个终端也需要设置同一 `HARNESS_TOKEN` 才能使用客户端。服务命令的参数顺序是 **`harness-server --data-dir /data serve --bind 0.0.0.0:8080`**；`--bind` 属于 `serve` 子命令。直接运行示例使用 loopback 监听。

## CLI 使用

优先从 GitHub Release 下载预构建客户端并按 [安装引导](docs/CLIENT-INSTALL.md) 放到 PATH；需要源码构建时也可使用：

```sh
cargo install --path . --locked --bin harness
```

以下示例假定客户端终端已设置 `HARNESS_TOKEN`；客户端从环境读取 Bearer token，不在配置中保存 token 值。

```sh
harness --url http://127.0.0.1:8080 health
harness --url http://127.0.0.1:8080 stats
harness --url http://127.0.0.1:8080 route --project demo --scenario 'coding task' --content
harness --url http://127.0.0.1:8080 start --agent codex --native-session-id '<原始任务ID>' --project demo --workstream-id demo-task --title task --scenario coding
```

记录实际 `start` 响应中的 Session ID、execution group（若有）及 Workstream SHA-256。`finish` 用 JSON 文件提交结果；先将下面的最小示例保存为本机 `/tmp/result.json`，并按实际结果修改：

```json
{
  "status": "COMPLETED",
  "current": "已完成部署文档和配置；仅做静态自检。"
}
```

```sh
harness --url http://127.0.0.1:8080 finish --session-id '<SESSION_ID>' --result-file /tmp/result.json --expected-workstream-sha256 '<SHA>'
# 若已有 execution group，可在同一命令追加 --execution-group-id '<GROUP>'。
harness --url http://127.0.0.1:8080 finish --mode log --project demo --result-file /tmp/result.json
harness --url http://127.0.0.1:8080 abort --session-id '<SESSION_ID>' --expected-sha256 '<Session SHA>'
harness --url http://127.0.0.1:8080 list --kind workstream --project demo --limit 3
harness --url http://127.0.0.1:8080 get --path '<文档路径>'
harness --url http://127.0.0.1:8080 put --path '<文档路径>' --file /tmp/document.md --expected-sha256 '<当前文档SHA>'
```

`finish --mode log` 是不创建 Session 的 log-only 用法。`abort` 仅撤销仍为空的临时记录，使用 Session 的当前 SHA。`list` 仅返回有界元数据摘要、不返回正文；可通过 `--offset` 分页，`--limit` 为 1–50。`put` 只有新建文档时可省略 `--expected-sha256`；覆盖已有文档必须提供当前哈希。尖括号内容须替换为真实值，文档路径为服务存储内的逻辑路径，不是本机绝对文件路径。

`finish` 结果支持 `status`（`IDLE`、`COMPLETED`、`WAITING_USER`）、`current`、`decisions`、`blockers`、`verification`、`next_steps`、`project_topics`、`process_learnings` 和 `process_id`。`project_topics` 中每个对象支持 `topic_id`、`title`、`summary`、`triggers`、`body`、`expected_sha256`。使用前核对本版 CLI 帮助及 [协议说明](docs/DEPLOYMENT.md#finish-结果与有限兼容范围)，不要将旧 Python finish JSON 或全部参数直接照搬。

### 客户端配置与项目推断

可通过 `--config <json文件>` 或 `HARNESS_CLIENT_CONFIG` 指定配置，示例：

```json
{
  "url": "http://localhost:8080",
  "token_env": "HARNESS_TOKEN",
  "projects": {
    "demo": "/home/agent/code/demo"
  }
}
```

`projects` 的目录必须是**运行客户端的机器上的绝对目录**；在 Linux 客户端中应换成对应的 Linux 路径。`token_env` 默认 `HARNESS_TOKEN`，指定的是环境变量名。配置不保存 token。

```sh
export HARNESS_CLIENT_CONFIG="$HOME/.config/harness-client.json"
harness --config "$HARNESS_CLIENT_CONFIG" --url http://127.0.0.1:8080 route --project demo --scenario coding --content
```

默认配置为用户配置目录下的 `harness-server/client.json`（Unix：`XDG_CONFIG_HOME` 或 `~/.config`；Windows：`APPDATA`）。地址优先级为 `--url` → `HARNESS_URL` → 配置 `url` → `http://localhost:8080`；没有配置地址仍需有效 token。Windows 项目路径写为 `C:\\code\\demo` 等 JSON 字符串。未显式指定 `--project` 时，客户端按当前目录选择配置中最长的祖先目录；必须唯一匹配，无匹配或有歧义时须明确传 `--project`。客户端只允许 **loopback 明文 HTTP**；访问外网或内网域名必须用 HTTPS。

### 写请求重试

所有写请求必须有 `Idempotency-Key`。客户端未指定 `--idempotency-key` 时会生成 UUID，并在发送请求前输出到 stderr；失败后保留该值，使用相同 key 重试同一次请求：

```sh
harness --url http://127.0.0.1:8080 put --path '<文档路径>' --file /tmp/document.md --expected-sha256 '<SHA>' --idempotency-key '<首次请求的KEY>'
```

显式复用 key 时保留原请求的操作和内容。新的逻辑写操作使用新 key。幂等 key 不能替代 SHA-256 并发检查。

## HTTP v1

所有 `/v1` 路由都需要 `Authorization: Bearer <token>`。所有 POST/PUT 写请求还必须带 `Idempotency-Key`；JSON 请求使用 `Content-Type: application/json`。

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| GET | `/healthz` | 公开 liveness，无鉴权 |
| GET | `/v1/health` | 鉴权健康接口 |
| GET | `/v1/stats` | HTTP 命令调用、耗时和失败统计 |
| GET | `/v1/documents?path=...` | 读取逻辑路径对应的文档 |
| GET | `/v1/documents?kind=workstream&project=demo&limit=3&offset=0` | 分页列出摘要，无正文 |
| POST | `/v1/route` | 路由记忆 |
| POST | `/v1/start` | 建立/恢复跟踪 |
| POST | `/v1/finish` | 完成跟踪或 log-only 提交 |
| POST | `/v1/abort` | 中止 Session |
| PUT | `/v1/documents` | JSON `{path, content, expected_sha256}` 写文档 |

`/v1/route` 是只读 POST，无需幂等键。`expected_sha256` 只在新建文档时允许省略。`/v1/stats` 仅覆盖本进程成功鉴权并进入处理器的调用，重启清零；不覆盖鉴权/JSON 解析失败，不等同原 CLI 持久统计。未观测字符数为 `null`，同时报告字符观测调用量；字符数不是 token。统计不代表业务净收益、任务质量或节省的 token。

```sh
curl --fail-with-body -H "Authorization: Bearer $HARNESS_TOKEN" http://127.0.0.1:8080/v1/health
curl --fail-with-body --get -H "Authorization: Bearer $HARNESS_TOKEN" \
  --data-urlencode 'path=<文档路径>' http://127.0.0.1:8080/v1/documents
```

## 存储与迁移

Markdown 正文存放在不可变 `revisions` 目录中；通过原子 `HEAD` 指针一次提交整个事务。`fs2` 文件锁保证一个数据目录只有一个 writer。修订、HEAD 和其他存储状态必须位于同一持久化卷，不拆分挂载、不并行启动多个实例写同一 store。

v0.1 保留全部修订，没有自动 GC；预留磁盘并监控使用量。备份应保留整个数据目录，而不是仅复制 HEAD 或当前 Markdown。

离线迁移命令：

```sh
harness-server --data-dir /data import --source '<agent-home目录>'
```

仅在**服务停止且 store 为空**时执行。导入保留 project/process/topics、全部 Session、Workstream 和 logs（含个人偏好）；不导入 token、runtime 或派生索引，也不执行原 Python 引擎。迁移是离线复制，部署后各设备访问同一 HTTP 服务；不做原目录与服务端的持续双向同步。原 Agent Home 全局入口未自动切换，切换方式见 [Agent 接入说明](docs/AGENT-BOOTSTRAP.md)。容器迁移及备份命令见 [部署文档](docs/DEPLOYMENT.md)。

## CI 与验证范围

[CI](.github/workflows/ci.yml) 在 Linux 上执行 `cargo fmt --check`、`cargo clippy --locked --all-targets -- -D warnings`、`cargo test --locked`，并用 Docker 构建包含两个二进制的镜像。构建依赖已存在的 `Cargo.lock`；缺失时由实现维护者提供，部署配置不会自动重新解析依赖。

源码快照的既有验证范围和限制见 [验证说明](docs/VALIDATION.md)。本仓库的独立运行记录以 [Actions](https://github.com/YS1109/harness-server-public/actions) 为准，不将旧仓库的运行结果标记成本仓库CI。公开仓库使用标准GitHub托管runner；产物与存储额度按GitHub计费规则核对。
