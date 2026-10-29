import { describe, expect, it } from 'vitest'
import { en, ko, zh } from '@/i18n/resources/chat'
import type { Translate } from '@/i18n/runtime'
import { presentRuntimeError } from './runtime-error'

const translate = (resource: Record<keyof typeof zh, string>): Translate<'chat'> => (key, params) => resource[key].replace(/\{(\w+)\}/g, (match, name: string) => (
  params && Object.hasOwn(params, name) ? String(params[name]) : match
))
const t = translate(zh)

describe('runtime error presentation', () => {
  it('distinguishes Provider web configuration and credential errors from model authentication', () => {
    const cases = [
      ['policy_denied: web credentials can only be sent to their owner’s computers', 'authorization'],
      ['execution: Provider web credential is missing; configure it in the Provider settings', 'credential'],
      ['conflict: Provider web settings changed; start a new task', 'changed'],
      ["invalid_input: this model does not support the Provider's built-in search", 'unsupported'],
    ] as const
    for (const resource of [zh, en, ko]) for (const [raw, category] of cases) {
      expect(presentRuntimeError(raw, translate(resource))).toEqual({
        title: resource['runtimeError.providerWeb.title'], message: resource[`runtimeError.providerWeb.${category}`], raw,
      })
    }
  })

  it('shows supplier tool rejection in all languages without treating unrelated errors as unsupported models', () => {
    for (const resource of [zh, en, ko]) for (const [capability, key] of [['search', 'rejectedSearch'], ['fetch', 'rejectedFetch']] as const) {
      const raw = `execution: Model API error (HTTP 400; unsupported_web_${capability}): supplier rejected this tool; Server request ID: mrq_fixture`
      expect(presentRuntimeError(raw, translate(resource))).toEqual({
        title: resource['runtimeError.providerWeb.rejectedTitle'], message: resource[`runtimeError.providerWeb.${key}`], raw, requestId: 'mrq_fixture',
      })
    }
    expect(presentRuntimeError('Model API error (HTTP 401; authentication_failed): web_search', t).title).toBe(zh['runtimeError.api.auth.title'])
  })
  it('distinguishes webpage failures from model failures in every locale', () => {
    for (const resource of [zh, en, ko]) {
      for (const [status, category] of [[400, 'request'], [401, 'auth'], [402, 'balance'], [403, 'blocked'], [404, 'missing'], [429, 'limit'], [504, 'timeout']] as const) {
        const raw = `execution: DeepSeek web tools error (HTTP ${status}): safe diagnostic`
        expect(presentRuntimeError(raw, translate(resource))).toEqual({
          title: resource['runtimeError.deepseekWeb.title'], message: resource[`runtimeError.deepseekWeb.${category}`], raw,
        })
      }
    }
  })

  it('keeps OpenAI billing, spend, usage and rate limits distinct in every locale', () => {
    const cases = {
      credit_balance_exhausted: 'balance', organization_spend_limit_exceeded: 'orgSpend', project_spend_limit_exceeded: 'projectSpend',
      organization_usage_limit_exceeded: 'orgUsage', insufficient_quota: 'quota', rate_limit_exceeded: 'rateLimit', slow_down: 'slowDown', request_limited: 'limited',
    } as const
    for (const resource of [zh, en, ko]) {
      for (const [code, category] of Object.entries(cases)) {
        const raw = `execution: Model API error (HTTP 429; ${code}): safe diagnostic`
        const result = presentRuntimeError(raw, translate(resource))
        expect(result).toEqual({ title: resource[`runtimeError.api.${category}.title`], message: resource[`runtimeError.api.${category}.message`], raw })
      }
    }
    expect(presentRuntimeError('Model API error (HTTP 400; context_length_exceeded): too long', t).title).toBe('上下文超出模型限制')
    expect(presentRuntimeError('Model API error (HTTP 400; service_tier): not allowed', t).message).toContain('service_tier')
    expect(presentRuntimeError('credit_balance_exhausted.txt could not be opened', t).title).toBe('运行失败')
    expect(() => presentRuntimeError('Model API error (HTTP 429; constructor): unknown code', t)).not.toThrow()
  })

  it('distinguishes DeepSeek balance from authentication and rate limits in all three locales', () => {
    for (const [resource, title] of [[zh, 'DeepSeek 余额不足'], [en, 'Insufficient DeepSeek balance'], [ko, 'DeepSeek 잔액 부족']] as const) {
      const raw = 'execution: DeepSeek API error (HTTP 402): the account balance is insufficient.'
      const shown = presentRuntimeError(raw, translate(resource))
      expect(shown).toMatchObject({ title, raw })
      expect(shown.message).toContain('platform.deepseek.com/top_up')
      expect(shown.message).not.toContain('Server model request log')
      for (const [status, category] of [[400, 'request'], [401, 'auth'], [422, 'parameters'], [429, 'rateLimit'], [500, 'service'], [503, 'overloaded']] as const) {
        const result = presentRuntimeError(`DeepSeek API error (HTTP ${status}): failure`, translate(resource))
        expect(result.title).toBe(resource[`runtimeError.deepseek.${category}.title`])
        expect(result.title).not.toBe(title)
      }
    }
    const unrelated = 'read_file deepseek-402.txt: Permission denied'
    expect(presentRuntimeError(unrelated, t)).toEqual({ title: '运行失败', message: unrelated, raw: unrelated })
    expect(presentRuntimeError('model endpoint returned 402 Payment Required', t).title).not.toContain('DeepSeek')
  })

  it('explains resident capacity exhaustion before generic Provider categories and preserves its raw diagnostic', () => {
    const raw = 'policy_denied: capacity_exhausted: all compatible Worker resident slots (429) are held by runs waiting for queued dependencies'
    expect(presentRuntimeError(raw, t)).toMatchObject({
      title: '执行资源已满',
      message: expect.stringContaining('嵌套执行已用满可同时保留的任务名额'),
      raw,
    })
    expect(presentRuntimeError(raw, t).message).toContain('已完成的结果会保留')
    expect(presentRuntimeError(new Error(raw), translate(en))).toMatchObject({
      title: 'Execution resources are full',
      message: expect.stringContaining('Completed results are preserved.'),
      raw,
    })
  })

  it('does not turn an unrelated filename into an execution capacity failure', () => {
    const raw = 'read_file capacity_exhausted_notes.md: Permission denied (os error 13)'
    expect(presentRuntimeError(raw, t)).toEqual({ title: '运行失败', message: raw, raw })
  })

  it('explains the local tool budget without misclassifying it as a Provider error', () => {
    const raw = 'policy_denied: turn exceeded max_tool_calls (429)'
    expect(presentRuntimeError(raw, t)).toMatchObject({
      title: '本轮工具调用已达上限',
      message: expect.stringContaining('429 次工具调用'),
      raw,
    })
    expect(presentRuntimeError(raw, translate(en))).toMatchObject({
      title: 'Tool call limit reached for this turn',
      message: expect.stringContaining('429 tool calls'),
      raw,
    })
  })

  it('turns provider authentication failures into an actionable message', () => {
    const raw = 'execution: model endpoint returned 401 Unauthorized: Invalid token'
    expect(presentRuntimeError(raw, t)).toEqual({
      title: '模型认证失败',
      message: 'API Key 无效、已过期或不属于当前端点。请在“设置 → 模型”中检查 Provider。',
      raw,
    })
  })

  it('keeps the raw protocol failure for the inspector', () => {
    const raw = 'execution: parse model stream event: invalid type: null, expected a sequence'
    expect(presentRuntimeError(raw, t)).toMatchObject({
      title: '模型响应格式不兼容',
      raw,
    })
  })

  it('does not mislabel a workspace filesystem denial as a Provider failure', () => {
    const raw = 'create cloud workspace: Permission denied (os error 13)'
    expect(presentRuntimeError(raw, t)).toEqual({
      title: '运行失败',
      message: raw,
      raw,
    })
    expect(presentRuntimeError(raw, translate(en))).toEqual({
      title: 'Run failed',
      message: raw,
      raw,
    })
  })

  it('presents every actionable category in the active English locale', () => {
    expect(presentRuntimeError('execution: model endpoint returned 429 rate limit', translate(en))).toMatchObject({
      title: 'Model service is busy',
      message: 'The Provider is temporarily rate-limiting requests or quota. Try again later or check the account quota.',
    })
    expect(presentRuntimeError('execution: model endpoint returned 404 Not Found: model_not_found', translate(en))).toMatchObject({
      title: 'Model is unavailable',
    })
    expect(presentRuntimeError('execution: model endpoint returned 400 Bad Request: context_length_exceeded', translate(en))).toMatchObject({
      title: 'Model context limit exceeded',
    })
    expect(presentRuntimeError('execution: model endpoint returned 503 Service Unavailable', translate(en))).toMatchObject({
      title: 'Provider service is unavailable',
    })
    expect(presentRuntimeError('execution: model endpoint returned 400 Bad Request: unsupported reasoning effort', translate(en))).toMatchObject({
      title: 'Provider rejected the model request',
    })
    expect(presentRuntimeError('execution: TLS certificate error', translate(en))).toMatchObject({
      title: 'Cannot reach the model service',
    })
  })
})

it('localizes gateway interruption and retains a bounded request ID for navigation', () => {
  const diagnostic = 'Server model connection ended without completion; its result is unknown and was not replayed'
  for (const resource of [zh, en, ko]) {
    const raw = `execution: ${diagnostic}; Server request ID: mrq_connection-1`
    expect(presentRuntimeError(raw, translate(resource))).toEqual({
      title: resource['runtimeError.gateway.title'], message: resource['runtimeError.gateway.message'], raw, requestId: 'mrq_connection-1',
    })
  }
  expect(presentRuntimeError(`${diagnostic}; Server request ID: ../../secrets`, t).requestId).toBeUndefined()
  expect(presentRuntimeError(diagnostic, t).requestId).toBeUndefined()
  expect(presentRuntimeError('execution: Model API error (HTTP 429; insufficient_quota): failure; Server request ID: mrq_404', t).title).toBe('模型 API 额度不足')
})
