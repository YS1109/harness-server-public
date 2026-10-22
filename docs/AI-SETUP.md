# 让 AI 引导配置 Harness

将本文件交给本机或云端的 AI，并说明“按这份文档帮我配置 Harness”。本文件是配置引导，不是每个任务都要读取的全局提示词。日常协议使用 [AGENT-BOOTSTRAP.md](AGENT-BOOTSTRAP.md)。

## 给配置 Agent 的约定

- 先读取 README 的相关入口，检查操作系统、客户端、现有配置与实际执行环境；已有信息不重复询问。
- 默认使用本地域名 `localhost`，服务地址 `http://localhost:8080`。不修改 hosts、DNS 或证书；`localhost` 永远指向运行客户端的机器，不指向另一台云机。用户提供云端地址时改用 HTTPS；自定义内网域名也需要 HTTPS，不放宽客户端校验。
- 只询问仍缺少且会影响操作的信息：连接本机还是已有远端、实际项目 ID 与本机根目录、要接入哪个 Agent/机器人。需要部署或迁移时，再识别来源目录、空目标与持久卷；不要为简单客户端配置重复创建跟踪记录。
- 凭据由用户在本机安全输入或通过已有凭据系统注入。不要让用户把 token 发到聊天，不输出 token、完整环境、`.env` 或 Docker 的展开配置；可以检查变量是否存在，不打印值。不要生成凭据并假定远端已认可它。
- 备份已有配置，合并项目映射，保留其他规则；显式 `--url` 优先于 `HARNESS_URL`、配置 `url`、默认地址。不要把 CLI 默认地址误认为已验证可用。
- 只有真实连接和必要迁移已验证，才切换日常调用入口。服务未就绪时保留当前工作路径，如实说明缺口；不双写两份记忆、不把网络错误解释为保存成功。

## 1. 安装客户端

优先检查已有 `harness --version`；需要安装时，从 [GitHub Releases](https://github.com/YS1109/harness-server-public/releases) 选择本机 OS / 架构对应的包，校验 `SHA256SUMS` 后解压，按包内 `INSTALL.md` 配置 PATH。预构建包支持 macOS Apple Silicon / Intel、Linux ARM64 / x64 和 Windows x64，不需要 Rust。兼容基线和具体命令见 [CLIENT-INSTALL.md](CLIENT-INSTALL.md)。公开仓库Release可直接下载；以实际发布版本为准。

已有源码且选择从源码安装时，在仓库根目录执行：

```sh
cargo install --path . --locked --bin harness
harness --version
```

源码安装需要 Rust 1.88 或以上。没有源码时先克隆README中的公开仓库。Windows 同样可使用 Cargo 安装。两种方式都要检查实际工具进程的 PATH 能找到 `harness` / `harness.exe`，不能只验证另一个终端。

## 2. 配置连接和项目映射

v0.4客户端可用下面的统一入口，URL使用全局参数，token仍由已有执行环境注入。命令核验服务身份与授权后才原子合并配置，已有配置会留备份；空配置沿用服务凭据绑定的Agent ID，已有ID不自动改名；不注册身份、不生成空间、不保存token值：

```sh
harness --url https://your-harness.example setup --token-env HARNESS_TOKEN --project demo --project-root /path/to/demo
harness doctor
harness bind demo /path/to/demo
```

参数化调用适合机器人；在交互终端未提供URL时会提示填写或保留现有值。独立空间仍由管理员创建授权，用`connect`选择。`doctor`只读显示有效配置/服务默认值、项目路径和连接问题，不写试验记忆；它不是日常任务步骤。新服务通过connect/setup启用scoped-v2；旧服务继续v1，不直接删掉预检。


默认配置位置：Unix/macOS 的 `$XDG_CONFIG_HOME/harness-server/client.json`，未设该变量时为 `~/.config/harness-server/client.json`；Windows 为 `%APPDATA%\harness-server\client.json`。也可用 `--config` 或 `HARNESS_CLIENT_CONFIG` 明确指定。

复制 [配置示例](../examples/client.example.json) 到上述用户目录并填入本机项目路径。已有配置时合并，不能覆盖其他项目。示例中的目录仅为格式展示，必须检查用户实际目录，不创建假的项目目录来让校验通过：

```json
{
  "url": "http://localhost:8080",
  "token_env": "HARNESS_TOKEN",
  "projects": {
    "my-project": "/absolute/local/project/root"
  }
}
```

Windows 路径使用 `C:\\code\\my-project` 等 JSON 字符串。目录是客户端所在机器的绝对路径，服务端不需要项目源码。省略 `--project` 时按当前目录选择最长祖先绑定；无匹配时显式传项目，歧义不能猜测。`daily` 完全跳过。

## 2a. Agent 身份与多空间（v0.3）

按 [空间接入指南](AGENT-NAMESPACES.md) 为长期 Agent 配置独立文件。`init --agent-id <用户自定义ID>` 只保存身份，不生成 namespace；未指定则一次生成 UUID，重复初始化保留。管理员在服务部署中绑定相同 ID 和独立凭据，然后在 `/admin` 创建空间、分配多个读写授权及默认范围。管理 token 与 Agent token 分开，不索取到聊天、不写入配置 JSON。

用 `harness namespaces` 读取已有授权，再 `harness connect` 核验保存选择；namespace/read_namespaces 为 null 时沿用服务默认值。不能用不存在的 ID 自动接入。先升级服务和备份工具，再验证客户端；确认 scoped identity/来源返回正确后才切换日常入口。任务的 Session 仍在开始时的空间收尾，不能因默认值变化改投；实际无部署、无授权时明确说明。

## 3. 注入设备 token

本机服务可使用自己的 `HARNESS_TOKEN`；远端必须使用服务端已配置的设备凭据。配置 JSON 仅保存环境变量名，不保存 token。

终端中的临时安全输入示例（Bash/Zsh）：

```sh
read -r -s HARNESS_TOKEN
export HARNESS_TOKEN
harness health
```

临时输入只对该进程及其子进程有效，不是持久配置。GUI Agent、机器人和后台服务必须通过它们实际的进程启动环境获得 token 与 PATH；使用已有凭据加载器或权限受限的环境文件，并按平台配置启动时注入。不要以“已写入 shell 配置”代替实际 Agent 环境验证。Windows 通过当前会话或既有启动器/凭据系统设置进程环境变量，不将明文值写进全局提示词或示例文件。

## 4. 服务与迁移

已有服务只验证连接，不另外创建服务或 store。新建本机服务参照 [部署说明](DEPLOYMENT.md)，默认端口仅绑定 loopback。

GitHub 镜像的发布与拉取参照 [镜像说明](IMAGES.md)。镜像只含源码构建产物，不含 token、项目记忆或备份；GHCR 拉取凭据与 Harness 设备 token 是两种独立凭据。

需要延续旧记忆时，在服务停止且目标 store 为空的条件下离线导入，保留源目录备份，核对文档数量和选定文档 SHA。包括个人偏好与全部 Session/Workstream；不导入旧 runtime、派生索引或凭据。已有目标不能用导入覆盖，不执行 `down -v`。迁移不是持续双向同步。文档模板也需单独核验：先按[模板资源指南](TEMPLATE-RESOURCES.md)确认服务/备份工具支持，再核对三份原文；已有store仅补迁缺失模板，不重新导入或仅迁流程链接。

## 5. 验证后接入 Agent / 机器人

在日常 Agent 实际执行命令的环境中验证，而不仅是在另一个终端：

```sh
harness health
harness route --project my-project --scenario coding --content
harness list --kind workstream --project my-project --limit 3
```

使用真实项目 ID。`health` 验证鉴权和受管文档结构，不证明所有业务结论正确；空项目可能没有记忆，不制造数据来伪装迁移成功。必要时只读取一份选定记忆或交接核对身份与 SHA，不全量输出私有数据。连接验证是只读操作，不创建 Session 或试写个人记忆。

用户要求接入日常 Agent 时，先验证客户端支持 `harness instructions`，再将 `harness instructions --bootstrap` 输出的短入口放入全局规则；完整协议留在客户端，按需读取一次。仅替换旧 Harness 标记块，备份并保留独立委派、标题、网络偏好和项目规则。设备空间须先通过 namespaces/connect 接入，同时读取设备空间与shared；共享稳定事实明确写shared，本机配置与交接写设备空间，不自动迁移旧记录。两次保存分别幂等、分别报告；见 [CLIENT-RULES.md](CLIENT-RULES.md)。旧Python参数不能照搬，标题仍按应用工具尽力管理。

机器人接入执行器时，每个独立任务持久化自己的任务 ID，作为原生 Session ID；不得复用群 ID 或机器人进程 ID。独立长期目标使用不同 Workstream；新会话续接同一目标时显式关联它。简单任务零调用或按需 route，跟踪任务 start→执行→finish；记录返回的 Workstream SHA 和执行组 ID。网络不确定写入使用原幂等键与原请求重试，409 先读取并合并，再以新 key 提交修改后的请求。需要调度与执行中持久化时，按 [Pi执行记录接入指南](PI-EXECUTION-INTEGRATION.md) 使用独立任务/尝试/检查点API；来源消息去重和状态保存由服务提供，实际队列调度、执行器启动去重和业务执行重试仍由机器人管理。知识保存的幂等不能保证业务动作恰好一次。

## 交付给用户的说明

简述实际配置的地址、配置路径、项目映射、凭据注入方式（不含值）、已完成的验证、日常入口是否切换及仍缺少的条件。未部署、未运行或未迁移的步骤不能声明完成。当前权限模型仍是同一用户的可信设备；旧模式有效token访问shared，新身份配置可限制namespace读写，但namespace内没有项目级或多用户权限隔离。
