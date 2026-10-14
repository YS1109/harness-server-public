# GitHub 构建与 GHCR 镜像

工作流 [Publish image](../.github/workflows/publish-image.yml) 使用 GitHub Actions 构建，发布到 `ghcr.io/ys1109/harness-server-public`。支持 `linux/amd64` 与 `linux/arm64`，包含服务端、Rust CLI和在线备份工具`harness-backup`。实际发布状态以本仓库Actions和包目录为准；源码快照的验证范围见 [验证记录](VALIDATION.md)。

## 发布

将工作流推送到 GitHub 后，可在 Actions 中手动运行 **Publish image**（默认选择 main）。也可推送版本标签，例如与当前 Cargo 版本一致的 `v0.3.2`；标签与 Cargo.toml 不一致时拒绝发布。发布前自动调用 CI：Linux 格式/Clippy/测试、容器检查及 Windows 客户端编译；全部通过后才上传镜像。

工作流使用仓库内置 `GITHUB_TOKEN` 和 `packages: write`，无需把个人 PAT 或 Harness token 配置成发布 secret。GHCR 新包通常默认私有，发布后检查包的访问权限及仓库关联，公开源码不代表GHCR包已公开；按实际包访问权限验证匿名拉取。保持只向可信用户提供记忆服务。

| 标签 | 含义 |
| --- | --- |
| `sha-<完整提交SHA>` | 精确对应一次源码提交 |
| `0.3.2` 等版本号 | 由 `v0.3.2` 等有效版本标签产生 |
| `latest` | main 手动发布或无预发布后缀的版本标签发布时更新 |
| `<镜像名>@sha256:<digest>` | 发布摘要记录在 Actions summary，可用于固定部署 |

非 main 分支的手动发布仅生成 SHA 标签；预发布版本不更新 latest。标签可以再次发布，因此要求不可变部署时使用 digest。版本标签应与 Cargo.toml 一致；不要将新版本标签贴在未升级的软件上。

## 原生并行构建与缓存

发布前保留完整 CI 检查。amd64 使用 `ubuntu-24.04`，arm64 使用 `ubuntu-24.04-arm`，各自原生编译，不启用 QEMU。这两种标准runner支持公开仓库；实际排队与存储额度按GitHub账户配置和计费规则核对。

每个架构先推送 digest 镜像，再在对应 runner 上验证服务健康、客户端鉴权健康接口、备份工具版本及非 root 用户。只有两边都成功，合并任务才验证平台清单并更新原有 SHA、版本和 latest 标签；一个架构失败不会发布缺少另一架构的标签。Actions summary 记录最终多架构 digest，部署命令无需区分架构。

Docker 使用固定版本 `cargo-chef 0.1.71`，Rust 仍为 1.88.0。依赖清单形成单独缓存层：依赖不变时，修改业务代码可复用依赖编译产物；文档不作为编译层输入。增加 build.rs、嵌入资源或 workspace 成员时，需同步更新 Dockerfile 的输入复制规则。

构建缓存按 `harness-image-amd64` / `harness-image-arm64` 分开。Docker CI与发布复用amd64缓存，Rust/Windows CI使用独立Cargo缓存；GitHub分支缓存可见性与淘汰仍适用，缓存失效时正常重新构建。新仓库首次构建使用冷缓存，不用单次耗时推断固定加速比例。

## 镜像访问权限

镜像发布并设为public后可匿名拉取；若实际包为private，部署机需要GHCR读取权限。通常使用拥有包访问权限、带 `read:packages` 的 PAT classic 登录；组织场景还需按实际 SSO/包权限配置。安全地把凭据输入 Docker：

```sh
read -r -s GHCR_READ_TOKEN
printf '%s' "$GHCR_READ_TOKEN" | docker login ghcr.io --username '<GitHub用户名>' --password-stdin
unset GHCR_READ_TOKEN
```

不要将登录凭据写入聊天、Git、`.env.example` 或 Harness 记忆。Docker 自身的凭据存储按部署机环境管理。登录成功不等于包访问权限已验证，实际 pull 结果才是依据。

## 使用发布镜像部署

在仓库目录准备 `.env` 并配置 `HARNESS_TOKEN`；`HARNESS_IMAGE` 默认是 `ghcr.io/ys1109/harness-server-public:latest`，可改为已发布版本或 digest。无需在部署机安装 Rust，也不执行源码构建：

```sh
docker compose -f compose.image.yaml config --quiet
docker compose -f compose.image.yaml pull
docker compose -f compose.image.yaml up -d
curl --fail http://localhost:8080/healthz
docker compose -f compose.image.yaml exec harness-server harness health
```

端口仍仅绑定 `127.0.0.1:8080`，持久化卷名和原 Compose 一致。默认本地域名为 localhost；远端访问配置 HTTPS 反向代理，客户端不能以明文 HTTP 访问公网或内网域名。持久卷、备份、token 和记忆不打进镜像。

后续所有操作都使用同一个 `-f compose.image.yaml` 和相同 Compose 项目名，避免意外创建另一个空卷。迁移、备份、恢复可使用 DEPLOYMENT 中的命令，但将 `docker compose` 改为 `docker compose -f compose.image.yaml`；将 `build` 替换为 `pull`，其余服务名、卷与非 root 权限不变。

升级前备份完整 store，确认目标镜像存在，再 pull/up；不要执行 `down -v`。首次迁移仍须先 pull、在空 store 离线 import，再启动服务。源代码构建入口 `compose.yaml` 继续可用，便于镜像尚未发布或本机开发时运行。
