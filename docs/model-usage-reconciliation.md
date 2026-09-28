# 核对未知模型用量

[English](model-usage-reconciliation.en.md) · 简体中文

当上游没有返回完整用量，或调用中途断开，Server 会保留未知消耗的额度预留。预留不是实际用量，不能直接按预留值结算，也不能把缺失的计数视为零。

平台 Owner 或 Admin 可在“模型服务 → 用量”展开请求的尝试记录，对已结束、确实调用过上游且用量仍不完整的尝试点击“核对用量”。先查明上游对应调用的实际计数，再填写输入、输出 token、可选缓存／推理分项，以及核对依据和说明。输入和输出必须明确填写，实际为零时填写 `0`；可选分项留空表示未知。依据可使用上游请求记录或内部核对单编号，不要填写密码或 API Key。

确认后，请求列表、额度和用量汇总使用补齐后的计数。原调用的操作者、受益账号、授权、月份、结果和错误保持不变；缓存与推理分项不再额外加算一次。托管调用同时更新原任务的预留及所在期间的额度。即使请求对应的 Key 或授权已撤销，已发生的调用仍可完成核对。

“核对记录”保存核对人、时间、依据、说明及原有部分用量，来自与结算同一事务写入的不可变平台审计。Operator 和 Auditor 可以查看记录，但不能核对；普通账号不能调用管理核对接口。相同补录重试返回原记录，不重复计量或追加审计。若另一份核对或迟到的完整上游用量先完成，系统拒绝覆盖，刷新后查看当前结果。

运行中的请求、未实际调用上游的尝试及已有确定用量均不能补录。设备直接连接本地 Provider 的自报用量不属于这一账本，也不能用此操作修改平台预算。该流程是有审计的管理员核对，不会自动向上游查询或证明计数。

## 管理接口

- `GET /api/v1/admin/models/requests/{request_id}/reconciliations`：读取该请求的核对记录，要求模型服务读取权限。
- `POST /api/v1/admin/models/requests/{request_id}/attempts/{attempt}/reconcile`：核对一次尝试，要求额度管理权限。

请求示例：

```json
{
  "expected_settled_at_ms": 1800000000005,
  "usage": {
    "input_tokens": 70,
    "output_tokens": 30,
    "cached_input_tokens": null,
    "cache_write_tokens": null,
    "reasoning_tokens": null
  },
  "reference": "provider-report/request-17",
  "note": "已与上游对应调用记录核对。"
}
```

`expected_settled_at_ms` 使用当前尝试的 `settled_at_ms`；尝试发生变化、已有确定用量或补录内容冲突时返回 409。接口不接受客户端提供 `raw_usage`。旧的上游原始用量保留在服务端；计数与审计使用现有数据库结构，无需数据迁移。
