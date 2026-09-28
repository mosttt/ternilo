import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { Translate } from '@/i18n/runtime'
import { en, zh } from '@/i18n/resources/conversation'
import { ComposerTakeover } from './composer-takeover'

function translate(dict: typeof zh): Translate<'conversation'> {
  return (key, params) => dict[key].replace(/\{(\w+)\}/g, (match, name: string) => (
    params && Object.hasOwn(params, name) ? String(params[name]) : match
  ))
}

const t = translate(zh)

const planReview = (plan = '# 发布改动\n\n- 运行测试\n- 更新文档') => ({
  session_id: 's',
  question: {
    id: 'plan-review',
    question: '请审阅完整计划',
    options: [{ label: 'Approve' }, { label: 'Keep planning' }],
    multi_select: false,
    presentation: {
      kind: 'plan_review' as const,
      title: '发布改动',
      plan,
      approve_label: 'Approve',
    },
  },
})

let root: Root
let host: HTMLDivElement

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
})

describe('ComposerTakeover', () => {
  it('keeps denied request details readable and reacts to a later grant', async () => {
    const onAnswer = vi.fn(async () => {})
    const item = { session_id: 's', question: {
      id: 'approval', question: 'Modify extensions?', options: [{ label: 'Allow once' }, { label: 'Deny' }], multi_select: false,
      tool_approval: { tool_name: 'extension_set_mounted', call_id: 'call', reason: 'Mount extension', arguments: {} },
    } }
    const render = (allowed: boolean) => root.render(<ComposerTakeover questions={[item, planReview()]} canAnswer={() => allowed} onAnswer={onAnswer} onError={vi.fn()} t={t} />)
    act(() => render(false))
    expect(host.querySelector<HTMLButtonElement>('[data-plan-review-action="approve"]')!.disabled).toBe(true)
    const allow = () => [...host.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent?.includes('允许一次'))!
    expect(allow().disabled).toBe(true)
    act(() => [...host.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent?.includes('查看调用详情'))!.click())
    expect(host.querySelector('pre')?.textContent).toBe('{}')
    expect(host.textContent).toContain('当前共享权限不允许回答这项请求。')
    expect(onAnswer).not.toHaveBeenCalled()
    act(() => render(true))
    expect(host.querySelector<HTMLButtonElement>('[data-plan-review-action="approve"]')!.disabled).toBe(false)
    await act(async () => allow().click())
    expect(onAnswer).toHaveBeenCalledWith('approval', { selected: ['Allow once'] })
  })

  it('does not submit an ordinary answer after its permission is revoked', async () => {
    const onAnswer = vi.fn(async () => {})
    const item = { session_id: 's', question: { id: 'ordinary', question: 'Choose', options: [{ label: 'A' }], multi_select: false } }
    const render = (allowed: boolean) => root.render(<ComposerTakeover questions={[item]} canAnswer={() => allowed} onAnswer={onAnswer} onError={vi.fn()} t={t} />)
    act(() => render(true))
    act(() => host.querySelector<HTMLButtonElement>('[role="radio"]')!.click())
    act(() => render(false))
    expect(host.querySelector<HTMLTextAreaElement>('textarea')!.disabled).toBe(true)
    const submit = [...host.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent === '提交')!
    expect(submit.disabled).toBe(true)
    await act(async () => submit.click())
    expect(onAnswer).not.toHaveBeenCalled()
  })

  it('keeps long option copy in an unconstrained container separate from its fixed marker', () => {
    const label = '先完成依赖梳理、接口核对和真实浏览器验证，再开始跨端发布流程'
    const description = '这个说明需要在窄屏上完整换行，并让选项按钮按内容自然增高，不能覆盖后续选项。'
    act(() => root.render(<ComposerTakeover
      questions={[{ session_id: 's', question: {
        id: 'long-copy',
        header: '发布策略',
        question: '请选择一个适合当前发布窗口且能够完整覆盖验证要求的执行策略',
        detail: '标题、说明和选项都可能包含很长的内容。',
        options: [{ label, description }, { label: '稍后处理' }],
        multi_select: false,
      } }]}
      onAnswer={vi.fn(async () => {})}
      onError={vi.fn()}
      t={t}
    />))

    const option = host.querySelector<HTMLButtonElement>('[data-question-option]')!
    const marker = option.querySelector<HTMLElement>('[data-question-option-mark]')!
    const copy = option.querySelector<HTMLElement>('[data-question-option-copy]')!
    expect(marker).not.toBe(copy)
    expect(marker.textContent).toBe('1')
    expect(copy.textContent).toContain(label)
    expect(copy.textContent).toContain(description)
    expect(host.querySelector('[data-question-heading]')?.textContent).toContain('完整覆盖验证要求')
    expect(host.querySelector('[data-question-body]')).not.toBeNull()
  })

  it('collects a described single choice, multi-select and Other before structured submit', async () => {
    const onAnswer = vi.fn(async () => {})
    act(() => root.render(<ComposerTakeover
      questions={[
        { session_id: 's', question: {
          id: 'q1', header: '选择模式', question: '第一题', detail: '**先确认**运行方式。',
          options: [{ label: 'A (Recommended)', description: '速度优先' }, { label: 'B' }], multi_select: false,
        } },
        { session_id: 's', question: {
          id: 'q2', question: '第二题', options: [{ label: '测试' }, { label: '文档' }], multi_select: true,
        } },
      ]}
      onAnswer={onAnswer}
      onError={vi.fn()}
      t={t}
    />))
    expect(host.textContent).toContain('1 / 2')
    expect(host.textContent).toContain('选择模式')
    expect(host.querySelector('strong')?.textContent).toBe('先确认')
    expect(host.textContent).toContain('速度优先')
    act(() => host.querySelector<HTMLButtonElement>('button[role="radio"][aria-label="A"]')!.click())
    expect(host.textContent).toContain('第二题')
    const checks = host.querySelectorAll<HTMLButtonElement>('button[role="checkbox"]')
    act(() => { checks[0]!.click(); checks[1]!.click() })
    const input = host.querySelector<HTMLTextAreaElement>('textarea')!
    act(() => {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set
      setter?.call(input, '发布说明')
      input.dispatchEvent(new Event('input', { bubbles: true }))
    })
    await act(async () => [...host.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent === '提交')!.click())
    expect(onAnswer).toHaveBeenCalledWith('q1', { selected: ['A (Recommended)'] })
    expect(onAnswer).toHaveBeenCalledWith('q2', { selected: ['测试', '文档'], custom: '发布说明' })

    const collapse = host.querySelector<HTMLButtonElement>('button[aria-label="收起问题"]')!
    act(() => collapse.click())
    expect(host.querySelector('textarea')).toBeNull()
  })

  it('keeps tool approval separate, opens the shared call inspector and sends the exact backend option', async () => {
    const onAnswer = vi.fn(async () => {})
    const onInspectApproval = vi.fn()
    act(() => root.render(<ComposerTakeover
      questions={[{
        session_id: 's',
        question: {
          id: 'approval', question: '允许吗', options: [{ label: 'Allow once' }, { label: 'Deny' }], multi_select: false,
          tool_approval: { tool_name: 'shell', call_id: 'call-1', reason: '需要运行测试', arguments: { command: 'cargo test' } },
        },
      }]}
      onAnswer={onAnswer}
      onError={vi.fn()}
      onInspectApproval={onInspectApproval}
      t={t}
    />))
    expect(host.querySelector('[data-tool-approval]')?.textContent).toContain('shell')
    act(() => [...host.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent?.includes('查看调用详情'))!.click())
    expect(onInspectApproval).toHaveBeenCalledWith('call-1')
    await act(async () => [...host.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent?.includes('允许一次'))!.click())
    expect(onAnswer).toHaveBeenCalledWith('approval', { selected: ['Allow once'] })
  })

  it('keeps IME Enter in Other and submits only after composition ends', async () => {
    const onAnswer = vi.fn(async () => {})
    act(() => root.render(<ComposerTakeover
      questions={[{ session_id: 's', question: { id: 'q', question: '补充说明', options: [], multi_select: false } }]}
      onAnswer={onAnswer}
      onError={vi.fn()}
      t={t}
    />))
    const input = host.querySelector<HTMLTextAreaElement>('textarea')!
    act(() => {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set
      setter?.call(input, '中文输入')
      input.dispatchEvent(new Event('input', { bubbles: true }))
    })
    const composing = new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true })
    Object.defineProperty(composing, 'isComposing', { value: true })
    act(() => input.dispatchEvent(composing))
    expect(onAnswer).not.toHaveBeenCalled()
    await act(async () => input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true })))
    expect(onAnswer).toHaveBeenCalledWith('q', { selected: [], custom: '中文输入' })
  })

  it('validates every page and encodes an explicit skip as an empty structured answer', async () => {
    const onAnswer = vi.fn(async () => {})
    act(() => root.render(<ComposerTakeover
      questions={[
        { session_id: 's', question: { id: 'q1', question: '第一题', options: [], multi_select: false } },
        { session_id: 's', question: { id: 'q2', question: '第二题', options: [{ label: '保留' }], multi_select: false } },
      ]}
      onAnswer={onAnswer}
      onError={vi.fn()}
      t={t}
    />))
    act(() => host.querySelector<HTMLButtonElement>('button[aria-label="下一个问题"]')!.click())
    act(() => host.querySelector<HTMLButtonElement>('button[role="radio"]')!.click())
    act(() => [...host.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent === '提交')!.click())
    expect(host.textContent).toContain('请先完成这道问题。')
    expect(host.textContent).toContain('第一题')
    act(() => [...host.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent?.includes('跳过'))!.click())
    await act(async () => [...host.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent === '提交')!.click())
    expect(onAnswer).toHaveBeenCalledWith('q1', { selected: [] })
    expect(onAnswer).toHaveBeenCalledWith('q2', { selected: ['保留'] })
  })

  it('renders plan review as a dedicated warning card with safe Markdown, not the generic quiz', () => {
    act(() => root.render(<ComposerTakeover
      questions={[planReview()]}
      onAnswer={vi.fn(async () => {})}
      onError={vi.fn()}
      t={t}
    />))

    const card = host.querySelector<HTMLElement>('[data-plan-review]')!
    expect(card.getAttribute('aria-label')).toBe('请审阅完整计划')
    expect(card.textContent).toContain('计划审阅')
    expect(card.querySelector('h1')?.textContent).toBe('发布改动')
    expect(card.querySelector('[data-plan-review-scroll]')).not.toBeNull()
    expect(host.querySelector('[data-question-takeover]')).toBeNull()
    expect(host.querySelector('[role="radio"]')).toBeNull()
    expect(host.querySelector('textarea')).toBeNull()
    expect([...card.querySelectorAll('button')].map(button => button.textContent?.trim())).toEqual([
      '讨论修改', '要求修改', '批准计划',
    ])
  })

  it('submits the protocol approve label once and locks every decision while it is in flight', async () => {
    let finish: (() => void) | undefined
    const onAnswer = vi.fn(() => new Promise<void>(resolve => { finish = resolve }))
    act(() => root.render(<ComposerTakeover questions={[planReview()]} onAnswer={onAnswer} onError={vi.fn()} t={t} />))

    const approve = [...host.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent?.includes('批准计划'))!
    await act(async () => approve.click())
    expect(onAnswer).toHaveBeenCalledWith('plan-review', { selected: ['Approve'] })
    expect([...host.querySelectorAll<HTMLButtonElement>('button')].every(button => button.disabled)).toBe(true)
    approve.click()
    expect(onAnswer).toHaveBeenCalledTimes(1)
    await act(async () => finish?.())
  })

  it('sends the backend revision option, exposes a retryable local failure, and re-arms actions', async () => {
    const onAnswer = vi.fn()
      .mockRejectedValueOnce(new Error('question response rejected'))
      .mockResolvedValue(undefined)
    act(() => root.render(<ComposerTakeover questions={[planReview()]} onAnswer={onAnswer} onError={vi.fn()} t={t} />))

    const revise = [...host.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent?.includes('要求修改'))!
    expect(revise.title).toBe('Keep planning')
    await act(async () => revise.click())
    expect(onAnswer).toHaveBeenLastCalledWith('plan-review', { selected: ['Keep planning'] })
    expect(host.querySelector('[role="status"]')?.textContent).toBe('question response rejected')
    expect(revise.disabled).toBe(false)
    await act(async () => revise.click())
    expect(onAnswer).toHaveBeenCalledTimes(2)
  })

  it('turns discuss into real free-form plan feedback and supports Enter submission', async () => {
    const onAnswer = vi.fn(async () => {})
    act(() => root.render(<ComposerTakeover questions={[planReview()]} onAnswer={onAnswer} onError={vi.fn()} t={t} />))

    const discuss = [...host.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent?.includes('讨论修改'))!
    act(() => discuss.click())
    const input = host.querySelector<HTMLTextAreaElement>('textarea')!
    expect(input.getAttribute('aria-label')).toBeNull()
    expect(host.querySelector(`label[for="${input.id}"]`)?.textContent).toContain('告诉 Agent')
    act(() => {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set
      setter?.call(input, '先补充回滚方案')
      input.dispatchEvent(new Event('input', { bubbles: true }))
    })
    await act(async () => input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true })))
    expect(onAnswer).toHaveBeenCalledWith('plan-review', { selected: [], custom: '先补充回滚方案' })
  })

  it('carries the dedicated plan decisions in English', () => {
    act(() => root.render(<ComposerTakeover
      questions={[planReview()]}
      onAnswer={vi.fn(async () => {})}
      onError={vi.fn()}
      t={translate(en)}
    />))
    expect(host.querySelector('[data-plan-review]')?.textContent).toContain('Plan review')
    expect(host.textContent).toContain('Discuss')
    expect(host.textContent).toContain('Request revisions')
    expect(host.textContent).toContain('Approve plan')
  })
})
