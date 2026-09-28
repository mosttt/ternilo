import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import type { SessionEvent } from '@/types'
import { DetailsPanel, type DetailsSelection } from './details-panel'

let host: HTMLDivElement
let root: Root

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

describe('DetailsPanel reasoning details', () => {
  it('shows reasoning usage/timing and keeps Think separate from model output', () => {
    const event: SessionEvent = {
      seq: 4,
      occurred_at_ms: 1_400,
      run_id: 'run-1',
      type: 'assistant_message',
      step: 1,
      response: {
        provider: 'fixture',
        model: 'fixture-model',
        finish_reason: 'stop',
        content: 'visible answer',
        reasoning_content: 'private summary',
        usage: { input_tokens: 10, output_tokens: 4, reasoning_tokens: 7 },
      },
    }
    const selection: DetailsSelection = {
      kind: 'event',
      event,
      relatedEvents: [event],
      reasoningContent: 'private summary',
      timing: {
        startedAt: 1_000,
        completedAt: 1_400,
        durationMs: 400,
        reasoningStartedAt: 1_100,
        reasoningCompletedAt: 1_300,
        reasoningDurationMs: 200,
      },
    }
    act(() => root.render(<DetailsPanel selection={selection} onClose={() => undefined} />))

    expect(host.querySelector('[data-details-state]')?.getAttribute('data-details-state')).toBe('ready')
    expect(host.textContent).toContain('推理 token')
    expect(host.textContent).toContain('推理耗时')
    expect(host.textContent).toContain('200 ms')
    expect(host.textContent).toContain('7')
    expect(host.textContent).toContain('fixture')
    expect(host.textContent).toContain('fixture-model')
    expect(host.textContent).toContain('stop')

    expect(host.textContent).toContain('思考')
    expect(host.textContent).toContain('private summary')
    expect(host.textContent).toContain('模型输出')
    expect(host.textContent).toContain('visible answer')
  })

  it('distinguishes running, empty, and failed details', () => {
    const started: SessionEvent = {
      seq: 1, occurred_at_ms: 100, run_id: 'run-state', type: 'tool_call_started',
    }
    act(() => root.render(<DetailsPanel selection={{
      kind: 'tool',
      trace: { id: 'call-1', name: 'read', arguments: {}, children: [], started, kind: 'tool' },
    }} onClose={() => undefined} />))
    expect(host.querySelector('[data-details-state]')?.getAttribute('data-details-state')).toBe('loading')

    act(() => root.render(<DetailsPanel selection={{
      kind: 'event',
      event: { seq: 2, occurred_at_ms: 200, run_id: 'run-state', type: 'custom_event' },
    }} onClose={() => undefined} />))
    expect(host.querySelector('[data-details-state]')?.getAttribute('data-details-state')).toBe('empty')

    act(() => root.render(<DetailsPanel selection={{
      kind: 'event',
      event: { seq: 3, occurred_at_ms: 300, run_id: 'run-state', type: 'turn_failed', message: 'provider unavailable' },
    }} onClose={() => undefined} />))
    expect(host.querySelector('[data-details-state]')?.getAttribute('data-details-state')).toBe('error')
  })

  it('presents a pending approval as the correlated call instead of a generic event', () => {
    const event: SessionEvent = {
      seq: 8, occurred_at_ms: 800, run_id: 'approval-run', type: 'user_question_asked',
    }
    act(() => root.render(<DetailsPanel selection={{
      kind: 'approval',
      event,
      approval: {
        tool_name: 'shell',
        call_id: 'call-approval',
        reason: 'Run the focused check',
        arguments: { command: 'cargo test' },
      },
    }} onClose={() => undefined} />))

    expect(host.querySelector('[data-details-state]')?.getAttribute('data-details-state')).toBe('loading')
    expect(host.querySelector('h2')?.textContent).toBe('shell')
    expect(host.textContent).toContain('等待回答')
    expect(host.textContent).toContain('call-approval')
    expect(host.textContent).toContain('Run the focused check')
    expect(host.textContent).toContain('cargo test')
  })

  it('uses the signed declarative result renderer for tool Details', () => {
    const started: SessionEvent = {
      seq: 1, occurred_at_ms: 100, run_id: 'run-table', type: 'tool_call_started',
    }
    const finished: SessionEvent = {
      seq: 2, occurred_at_ms: 200, run_id: 'run-table', type: 'tool_call_finished',
    }
    act(() => root.render(<DetailsPanel selection={{
      kind: 'tool',
      trace: {
        id: 'call-table',
        name: 'signed_fixture',
        arguments: { subject: 'browser' },
        presentation: {
          title: 'Signed fixture report',
          icon_kind: 'sparkles',
          input_summary: [{ label: 'Subject', path: ['subject'] }],
          result: {
            kind: 'table',
            columns: [
              { label: 'Trust', path: ['trust'] },
              { label: 'Message', path: ['message'] },
            ],
          },
        },
        output: {
          content: JSON.stringify([{ trust: 'signed', message: 'hello from fixture' }]),
          is_error: false,
        },
        children: [],
        started,
        finished,
        kind: 'tool',
      },
    }} onClose={() => undefined} />))

    const result = host.querySelector('[data-details-tool-presentation]')
    expect(result?.getAttribute('data-tool-contribution')).toBe('builtin.declarative')
    expect(result?.querySelector('[data-tool-view="declarative-table"]')).not.toBeNull()
    expect([...result!.querySelectorAll('th')].map(cell => cell.textContent)).toEqual(['Trust', 'Message'])
    expect([...result!.querySelectorAll('td')].map(cell => cell.textContent)).toEqual(['signed', 'hello from fixture'])
  })

  it('makes the generic tool output viewport keyboard-scrollable and named', () => {
    const started: SessionEvent = {
      seq: 1, occurred_at_ms: 100, run_id: 'run-generic', type: 'tool_call_started',
    }
    const finished: SessionEvent = {
      seq: 2, occurred_at_ms: 200, run_id: 'run-generic', type: 'tool_call_finished',
    }
    act(() => root.render(<DetailsPanel selection={{
      kind: 'tool',
      trace: {
        id: 'call-generic', name: 'echo', arguments: { text: 'hello' }, children: [],
        output: { content: 'hello', is_error: false }, started, finished, kind: 'tool',
      },
    }} onClose={() => undefined} />))

    const output = host.querySelector<HTMLPreElement>('pre[data-tool-view="generic"][data-tool-scroll]')
    expect(output?.tabIndex).toBe(0)
    expect(output?.getAttribute('role')).toBe('region')
    expect(output?.getAttribute('aria-label')).toBe('输出')
  })
})
