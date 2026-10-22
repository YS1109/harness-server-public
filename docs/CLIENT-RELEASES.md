# GitHub Release 客户端构建

[Release client](../.github/workflows/release-client.yml) 构建 `harness` 客户端的五款原生安装包。下载与使用见 [客户端安装](CLIENT-INSTALL.md)。服务端继续通过独立 [GHCR 镜像工作流](IMAGES.md) 交付。

## 验证构建

在 Actions 选择 **Release client → Run workflow → main**，或运行：

```sh
gh workflow run release-client.yml --ref main
```

手动运行 main 只生成 Actions artifacts，不创建 Release、标签或镜像。通过后下载 `release-client-<版本>` artifact，包含五个压缩包和 `SHA256SUMS`。artifact的访问按GitHub权限规则执行，保留14天；公开Release可直接下载。

## 发布版本

先确保 `Cargo.toml` 与 `Cargo.lock` 的包版本一致，将变更提交、推送。版本标签必须严格匹配 `v<package.version>`：

```sh
# 下一版本示例（尚未发布）；发布前确认该版本未被占用。
git tag -a v0.4.0 -m 'Harness v0.4.0'
git push origin v0.4.0
```

标签触发客户端 Release 和独立镜像发布。两个工作流不互相等待，也不保证同时完成。常规 main push 不发 Release；不要移动已发布版本标签。新版本应更新 Cargo 包版本/锁文件后使用新标签。

客户端工作流先校验版本，运行现有 CI，并在原生 runner 上并行构建，避免 QEMU。Linux 使用 Ubuntu 22.04 作为 glibc 基线，macOS 显式区分 Apple Silicon / Intel，Windows 静态链接 MSVC CRT；全部使用 Rust 1.88 与 `--locked`。

每个包解压后执行 `--version`、`--help` 及 loopback HTTP 鉴权响应检查。这是客户端产物检查，不等于真实云端部署或 QQ/Pi 端到端验证。聚合阶段要求五款包全部存在且 SHA 与构建阶段一致；只有 CI 和所有构建成功后才创建草稿、上传并核对六个资产的名字/大小，最后公开 Release。失败保留草稿，不发布半套安装包。已公开版本不允许被重跑覆盖。

若标签发布中断，可以在该标签上重跑失败 job 或 `gh workflow run release-client.yml --ref v0.4.0`。已有草稿可恢复，已公开 Release 会拒绝覆盖。未配置签名凭据，不声明 macOS 公证或 Windows 代码签名；发布资产不含个人配置、token、记忆或服务端数据。

## 本机验证打包脚本

Python 3.12 是维护者/CI 打包依赖，使用方无需安装：

```sh
python3 -m unittest discover -s tools -p 'test_*.py'
# Apple Silicon 示例；其他平台使用工作流中的 target/platform。
cargo +1.88.0 build --release --locked --bin harness --target aarch64-apple-darwin
python3 tools/package_client.py package --target aarch64-apple-darwin --platform macos-arm64 --output /tmp/harness-client-package
```
