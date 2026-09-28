import * as React from 'react'
import { act } from 'react'
import { createRoot } from 'react-dom/client'
import { afterEach, describe, expect, it } from 'vitest'
import { ContributionRegistry } from './contribution-registry'

;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true

const roots: ReturnType<typeof createRoot>[] = []
afterEach(() => {
  for (const root of roots.splice(0)) act(() => root.unmount())
  document.body.replaceChildren()
})

describe('ContributionRegistry React binding', () => {
  it('updates a mounted outlet when a plugin registers and unloads', () => {
    const registry = new ContributionRegistry<{ id: string }>(entry => entry.id)
    const host = document.body.appendChild(document.createElement('div'))
    const root = createRoot(host)
    roots.push(root)

    function Outlet() {
      const entries = React.useSyncExternalStore(
        registry.subscribe,
        registry.getSnapshot,
        registry.getSnapshot,
      )
      return <div>{entries.map(entry => <span key={entry.id}>{entry.id}</span>)}</div>
    }

    act(() => root.render(<Outlet />))
    expect(host.textContent).toBe('')

    let dispose: () => void = () => undefined
    act(() => { dispose = registry.register({ id: 'timeline' }) })
    expect(host.textContent).toBe('timeline')

    act(() => dispose())
    expect(host.textContent).toBe('')
  })
})
