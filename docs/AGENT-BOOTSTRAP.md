# Agent 接入

部署后，每台可信设备使用同一个 HTTPS 服务地址、自己的 Bearer token 和本机项目目录映射。v0.3通过服务管理页面为稳定Agent配置多空间授权和默认范围；旧模式共享全部shared文档。namespace内仍无项目级权限隔离。服务不执行代码，不替代各项目的 AGENTS.md，也不提供原 Python CLI 的完全兼容层。

首次配置交给 [AI 配置引导](AI-SETUP.md)。本机默认使用 `http://localhost:8080`，无需设置 URL；连接另一台机器时显式配置 HTTPS 地址。镜像发布与拉取见 [镜像说明](IMAGES.md)。

从 GitHub Release 下载本机平台客户端，校验并按包内 `INSTALL.md` 安装；源码方式仍可用 `cargo install --path . --locked --bin harness`。配置 `HARNESS_URL`、token 环境变量及客户端 JSON 后，先用 `harness health` 验证。也可直接使用 HTTP JSON API。原 Agent Home 规则不会自动修改；确认服务可用并完成离线导入后，将其生命周期调用入口替换为下面的约定，避免同时向本地与远端保存。

身份与空间配置按[Agent身份与namespace](AGENT-NAMESPACES.md)执行。首次初始化只生成一次Agent ID或使用用户自定义ID，不生成namespace；管理员在服务端创建空间并授权，namespaces/connect核验接入，不每个对话重新初始化；实际连接和授权验证通过前不切换旧入口。

## 客户端内置规则与全局短入口

v0.3.1起，协议由客户端二进制统一提供。运行 `harness instructions --bootstrap`，将输出放进 Agent 的全局规则；需要实际记忆操作时再读取 `harness instructions`，已在当前上下文不重复。普通任务背景足够时零调用，daily完全跳过。

读取规则是本地操作，不访问服务或读取凭据；会显示当前本机身份和空间选择。空间须在首次配置时通过 namespaces/connect 核验接入，详见[客户端规则](CLIENT-RULES.md)。共享事实用 shared 的 log-only，设备配置与任务交接保留在设备空间；两个保存请求分别幂等、分别报告，不宣称跨空间原子性。已有 Session 仍在原空间收尾。

短入口由[client-bootstrap.md](../src/client-bootstrap.md)维护，完整规则由[client-instructions.md](../src/client-instructions.md)维护并编入二进制；此文档不另存一套易漂移的完整提示词。旧v0.3.0客户端没有instructions命令，升级并验证后再换入口。

## 新会话如何查找跨设备交接

1. 知道同一个原生任务 ID 时，直接 start 恢复它绑定的 Session/Workstream。
2. 新任务不知道 Workstream 时，先 `harness list --kind workstream --project <id> --limit 3`，按标题与元数据选候选，必要时用 `--offset` 分页；只 `get` 选中的文档。
3. 新 Session 用自己的原生 ID，并通过 `--workstream-id` 关联已确认的长期任务。项目冲突不自动迁移或覆盖；没有合适交接时仍可 route 复用项目与流程记忆。

v0.1 列举按逻辑路径稳定排序，不提供语义检索或按最近时间排序；不要把第一条当作最相关任务。未部署/断连时完成不依赖记忆的业务工作，明确未恢复或未保存的部分，不自动在另一目录制造第二份事实源。
