// @vitest-environment jsdom

import * as React from 'react'
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { useTrajectoryTail } from './use-trajectory-tail'

;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true

function Probe({ revision }: { revision: number }) {
  const ref = useTrajectoryTail('session-a', revision)
  return <div ref={ref} />
}

let host: HTMLDivElement
let root: Root
let scrollTop: number
let scrollHeight: number

beforeEach(() => {
  vi.stubGlobal('ResizeObserver', class {
    constructor(_callback: ResizeObserverCallback) {}
    observe() {}
    unobserve() {}
    disconnect() {}
  })
  host = document.createElement('div')
  host.className = 'conversation-scroll'
  scrollTop = 0
  scrollHeight = 1_000
  Object.defineProperties(host, {
    scrollTop: {
      configurable: true,
      get: () => scrollTop,
      set: value => { scrollTop = Number(value) },
    },
    scrollHeight: { configurable: true, get: () => scrollHeight },
    clientHeight: { configurable: true, get: () => 300 },
  })
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
})

describe('trajectory view scroll ownership', () => {
  it('opens at the restored view position and follows later growth only after the reader reaches the tail', async () => {
    act(() => root.render(<Probe revision={1} />))
    await act(async () => { await new Promise(resolve => setTimeout(resolve, 50)) })
    expect(scrollTop).toBe(0)

    scrollTop = 700
    act(() => host.dispatchEvent(new Event('scroll')))
    scrollHeight = 1_200
    act(() => root.render(<Probe revision={2} />))
    await act(async () => { await new Promise(resolve => setTimeout(resolve, 30)) })
    expect(scrollTop).toBe(1_200)
  })
})
