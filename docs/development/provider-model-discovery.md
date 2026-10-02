# 模型发现与 Claude 官方接口排查

2026-10-03 完成源码核对、错误展示修正和真实浏览器入口验收。依据 [Anthropic 官方 Models API](https://platform.claude.com/docs/en/api/models/list)：版本入口为 `https://api.anthropic.com/v1`，发现调用 `/models`，使用 `x-api-key` 和 `anthropic-version: 2023-06-01`；分页读取 `has_more`／`last_id` 并传 `after_id`，读取官方名称、容量与能力字段。

当前原生协议实现的路径、鉴权及分页已经匹配文档，补充 HTTP 合同验证这些字段。发现上游失败原来以 Execution 错误传播，被 Server 通用错误脱敏转成 `control-plane request failed`，因此界面不能区分配置问题与 Server 内部错误。现在模型发现使用可安全展示的 Unavailable 错误：标明上游 HTTP 状态及对应的 Key、权限、版本入口、限流检查方向；连接、超时、非 JSON 或目录格式不匹配也有独立原因。不返回响应正文、请求 Key、私有服务地址或底层异常内容，数据库等其他内部错误继续保留原有脱敏。

验证通过：原生 HTTP 分页合同 1 项及目录解析 5 项；Builtins all-targets 严格 Clippy；重新构建 CLI 与 Server 后，以真实浏览器验证本机和 Server 设置中的失败展示及再次获取成功。HTTP 入口分别验证 401、403、404、429、500，确认原因保留而故意包含秘密的上游正文未回显；有效目录读取两页，保留 null 容量和实际自适应档位。浏览器无未预期控制台或网络错误。

使用符合官方合同的受控上游完成验收，没有调用用户的 Anthropic 账号或使用其 Key。原截图缺少当时上游响应，不能据此认定唯一历史根因或宣称真实账号鉴权已验证；更新程序后可从安全错误提示继续定位具体账号或网络问题。
