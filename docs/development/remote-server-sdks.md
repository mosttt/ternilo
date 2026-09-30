# Server 远程 SDK

状态：Python／TypeScript HTTP 与 Live 客户端已实现，传输与真实 Server／Node 闭环已通过；发布包、双语文档与 CI 已补齐，已合入 main。

沿用现有账号和空间权限、HTTP 队列／历史接口与 Live v1，不增加协议、Node 登记或绕过登录的凭据。客户端固定绑定身份与空间；HTTP 不重试变更请求，Live 使用交付游标重连和去重，暴露 reset，授权失败停止。Python 仅 Live／run 额外依赖 websockets；TypeScript 使用 Node 内置网络 API。

已通过真实共享成员读写 Node 文件、历史前向／向后衔接、停 Server 后的本机任务与重启补传、游标去重、撤权后的 Live 和 HTTP 拒绝。传输测试还覆盖跨连接重放、日志重置、静默握手超时、不重复提交与重定向凭据边界。发布脚本须显式包含新增模块，避免 SDK 入口导出不存在的文件。

最终验收：Python 4 项测试（含原 stdio）、TypeScript 类型检查及 3 项传输测试和原 stdio smoke、23 项部署／发行包工具测试、51 篇文档门禁、actionlint 全部通过。真实 Server／Node 同时验证两套 SDK 的共享成员文件读写、入队前配置拒绝、历史分页、Server 重启后的离线事件补读与撤权，未借助模拟业务后端。
