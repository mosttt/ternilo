import { describe, expect, it } from 'vitest'
import { en, zh } from '@/i18n/resources/chat'
import type { Translate } from '@/i18n/runtime'
import { presentRuntimeError } from './runtime-error'

const translate = (resource: Record<keyof typeof zh, string>): Translate<'chat'> => (key, params) => resource[key].replace(/\{(\w+)\}/g, (match, name: string) => (
  params && Object.hasOwn(params, name) ? String(params[name]) : match
))
const t = translate(zh)

describe('runtime error presentation', () => {
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
