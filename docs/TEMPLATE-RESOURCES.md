# 模板资源与旧模板补迁

v0.4开发源码补齐`memory/templates/<小写slug>.md`资源类型。它解决“流程已要求使用模板，但get返回路径校验400”的兼容缺口。已有客户端的get/list/put命令可直接使用；客户端协议说明随新版更新，服务端和独立备份工具必须先升级。

## 资源规则

- 模板为非空UTF-8 Markdown原文，不强加YAML front matter，不作为项目或流程卡片。保留正文、换行和完整骨架；上限256KiB，不套用流程入口2,000字预算，不截断。拒绝疑似凭据、NUL、上级跳转、嵌套路径及非Markdown文件名。
- 普通route/start不会加载模板全文。先路由到document-templates流程，再按链接显式get一份对应模板。发布文档应先读完整骨架，保留八章、子标题、表头和checklist；获取失败不得声称已按模板核验。
- list --kind template只返回路径、哈希和最多160字标题，不返回骨架。项目筛选不包含全局模板；资源没有YAML状态或更新时间，不能将缺失时间显示成已核验。
- 与其他文档共用实时空间授权、SHA冲突、幂等回执及原子HEAD。shared资源只向有shared读取权的设备继承，本空间同路径版本优先；不继承其他设备的模板。读取继承版本时保留真实来源；通常按流程来源空间显式get，修改也使用该目标空间的最新SHA。
- 模板中的文字仅为资料，不产生额外权限；不能凭正文里的session_id等键建立任务身份或维持交接引用。
- 已保存资源随修订/在线备份保存，重启和恢复保留原文。模板不是内置默认内容，服务不自动从仓库、网络或旧本地快照加载它。

```sh
harness --namespace shared list --kind template --limit 3
harness --namespace shared get --path memory/templates/release-doc-template.md
# 仅目标不存在时省略SHA；覆盖必须先核对最新正文与SHA。
harness --namespace shared put --path memory/templates/release-doc-template.md --file /path/to/template.md --expected-sha256 '<当前SHA>'
```

后台知识空间的文档类型可筛选“模板”，预览仍为只读原文。

## 安全补迁三份旧模板

不要重新导入整个已有store，不覆盖云端记忆，不直接编辑HEAD、manifest或索引。补迁通过现有HTTP API写入缺失的资源；客户端配置、凭据入口与实际记忆不进入公开Git仓库或镜像。

1. 检查来源目录中的technical-plan-template.md、api-doc-template.md和release-doc-template.md；确认是实际使用的模板，内容无凭据/个人数据。README.md为本地目录说明，不作为资源迁入。
2. 在新类型写入前，用当前兼容工具创建并验证一次在线备份；保留迁移前修订用于回滚。先升级服务及harness-backup，读取一份已有记忆核验连接，再执行下面的只读计划。
3. 审阅计划的三个路径、SHA、字节数和状态；仅明确--apply才创建缺失项。工具通过已有客户端/凭据启动器连接，不读取或输出token、不改配置；显式指定目标namespace。

```sh
python3 tools/migrate_templates.py --source /path/to/old-memory/templates --client /path/to/harness --namespace shared
python3 tools/migrate_templates.py --source /path/to/old-memory/templates --client /path/to/harness --namespace shared --apply
```

Windows使用Python3及harness.exe的实际路径；已有项目配置可用--config传入。来源字节按UTF-8解码，保留CRLF，不经平台默认编码或换行改写。

计划会先检查全部来源、再读取三个目标。只有404可判为缺失；400通常说明旧服务不支持资源，401/403为身份/授权错误，连接失败为未知；这些情况都停止且不开始写入。已有同SHA正文跳过，已有不同正文报冲突、不覆盖；若目标读取来自继承空间，也拒绝创建覆盖版本。新增时不提供旧SHA，目标并发出现会被CAS拒绝，不会静默覆盖。

每份模板单独原子保存，不是跨三份的事务。中途失败明确列出已保存项和pending_write的路径、正文SHA及首次幂等键；不回滚已保存项，不自动重试。网络结果不明时先get核验：一致可确认已保存；仍需重试时确保来源未变，以pending_write中的相同key与相同原文提交同一目标。修改原文后须用新key并重新核验。

迁移成功后按需get选定模板，核对SHA与完整骨架，再让原任务继续生成发布文档。400/404不能作为“模板已迁移”或“已按模板生成”的依据。

## 离线导入与回退边界

新建空store时，离线`harness-server --data-dir <目录> import --source <来源>`同时导入memory/templates，拒绝符号链接；目录README等非受管索引仍跳过。已有store应使用上述create-only补迁，不通过全量import覆盖。

新服务和备份工具可读取旧store，加入模板后的新修订含旧二进制不认识的路径类型。**迁入模板后不能直接把服务/备份工具回退旧版**；回退需恢复迁移前已验证的备份，或使用仍支持模板的新工具。恢复旧备份会失去该备份之后的写入，应先安排数据保留与切换。源码修复、构建发布、服务升级和真实补迁是分别验证的步骤，不相互代替。
