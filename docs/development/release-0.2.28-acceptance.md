# v0.2.28 发行验收

本版首次公开模型连接与工具可靠性改进，实现由 [PR #17](https://github.com/mosttt/ternilo/pull/17) 合入；范围见[模型转发](model-gateway-reliability.md)、[文件与检索](file-and-history-contracts.md)和[工具失败与只读验证](tool-execution-reliability.md)。v0.2.27 的正式浏览器检查拦截了未等待界面保存收尾的验收，修正及失败证据见[候选记录](release-0.2.27-acceptance.md)。

## 当前验证

保存响应延迟的确定性诊断在旧等待条件下复现字段消失，补齐界面完成条件后通过。测试不改变产品保存行为，不增加等待上限，不减少原有断言。代码图谱的默认精简、`detail: "full"` 和影响查询说明已同步到中英文指南。

版本元数据、完整检查、正式发行及公开包验收按当前候选继续执行；公开完成时间与最终成品证据在相应步骤完成后更新。
