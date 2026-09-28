import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { Translate } from '@/i18n/runtime'
import { zh } from '@/i18n/resources/conversation'
import { resetProviderInventoryForTest } from '@/domain/provider-inventory'
import { ComposerContextMeter } from './composer-context-meter'

const t: Translate<'conversation'> = (key, params) => zh[key].replace(/\{(\w+)\}/g, (match, name: string) => (
  params && Object.hasOwn(params, name) ? String(params[name]) : match
))

let root: Root
let host: HTMLDivElement
beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  resetProviderInventoryForTest()
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
})
afterEach(() => { act(() => root.unmount()); host.remove(); vi.restoreAllMocks() })

describe('ComposerContextMeter', () => {
  it('stays absent until both exact usage and the selected model window exist', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response(JSON.stringify([]), { status: 200 }))
    await act(async () => root.render(<ComposerContextMeter model={{ provider: 'profile_default' }} events={[]} t={t} />))
    expect(host.innerHTML).toBe('')
  })

  it('uses real Provider metadata and model response usage and warns only at measured pressure', async () => {
    const fetchMock = vi.spyOn(globalThis, 'fetch').mockImplementation(async input => {
      const path = String(input)
      return new Response(JSON.stringify(path.includes('/credentials') ? { references: [], records: [] } : [{
        id: 'fixture', display_name: 'Fixture', base_url: 'http://fixture', protocol: 'openai-responses',
        defaults: { context_window: 100_000, max_output_tokens: 16_000 },
        models: [{ id: 'm', settings: { mode: 'inherit' } }],
        timeout_ms: 1, max_attempts: 1, retry_base_delay_ms: 1,
      }]), { status: 200 })
    })
    await act(async () => {
      root.render(<ComposerContextMeter
        model={{ provider: 'named_provider', provider_id: 'fixture', model: 'm' }}
        events={[{
          seq: 1, occurred_at_ms: 1, run_id: 'r', type: 'model_response',
          response: {
            provider: 'fixture', model: 'm', finish_reason: 'stop', content: 'done',
            usage: { input_tokens: 70_000, output_tokens: 15_000 },
          },
        }]}
        t={t}
      />)
      await Promise.resolve(); await Promise.resolve()
    })
    const trigger = host.querySelector<HTMLButtonElement>('button[aria-label="上下文已用 85%"]')!
    expect(trigger).not.toBeNull()
    act(() => trigger.click())
    expect(host.textContent).toContain('/compact')
    await act(async () => {
      root.render(<ComposerContextMeter
        model={{ provider: 'named_provider', provider_id: 'fixture', model: 'm' }}
        events={[]}
        t={t}
      />)
      await Promise.resolve()
    })
    expect(fetchMock).toHaveBeenCalledTimes(2)
    expect(fetchMock.mock.calls.filter(([input]) => String(input).includes('/providers'))).toHaveLength(1)
    expect(fetchMock.mock.calls.filter(([input]) => String(input).includes('/credentials'))).toHaveLength(1)
  })
})

it.each(['platform_model', 'named_provider'] as const)('uses the exact %s cloud binding capacity without reading the collaborator private providers', async provider => {
  const selection = provider === 'platform_model'
    ? { provider, grant_id: 'owner-budget', model_id: 'published', reasoning_effort: 'high' as const }
    : { provider, provider_id: 'owner-byok', model: 'published', reasoning_effort: 'high' as const }
  const fetchMock = vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response(JSON.stringify({ current: { selection, available: true, source_name: 'Owner budget', model: { model_id: 'published', display_name: 'Published', protocol: 'openai-responses', defaults: { context_window: 100_000, max_output_tokens: 4000 } } }, options: [], next_cursor: null }), { status: 200 }))
  const events: import('@/types').SessionEvent[] = [{ seq: 1, occurred_at_ms: 1, run_id: 'r', type: 'model_response', response: { provider: 'platform', model: 'published', finish_reason: 'stop', content: 'done', usage: { input_tokens: 70_000, output_tokens: 15_000 } } }]
  await act(async () => root.render(<ComposerContextMeter model={selection} target={{ sessionId: 'shared-session', placement: 'cloud' }} events={events} t={t} />))
  expect(host.querySelector('button[aria-label="上下文已用 85%"]')).not.toBeNull()
  expect(fetchMock).toHaveBeenCalledTimes(1)
  expect(String(fetchMock.mock.calls[0][0])).toContain('/model-options?limit=25&session_id=shared-session')
  await act(async () => root.render(<ComposerContextMeter model={{ ...selection, reasoning_effort: 'low' }} target={{ sessionId: 'shared-session', placement: 'cloud' }} events={events} t={t} />))
  expect(host.innerHTML).toBe('')
})
