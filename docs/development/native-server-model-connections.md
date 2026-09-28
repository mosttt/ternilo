# 原生协议 Server 模型连接 / Native Server model connections

状态：修复已实现，3 项连接单元测试和 2 项真实浏览器端到端测试通过。

Server 和本机 Provider 均支持 Gemini／Anthropic，但本机“连接 Server”的来源转换只枚举了 OpenAI Chat、OpenAI Responses 与 DeepSeek Responses。授权响应中即使包含原生模型，也不会生成可选择的本地 Provider。

补齐两种原生协议，继续按连接、平台授权／账号 Provider 与协议生成稳定独立来源；仍使用 Server 模型专用设备凭据，保留原预算和私有模型来源，不新增 Node 登记或上游凭据下发。单元测试覆盖五种协议的十个独立来源及持久目录。浏览器验收实际授权两类模型、选择模型、读取本机文件、工具结果的原生签名回传、Server 来源账本，并确认不重复生成设备直连用量。

Server and local Providers already support Gemini and Anthropic, but connected-Server catalog conversion enumerated only the three OpenAI-compatible protocols. Native models therefore disappeared from the local selector despite successful authorization. The fix adds both protocols while retaining stable source IDs, model-only device credentials, separate platform/private attribution and no upstream secret distribution or Node enrollment. Three connection tests and two actual browser scenarios passed: both Gemini and Anthropic platform/private authorization, local file tools, signed history, source-specific Server settlement and no duplicate direct-device usage.

验收结果：每种原生协议分别通过平台授权与账号私有模型执行一次真实 read_file 及后续回答，四条 Server 请求记录各结算 65 token，保留同一模型授权设备身份和不同预算来源；客户端未登记 Node，没有取得上游 API Key，也没有追加设备直连用量。桌面／390 像素页面正常，浏览器无控制台或网络错误。
