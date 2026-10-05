# Server 登录与人机验证

实例所有者登录后，从“平台管理 → 实例设置 → 登录与人机验证”配置 OAuth 2.0／OIDC 和 Cloudflare Turnstile。桌面宽度与手机网页使用同一套设置；普通账号和平台管理员不能读取或修改这些配置。Ternilo 本地服务不增加公网账号登录。

保存成功后，新请求使用新配置，无需重启。修改采用版本校验；其他页面已保存时会拒绝覆盖，需要重新加载。数据库中的配置优先于部署文件及 OIDC 环境变量；在网页中关闭功能不会重新启用部署文件里的旧值。用户名和密码登录始终保留。

单用户模式仅允许实例所有者访问，不接受新账号注册。账号页只显示模式说明；切换到多用户模式后，才显示注册方式、审核设置和账号邀请管理。

## OAuth 2.0／OIDC

先填写 Server 公网 HTTPS 根地址，再点击“添加登录方式”。每项填写名称、Issuer、Client ID 和 Scopes，并分别启用或停用。登录页按名称显示按钮，用户可选择账号所属的提供方；已登录的密码账号绑定时同样可以选择。提供方 ID 由界面生成并保持固定，修改名称不改变现有会话。Scopes 必须包含 `openid`。在身份提供方登记页面显示的回调地址，例如 `https://ternilo.example.com/auth/callback`。网页登录不需要填写 Audience；“高级兼容设置”中的 Audience 只用于仍直接携带上游 JWT access token 的旧 API 客户端，一般留空。

公网地址必须是浏览器实际访问的地址。`0.0.0.0` 和 `::` 只用于监听，不能作为公网或回调地址。将 Server 放在提供有效证书的 HTTPS 反向代理后，填写代理对外的地址；只在设置中将 `http` 改成 `https` 不会启用 TLS。浏览器地址、Server 公网地址和已登记的回调地址必须使用相同协议、主机及端口。

浏览器使用 Web Crypto 生成 PKCE。普通 HTTP IP／域名页面缺少此能力时，登录入口会提示改用 HTTPS，不会发起授权跳转。页面与回调地址不同源时也会在跳转前提示，因为登录状态只保存在当前来源的 sessionStorage。已经保存的无效公网地址会停用组织登录；密码登录仍保留，所有者可修正设置，已启用的 Turnstile 校验不会被绕过。

这里接入支持 OpenID Connect discovery、JWKS、签名 ID Token 的身份提供方，使用授权码和 S256 PKCE。上游 access token 可以是不透明字符串；Server 校验 ID Token 的签名、Issuer、Client ID 对应的 Audience、时间、nonce、azp 和存在时的 at_hash，并将 UserInfo 的 sub 与已验证身份严格匹配。当前只接受非对称签名，不支持对称签名或加密 ID Token。仅有普通 OAuth 接口、没有 OIDC ID Token 的服务商仍需专门适配。

Server 成功校验后签发本站短期 OIDC 会话及可轮换的刷新凭据，而不是把上游令牌当成本站登录凭据。浏览器仅在当前标签页 sessionStorage 保存本站凭据；上游 refresh token 加密存入数据库，不返回浏览器。刷新会校验新 ID Token（若提供）及 UserInfo 的身份一致性，单次消费本站刷新凭据；并发重复刷新、退出登录后的刷新和账号封禁后的旧凭据均被拒绝。刷新会话最长七天，短期访问会话不超过一小时，且不超过上游返回的相关有效期。

Token 端点支持公开客户端（不带 Client Secret）、`client_secret_basic` 和 `client_secret_post`。按身份提供方的应用配置选择；私密客户端填写 Client Secret。每个启用的提供方独立核验 discovery 和签名公钥。无法核验时，配置页标记该项不可用，登录页暂不提供该入口；其他可用提供方和密码登录继续可用。授权码交换、本站会话、刷新和 MFA 都绑定所选提供方；不能用另一家的配置刷新或完成验证。停用期间或移除后立即拒绝该提供方的本站会话，不删除平台账号和资源。

已有账号从“用户设置 → 通用 → 账号”显式绑定组织身份，绑定后两种登录方式共用原 user_id。每个平台账号可显式绑定一个 OIDC 身份。外部身份由 issuer + sub 识别，不使用用户名或邮箱作为身份键。未注销账号的平台注册邮箱不能重复；外部登录返回相同邮箱也不能自动登录或合并已有账号，必须先登录原账号并绑定。修改 Issuer 或客户端身份前，应确保实例所有者仍持有可用的原生密码；旧组织身份的登录可能失效，但不会删除账号或资源。

生产使用 HTTPS。仅显式开启部署配置 `oidc.allow_insecure` 的本机开发环境允许回环 HTTP，不允许任意明文远程地址。

### LINUX DO

点击“使用 LINUX DO 配置”，自动填写 `https://connect.linux.do/`、`openid profile email` 和请求正文中的 Client Secret 认证。填入自己的 Client ID、Client Secret，将页面显示的回调地址登记到 LINUX DO Connect 后保存。该按钮只填入公开参数，不带任何用户凭据，也不会自动保存。

LINUX DO 的[公开 discovery](https://connect.linux.do/.well-known/openid-configuration)声明授权码、S256 PKCE、RS256、UserInfo 和 Client Secret 两种认证方式。此实现按其公开 OIDC 合同支持接入；本地验证使用真实签名的兼容测试身份服务，不代表已登录你的生产 LINUX DO 应用。生产验收应完成一次授权、绑定／注册及退出重登，不需要把 Client Secret 发送给开发者。

## 注册方式与 OAuth2 限制

多用户模式下，在“平台管理 → 账号”选择开放注册或管理员邀请，并可勾选“仅限 OAuth2 方式注册”。需要至少一个已启用且可用的 OIDC 登录提供方才能保存该条件。开放注册可同时要求审核；邀请注册无需重复审核。

勾选后，新账号通过登录提供方验证身份，再填写平台用户名和联系邮箱。密码注册接口和通过密码创建邀请账号的接口都会拒绝请求；已有账号的密码登录、首次实例所有者设置及账号密码修改保持可用。只有单独更改网页入口不能绕过后端限制。

邀请链接在 OAuth 跳转前保存邀请令牌，验证返回后仍需完成账号信息；Server 在同一事务内核对、消费邀请并创建账号。过期、无效或已使用的邀请不能注册。团队邀请仍要求已有账号，不替代账号注册邀请。

## Cloudflare Turnstile

在 Cloudflare 创建 Turnstile widget，将 Server 公网地址对应的域名加入允许列表，然后填写 Site Key 和 Secret Key 并启用。Site Key 可以公开；Secret Key 仅供 Server 调用 Siteverify，不发送给浏览器。此功能不要求整个站点通过 Cloudflare 代理。

启用后，密码登录、新用户注册（包括首次 OIDC 注册）和邀请注册必须提交有效验证令牌。已有组织账号登录及绑定流程由身份提供方验证，不在 OAuth 跳转和刷新令牌时重复弹出验证码；已登录账号接受团队邀请、API Key 和 Node 凭据也不使用网页验证码。

Server 实际调用 Cloudflare Siteverify，并核对成功状态、`action` 和配置的域名；不接受其他域名或其他表单签发的令牌。网络故障、失效或重复令牌都会拒绝本次请求，不会降级绕过。提交失败、令牌过期或切换表单后，网页重新获取验证码。组件支持深浅主题及窄屏紧凑布局。

仅启用 Turnstile 时，Server 的 CSP 才允许 `https://challenges.cloudflare.com` 的脚本与 iframe，未开放任意第三方来源。部署入口不要覆盖这些 CSP 指令；外层反向代理的策略也需要允许相同来源。规则依据 Cloudflare 的[服务端校验](https://developers.cloudflare.com/turnstile/get-started/server-side-validation/)和[CSP 文档](https://developers.cloudflare.com/turnstile/reference/content-security-policy/)。

## 密钥、备份与恢复

Client Secret 和 Turnstile Secret Key 与登录设置一起使用实例主密钥加密保存在数据库，不写入审计正文，不通过管理 API 回显。编辑框留空保留原密钥；更换 Issuer、Client ID 或 Site Key 后必须重新填写对应密钥。停用登录方式会保留该项配置和密钥，移除登录方式才清除该项；关闭 Turnstile 或邮件服务会清除对应配置。

备份必须同时保存数据库和匹配的 `secret_master_key`；主密钥轮换包含登录设置及上游刷新凭据。认证配置、OIDC 会话及 OAuth2 注册条件分别使用独立的 `authentication`、`oidc_sessions`、`registration_policy` 组件，不改变既有账号身份和资源。首次安装这些组件时，PostgreSQL 需要提供 schema-owner 连接进行初始化，此后受限 runtime 使用固定授权访问。

如果保存的登录设置无效，或错误的 Turnstile 配置导致无法登录，在受信任的 Server 主机上执行：

```bash
ternilo-server admin reset-authentication --config-dir /path/to
```

使用发布的 Compose 时，在部署目录执行：

```bash
docker compose -f compose.server.yml exec --user 10001:10001 server ternilo-server admin reset-authentication --config-dir /var/lib/ternilo
```

此操作清除网页保存的 OAuth／OIDC、Turnstile 和邮件服务设置，关闭网页配置的 Turnstile 与邮件服务，并让 OIDC 恢复到部署配置；保留账号、密码、MFA、模型、电脑、工作区和会话，同时写入操作审计。刷新网页后，用原账号密码登录，再重新配置这些服务。无需重新生成 Server 配置或重新创建管理员。

命令必须使用该实例实际的配置、数据库连接和主密钥；如果运行进程临时覆盖了数据库地址，应先确认配置指向同一实例。保留原 `config.json` 与数据卷：重新生成主密钥可能导致现有加密凭据无法读取，修改数据库地址会连接另一个数据库。登录设置保存在 Server 数据库，清除浏览器 Cookie 无法修复这类配置错误。

管理 API 为 `GET/PUT /api/v1/admin/instance/authentication`，其中 `oidc_providers` 是含固定 `id`、显示 `name`、`enabled` 及独立凭据的列表。`POST /auth/token`、`/auth/refresh` 和 `/auth/mfa` 必须传入所选 `provider_id`。管理 API 仅接受实例所有者的有效登录；`GET /auth/config` 只公开可用提供方的 `id`、`name`、公开授权参数及 Turnstile Site Key。受保护的注册／登录 JSON 请求使用 `turnstile_token` 传递本次表单的验证令牌。
