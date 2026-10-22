# v0.4 运行与性能指南

本版源码版本为0.4.0；发布、安装和部署是独立步骤。先验证服务和客户端版本，再启用新能力。

## 日常调用预算

上下文齐备、无冲突时：简单任务零调用；仅检索一次route --content；仅稳定增量一次log-only finish；同一空间完整跟踪start --content和finish各一次。普通中间回复不重复生命周期操作。首次协议读取、额外扩读、409处理和多空间保存单独计数，不隐藏成本。

服务和客户端均支持v0.4时，`harness connect`或`setup`通过已有namespace目录接口确认`scoped-v2`，原子保存`write_protocol`。后续写入使用独立`/v2/scoped/start`、`finish`、`abort`和`documents`入口，授权、CAS、幂等和事务与v1共用，不重复请求health。服务不支持该路由时请求失败，不自动切到其他写入入口。既有配置默认v1，旧服务仍使用写前预检；配置回退可显式`--write-protocol v1`，回退旧二进制需恢复旧配置备份（旧客户端不识别新字段）。connect/setup不缓存授权，服务每次写入仍核验实时权限。

```sh
harness --timings start --agent codex --native-session-id '<原生任务ID>' --project demo --title '<目标>' --content
# 后续复用start返回的namespace、Session、Workstream SHA及执行组。
cat /path/to/result.json | harness finish --session-id '<ID>' --expected-workstream-sha256 '<SHA>' --result-file -
harness --scope shared finish --mode log --project demo --result-file /path/to/knowledge.json
```

`--scope device`映射到已明确接入的本机空间；未配置或当前为shared时拒绝猜测。`--scope`是明确范围选择，不分析正文，不增加授权；跟踪收尾仍应复用原Session空间。`put --file -`也支持stdin，最多1MiB。stdin内存处理，不额外保存正文草稿或凭据。

`finish`自动在本地校验结果JSON，与服务端共用无状态校验器；类型、条数、长度或触发词等错误会指出字段、期望格式及实际类型/数量，不回显错误正文，也不发HTTP。无需增加手动校验步骤。最新SHA、授权、已有流程累计长度和最终交接预算仍由服务端检查；两端都不截断。不同客户端/服务版本可能有协议差异，客户端校验通过不保证保存成功。

`--timings`只向stderr输出`http_requests`和`cli_elapsed_ms`。后者为参数解析后的进程内墙钟时间，含配置准备及HTTP，不含OS进程启动、启动器、模型思考与工具往返。服务端handler耗时与它不同，不能直接当成任务总耗时或节省比例。

## 接入与诊断

```sh
harness --url https://your-harness.example setup --token-env HARNESS_TOKEN --project demo --project-root /path/to/demo
harness bind demo /path/to/demo
harness doctor
```

setup在交互终端未指定URL时可输入，机器人用参数。需要有效的服务凭据及已绑定身份；空配置采用服务凭据已绑定的Agent ID，已有身份不自动改名；验证失败不覆盖现有配置。setup/bind/connect原子合并配置，已有配置保留同目录备份；凭据只引用环境变量名。bind只修改映射，不创造或切换Agent身份。doctor只读显示配置来源、本机选择与服务默认值、目录有效性、当前项目匹配及连接状态，不输出token。

项目推断仍选最长祖先目录，不猜项目ID；不存在的无关绝对路径不会阻塞当前匹配。当前目录自身的失效绑定、相对路径及歧义会明确失败。安装/诊断命令不加入日常调用路径。

## 后台工作台

`/admin`继续使用独立管理员凭据，仅保留在当前页面内存。新增：

- 运行概览：版本、运行时间和全局内存文档/回执计数。概览不递归扫描磁盘；单独点击“读取磁盘容量”才访问容量API，缓存30秒，“重新扫描”显式绕过缓存。扫描释放业务锁后执行，容量为全局近似值，可能包含正在提交的修订；扫描失败不清空成本统计。
- 成本：7/30/90天窗口，可按namespace筛选；调用量、错误/幂等重放、handler中位数/P90、返回字符数、各命令分阶段P50/P90及样本数、协议和服务版本分布。没有采集时显示暂无数据，未观测值不伪造为零，净收益尚未测量。
- 最近交接：Session或Workstream按最近更新时间浏览，按空间、项目、状态、标题/路径筛选。
- 执行任务：按空间、状态、项目和任务标题/ID查询；只读查看任务摘要、结果、检查点和最近20条执行尝试的时间线，有剩余数量提示，原始JSON默认折叠。真实调度/取消/重试仍由机器人管理。

交接和执行任务分别保留筛选条件与页码；非凭据筛选保存在当前浏览器sessionStorage，刷新可恢复，退出清除。凭据始终仅存页面内存。断连保留上次快照并标记其真实空间、窗口和采样时间，过期响应和退出后的响应不覆盖当前页面。

管理API包括`GET /admin/api/runtime?days=30[&namespace=...]`、`/storage[?fresh=true]`、`/routing-evaluation[?namespace=...]`、`/executions?namespace=...`、`/execution?namespace=...&task_id=...`，路径均以`/admin/api`开头。只接受独立管理员凭据，查询不产生记忆事务或业务指标，不后台轮询。

路由评测为显式操作，不进入日常任务；最近一次结果随概览返回，没有评测显示暂无评测，损坏结果显示不可读取。使用及分母见[路由评测指南](ROUTING-EVALUATION.md)。

## 统计与维护

`harness stats --days 30 [--project demo]`仅返回当前空间；管理员可看全局。统计以UTC日文件保存于服务数据目录`.runtime/metrics/`，与Markdown和幂等事务独立，不进入备份、导入、路由或发布包。重启保留成功采集的事件；旧进程统计不补算。

仅记录事件ID、服务版本、协议、可选执行组ID、时间、空间/项目ID、固定命令、handler与分阶段耗时、成功/失败代码、幂等重放和已知返回字符数。不记录原始查询、正文、token、绝对项目路径或完整错误。daily、health、stats、路由评测、发现和管理读取不采集；鉴权/JSON解析失败不在handler指标内。追加使用非等待文件锁，采集失败不改变业务结果，报表披露损坏/不可读文件及本进程丢弃事件。损坏/不可读/丢弃计数为全局采集健康，无法分配到单个空间。单日最多约2MiB，超限停止采集并披露，不阻塞保存。

聚合按窗口/空间/项目缓存最多30秒，返回采样时间、缓存命中和缓存年龄；本进程新增事件或采集失败使旧缓存失效，外部文件改动最多延迟30秒反映。CLI stats与后台共用聚合逻辑，但查询范围不同。

耗时从服务业务处理线程实际开始计时：handler包含业务锁等待、逻辑/事务准备及提交，排除中间件鉴权/解析、执行队列等待、指标追加及响应传输。lock_wait是等待业务锁时间，operation包括身份/路由/事务准备及校验但排除commit，commit为实际提交尝试。各阶段独立计算分位数，不能相加；无阶段样本显示未观测。幂等重放计入请求数和处理成本，不新增commit样本。失败提交尝试也有commit耗时，不表示保存成功。返回字符和交接字符含重放结果，不能解读成新增写入量或实际阅读量。

关联覆盖为带执行组的调用数/观测调用数，执行组数仅为已观测的不同ID，不推断完整任务或收益。v1 health预检不在服务业务统计中；比较请求总数应查看客户端--timings，不能仅凭服务端协议分布推断往返成本。

不每轮扫描或自动删除。维护命令需离线持有store writer锁，默认只读预览；仅显式参数才清理90天窗口之前的统计日文件（保留边界日）：

```sh
harness-server --data-dir /path/to/store maintenance
harness-server --data-dir /path/to/store maintenance --prune-metrics
```

本版不删除记忆、旧修订或幂等回执，不改变备份/恢复语义。全量修订仍随写入增长，容量页用于观测后续GC需求。修订GC须另行设计与在线备份的协调。
