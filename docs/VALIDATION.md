# 验证范围

本公开仓库从v0.3.2干净源码快照开始；旧仓库的提交、标签、Actions日志、Release资产和个人部署记录没有导入。当前代码与依赖锁文件保留，仓库链接及示例已通用化。

## 源码快照已有验证

- Rust1.88格式检查、全目标Clippy和64项契约/客户端回归通过，覆盖存储事务、幂等、SHA冲突、路由预算、身份/授权、跨空间交接、执行记录及在线备份。
- 6项打包集合保护/UTF-8编码回归通过；macOS Apple Silicon/Intel、Linux ARM64/x64、Windows x64原生构建及解压后规则/身份/HTTP smoke通过。
- Linux amd64/arm64原生镜像构建、非root运行、服务/备份工具健康、持久卷与重启、合并前后平台清单检查通过。

以上是源码快照在此前验证环境中的结果，不代表本公开仓库已发布安装包、镜像或已完成独立CI。公开仓库的实际运行记录查看 [Actions](https://github.com/YS1109/harness-server-public/actions)。

## 复核命令

```sh
cargo +1.88.0 fmt --check
cargo +1.88.0 clippy --locked --all-targets -- -D warnings
cargo +1.88.0 test --locked
python3 -m unittest discover -s tools -p 'test_*.py'
```

## 边界

测试使用临时store和临时凭据，不打包实际记忆、凭据、日志或备份。服务仍为同一用户可信设备的单writer，不提供水平扩容；跨空间共享与设备增量分两次保存，各自原子，可能部分成功。

业务QQ/Pi集成、灾难切换、OSS上传/恢复、各OS最低版本及实际任务净收益未因本仓库公开而增加验证。小型生产主机只安装已构建产物，不用于Rust/Docker编译；部署与升级另按部署和备份指南执行。
