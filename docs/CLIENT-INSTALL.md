# Harness 客户端安装包

这是 Rust 原生 HTTP 客户端 `harness`，不需要安装 Rust、Python 或服务端。不含 token、记忆或本机项目路径配置；服务需另外部署。

## 选择安装包

从 [GitHub Releases](https://github.com/YS1109/harness-server-public/releases) 下载相同版本的安装包和 `SHA256SUMS`。本仓库公开Release可直接下载。选择与系统和架构匹配的安装包；需要源码构建时按README操作。

| 系统 | 包名后缀 | 环境要求 |
| --- | --- | --- |
| macOS Apple Silicon | `macos-arm64.tar.gz` | 部署目标 macOS 11.0+；实际验证在 macOS 14 runner |
| macOS Intel | `macos-x64.tar.gz` | 部署目标 macOS 11.0+；实际验证在 macOS 15 Intel runner |
| Linux ARM64 | `linux-arm64.tar.gz` | glibc 2.35+；实际验证在 Ubuntu 22.04 ARM64 |
| Linux x64 | `linux-x64.tar.gz` | glibc 2.35+；实际验证在 Ubuntu 22.04 x64 |
| Windows x64 | `windows-x64.zip` | 静态 MSVC 运行库；实际验证在 Windows Server 2022 runner |

macOS 用 `uname -m` 区分 `arm64` / `x86_64`，Linux 的 `aarch64` 选 ARM64。Linux 包不是 musl 版本，不直接适用于 Alpine。Windows ARM64 原生包暂未提供。最低 OS 版本仅为构建目标，不代表逐版本实测。

## 校验并解压

下面以已发布v0.3.2为例；其他版本替换文件名中的版本。只下载一款时，先找到 `SHA256SUMS` 内对应文件名的一行，与本机计算值比较：

```sh
# macOS
shasum -a 256 harness-v0.3.2-macos-arm64.tar.gz
# Linux
sha256sum harness-v0.3.2-linux-x64.tar.gz
```

Windows PowerShell：

```powershell
Get-FileHash .\harness-v0.3.2-windows-x64.zip -Algorithm SHA256
Expand-Archive .\harness-v0.3.2-windows-x64.zip -DestinationPath .\harness-client
```

Unix 使用 `tar -xzf <包名>` 解压。包内只有 `harness`（Windows 是 `harness.exe`）、本引导和无凭据的 `client.example.json`。

## 放到 PATH

Unix 可将 `harness` 放入用户目录，无需管理员权限：

```sh
mkdir -p "$HOME/.local/bin"
# 在解压后的目录内运行；已有版本先备份。
install -m 755 ./harness "$HOME/.local/bin/harness"
export PATH="$HOME/.local/bin:$PATH"
harness --version
```

Windows 将解压后的目录加入用户 PATH，或直接执行 `& 'C:\实际解压目录\harness.exe' --version`。修改 PATH 后需重新启动终端或 Agent，让实际执行进程继承新 PATH。

macOS 包没有 Developer ID 签名或公证。若系统阻止运行，核验来源和 SHA 后按系统提示允许该文件，不全局关闭 Gatekeeper。从源码构建仍是可选路径。

## 连接服务

将 `client.example.json` 复制到用户配置目录并填入真实项目 ID / 根目录。Unix 默认 `~/.config/harness-server/client.json`（设了 `XDG_CONFIG_HOME` 则用它）；Windows 默认 `%APPDATA%\harness-server\client.json`。已有配置需合并备份；模板路径不能原样当成实际项目。

默认连接 `http://localhost:8080`；访问云端必须改用实际 HTTPS 地址。客户端所在机器的 localhost 不指向另一台云机。设备 token 通过服务端已认可的 `HARNESS_TOKEN` 环境变量注入，JSON 不存 token 值。

```sh
harness health
harness route --project '<真实项目ID>' --scenario coding --content
```

必须在 Agent / 机器人的实际执行环境验证连接、token 和 PATH；未成功前保留旧入口，不双写记忆。进一步引导：[AI-SETUP.md](https://github.com/YS1109/harness-server-public/blob/main/docs/AI-SETUP.md)，日常协议：[AGENT-BOOTSTRAP.md](https://github.com/YS1109/harness-server-public/blob/main/docs/AGENT-BOOTSTRAP.md)。下载更新后重新校验，先验证连接，再切换客户端；配置和记忆不随二进制覆盖。

## v0.3 独立 Agent 与多个空间

支持本版的包可 `harness init --agent-id codex-example`，不带 ID 则一次生成 UUID；重复初始化保留，namespace 不再自动生成。不同长期 Agent 用独立 `--config <文件>`，URL/token_env/项目映射保留。`harness identity` 仅看本机，`harness namespaces` 查询服务已有授权，`harness connect --namespace <服务ID> --read-namespaces <ID1,ID2>` 核验并保存选择；无显式选择则遵循服务默认值。

管理员通过部署凭据绑定身份，通过 `/admin` 创建空间并分配权限。客户端不能自行创建 namespace。旧 v0.1.0 包和服务不支持这些参数；新客户端会在 scoped 写入前拒绝不支持的旧服务。指南：https://github.com/YS1109/harness-server-public/blob/main/docs/AGENT-NAMESPACES.md 。

## v0.3.1 客户端规则入口

安装支持本版的客户端后运行 `harness instructions`，规则随二进制提供，无需下载另一份提示词。`--bootstrap`输出全局短入口，`--json`提供结构化协议；均为本地只读操作，不需要token。首次接入设备空间后，规则会显示实际本机选择；授权仍由服务验证。详见[客户端规则](https://github.com/YS1109/harness-server-public/blob/main/docs/CLIENT-RULES.md)。当前源码v0.3.2包含本功能及Windows UTF-8发布校验修复；本仓库安装包状态以[Releases](https://github.com/YS1109/harness-server-public/releases)为准。旧v0.3.0没有instructions命令，应升级并验证后再切换全局入口。
