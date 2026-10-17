import { api } from '@/api/client'
import { WorkspaceImageContext } from './markdown-workspace-images'
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { Translate } from '@/i18n/runtime'
import { AssistantMarkdown } from './assistant-markdown'

const labels: Record<string, string> = {
  'message.copy': '复制',
  'message.copyCode': '复制代码',
  'message.copied': '已复制',
  'message.copyFailed': '复制失败，请检查浏览器剪贴板权限。',
  'message.taskCompleted': '已完成任务',
  'message.taskPending': '未完成任务',
}
const t = ((key: string) => labels[key] ?? key) as Translate<'chat'>

let host: HTMLDivElement
let root: Root

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  Object.defineProperty(navigator, 'clipboard', {
    configurable: true,
    value: { writeText: vi.fn(async () => { throw new DOMException('denied', 'NotAllowedError') }) },
  })
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  vi.restoreAllMocks()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

describe('AssistantMarkdown code copy', () => {
  it('loads local images through the session workspace and discards them on account changes', async () => {
    const request = vi.spyOn(api, 'request').mockResolvedValue({ encoding: 'base64', media_type: 'image/png', content: 'iVBORw0KGgo=', truncated: false })
    const render = (source: string, scope = 'owner') => act(async () => root.render(
      <WorkspaceImageContext.Provider value={{ sessionId: 'public-session', root: '/home/project', scope }}>
        <AssistantMarkdown source={source} streaming={false} t={t} />
      </WorkspaceImageContext.Provider>,
    ))
    await render('![Map](/home/project/image.png)')
    expect(request).toHaveBeenCalledWith('/sessions/public-session/workspace', expect.objectContaining({ method: 'POST', body: { kind: 'read', path: 'image.png' } }))
    expect(host.querySelector('img')?.src).toBe('data:image/png;base64,iVBORw0KGgo=')
    await render('![Map](/home/project/image.png)')
    expect(host.querySelector('img')).not.toBeNull()
    expect(request).toHaveBeenCalledTimes(1)
    request.mockRejectedValue(new Error('denied'))
    await render('![Map](/home/project/image.png)', 'other-account')
    expect(host.querySelector('img')).toBeNull()
    expect(host.textContent).toContain('image.workspaceUnavailable')
    await render('![Secret](/etc/secret.png)')
    expect(request).toHaveBeenCalledTimes(2)
    expect(host.textContent).toContain('image.outsideWorkspace')
  })

  it('renders a redacted Node workspace destination as an image, not literal Markdown', async () => {
    const request = vi.spyOn(api, 'request').mockResolvedValue({ encoding: 'base64', media_type: 'image/png', content: 'iVBORw0KGgo=', truncated: false })
    await act(async () => root.render(<WorkspaceImageContext.Provider value={{ sessionId: 'node-session', root: '/home/project', scope: 'owner' }}>
      <AssistantMarkdown source={'Delivered.\n\n![Result](<local-workspace>/image.png)'} streaming={false} t={t} />
    </WorkspaceImageContext.Provider>))
    expect(request).toHaveBeenCalledWith('/sessions/node-session/workspace', expect.objectContaining({ body: { kind: 'read', path: 'image.png' } }))
    expect(host.querySelector('img')?.alt).toBe('Result')
    expect(host.textContent).not.toContain('![Result]')
  })

  it('shows a localized result instead of leaking a rejected clipboard promise', async () => {
    act(() => root.render(<AssistantMarkdown source={'```rust\nfn main() {}\n```'} streaming={false} t={t} />))
    const copy = host.querySelector<HTMLButtonElement>('.markdown-copy')!
    await act(async () => copy.click())
    expect(copy.textContent).toBe(labels['message.copyFailed'])
    expect(copy.getAttribute('aria-label')).toBe(labels['message.copyFailed'])
  })

  it('keeps completed streaming code lines mounted while grammar state advances', () => {
    act(() => root.render(<AssistantMarkdown source={'```ts\nconst value = `first\nsecond'} streaming t={t} />))
    const firstLine = host.querySelector<HTMLElement>('[data-streaming-line="0"]')!
    expect(firstLine.textContent).toBe('const value = `first')

    act(() => root.render(<AssistantMarkdown source={'```ts\nconst value = `first\nsecond ${name}\n`\nconsole.log(value)'} streaming t={t} />))
    const lines = host.querySelectorAll<HTMLElement>('[data-streaming-line]')
    expect(lines[0]).toBe(firstLine)
    expect([...lines].map(line => line.textContent)).toEqual([
      'const value = `first', 'second ${name}', '`', 'console.log(value)',
    ])
  })

  it('uses the same Shiki tokens and theme before and after a fence settles', () => {
    const tokenTree = () => [...host.querySelectorAll<HTMLElement>('code .line')].map(line =>
      [...line.children].map(token => ({
        text: token.textContent,
        style: (token as HTMLElement).style.cssText,
      })),
    )
    act(() => root.render(<AssistantMarkdown source={'```ts\nconst value: number = 1\nconsole.log(value)'} streaming t={t} />))
    const streamingTokens = tokenTree()
    expect(streamingTokens.flat().some(token => token.style.includes('--shiki-token-keyword'))).toBe(true)

    act(() => root.render(<AssistantMarkdown source={'```ts\nconst value: number = 1\nconsole.log(value)\n```'} streaming t={t} />))
    expect(tokenTree()).toEqual(streamingTokens)
    expect(host.querySelector('pre')?.classList.contains('shiki')).toBe(true)
    expect(host.querySelector('code')?.classList.contains('language-typescript')).toBe(true)
  })

  it('recognizes a matching closing marker instead of treating it as live code', () => {
    act(() => root.render(<AssistantMarkdown source={'before\n\n```rust\nfn main() {}\n```\nafter'} streaming t={t} />))
    expect(host.querySelector('[data-streaming-code-block]')).toBeNull()
    expect(host.querySelector('code.language-rust')?.textContent).toBe('fn main() {}')
  })
})
