import { afterEach, describe, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import { createWorker, revokeWorker, workerSetupCommand } from './workers-api'

afterEach(() => vi.restoreAllMocks())

describe('Worker management requests', () => {
  it('lets the server assign independent storage unless shared storage is explicit', async () => {
    const request = vi.spyOn(api, 'request').mockResolvedValue({})
    await createWorker('worker-01')
    expect(request).toHaveBeenLastCalledWith('/admin/workers', { method: 'POST', body: { worker_id: 'worker-01' } })
    await createWorker('worker-02', 'shared-volume')
    expect(request).toHaveBeenLastCalledWith('/admin/workers', { method: 'POST', body: { worker_id: 'worker-02', storage_id: 'shared-volume' } })
  })

  it('encodes the exact Worker identity when revoking access', async () => {
    const request = vi.spyOn(api, 'request').mockResolvedValue(undefined)
    await revokeWorker('worker/one')
    expect(request).toHaveBeenCalledWith('/admin/workers/worker%2Fone', { method: 'DELETE' })
  })

  it('quotes both the independent credential and Server URL without database configuration', () => {
    expect(workerSetupCommand('https://server.example', 'ter_w_one')).toBe("TERNILO_WORKER_TOKEN='ter_w_one' ternilo-worker init \\\n  --server-url 'https://server.example'")
    const command = workerSetupCommand("https://server.example/path'$(id)", "token'`id`")
    expect(command).toContain("'token'\\''`id`'")
    expect(command).toContain("'https://server.example/path'\\''$(id)'")
    expect(command).not.toMatch(/database|master.key|policy|--token|\n\+/i)
  })
})
