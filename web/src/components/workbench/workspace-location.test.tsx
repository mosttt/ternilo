import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import type { Workspace } from '@/types'
import { LocaleProvider } from '@/i18n/provider'
import { WorkspaceRow } from './workspace-rows'

vi.mock('@/api/client', () => ({ api: { request: vi.fn() } }))

let host: HTMLDivElement
let root: Root
const workspace: Workspace = {
  workspace_id: 'node/workspace', path: 'a / Documents', title: 'Documents', node_id: 'a',
  placement: 'local_node', status: 'online', created_at_ms: 1_720_000_000_000, updated_at_ms: 1,
}
const location = { status: 'available', path: '/home/remote/Documents', home: '/home/remote', created_at_ms: 1_700_000_000_000 }
const drag = { draggable: false, marker: null, onDragStart() {}, onDragOver() {}, onDrop() {}, onDragEnd() {} }

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  vi.useFakeTimers()
  const entries = new Map<string, string>()
  vi.stubGlobal('localStorage', { getItem: (key: string) => entries.get(key) ?? null, setItem: (key: string, value: string) => entries.set(key, value), removeItem: (key: string) => entries.delete(key) })
  vi.stubGlobal('ResizeObserver', class { observe() {}; unobserve() {}; disconnect() {} })
  vi.mocked(api.request).mockReset().mockResolvedValue(location)
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  vi.useRealTimers()
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

function render(value = workspace, platform = true, showPlacement = false) {
  act(() => root.render(<LocaleProvider><WorkspaceRow
    workspace={value} platform={platform} showPlacement={showPlacement} expanded active drag={drag}
    onToggle={() => {}} onNewSession={() => {}} onRename={() => {}} onUnregister={() => {}}
  /></LocaleProvider>))
}

async function hover(open: boolean) {
  const row = host.querySelector('[data-sidebar-workspace-row]')!
  await act(async () => {
    row.dispatchEvent(new MouseEvent(open ? 'pointerover' : 'pointerout', { bubbles: true, relatedTarget: document.body }))
    await vi.advanceTimersByTimeAsync(open ? 600 : 300)
  })
}

describe('private workspace locations', () => {
  it('keeps one compact title and fetches the real directory only while its hover is open', async () => {
    render()
    expect(host.querySelector('[data-sidebar-workspace-title]')?.textContent).toBe('Documents')
    expect(host.querySelector('[data-sidebar-workspace-machine]')).toBeNull()
    expect(api.request).not.toHaveBeenCalled()
    await hover(true)
    expect(api.request).toHaveBeenCalledWith('/workspaces/node%2Fworkspace/location', expect.objectContaining({ cache: 'no-store', signal: expect.any(AbortSignal) }))
    expect(document.querySelector('[data-workspace-location]')?.textContent).toContain('~/Documents')
    expect(document.querySelector('[data-workspace-location]')?.textContent).toContain('电脑 a · 在线')
    expect(document.querySelector('[data-workspace-location]')?.textContent).toContain('创建于')
    expect(document.body.textContent).not.toContain('a / Documents')
    expect(document.querySelector('[aria-label="复制工作区完整路径：/home/remote/Documents"]')).not.toBeNull()
    const signal = vi.mocked(api.request).mock.calls[0][1]!.signal!
    await hover(false)
    expect(signal.aborted).toBe(true)
    expect(document.querySelector('[data-workspace-location]')).toBeNull()
    expect(document.body.innerHTML).not.toContain('/home/remote')
    await hover(true)
    expect(api.request).toHaveBeenCalledTimes(2)
  })

  it('discards a late response after the hover closes', async () => {
    let resolve!: (value: unknown) => void
    vi.mocked(api.request).mockReturnValue(new Promise(ready => { resolve = ready }))
    render()
    await hover(true)
    expect(document.body.textContent).toContain('正在读取目录')
    await hover(false)
    await act(async () => resolve(location))
    expect(document.body.innerHTML).not.toContain('/home/remote')
  })

  it.each(['shared', 'offline', 'cloud'] as const)('does not request or copy a %s location', async kind => {
    const value: Workspace = kind === 'shared' ? { ...workspace, access: {
      owner_user_id: 'owner', storage_user_id: 'owner', ownership_revision: 0, is_execution_owner: false, is_owner: false, sources: [], role_limited: false,
      permissions: { view: true, submit: false, stop: false, configure: false },
    } } : kind === 'offline' ? { ...workspace, status: 'offline' } : { ...workspace, placement: 'cloud' }
    render(value)
    await hover(true)
    expect(api.request).not.toHaveBeenCalled()
    expect(document.querySelector('[aria-label^="复制工作区完整路径"]')).toBeNull()
    expect(document.querySelector('[data-workspace-location]')?.textContent).toContain('添加于')
    expect(document.body.textContent).toContain(kind === 'shared' ? '目录路径仅所有者可见' : kind === 'offline' ? '电脑离线' : '文件保存在云端工作区')
  })

  it('shows a short failure without exposing a server error or fabricated path', async () => {
    vi.mocked(api.request).mockRejectedValue(new Error('private internal path /secret/location'))
    render()
    await hover(true)
    expect(document.body.textContent).toContain('暂时无法读取目录')
    expect(document.body.textContent).not.toContain('/secret/location')
    expect(document.body.textContent).not.toContain('a / Documents')
    expect(document.querySelector('[aria-label^="复制工作区完整路径"]')).toBeNull()
  })

  it('uses the local path without a remote request and shows a machine badge only when requested', async () => {
    render({ ...workspace, path: '/home/local/Documents' }, false, true)
    expect(host.querySelector('[data-sidebar-workspace-machine]')?.textContent).toBe('a')
    await hover(true)
    expect(api.request).not.toHaveBeenCalled()
    expect(document.querySelector('[aria-label="复制工作区完整路径：/home/local/Documents"]')).not.toBeNull()
    expect(document.querySelector('[data-workspace-location]')?.textContent).toContain('创建于')
  })
})


it('does not reopen a delayed hover after overlapping pointer and focus triggers have left', async () => {
  render()
  const row = host.querySelector('[data-sidebar-workspace-row]')!
  const button = row.querySelector('button')!
  const outside = document.createElement('textarea'); document.body.append(outside)
  try {
    await act(async () => {
      row.dispatchEvent(new MouseEvent('pointerover', { bubbles: true, relatedTarget: document.body }))
      button.focus()
      await vi.advanceTimersByTimeAsync(100)
      row.dispatchEvent(new MouseEvent('pointerout', { bubbles: true, relatedTarget: outside }))
      outside.focus()
      await vi.advanceTimersByTimeAsync(1000)
    })
    expect(document.querySelector('[data-radix-popper-content-wrapper]')).toBeNull()
    expect(api.request).not.toHaveBeenCalled()
  } finally { outside.remove() }
})
