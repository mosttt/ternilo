import * as React from 'react'
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { navigate, usePathname } from '@/app/navigation'
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from './dropdown-menu'

let host: HTMLDivElement
let root: Root

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} })
  window.history.replaceState({}, '', '/')
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  vi.unstubAllGlobals()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

function CachedMenu({ controlled, onChange }: { controlled: boolean; onChange(open: boolean): void }) {
  const pathname = usePathname()
  const [open, setOpen] = React.useState(true)
  return <React.Activity mode={pathname === '/' ? 'visible' : 'hidden'}>
    <DropdownMenu defaultOpen open={controlled ? open : undefined} onOpenChange={next => { setOpen(next); onChange(next) }}>
      <DropdownMenuTrigger><span>Options</span></DropdownMenuTrigger>
      <DropdownMenuContent><DropdownMenuItem>Grouping</DropdownMenuItem></DropdownMenuContent>
    </DropdownMenu>
  </React.Activity>
}

it.each([false, true])('closes a cached menu before route navigation and keeps it closed on return (controlled=%s)', async controlled => {
  const onChange = vi.fn()
  await act(async () => root.render(<CachedMenu controlled={controlled} onChange={onChange} />))
  expect(document.querySelector('[role="menu"]')).not.toBeNull()
  await act(async () => navigate('/settings/general'))
  expect(document.querySelector('[role="menu"]')).toBeNull()
  expect(document.querySelector('[data-radix-popper-content-wrapper]')).toBeNull()
  expect(onChange).toHaveBeenCalledWith(false)
  await act(async () => navigate('/'))
  expect(document.querySelector('[role="menu"]')).toBeNull()
  await act(async () => host.querySelector('button')!.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, cancelable: true, button: 0 })))
  expect(document.querySelector('[role="menu"]')).not.toBeNull()
})

it.each([false, true])('closes cached menus before history listeners hide their Activity (controlled=%s)', async controlled => {
  const onChange = vi.fn()
  await act(async () => root.render(<CachedMenu controlled={controlled} onChange={onChange} />))
  await act(async () => {
    window.history.pushState({}, '', '/files')
    window.dispatchEvent(new PopStateEvent('popstate'))
  })
  expect(document.querySelector('[role="menu"]')).toBeNull()
  expect(onChange).toHaveBeenCalledWith(false)
  await act(async () => navigate('/'))
  expect(document.querySelector('[role="menu"]')).toBeNull()
})
