# 显式路由评测

评测用于回答“这组问题能否命中预期专题”，不证明Harness已产生净收益。它是维护者按需运行的诊断，不加入日常任务、start/finish或每轮收尾。

## 运行

```sh
harness eval-routing --suite /path/to/routing-suite.json --json
```

需要v0.4客户端和服务。客户端先本地校验用例，随后一次`POST /v1/eval-routing`，不做写入预检、不使用幂等键，也不自动重试到其他入口。沿用已配置身份、当前空间和读取范围；服务在取得快照时重新检查全部空间权限。在业务锁内仅复制内存文档，锁外调用原路由引擎评测。无需提供管理凭据。

这是对服务端当前快照的只读评测，需要连接服务；不是客户端离线模式。它不修改项目、专题、Session、修订或幂等回执，也不进入业务调用统计。服务尽力把脱敏报告保存到本地`.runtime/evaluations/latest-<namespace>.json`及全局`latest.json`；写入失败仍返回评测结果，并标记`report_saved:false`。这两份运行报告不是一个记忆事务，不备份、不路由、不进入发布包。

后台概览读取最近报告，空间筛选读取对应空间报告，全部空间读取最后一次全局报告。缺失显示“暂无评测”，损坏或不可读明确报错。报告是最近一次执行时的结论，不保证代表当前记忆；页面不自动运行评测。

## 用例

```json
{
  "version": "demo-v1",
  "cases": [
    {
      "id": "explicit-rules",
      "project": "demo",
      "query": "客户端规则",
      "expected_topic": "client-rules"
    },
    {
      "id": "unrelated",
      "project": "demo",
      "query": "天文望远镜选型",
      "expected_topic": null
    }
  ]
}
```

每例必须显式填写`expected_topic`；`null`表示不自动选择专题，不能省略。可选字段：

| 字段 | 用途 |
| --- | --- |
| `expected_namespace` | 限定预期专题来源；显式指定`topic`时也用于原路由请求的专题来源 |
| `topic` | 用户明确要求的专题ID；不填则验证自然检索 |
| `required_topics` | 本例需存在的竞争专题；缺失则不可评估，避免缺少干扰项时虚假通过 |

项目与用例/专题ID使用slug；version为1–80字，1–200例，query≤2000字，依赖专题≤16个。不接受daily、未知字段或明显凭据。查询通过HTTP发送给已配置的服务；不要在用例里放代码、凭据或私密聊天。

仓库[初始用例](../examples/routing-suite.json)包含8例中文、符号、别名、明确ID、主体与示例竞争、无关问题、项目隔离及否定表达。它对应`tests/refinement.rs`中的合成语料：`agent-home`项目含`client-rules`、`namespace`、`shared-http-service`三个专题，另有无专题的`other`项目；client-rules含instructions触发词、embedded rules别名和write_protocol/client_rules正文，服务专题仅以示例出现client_rules。

该夹具通过不等于真实记忆通过。用于真实服务前应审阅、修改项目/依赖/预期并提升version；项目或预期/依赖的ACTIVE专题不存在时明确为不可评估，不自动导入模拟记忆、不计为成功。

[真实结构示例](../examples/routing-suite-live.json)改用实际Agent Home专题结构，覆盖client-rules、namespace、cost-and-evaluation，以及与shared-http-service的竞争、无关及否定表达。它不包含记忆正文，也不自动建项目。先核对这些依赖并冻结预期，再执行；保留失败，不通过修改预期美化结果。私人业务场景的查询与用例文件应留在忽略的本地目录，只公开脱敏汇总。2026-10-23的真实挑战结果与限制见[验证记录](VALIDATION.md)。

## 评分与边界

| 指标 | 口径 |
| --- | --- |
| 首选正确率 | 正确首选数 / 可评估正例数；无正例为null |
| 漏召回 | 可评估正例中没有自动选择专题的数量；选错其他专题记错误但不记漏召回 |
| 负例误选 | 可评估负例中自动选择了任何专题的数量；分母为可评估负例数 |
| 不可评估 | 缺项目、缺预期/依赖专题或路由错误的用例，不进入上述分母 |

首选取路由结果的首个实际加载专题，不把候选摘要当成命中。预期namespace存在时还需匹配来源；不同项目的同名专题不算可用依赖。报告逐例列出correct/incorrect/unavailable及预期/实际ID，方便定位问题。

报告记录评测时间、引擎版本、用例版本、用例SHA-256、授权读取范围及相关记忆快照SHA-256。快照指纹覆盖读取范围内的共享记忆文档，不含交接、执行记录或未授权空间。报告不保存原始query、正文、绝对项目路径或完整错误；ID和version也应脱敏。指纹用于识别评测输入变化，不解释变化原因。

这里没有综合价值分、真实任务A/B、模型token账单或省时估算。服务处理耗时、HTTP请求数、返回字符数和这组用例的准确率可供后续决策；真实任务净收益仍是尚未测量。
