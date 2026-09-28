import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { ChatTurnDeliverable } from '@/domain/chat-turns'
import { basename, fitProducedFiles, ProducedFiles } from './produced-files'

let host: HTMLDivElement
let root: Root

function deliverable(path: string, content: string): ChatTurnDeliverable {
  return {
    path, operation: 'write', seq: 1,
    attachment: { name: basename(path), media_type: 'text/plain', content },
  }
}

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

describe('ProducedFiles', () => {
  it('fits the largest prefix while reserving the exact remainder label', () => {
    expect(fitProducedFiles(145, 8, [60, 60, 60], [50, 50, 50, 0])).toBe(1)
    expect(fitProducedFiles(300, 8, [60, 60, 60], [50, 50, 50, 0])).toBe(3)
    expect(fitProducedFiles(145, 8, [60, 60], [50, 50, 50], 10)).toBe(1)
  })

  it('renders generated-file chips and previews exact durable text as plain content', async () => {
    const values = [deliverable('out/index.html', '<script>plain</script>\nbody')]
    await act(async () => root.render(<ProducedFiles sessionId="session-1" deliverables={values} />))
    const chip = host.querySelector<HTMLButtonElement>('[aria-label="预览生成文件 out/index.html"]')
    expect(chip?.textContent).toContain('index.html')
    await act(async () => chip?.click())
    await act(async () => Promise.resolve())
    const preview = document.querySelector<HTMLElement>('[data-produced-file-text]')
    expect(preview?.textContent).toBe('<script>plain</script>\nbody')
    expect(preview?.querySelector('script')).toBeNull()
  })

  it('opens an accessible complete list when responsive chips hide files', async () => {
    const values = Array.from({ length: 8 }, (_, index) => deliverable(`out/file-${index + 1}.txt`, `value-${index + 1}`))
    await act(async () => root.render(<ProducedFiles sessionId="session-1" deliverables={values} />))
    const more = host.querySelector<HTMLButtonElement>('[aria-label="查看全部 8 个生成文件"]')
    expect(more).not.toBeNull()
    await act(async () => more?.click())
    const list = document.querySelector<HTMLElement>('[data-produced-files-list]')
    expect(list?.querySelectorAll('button')).toHaveLength(8)
    const last = list?.querySelector<HTMLButtonElement>('[aria-label="预览生成文件 out/file-8.txt"]')
    await act(async () => last?.click())
    await act(async () => Promise.resolve())
    expect(document.querySelector<HTMLElement>('[data-produced-file-text]')?.textContent).toBe('value-8')
  })

  it('exposes loading, error, retry, empty, and ready without executing preview text', async () => {
    const reference = (hash: string): ChatTurnDeliverable => ({
      path: 'out/state.txt', operation: 'write', seq: 2,
      attachment: {
        name: 'state.txt', media_type: 'text/plain',
        content: `ternilo-attachment://sha256/${hash.repeat(64)}`,
      },
    })
    let resolveFirst!: (response: Response) => void
    const fetch = vi.fn()
      .mockImplementationOnce(() => new Promise(resolve => { resolveFirst = resolve }))
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce(new Response(JSON.stringify({
        name: 'state.txt', media_type: 'text/plain', content: '<b></b>',
      }), { status: 200, headers: { 'content-type': 'application/json' } }))
    vi.stubGlobal('fetch', fetch)

    await act(async () => root.render(<ProducedFiles sessionId="state-session" deliverables={[reference('d')]} />))
    act(() => host.querySelector<HTMLButtonElement>('[data-produced-file-chip]')?.click())
    expect(document.querySelector('[data-produced-file-state="loading"]')).not.toBeNull()

    await act(async () => {
      resolveFirst(new Response(JSON.stringify({
        name: 'state.txt', media_type: 'text/plain', content: '',
      }), { status: 200, headers: { 'content-type': 'application/json' } }))
      await Promise.resolve()
    })
    const empty = document.querySelector<HTMLElement>('[data-produced-file-text]')
    expect(empty?.getAttribute('data-produced-file-state')).toBe('empty')
    expect(empty?.textContent).toBe('文件为空')

    act(() => document.querySelector<HTMLButtonElement>('[aria-label="关闭"]')?.click())
    await act(async () => root.render(<ProducedFiles sessionId="state-session" deliverables={[reference('e')]} />))
    act(() => host.querySelector<HTMLButtonElement>('[data-produced-file-chip]')?.click())
    await act(async () => { await Promise.resolve(); await Promise.resolve() })
    expect(document.querySelector('[data-produced-file-state="error"]')).not.toBeNull()

    const retry = [...document.querySelectorAll<HTMLButtonElement>('button')]
      .find(item => item.textContent?.trim() === '重试')
    await act(async () => {
      retry?.click()
      await Promise.resolve()
      await new Promise(resolve => window.setTimeout(resolve, 0))
    })
    const ready = document.querySelector<HTMLElement>('[data-produced-file-text]')
    expect(ready?.getAttribute('data-produced-file-state')).toBe('ready')
    expect(ready?.textContent).toBe('<b></b>')
    expect(ready?.querySelector('b')).toBeNull()
    expect(fetch).toHaveBeenCalledTimes(3)
  })
})
