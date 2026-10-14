# Harness v0.3 部署与使用

适用于 Rust 原生 `harness-server` / `harness` 0.1.0。Linux 服务器使用 Docker Engine + Compose v2；macOS 本地使用 Docker Desktop + Compose v2。直接编译需要 Rust 1.88 或更高版本及完整源码中的 `Cargo.lock`。以下 Linux 路径为示例，须替换为实际路径。

默认本地域名为 `localhost`，客户端未配置地址时连接 `http://localhost:8080`。AI 引导配置见 [AI-SETUP.md](AI-SETUP.md)。下面是源码构建入口；首次成功发布 GHCR 镜像后，也可使用 [compose.image.yaml](../compose.image.yaml) 直接拉取部署，步骤见 [IMAGES.md](IMAGES.md)。

## 1. 配置 token 与启动

在源码目录准备 `.env`：

```sh
cp .env.example .env
chmod 600 .env
openssl rand -hex 32
```

将生成的值填入 `.env` 的 `HARNESS_TOKEN=`。示例文件中的空值不能启动服务。不要提交 `.env`；`.dockerignore` 会排除它以及本地默认 `data/` 和 `backups/`，避免凭据、存储及备份进入镜像构建上下文。避免把含 token 的 `docker compose config` 输出保存到日志；仅检查配置可以用 `docker compose config --quiet`。

```sh
docker compose config --quiet
docker compose build
docker compose up -d
docker compose ps
curl --fail http://127.0.0.1:8080/healthz
docker compose exec harness-server harness --url http://127.0.0.1:8080 health
```

服务实际命令是：

```sh
harness-server --data-dir /data serve --bind 0.0.0.0:8080
```

容器以 UID/GID `10001:10001` 运行，根文件系统只读，`/data` 为可写 named volume，`/tmp` 为临时文件系统。新 named volume 从镜像中的 `/data` 初始化目录权限。若改用已有卷或 bind mount，必须确保该 UID/GID 可写；不要递归修改原 agent-home 的所有权。

宿主机只绑定 `127.0.0.1:8080`。`/healthz` 是公开 liveness，Docker HEALTHCHECK 调用它；它不证明凭据有效、存储可写或 v1 业务可用。另用携带 Bearer token 的 `/v1/health` 或 CLI `health` 检查鉴权接口。Docker unhealthy 状态本身不会触发 Compose 自动重启；`restart: unless-stopped` 针对容器进程退出。

### 多设备 token

默认 Compose 使用单个 `HARNESS_TOKEN`，至少 32 字符。服务也支持用 `HARNESS_TOKENS_JSON` 替代单 token：它是 JSON 对象 `{device_id: token}`，每个 token 都应至少 32 字符。下面仅为结构展示，不可原样部署：

```json
{"laptop":"<设备一至少32字符的随机token>","linux-agent":"<设备二至少32字符的随机token>"}
```

选择多设备模式时，调整服务的环境配置，移除 `HARNESS_TOKEN` 注入，改为必填的 `HARNESS_TOKENS_JSON`，例如：

```yaml
environment:
  HARNESS_TOKENS_JSON: "${HARNESS_TOKENS_JSON:?Set a device-to-token JSON object}"
```

在 `.env` 中用单引号包住完整 JSON 值。两种服务端模式择一配置，避免依赖未明确约定的优先级。客户端仍从自己的 `HARNESS_TOKEN`（或 `token_env` 指定的变量）读取该设备的 token，并发送普通 Bearer 头；无需向客户端提供整个对象。

修改 token 后，用 `docker compose up -d --force-recreate harness-server` 重建容器以加载环境，并更新对应客户端环境。凭据不写入文档 store 或客户端 JSON 配置。

## 2. Linux 服务器与 HTTPS 反向代理

将完整源码放到例如 `/opt/harness-server`，在该目录运行上述 Compose 命令。Compose 项目名决定 named volume 的实际名称；部署后保持项目名/目录一致。若需要固定名称，可在每个命令中一致使用 `docker compose -p harness ...`，不要升级时才换项目名，否则可能挂载到新的空卷。

外网客户端必须通过 HTTPS。在 Linux 宿主机部署反向代理，向 `127.0.0.1:8080` 转发；只公开代理的 HTTPS 入口，不公开 8080。准备域名、DNS、证书和代理服务属于部署者的实际环境配置，本次没有执行联网部署或签发证书。

已配置证书的 Nginx 示例（替换域名和证书路径，作为 HTTPS server 块）：

```nginx
server {
    listen 443 ssl;
    server_name harness.example.com;
    ssl_certificate /etc/nginx/tls/fullchain.pem;
    ssl_certificate_key /etc/nginx/tls/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header Authorization $http_authorization;
        proxy_set_header Idempotency-Key $http_idempotency_key;
    }
}
```

代理必须保留 `Authorization` 和 `Idempotency-Key`。不要记录 Bearer 头，也不要对 POST/PUT 自动改写请求体或重试为新 key；客户端超时后应按原 key 重试原请求。按实际文档大小和调用耗时配置代理请求体限制及超时，不将 `/healthz` 当作完整业务健康证明。

```sh
harness --url https://harness.example.com health
harness --url https://harness.example.com route --project demo --scenario 'coding task' --content
```

上述代理在**宿主机**运行。如果把代理放进容器，容器中的 `127.0.0.1` 指向代理自身；应与服务使用同一 Docker 网络，并将 upstream 改为 `http://harness-server:8080`。HTTPS 是外部访问入口；同机容器网络中的 HTTP upstream 不改变客户端必须使用 HTTPS 的限制。

## 3. 离线导入原 agent-home

导入只支持**停止服务后的空 store**，不能合并到已运行或已有数据的 store。首次安装计划迁移时，先构建镜像，执行导入，再启动服务；不要先 `up` 初始化目标。若目标已有数据，先备份并选择独立的空目标，不删除已有卷来强行导入。

原生命令：

```sh
harness-server --data-dir /srv/harness-data import --source /srv/agent-home
```

Compose 命令使用同一个 named volume `/data`，将源目录只读挂载到 `/import`：

```sh
docker compose build
docker compose stop harness-server
docker compose run --rm --no-deps \
  --volume /srv/agent-home:/import:ro \
  harness-server --data-dir /data import --source /import
docker compose up -d harness-server
```

在 Mac 上把 `/srv/agent-home` 换为本机 `$AGENT_HOME` 对应的绝对路径，并确保 Docker Desktop 允许访问该路径。导入仍使用非 root UID 10001，源目录需要对它可读；如权限不满足，使用只读的迁移副本并为副本设置读取权限，勿修改原 agent-home。

导入保留 project/process/topics、全部 Session/Workstream/logs。不导入 token、runtime、派生索引，不运行原 Python 引擎。迁移后通过 CLI `get --path '<文档路径>'` 核对已知项目、流程、专题及历史记录；不要仅凭 `/healthz` 判断迁移完整。保留源目录备份，迁移是离线复制而非持续双向同步。

## 4. 客户端配置与运行位置

客户端配置通过 `harness --config <json文件> ...` 或环境变量 `HARNESS_CLIENT_CONFIG` 选择：

```json
{
  "url": "https://harness.example.com",
  "token_env": "HARNESS_TOKEN",
  "projects": {
    "demo": "/home/agent/code/demo"
  }
}
```

配置中只放 URL、token 环境变量名和项目映射，不放 token 值。项目路径必须是客户端所在机器的绝对路径，服务容器不需要挂载这些项目源码。推断项目时使用当前工作目录的最长祖先匹配；无法唯一匹配时明确指定 `--project`。在其他机器、容器或目录运行客户端时，重新配置映射或明确传项目。

`HARNESS_TOKEN` 为默认 token 环境变量名；可以在配置中指定其他 `token_env`。HTTP 明文只允许 loopback；LAN、公网地址均使用 HTTPS，不能以 HTTP 直接调用远端 8080。

### finish 结果与有限兼容范围

受支持的结果字段：

| 字段 | 用途 |
| --- | --- |
| `status` | `IDLE`、`COMPLETED` 或 `WAITING_USER` |
| `current` | 当前进展 |
| `decisions` | 已确定的决定 |
| `blockers` | 阻塞事项 |
| `verification` | 实际验证范围和结果 |
| `next_steps` | 续接工作 |
| `project_topics` | 专题对象列表，支持 `topic_id/title/summary/triggers/body/expected_sha256` |
| `process_learnings` | 稳定、可复用的流程增量 |
| `process_id` | 对应流程标识 |
| `process_title` | 新建流程时的标题 |

专题还支持显式 `verified_at` 与 `verification_evidence`；普通更新保留旧核验声明。修改已有专题时必须使用它的当前 SHA，正文是完整替换，不是追加。

将结果写入本机 `/tmp/result.json`，由 `--result-file` 读取。提交跟踪结果需要实际 Session ID 和当前 Workstream SHA-256：

```sh
harness --url http://127.0.0.1:8080 finish \
  --session-id '<SESSION_ID>' \
  --result-file /tmp/result.json \
  --expected-workstream-sha256 '<SHA>'
```

如需关联已知 execution group，追加 `--execution-group-id '<GROUP>'`。log-only 使用 `finish --mode log --project demo --result-file /tmp/result.json`，不创建 Session。`abort --session-id '<SESSION_ID>' --expected-sha256 '<Session SHA>'` 仅撤销仍为空的临时记录；使用 Session SHA，不是 Workstream SHA。

v1 不支持旧 Python 的全部 flags/fields，请求中的未知字段会明确拒绝。只提交本版明确支持的字段；使用 `harness finish --help` 核对参数。未实现旧引擎的 `project_facts`、`replacements`、`process_replacements` 等字段；精确纠错需要读取当前文档后使用带 SHA 的 `put`，不向新正文追加相反规则。

跨设备续接时可先 `harness list --kind workstream --project demo --limit 3`，再对选中的逻辑路径 `get`。列举只返回元数据与 SHA，不返回正文；默认 20 条，可设 1–50 条并用 `--offset` 分页。Session 的原生 ID 仍按 Agent 生成，换会话时以新 Session 关联已知 Workstream，不伪造旧 ID。

## 5. HTTP 写入、幂等与并发

所有 `/v1/*` 请求带 `Authorization: Bearer <token>`；所有写请求带 `Idempotency-Key`。写接口是 POST `/v1/start`、`/v1/finish`、`/v1/abort` 和 PUT `/v1/documents`；POST `/v1/route` 是只读接口，无需幂等键。直接调用 JSON 接口须带 `Content-Type: application/json`，请求体上限 1 MiB。

HTTP `finish` 与 CLI 结果文件不同，HTTP 必须把结果放在 `result` 对象内。例如保存为 `/tmp/finish-request.json`：

```json
{
  "mode": "tracked",
  "session_id": "codex-example",
  "expected_workstream_sha256": "<start返回的Workstream SHA>",
  "result": {
    "status": "IDLE",
    "current": "已完成当前阶段",
    "verification": ["如实填写实际验证"]
  }
}
```

```sh
REQUEST_KEY="$(uuidgen)"
curl --fail-with-body --request POST http://127.0.0.1:8080/v1/finish \
  --header "Authorization: Bearer $HARNESS_TOKEN" \
  --header "Idempotency-Key: $REQUEST_KEY" \
  --header 'Content-Type: application/json' \
  --data-binary @/tmp/finish-request.json
```

直接 HTTP 接入的 Agent 不需要安装 Rust 客户端。接入约定见 [Agent 接入说明](AGENT-BOOTSTRAP.md)。

直接写文档只接受受管的逻辑相对路径，不能指定绝对路径或派生索引。非日志 Markdown 必须满足对应类型的 YAML front matter 和正文校验，不能只发送任意 Markdown 正文。

新建流程文档的示例：先将下面 JSON 保存为本机 `/tmp/document-put.json`。`memory/process/deployment-example.md` 的文件名与 `process_id` 一致；如果该路径已经存在，先读取当前 SHA 并在 JSON 中增加 `expected_sha256`。

```json
{
  "path": "memory/process/deployment-example.md",
  "content": "---\nschema_version: 1\ntitle: Deployment example\nprocess_id: deployment-example\nmemory_type: process\nstatus: ACTIVE\nverified_at: unknown\n---\n\n# Example\n"
}
```

```sh
REQUEST_KEY="$(uuidgen)"
curl --fail-with-body --request PUT http://127.0.0.1:8080/v1/documents \
  --header "Authorization: Bearer $HARNESS_TOKEN" \
  --header "Idempotency-Key: $REQUEST_KEY" \
  --header 'Content-Type: application/json' \
  --data-binary @/tmp/document-put.json
```

修改已有文档时，JSON 必须增加 `"expected_sha256":"<当前文档SHA>"`。用 GET `/v1/documents?path=...` 或 `harness get --path ...` 读取文档及对应版本信息；请求路径须 URL 编码。CLI `put` 读取本机 `--file`，服务端 `path` 则是 store 内的逻辑路径，两者用途不同。

幂等重试使用**同一个 key、同一操作和原内容**。CLI 未指定 `--idempotency-key` 会生成 UUID，发请求前将 key 写到 stderr；失败重试显式传回该值。网络断开不代表未提交，先保留原 key 重试，不随手生成新 key。新的逻辑操作才使用新 key。

幂等与乐观并发是两种独立机制。哈希冲突时重新读取当前文档或 Workstream，审查和整合差异；如提交内容发生变化，将其作为新的操作使用新 key 和新的预期哈希。

## 6. 持久化、备份与恢复

实际 Markdown 存在不可变 `revisions` 目录中，原子切换 `HEAD` 提交整个事务。整个数据目录挂载到同一持久化卷，不能把 HEAD 和 revisions 分到不同文件系统，也不要手工编辑它们。`fs2` 锁限制同一 store 为单 writer，不支持多个副本共同写卷；锁冲突时检查是否仍有旧服务或导入进程运行。

v0.1 保留全部修订与幂等回执，未实现自动 GC。每次写入保存完整快照，持续写入会增长磁盘占用；监控整个卷容量，不手动删除“旧”修订。活跃 Markdown 上限 4096 份 / 32 MiB，普通单文档上限 256 KiB，日志 2 MiB；正文预算 Session 600、Workstream 1200、项目/流程 2000、专题 6000 字，不自动截断。

`/v1/stats` 仅统计本进程内成功鉴权并进入业务处理器的调用，重启清零；包括重试调用成本，不含认证/JSON解析失败及模型、网络耗时；daily、health/stats 不采集。未观测字符数为 `null`，返回字符数也不是 token。它不等同原 CLI 的持久化统计，不衡量业务净收益，也不能代替磁盘监控。

日常不停机备份可使用 [在线备份与OSS](BACKUP-OSS.md) 中的 `harness-backup`，包含手动命令、独立配置与每日systemd任务；它固定不可变HEAD修订，备份完整文档/回执及已有完整历史修订。工具运行在部署机，原服务镜像无需更新；OSS上传另需官方ossutil。

需要停止写入的维护场景，也可使用下面的离线完整目录备份。使用 Compose 临时容器读取同一卷，备份文件留在容器外：

```sh
mkdir -p backups
docker compose stop harness-server
docker compose run --rm --no-deps -T --entrypoint tar \
  harness-server -C /data -czf - . > backups/harness-data.tar.gz
# 只有上一条命令成功退出，才将该文件视为有效备份。
docker compose up -d harness-server
```

备份包含整个 store；`.env` 中的凭据单独保存，不在 store 备份内。Compose `stop` / `down` 不删除 named volume，**`docker compose down -v` 会删除它**。不要在升级或排障时使用 `down -v`。

恢复时先停服务，保留现有数据备份，选用空的目标 named volume，并确保 UID/GID 10001 可写。在该目标对应的 Compose 项目目录执行：

```sh
docker compose run --rm --no-deps -T --entrypoint tar \
  harness-server -C /data --no-same-owner -xzf - < backups/harness-data.tar.gz
docker compose up -d harness-server
docker compose exec harness-server harness --url http://127.0.0.1:8080 health
```

`-T` 让归档通过 stdin 输入；`--no-same-owner` 以容器用户恢复文件。还应使用 `get` 核对已知文档及 SHA-256。恢复不是在已有 store 上叠加文件，不要同时运行服务和恢复操作。

## 7. 升级与故障定位

升级前备份，保持 Compose 项目名、持久化卷和凭据来源一致。从目标版本的完整源码构建新镜像，停止旧 writer 后替换容器：

```sh
docker compose build
docker compose stop harness-server
docker compose up -d harness-server
docker compose logs --tail 100 harness-server
docker compose exec harness-server harness --url http://127.0.0.1:8080 health
```

回退须确认旧二进制支持当前存储格式；不兼容时使用升级前的完整 store 备份及对应版本镜像，不混合不同版本的修订数据。

| 现象 | 检查 |
| --- | --- |
| Compose 拒绝缺失 token | 检查当前目录 `.env` 中 `HARNESS_TOKEN` 非空且至少 32 字符 |
| `/healthz` 成功但 v1 鉴权失败 | 核对客户端 token 与服务配置，检查代理保留 Bearer 头 |
| 远端 HTTP URL 被客户端拒绝 | 使用有效证书的 HTTPS 地址 |
| 写入缺少 key / 重试不一致 | 保留并复用首次 stderr key 和原请求内容 |
| SHA 冲突 | 重新读取和合并，不盲目覆盖 |
| store 锁冲突 | 停止旧 writer / importer，同卷只运行一个服务 |
| `/data` 权限失败 | 检查卷是否允许 UID/GID 10001 读写 |
| 导入失败 | 核对源目录可读、服务已停、目标 store 为空 |
| `--locked` 构建失败 | 检查 Cargo.lock 和源码是否属于同一版本，以及 Rust 工具链要求 |
| 磁盘空间不足 | 检查完整卷容量；本版没有自动 GC |

## 验证边界

源码快照验证范围见 [验证说明](VALIDATION.md)。本仓库CI覆盖格式、Clippy、Rust契约测试、Windows客户端及Docker持久卷/重启检查；部署环境、备份恢复和业务调用需在使用方环境单独验证。

## v0.3 空间管理与多空间接入

[接入指南](AGENT-NAMESPACES.md)覆盖 `/admin`、独立 HARNESS_ADMIN_TOKEN、服务生成空间 ID、持久授权和审计、客户端 init/namespaces/connect。部署身份文件只绑定凭据，页面管理空间授权和默认范围；一位 Agent 可接入多个空间，每次写入只落一个空间。

route/start 可带 X-Harness-Read-Namespaces，所有空间共用读取预算。响应来源为 namespace/path；同专题 ID 在多个空间出现时需 topic_namespace。Session 无显式 namespace 时定位已有记录，多个同ID则拒绝猜测；start/finish不能因默认值变化创建另一份交接。文档 API 不允许 control 或 namespaces 物理路径。

只使用本机或GitHub构建产物，先保留升级前备份并升级备份工具，再部署和只读核验，最后切客户端及Agent入口。产生control记录后旧服务/备份工具不兼容；降级恢复升级前快照，不改写旧ID。完整配置和HTTP字段见接入指南。
