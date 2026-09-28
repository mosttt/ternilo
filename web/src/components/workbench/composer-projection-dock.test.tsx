import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { Translate } from '@/i18n/runtime'
import { zh } from '@/i18n/resources/conversation'
import { ComposerProjectionDock } from './composer-projection-dock'

const t: Translate<'conversation'> = (key, params) => zh[key].replace(/\{(\w+)\}/g, (match, name: string) => (
  params && Object.hasOwn(params, name) ? String(params[name]) : match
))

let root: Root
let host: HTMLDivElement
beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
})
afterEach(() => { act(() => root.unmount()); host.remove() })

function fill(input: HTMLInputElement, value: string) {
  Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(input, value)
  input.dispatchEvent(new Event('input', { bubbles: true }))
}

describe('ComposerProjectionDock', () => {
  it('renders only real goal/todo projection values and expands the todo list', () => {
    act(() => root.render(<ComposerProjectionDock t={t} projection={{
      session_id: 's', as_of_seq: 3, values: {
        goal: { objective: '交付 Web', status: 'active' },
        todos: { items: [{ step: '完成 Composer', status: 'in_progress' }, { step: '测试', status: 'pending' }] },
      },
    }} />))
    expect(host.textContent).toContain('交付 Web')
    expect(host.textContent).not.toContain('完成 Composer')
    act(() => host.querySelector<HTMLButtonElement>('button[aria-expanded="false"]')!.click())
    expect(host.textContent).toContain('完成 Composer')
  })

  it('does not invent a dock when the projection capability is absent', () => {
    act(() => root.render(<ComposerProjectionDock t={t} projection={{ session_id: 's', as_of_seq: null, values: {} }} />))
    expect(host.innerHTML).toBe('')
  })

  it('executes only real goal status commands and edits the objective inline', async () => {
    const onGoalCommand = vi.fn(async (_command: string) => undefined)
    act(() => root.render(<ComposerProjectionDock t={t} onGoalCommand={onGoalCommand} projection={{
      session_id: 's', as_of_seq: 3, values: { goal: { objective: '交付 Web', status: 'active' } },
    }} />))

    await act(async () => host.querySelector<HTMLButtonElement>('[data-goal-action="blocked"]')!.click())
    expect(onGoalCommand).toHaveBeenLastCalledWith('/goal blocked 交付 Web')

    act(() => host.querySelector<HTMLButtonElement>('[data-goal-action="edit"]')!.click())
    const input = host.querySelector<HTMLInputElement>('input[aria-label="目标内容"]')!
    act(() => fill(input, '交付全部 Web'))
    await act(async () => host.querySelector<HTMLButtonElement>('[data-goal-action="save"]')!.click())
    expect(onGoalCommand).toHaveBeenLastCalledWith('/goal edit 交付全部 Web')

    act(() => root.render(<ComposerProjectionDock t={t} onGoalCommand={onGoalCommand} projection={{
      session_id: 's', as_of_seq: 4, values: { goal: { objective: '交付全部 Web', status: 'blocked' } },
    }} />))
    await act(async () => host.querySelector<HTMLButtonElement>('[data-goal-action="resume"]')!.click())
    expect(onGoalCommand).toHaveBeenLastCalledWith('/goal resume 交付全部 Web')
    await act(async () => host.querySelector<HTMLButtonElement>('[data-goal-action="complete"]')!.click())
    expect(onGoalCommand).toHaveBeenLastCalledWith('/goal complete 交付全部 Web')
  })

  it('keeps a failed edit open, reports the error, and disables actions while busy', async () => {
    const onGoalCommand = vi.fn(async () => { throw new Error('目标更新失败') })
    act(() => root.render(<ComposerProjectionDock t={t} onGoalCommand={onGoalCommand} projection={{
      session_id: 's', as_of_seq: 3, values: { goal: { objective: '交付 Web', status: 'active' } },
    }} />))
    act(() => host.querySelector<HTMLButtonElement>('[data-goal-action="edit"]')!.click())
    act(() => fill(host.querySelector<HTMLInputElement>('input[aria-label="目标内容"]')!, '重试目标'))
    await act(async () => host.querySelector<HTMLButtonElement>('[data-goal-action="save"]')!.click())
    expect(host.querySelector('[role="alert"]')?.textContent).toBe('目标更新失败')
    expect(host.querySelector<HTMLInputElement>('input[aria-label="目标内容"]')?.value).toBe('重试目标')

    act(() => root.render(<ComposerProjectionDock t={t} busy onGoalCommand={onGoalCommand} projection={{
      session_id: 's', as_of_seq: 3, values: { goal: { objective: '交付 Web', status: 'active' } },
    }} />))
    act(() => host.querySelector<HTMLButtonElement>('[data-goal-action="cancel"]')!.click())
    expect([...host.querySelectorAll<HTMLButtonElement>('[data-goal-action]')].every(button => button.disabled)).toBe(true)
  })
})
