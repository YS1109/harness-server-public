# 客户端内置记忆规则（v0.3.1）

规则随客户端二进制发布，所有 Agent 共用同一来源。全局提示词只放入口；需要记忆操作时读取一次，普通任务仍可零调用，不逐轮读取。规则优先级低于用户当前要求、项目约束与权限，不替代服务授权。

```sh
harness instructions                 # Markdown 协议 + 当前本机空间选择
harness instructions --json          # 结构化输出，version/source/空间/协议
harness instructions --bootstrap     # 可复制的精简全局入口
```

三个形式均不联网、不读取 token、不写配置、记忆或统计；URL、项目绝对路径与凭据不会进入输出。`rules` 是 `instructions` 的别名。协议正文为 [client-instructions.md](../src/client-instructions.md)，短入口为 [client-bootstrap.md](../src/client-bootstrap.md)，由 Rust `include_str!` 编入二进制，安装包不需要另行复制协议文件。每次更新客户端后，新上下文读取对应版本；已加载协议无需反复读取。客户端的实际入口若是凭据启动器，它也应允许这个命令直接运行，不先加载 token。

## 一次性接入设备空间

管理员在服务端创建、授权空间。安装时先查询目录，明确绑定设备空间和共享读取范围：

```sh
harness namespaces
harness --agent-id <凭据绑定ID> connect \
  --namespace <实际设备空间ID> --read-namespaces <实际设备空间ID>,shared
```

客户端保留 URL、凭据变量名和项目映射，不为每个对话创建 namespace。未配置时 instructions 如实显示“未在本机指定”，不将 shared 假称设备空间，也不猜服务器默认值；管理页面与本机显式选择的优先级沿用 v0.3。未获共享授权时，不静默将共享增量写到另一空间。只读权限、归档及哈希错误仍由服务拒绝。

## 由 AI 判断共享范围

| 内容 | 写入目标 |
| --- | --- |
| 可跨设备复用的稳定项目事实、通用流程、明确长期个人偏好 | shared |
| 本机路径、端口、安装状态、环境配置、设备限定结论 | 设备空间 |
| Session、Workstream、执行记录 | 开始时的任务空间；通常为设备空间 |
| 用户明确禁止进入 Harness 的本机规则 | 不存入任何空间 |

稳定性与共享范围分别判断；“已经验证且长期有效”不代表适合跨设备共享。shared 仅面向同一用户的获授权设备，个人偏好仍不进入基础分享包。规则负责指导 AI；客户端不会依据正文猜测范围、解析隐私或自动拆改请求。

共享知识使用 `harness --namespace shared finish --mode log --project <id> --result-file <JSON>`；设备增量使用设备空间的 log-only。跟踪任务的 finish 沿用原 Session 空间，共享增量单独保存。两类增量同时出现时分两个结果文件与独立幂等键；两次 HTTP 请求各自原子，可能一边成功一边失败，必须分别报告与重试，不能声称跨空间整体提交成功。不增加服务端字段，不需要升级 v0.3 服务。

SHA 来自实际写入目标，设备覆盖版本不能用来更新 shared。操作 put 时按 get 返回的 namespace 明确原目标。跨空间读取仍共用预算；规则读取不触发 start/finish，不新增日常评分或审查流程。

## 升级与回退

先验证新客户端 instructions、health、namespaces 和一次实际项目读取，再切换全局短入口；原二进制、入口和配置先备份。客户端规则不产生管理授权、不修改服务默认值、不搬迁已有 Session 或长期记忆。回退旧客户端时恢复原全局规则，避免旧版本没有 instructions 命令。

本版源码与本机安装不等于 GitHub Release/镜像已经发布；发布需沿用版本标签和五平台工作流。
