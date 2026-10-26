# v0.2.27 未发布候选验收

可靠性实现由 [PR #17](https://github.com/mosttt/ternilo/pull/17) 合入，标签固定在 `e7ff95d245a7c79a64949f91279be9983e825e1d`。[最终候选 Checks](https://github.com/mosttt/ternilo/actions/runs/40351597379) 及[相同合并提交的 main 检查](https://github.com/mosttt/ternilo/actions/runs/40359667550) 全部通过，包括 Intel／Apple Silicon 的系统 Python 缓存隔离、只读检查和 LaunchAgent 更新／回退。

[正式 Release](https://github.com/mosttt/ternilo/actions/runs/40359687837) 六目标包与双架构镜像构建成功，但浏览器检查失败，未进入镜像发布、清单签名或公开 Release。标签不移动；修正后的发行转入 [v0.2.28](release-0.2.28-acceptance.md)。

## 文件面板偶发失败仍待定位

首轮 `workspace-panel-browser-e2e.test.mjs` 在切换到 Server 会话后，等待文件面板内 `src` 超时，失败截图没有文件面板。同一提交的独立 main 检查、本地原用例及放慢浏览器执行的诊断用例分别通过，原断言与超时未改；指定失败作业仅复验一次，文件面板用例通过（11.77 秒）。这些结果尚未确定首轮原因，保留任务 43，不继续反复重试。

## 插件保存验收补齐界面完成条件

复验在 `plugin-settings-browser-e2e.test.mjs` 的 Server／Node 韩文配置场景失败：后台查询已读到新检查配置，浏览器保存回调尚未完成，测试就切换到独立配置并展开卡片；稍后保存收尾将卡片折叠，字段因此消失。

隔离诊断将 PATCH 的实际响应延后 1 秒，在展开后等待保存收尾，稳定复现相同字段消失与滚动超时（51.44 秒），页面、控制台和 HTTP 无意外错误。测试改为先等待按钮恢复“保存”且禁用，再切换视图；相同延迟场景通过（45.20 秒）。此条件同时确认请求收尾和草稿已保存，不只检查后台持久状态。产品原有保存折叠行为、权限、断言和超时不变；诊断延迟不写入标准验收流程。
