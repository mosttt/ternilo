import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { LocaleProvider } from '@/i18n/provider'
import { AboutSettings } from './about-settings'

const workbench = vi.hoisted(() => ({
  platform: false,
  remote: false,
}))

vi.mock('@/state/workbench', () => ({
  useWorkbench: () => ({
    catalog: { revision: 'test' },
    currentSession: null,
    currentWorkspace: null,
    remote: workbench.remote,
    platform: workbench.platform,
  }),
}))

let host: HTMLDivElement
let root: Root

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
  workbench.platform = false
  workbench.remote = false
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

function renderSettings() {
  act(() => root.render(<LocaleProvider><AboutSettings /></LocaleProvider>))
}

describe('AboutSettings connection presentation', () => {
  it('uses a cloud icon for Server and relay connections, and a laptop only for local direct', () => {
    workbench.platform = true
    renderSettings()
    expect(host.textContent).toContain('Ternilo Server')
    expect(host.querySelector('.lucide-cloud-cog')).not.toBeNull()
    expect(host.querySelector('.lucide-laptop')).toBeNull()

    workbench.platform = false
    workbench.remote = true
    renderSettings()
    expect(host.textContent).toContain('远程中继')
    expect(host.querySelector('.lucide-cloud-cog')).not.toBeNull()

    workbench.remote = false
    renderSettings()
    expect(host.textContent).toContain('本机直连')
    expect(host.querySelector('.lucide-laptop')).not.toBeNull()
    expect(host.querySelector('.lucide-cloud-cog')).toBeNull()
  })
})
