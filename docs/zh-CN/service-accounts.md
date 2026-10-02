# 服务账号

服务账号为自动化程序提供独立身份。在工作台进入“空间管理 → 服务账号”，创建名称和备注，再为该账号创建访问凭据。名称在当前空间内唯一；系统生成固定 `ter_sa_` ID，改名不会改变资源和历史归属。

只有空间管理员或所有者可以管理服务账号。服务账号本身不能创建其他账号或签发新凭据，也不会取得创建者的私有工作区、会话或平台管理权限。账号与凭据都属于一个明确空间。

## 凭据与权限

新访问凭据以 `ter_t_` 开头，只在创建时显示；列表提供名称、范围、到期时间、最后使用时间和撤销状态。关闭详情后不能再次查看完整凭据。创建时选择 1–365 天有效期，默认 30 天。

| 凭据范围 | 允许的操作 |
|---|---|
| `resource.read` | 查看当前身份、项目、可访问工作区、会话列表，以及已有权限的历史、事件、统计、投影、队列、文件内容和运行记录 |
| `run.execute` | 创建工作区及会话、提交／停止任务，提交、编辑、移除和插入排队消息 |

凭据范围不能扩大空间成员或资源权限。服务账号以独立成员身份接受已有的资源授权；创建资源时，所有者及会话身份是服务账号自身。读取凭据不能创建任务，执行凭据也不能取得未授权的私有会话。托管执行需由部署者启用，并满足已有执行器和模型要求。

访问凭据用于指定空间的 HTTP 接口，要求 `X-Ternilo-Tenant`；路径本身含空间 ID 时可使用路径指定的空间，同时提供请求头时两者必须一致。凭据不是浏览器登录凭据，不可用于原生登录、OIDC、Live、账号管理、电脑登记、Provider／用户凭据管理或平台管理接口。模型 API Key 仍使用其独立认证规则，服务凭据不能替代模型 Key。

例如读取自己的身份，将地址、空间 ID 和凭据替换为实际值：

```http
GET /api/v1/me HTTP/1.1
Host: ternilo.example
Authorization: Bearer ter_t_<credential>
X-Ternilo-Tenant: ten_<space>
```

## 启停与撤销

停用服务账号后，该账号所有凭据的后续请求被拒绝；重新启用时，未撤销且未到期的凭据可继续使用。已撤销凭据不能恢复，需创建新凭据。停用与撤销保留账号、工作区、会话历史和既有作者记录，不改为创建者的身份。

服务账号与凭据列表不返回认证摘要。凭据使用时间按已认证请求记录，通常每分钟更新一次。空间变更、关闭账号详情和离开管理页会清除页面内的一次性凭据展示。空间管理记录沿用审计，包含操作者、动作、目标 ID 和权限范围，不保存完整凭据。

## 管理接口

下列接口要求已登录用户及当前空间管理权限，`{tenant_id}` 是账号所在空间：

| 方法与路径 | 行为 |
|---|---|
| `GET /api/v1/tenants/{tenant_id}/service-accounts` | 列出服务账号 |
| `POST /api/v1/tenants/{tenant_id}/service-accounts` | 创建账号，JSON 字段为 `name`、可选 `notes` |
| `PATCH /api/v1/tenants/{tenant_id}/service-accounts/{service_account_id}` | 更新 `name`、`notes`、`enabled`，携带 `expected_revision` |
| `GET /api/v1/tenants/{tenant_id}/service-accounts/{service_account_id}/credentials` | 查看凭据元信息 |
| `POST /api/v1/tenants/{tenant_id}/service-accounts/{service_account_id}/credentials` | 创建凭据，字段为 `name`、`scopes`、`expires_at_ms`；返回一次性 `access_token` 和 `credential` |
| `DELETE /api/v1/tenants/{tenant_id}/service-accounts/{service_account_id}/credentials/{credential_id}` | 撤销指定凭据 |

SQLite 与 PostgreSQL 使用相同服务账号数据组件；PostgreSQL 将表访问限制为当前租户作用域。账号与凭据由现有 Server 配置数据库持久保存，备份该数据库时包含它们。
