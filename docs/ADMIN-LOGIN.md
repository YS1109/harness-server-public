# 管理员登录与会话

`/admin` 默认使用用户名和密码登录。管理员登录独立于 Agent 凭据，没有开放注册接口。原有独立管理员 Bearer 仍可用于 API、自动化和初次配置；页面的“高级：独立管理员令牌”会将令牌换成浏览器会话，后续请求不再发送原始令牌。

## 配置

单服务设置以下环境变量：

| 变量 | 用途 |
| --- | --- |
| `HARNESS_ADMIN_USERNAME` | 管理员用户名，1–128 UTF-8 字节，不含首尾空白或控制字符 |
| `HARNESS_ADMIN_PASSWORD_HASH` | 离线生成的 Argon2id PHC 字符串，由私有环境或凭据管理器注入 |
| `HARNESS_ADMIN_SECURE_COOKIE` | 默认 `true`；HTTPS 或终止 HTTPS 的可信反向代理使用 `true` |
| `HARNESS_ADMIN_TOKEN` | 原有独立管理员令牌，可选；始终与 Agent 令牌分开 |

用户名与密码哈希必须同时配置；空字符串按未配置处理，仅配置一项会拒绝启动。不配置两者时关闭密码登录，原有管理员 Bearer API 及令牌换会话入口仍可用。仅配置用户名和密码时，也能使用完整的浏览器管理功能。`HARNESS_ADMIN_SECURE_COOKIE` 仅接受严格的 `true` 或 `false`，未配置时为 `true`；即使未启用密码登录也会校验该值，不接受空值、`1` 或 `0`。

离线生成哈希：

```sh
harness-server hash-password
```

工具在终端隐藏输入并要求再次确认，不接受密码命令参数或管道。它只输出哈希。将输出交给私有凭据配置，不要把密码或哈希写进 Markdown、仓库、工单或聊天记录。生成要求密码为 12–1024 UTF-8 字节；推荐由密码管理器生成。

生成参数为 Argon2id v19、19 MiB 内存、两轮、单通道、16 字节随机盐和 32 字节输出。配置验证只接受 19–64 MiB、2–4 轮、单通道以及 32 字节输出，避免过弱参数及超大参数导致资源耗尽。验证在阻塞任务中运行，不持有业务 Store 锁；未知用户名也执行同样的密码验证。所有租户共用进程级的两个密码验证名额，先取得名额再调度计算；繁忙时立即返回 HTTP 429 和 `Retry-After: 60`，不排队积压验证任务。名额通过 RAII 在计算完成或异常退出时释放。

本机 HTTP 开发需要显式设置 `HARNESS_ADMIN_SECURE_COOKIE=false`，并通过 HTTP 同源访问。该变量也适用于仅使用管理员令牌的会话交换。生产环境保持 `true`，并通过 HTTPS 访问。服务不会根据客户端提交的 `X-Forwarded-Proto` 或其他转发头放宽检查；反向代理需要保留正确的 `Host`。开启 Secure 时，浏览器写请求的 Origin 必须是同 Host 的 HTTPS Origin；显式关闭时要求 HTTP Origin。

## 浏览器行为

页面支持密码管理器的用户名和当前密码自动填写。加载或刷新时自动向服务确认现有会话；“保持登录 30 天”允许浏览器重启后继续使用未过期会话。未勾选时使用浏览器会话 Cookie，服务端最长有效期为 12 小时。浏览器自身的会话恢复功能可能保留会话 Cookie，服务端的固定期限始终有效。

会话 Cookie 使用随机值，包含 `HttpOnly`、`SameSite=Strict`、`Path=/admin`，默认附带 `Secure`。勾选保持登录时附带固定 30 天的 `Max-Age`。服务器仅保留会话值的哈希；JSON 响应不返回 Cookie 值、密码、密码哈希或 Bearer。读取会话及执行管理操作不会延长有效期。

每个服务实例最多保留 32 个会话，达到上限后淘汰最早创建的会话。每分钟最多接受十次登录尝试，包括成功、失败及令牌换会话请求。限制按服务实例或租户整体执行，不根据不可信转发 IP 分组。过量尝试返回 HTTP 429 和 `Retry-After: 60`。

退出先向服务撤销当前会话，再清除页面数据和 Cookie。若网络故障，页面明确提示“退出尚未确认”，允许重试。修改中的文档、冲突草稿及结果未知的原请求不会因为会话过期而丢失；重新登录原租户后继续处理，结果未知的写入仍使用原始请求和幂等键。刷新页面依然会丢失这些仅在页面内存中的草稿。

会话保存在进程内存中。重启服务或重新创建路由器会使现有会话失效，需要重新登录。每个实例的 Cookie 名含随机后缀；旧实例的 Cookie 无法用于新实例，保持登录的旧 Cookie 由浏览器按原期限清理。

## 多租户

网关页面提供可选“租户 ID”，单服务可留空。登录后的工作台页头持续显示“租户 ID”或“单租户”，所有视图均保留该上下文；第一版租户由服务配置提供，不在页面在线创建。页面只将最后选择的租户 ID 写入 `localStorage`；用户名、密码、原始令牌和会话值均不写入 Web Storage。已有列表筛选仍可写入 `sessionStorage`，并按租户分别保存；报告、文档草稿及待确认写入仍只保留在当前页面内存。

选中租户后，所有管理请求，包括登录、恢复会话、退出和文档操作，均带 `X-Harness-Tenant`。网关必须使用该头将 `/admin/api/*` 分发给对应的服务实例；该头只选择目标，不授予权限。每个租户分别校验凭据、维护登录限流和会话状态，只有授权成功才能访问管理数据。

每个 `AdminSessions` 实例使用独立的随机 Cookie 名，即使不同租户使用相同用户名与密码哈希，登录 B 也不会覆盖 A。浏览器可以同时持有多个 `/admin` Cookie；每个实例只读取自己的 Cookie。切换租户时旧请求会失效，草稿或待确认写入要求先在原租户处理或显式退出。

## HTTP 契约

| 方法 | 路径 | 行为 |
| --- | --- | --- |
| POST | `/admin/api/login` | JSON `username`、`password`、可选 `remember`；成功设置会话 Cookie |
| POST | `/admin/api/login` | 显式携带独立管理员 `Authorization: Bearer …`，JSON 只带可选 `remember`，交换会话 |
| GET | `/admin/api/session` | 验证当前实例 Cookie；返回 `authenticated`、`expires_at`、`remember` |
| POST | `/admin/api/logout` | 撤销当前 Cookie 会话并清除 Cookie；返回 `{"authenticated":false}`，重复退出安全 |

登录成功返回与会话查询相同的非敏感字段；失败不设置会话 Cookie。非法凭据、未知用户及过期会话统一返回 HTTP 401；没有任何管理配置时，原管理数据接口返回 HTTP 503。请求体限制为 4 KiB。不存在账户注册路由。

登录、退出及所有 Cookie 鉴权的 POST/PUT 等写请求必须包含：

- 与服务器配置的协议及请求 Host 严格匹配的 `Origin`，不接受 `null`、路径或其他 Origin。
- `X-Harness-Admin-Request: 1`。
- 若包含 `Sec-Fetch-Site`，其值必须为 `same-origin`。

这些约束也应用于管理诊断及预览的 POST。浏览器自动提供 Origin；页面提供自定义请求头。已有独立 Bearer API 继续接受不带 Origin 的非浏览器请求，但一旦提供 Origin，仍必须同源。显式提供无效 Bearer 时不会回退借用有效 Cookie。现有管理写入的幂等键、SHA 冲突检查、授权边界和原子保存要求保留。

## Rust 集成

`admin_session` 模块导出：

```rust
pub struct LoginConfig {
    pub username: String,
    pub password_hash: String,
    pub secure_cookie: bool,
}

impl LoginConfig {
    pub fn new(
        username: impl Into<String>,
        password_hash: impl Into<String>,
    ) -> crate::error::Result<Self>;
    pub fn from_environment() -> crate::error::Result<Option<Self>>;
}

impl AdminSessions {
    pub fn new(config: Option<LoginConfig>) -> crate::error::Result<Self>;
    pub fn with_policy(
        config: Option<LoginConfig>,
        secure_cookie: bool,
    ) -> crate::error::Result<Self>;
    pub fn disabled() -> Self;
}

pub fn hash_password(password: &str) -> crate::error::Result<String>;
```

`AdminSessions` 也实现 `Default`，密码登录默认关闭，默认 Cookie 策略为 Secure。`new(None)` 允许通过显式 `HARNESS_ADMIN_SECURE_COOKIE` 调整仅 Bearer 换会话的本机开发策略；`Some(LoginConfig)` 以传入配置为准。多租户通过`with_policy`显式传入该租户策略，包括仅令牌登录的租户；不会继承全局管理员环境配置。

App 持有 `Arc<AdminSessions>`；扩展入口为 `api::router_with_admin_login(store, principals, admin_token, Option<LoginConfig>)`。原有 `router_with_admin` 转发 `None`。初始化管理目录时，管理员启用条件为“存在独立管理员令牌，或启用密码登录”。每个租户必须分别构造一个 App 和会话管理器。

服务 feature 需要 `argon2 0.5` 的 `std`、`rand_core 0.6` 的 `getrandom`，以及项目已有的 UUID v4。父级 CLI 负责隐藏输入，调用离线 `hash_password` helper；登录模块不处理终端或持久化凭据。

## 验证范围

`tests/admin_sessions.rs` 覆盖配置参数边界、密码登录、会话恢复与轮换、保持登录期限、退出撤销、Cookie CSRF、独立 Bearer 兼容、Agent 凭据隔离、租户会话隔离及限流。模块单测使用单调时间验证固定过期与容量边界，不依赖长时间休眠。`tools/test_console.cjs` 保留 v0.5 文档编辑、冲突、诊断和未知写入测试，并覆盖会话恢复、租户请求头、令牌交换、退出失败及过期后原请求重试。
