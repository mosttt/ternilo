import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { LocaleProvider } from '@/i18n/provider'
import { en, ko, zh } from '@/i18n/resources/settings'
import type { WorkspaceInfo } from './workspace-panel-context'
import { SandboxStatus } from './sandbox-status'

const surface = vi.hoisted(() => ({ sessionId: 'session', info: null as WorkspaceInfo | null, error: '', reloadInfo: vi.fn() }))
vi.mock('./workspace-panel-context', () => ({ useWorkspaceSurface: () => surface }))
let host: HTMLDivElement
let root: Root
beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  const entries = new Map<string, string>()
  vi.stubGlobal('localStorage', { getItem: (key: string) => entries.get(key) ?? null, setItem: (key: string, value: string) => entries.set(key, value) })
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
  surface.info = null; surface.error = ''; surface.reloadInfo.mockReset()
})
afterEach(() => {
  act(() => root.unmount()); host.remove(); vi.unstubAllGlobals()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

describe('execution-computer sandbox warning', () => {
  it.each([['zh', zh], ['en', en], ['ko', ko]] as const)('shows installation, repair and retry in %s without changing permissions', (locale, copy) => {
    localStorage.setItem('ternilo.locale', locale)
    surface.info = { root: '/work', applications: [], can_browse: true, sandbox: { backend: 'bubblewrap', status: 'missing', install_command: 'sudo apt install bubblewrap' } }
    act(() => root.render(<LocaleProvider><SandboxStatus /></LocaleProvider>))
    expect(host.textContent).toContain(copy['sandbox.installLinux'])
    expect(host.textContent).toContain('sudo apt install bubblewrap')
    expect(host.textContent).toContain(copy['sandbox.otherTools'])
    act(() => host.querySelector('button')!.click())
    expect(surface.reloadInfo).toHaveBeenCalledOnce()
    surface.info.sandbox = { backend: 'windows', status: 'missing' }
    act(() => root.render(<LocaleProvider><SandboxStatus /></LocaleProvider>))
    expect(host.textContent).toContain(copy['sandbox.installWindows'])
    surface.info.sandbox = { backend: 'bubblewrap', status: 'unusable', detail: 'unshare: Operation not permitted' }
    act(() => root.render(<LocaleProvider><SandboxStatus /></LocaleProvider>))
    expect(host.textContent).toContain(copy['sandbox.repair'])
    expect(host.querySelector('details')?.textContent).toContain('unshare: Operation not permitted')
    surface.info.sandbox.status = 'ready'
    act(() => root.render(<LocaleProvider><SandboxStatus /></LocaleProvider>))
    expect(host.textContent).toBe('')
  })

  it('does not claim that an older or unavailable execution host passed a check', () => {
    surface.info = { root: '/work', applications: [], can_browse: true }
    act(() => root.render(<LocaleProvider><SandboxStatus /></LocaleProvider>))
    expect(host.textContent).toBe('')
  })
})
