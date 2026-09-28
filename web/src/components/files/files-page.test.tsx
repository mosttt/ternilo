import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import { navigate } from '@/app/navigation'
import type { ApplicationState } from '@/types'
import { FilesPage } from './files-page'
import { fileInventoryKey, invalidateFileInventory, peekFileInventory } from '@/domain/file-inventory'
import type { FilePage, SessionFile, SessionFileContent } from './files-api'

const workbench = vi.hoisted(() => ({
  platform: false, accountScope: undefined as string | undefined,
  loading: false, error: '', authRequired: false, accessPaused: false,
  currentTenantId: null as string | null, currentTenantRole: null as string | null,
  serverIdentity: null as { user: { user_id: string; username: string } } | null,
  tenants: [], snapshot: { workspaces: [], sessions: [] } as ApplicationState,
  selectSession: vi.fn(), refresh: vi.fn(),
}))
vi.mock('@/state/workbench', () => ({ useWorkbench: () => workbench }))
vi.mock('@/api/client', () => ({ api: { request: vi.fn() } }))
vi.mock('@/components/admin/space-switcher', () => ({ SpaceSwitcher: () => <div data-space-switcher /> }))

const file = (changes: Partial<SessionFile> = {}): SessionFile => ({
  id: 'generated-10-0', session_id: 'session-a', session_title: 'Research', session_archived: false,
  workspace_id: 'workspace-a', workspace_name: 'Workspace A', kind: 'generated',
  name: 'report.txt', media_type: 'text/plain', path: 'output/report.txt',
  occurred_at_ms: 1000, event_seq: 10, attachment_index: 0, run_id: 'run-a', source_status: 'online',
  ...changes,
})
const page = (items: SessionFile[] = [], next_cursor: string | null = null): FilePage => ({ items, next_cursor, offline_sources: [] })
const content = (text: string, media_type = 'text/plain'): SessionFileContent => ({ name: 'report.txt', media_type, content_base64: Buffer.from(text).toString('base64') })
function deferred<T>() {
  let resolve!: (value: T) => void
  return { promise: new Promise<T>(done => { resolve = done }), resolve: (value: T) => resolve(value) }
}

let root: Root
let host: HTMLDivElement
const request = vi.mocked(api.request)
beforeEach(() => {
  invalidateFileInventory()
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  history.replaceState({}, '', '/files')
  Object.assign(workbench, { platform: false, accountScope: undefined, loading: false, error: '', authRequired: false, accessPaused: false, currentTenantId: null, currentTenantRole: null, serverIdentity: null })
  workbench.snapshot = {
    workspaces: [{ workspace_id: 'workspace-a', title: 'Workspace A', path: '/workspace', created_at_ms: 1, updated_at_ms: 1 }],
    sessions: [{
      identity: { tenant_id: 'local', user_id: 'user', agent_id: 'agent', session_id: 'session-a' },
      workspace_id: 'workspace-a', workspace_path: '/workspace', title: 'Research', permissions: 'workspace_write',
      model: { provider: 'profile_default' }, agent_preset: 'standard', preset_plugins: [], profile_plugins: [],
      mode: 'execute', created_at_ms: 1, updated_at_ms: 1,
    }],
  }
  workbench.selectSession.mockReset()
  request.mockReset()
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})
afterEach(() => {
  act(() => root.unmount())
  host.remove()
  history.replaceState({}, '', '/')
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
})
async function render() { await act(async () => root.render(<FilesPage />)) }
async function click(label: string) {
  const button = [...document.querySelectorAll<HTMLButtonElement>('button')].find(button => button.getAttribute('aria-label') === label || button.textContent === label)
  expect(button, `missing button ${label}`).toBeDefined()
  await act(async () => button!.click())
}

it('loads metadata with URL filters and appends cursor pages without merging generated versions', async () => {
  history.replaceState({}, '', '/files?session_id=session-a&kind=generated&query=report')
  request.mockResolvedValue(page()).mockResolvedValueOnce(page([file()], 'opaque +/token')).mockResolvedValueOnce(page([file({ id: 'generated-11-0', event_seq: 11 })]))
  await render()
  const first = new URL(request.mock.calls[0][0], 'http://localhost')
  expect(first.searchParams.get('session_id')).toBe('session-a')
  expect(first.searchParams.get('kind')).toBe('generated')
  expect(first.searchParams.get('query')).toBe('report')
  expect(first.searchParams.get('limit')).toBe('100')
  expect(request).toHaveBeenCalledTimes(1)
  await click('加载更多')
  expect(new URL(request.mock.calls[1][0], 'http://localhost').searchParams.get('cursor')).toBe('opaque +/token')
  expect(host.querySelectorAll('[data-file-id]')).toHaveLength(2)
  expect(host.textContent).toContain('生成记录 #10')
  expect(host.textContent).toContain('生成记录 #11')
  expect(request.mock.calls.every(([path]) => !path.endsWith('/content'))).toBe(true)
  await click('打开原会话')
  expect(workbench.selectSession).toHaveBeenCalledWith('session-a')
  expect(location.pathname).toBe('/')
})

it('waits for Server account and space bootstrap before requesting any inventory', async () => {
  Object.assign(workbench, { platform: true, loading: true })
  request.mockResolvedValue(page([file()]))
  await render()
  expect(request).not.toHaveBeenCalled()
  Object.assign(workbench, { loading: false, accountScope: 'user:space', currentTenantId: 'space', currentTenantRole: 'viewer', serverIdentity: { user: { user_id: 'user', username: 'Reader' } } })
  await render()
  expect(request).toHaveBeenCalledTimes(1)
  expect(host.querySelector('[data-file-id]')).not.toBeNull()
})

it('reuses loaded cursor pages on return, but refreshes invalidated and expired metadata', async () => {
  request.mockResolvedValueOnce(page([file()], 'next')).mockResolvedValueOnce(page([file({ id: 'second', name: 'second.txt' })]))
  await render()
  await click('加载更多')
  await act(async () => root.render(null))
  await render()
  expect(request).toHaveBeenCalledTimes(2)
  expect(host.querySelectorAll('[data-file-id]')).toHaveLength(2)
  request.mockResolvedValue(page([file({ name: 'fresh.txt' })]))
  await act(async () => invalidateFileInventory())
  expect(request).toHaveBeenCalledTimes(3)
  expect(new URL(request.mock.calls[2][0], 'http://localhost').searchParams.has('cursor')).toBe(false)
  expect(host.textContent).toContain('fresh.txt')
  expect(host.textContent).not.toContain('second.txt')
  await act(async () => root.render(null))
  vi.spyOn(Date, 'now').mockReturnValue(Date.now() + 31_000)
  await render()
  expect(request).toHaveBeenCalledTimes(4)
})

it('does not repopulate invalidated metadata from a late in-flight response', async () => {
  const older = deferred<FilePage>(), current = deferred<FilePage>()
  request.mockReturnValueOnce(older.promise).mockReturnValueOnce(current.promise)
  await render()
  await act(async () => {
    invalidateFileInventory()
    older.resolve(page([file({ name: 'revoked.txt' })]))
    await Promise.resolve()
  })
  expect(peekFileInventory(fileInventoryKey('local', { workspace_id: '', session_id: '', kind: '', query: '' }))).toBeUndefined()
  expect(host.textContent).not.toContain('revoked.txt')
  await act(async () => current.resolve(page([file({ name: 'current.txt' })])))
  expect(host.textContent).toContain('current.txt')
})

it('never displays a previous account or filter response after its request was aborted', async () => {
  const stale = deferred<FilePage>()
  request.mockReturnValueOnce(stale.promise).mockResolvedValueOnce(page([file({ name: 'current.txt' })]))
  await render()
  const previousSignal = request.mock.calls[0][1]?.signal
  workbench.accountScope = 'another-user:space'
  await render()
  expect(previousSignal?.aborted).toBe(true)
  await act(async () => stale.resolve(page([file({ name: 'private-owner.txt' })])))
  expect(host.textContent).toContain('current.txt')
  expect(host.textContent).not.toContain('private-owner.txt')

  const staleFilter = deferred<FilePage>()
  request.mockReturnValueOnce(staleFilter.promise).mockResolvedValueOnce(page([file({ name: 'new-query.txt' })]))
  await act(async () => navigate('/files?query=old'))
  const filterSignal = request.mock.calls.at(-1)?.[1]?.signal
  await act(async () => navigate('/files?query=new'))
  await act(async () => staleFilter.resolve(page([file({ name: 'old-query.txt' })])))
  expect(filterSignal?.aborted).toBe(true)
  expect(host.textContent).toContain('new-query.txt')
  expect(host.textContent).not.toContain('old-query.txt')
})

it('loads immutable contents only on preview and renders HTML as literal text', async () => {
  const html = '<script>window.leaked = true</script><h1>literal file</h1>'
  request.mockResolvedValueOnce(page([file({ id: 'generated/10', session_id: 'session/a', name: 'report.html', media_type: 'text/html' })])).mockResolvedValueOnce(content(html, 'text/html'))
  await render()
  expect(request).toHaveBeenCalledTimes(1)
  await click('预览 report.html')
  expect(request.mock.calls[1][0]).toBe('/sessions/session%2Fa/files/generated%2F10/content')
  expect(document.querySelector('[data-file-preview] pre')?.textContent).toBe(html)
  expect(document.querySelector('[data-file-preview] script')).toBeNull()
  expect(document.querySelector('[data-file-preview] iframe')).toBeNull()
  await click('关闭预览')
  expect(document.querySelector('[data-file-preview]')).toBeNull()
})

it('discards pending preview and download contents on filter or account changes', async () => {
  const preview = deferred<SessionFileContent>()
  request.mockResolvedValueOnce(page([file()])).mockReturnValueOnce(preview.promise).mockResolvedValueOnce(page([file({ name: 'current.txt' })]))
  await render()
  await click('预览 report.txt')
  const previewSignal = request.mock.calls[1][1]?.signal
  await act(async () => navigate('/files?query=current'))
  await act(async () => preview.resolve(content('PRIVATE PREVIEW')))
  expect(previewSignal?.aborted).toBe(true)
  expect(document.querySelector('[data-file-preview]')).toBeNull()
  expect(document.body.textContent).not.toContain('PRIVATE PREVIEW')

  const download = deferred<SessionFileContent>()
  const createObjectURL = vi.fn()
  vi.stubGlobal('URL', class extends URL { static createObjectURL = createObjectURL })
  request.mockReturnValueOnce(download.promise).mockResolvedValueOnce(page())
  await click('下载 current.txt')
  const downloadSignal = request.mock.calls.at(-1)?.[1]?.signal
  workbench.accountScope = 'new-account'
  await render()
  await act(async () => download.resolve(content('PRIVATE DOWNLOAD')))
  expect(downloadSignal?.aborted).toBe(true)
  expect(createObjectURL).not.toHaveBeenCalled()
})

it('keeps offline empty inventory distinct from a complete empty result and retries failed requests', async () => {
  request.mockRejectedValueOnce(new Error('network interrupted')).mockResolvedValueOnce({ ...page(), offline_sources: [{ workspace_id: 'workspace-a', executor_id: 'node-a' }] })
  await render()
  expect(host.querySelector('[role="alert"]')?.textContent).toContain('network interrupted')
  await click('重试')
  expect(host.querySelector('[data-files-offline]')?.textContent).toContain('Workspace A')
  expect(host.querySelector('[data-files-empty]')?.textContent).toContain('当前已同步的记录')
  expect(host.querySelector('[data-files-empty]')?.textContent).not.toBe('没有符合条件的文件。')
})

it.each([
  { session_archived: true, session_id: 'session-a', label: '归档会话' },
  { session_archived: false, session_id: 'not-in-snapshot', label: '会话不在当前列表中' },
])('keeps $label sources inside the library without switching or unarchiving conversations', async values => {
  request.mockResolvedValue(page([file(values)]))
  await render()
  expect(host.textContent).toContain(values.label)
  await click('查看会话文件')
  expect(location.pathname).toBe('/files')
  expect(new URLSearchParams(location.search).get('session_id')).toBe(values.session_id)
  expect(workbench.selectSession).not.toHaveBeenCalled()
  expect(request.mock.calls.every(([path]) => path.startsWith('/files?'))).toBe(true)
})

it('uses the sidebar to filter without switching chat and preserves the search and file kind', async () => {
  history.replaceState({}, '', '/files?session_id=session-a&kind=generated&query=report')
  workbench.snapshot.workspaces.push({ workspace_id: 'workspace-b', title: 'Workspace B', path: '/second', created_at_ms: 2, updated_at_ms: 2 })
  request.mockResolvedValue(page())
  await render()
  const sidebar = host.querySelector('[data-files-navigation]')!
  expect(sidebar.querySelector('[data-file-kind="generated"]')?.getAttribute('aria-pressed')).toBe('true')
  await act(async () => sidebar.querySelector<HTMLButtonElement>('[data-file-workspace="workspace-b"]')!.click())
  const query = new URLSearchParams(location.search)
  expect(query.get('workspace_id')).toBe('workspace-b')
  expect(query.has('session_id')).toBe(false)
  expect(query.get('kind')).toBe('generated')
  expect(query.get('query')).toBe('report')
  expect(host.querySelector('[data-file-session-filter="session-a"]')).toBeNull()
  expect(workbench.selectSession).not.toHaveBeenCalled()
  await click('清除筛选')
  expect(location.search).toBe('')
})

it('keeps mobile filters accessible without duplicating desktop controls', async () => {
  vi.stubGlobal('innerWidth', 390)
  history.replaceState({}, '', '/files?session_id=archived-session')
  request.mockResolvedValue(page())
  await render()
  expect(host.querySelector('[data-files-navigation]')).toBeNull()
  expect(host.querySelector('[data-files-mobile-filters]')).not.toBeNull()
  const selected = host.querySelector('[data-choice-value="archived-session"]')
  expect(selected?.textContent).toBe('所选会话')
})
