import { api } from '@/api/client'

export interface WorkerRecord {
  worker_id: string
  storage_id: string
  registered: boolean
  online: boolean
  created_at_ms: number
  last_seen_at_ms: number | null
  lease_expires_at_ms: number | null
  revoked_at_ms: number | null
}

export interface WorkerGrant {
  worker_id: string
  storage_id: string
  token: string
}

export function listWorkers(signal?: AbortSignal) {
  return api.request<WorkerRecord[]>('/admin/workers', { signal })
}

export function createWorker(workerId: string, storageId?: string) {
  return api.request<WorkerGrant>('/admin/workers', {
    method: 'POST', body: { worker_id: workerId, ...(storageId ? { storage_id: storageId } : {}) },
  })
}

export function revokeWorker(workerId: string) {
  return api.request<void>(`/admin/workers/${encodeURIComponent(workerId)}`, { method: 'DELETE' })
}

export function workerSetupCommand(serverUrl: string, token: string): string {
  const quote = (value: string) => `'${value.replaceAll("'", "'\\''")}'`
  return `TERNILO_WORKER_TOKEN=${quote(token)} ternilo-worker init \\\n  --server-url ${quote(serverUrl)}`
}
