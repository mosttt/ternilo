import { describe, expect, it, vi } from 'vitest'
import { registerPwa } from './pwa'

function serviceWorkers(controller: ServiceWorker | null, waiting: { postMessage: ReturnType<typeof vi.fn> } | null = null) {
  const listeners = new Map<string, EventListener>()
  const update = vi.fn().mockResolvedValue(undefined)
  const register = vi.fn().mockResolvedValue({ waiting, update })
  return {
    value: {
      controller,
      register,
      addEventListener: vi.fn((name: string, listener: EventListener) => listeners.set(name, listener)),
      removeEventListener: vi.fn((name: string) => listeners.delete(name)),
    } as unknown as ServiceWorkerContainer,
    register,
    listeners,
    update,
  }
}

describe('registerPwa', () => {
  it('does not report the first worker as an application update', async () => {
    const worker = serviceWorkers(null)
    vi.stubGlobal('navigator', { serviceWorker: worker.value })
    const onUpdate = vi.fn()
    const dispose = await registerPwa(onUpdate)
    worker.listeners.get('controllerchange')?.(new Event('controllerchange'))
    expect(worker.register).toHaveBeenCalledWith('/service-worker.js', { scope: '/', updateViaCache: 'none' })
    expect(onUpdate).not.toHaveBeenCalled()
    dispose()
    vi.unstubAllGlobals()
  })

  it('activates a waiting replacement and reports one controller change', async () => {
    const waiting = { postMessage: vi.fn() }
    const worker = serviceWorkers({} as ServiceWorker, waiting)
    vi.stubGlobal('navigator', { serviceWorker: worker.value })
    const onUpdate = vi.fn()
    const dispose = await registerPwa(onUpdate)
    expect(waiting.postMessage).toHaveBeenCalledWith({ type: 'SKIP_WAITING' })
    worker.listeners.get('controllerchange')?.(new Event('controllerchange'))
    worker.listeners.get('controllerchange')?.(new Event('controllerchange'))
    expect(onUpdate).toHaveBeenCalledOnce()
    dispose()
    vi.unstubAllGlobals()
  })
})
