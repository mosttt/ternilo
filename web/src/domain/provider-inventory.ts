import { api } from '@/api/client'
import { invalidateAllCloudModelInventories, invalidateCloudModelInventory, resetCloudModelInventoryForTest } from './cloud-model-inventory'
import {
  executionTargetKey,
  executionTargetHeaders,
  executionTargetPath,
  type ExecutionTarget,
} from '@/domain/execution-target'
import type { CredentialInventory, ProviderProfile } from '@/types'

export interface ProviderInventorySnapshot {
  providers: ProviderProfile[]
  credentials: CredentialInventory
}

interface ProviderInventoryStore {
  cached: ProviderInventorySnapshot | null
  inFlight: Promise<ProviderInventorySnapshot> | null
  generation: number
  listeners: Set<(snapshot: ProviderInventorySnapshot | null) => void>
}

const stores = new Map<string, ProviderInventoryStore>()

function storeFor(target: ExecutionTarget = {}) {
  const key = executionTargetKey(target)
  let store = stores.get(key)
  if (!store) {
    store = { cached: null, inFlight: null, generation: 0, listeners: new Set() }
    stores.set(key, store)
  }
  return store
}

function publish(store: ProviderInventoryStore) {
  for (const listener of store.listeners) listener(store.cached)
}

function invalidate(store: ProviderInventoryStore) {
  store.generation += 1
  store.cached = null
  store.inFlight = null
  publish(store)
}

export function peekProviderInventory(target: ExecutionTarget = {}) {
  return storeFor(target).cached
}

export function invalidateProviderInventory(target: ExecutionTarget = {}) {
  if (!target.sessionId && !target.workspaceId) invalidateAllCloudModelInventories()
  else invalidateCloudModelInventory(target)
  if (target.executorId) {
    // Session and workspace routes can refer to the same computer inventory.
    for (const [key, store] of stores) {
      if (!target.tenantId || key.startsWith(`space:${target.tenantId}:`)) invalidate(store)
    }
  } else invalidate(storeFor(target))
}

export function invalidateAllProviderInventories() {
  invalidateAllCloudModelInventories()
  for (const store of stores.values()) invalidate(store)
}

export function subscribeProviderInventory(
  listener: (snapshot: ProviderInventorySnapshot | null) => void,
  target: ExecutionTarget = {},
) {
  const store = storeFor(target)
  store.listeners.add(listener)
  return () => { store.listeners.delete(listener) }
}

/**
 * Provider configuration belongs to an execution target. Keep one shared
 * read in flight per Cloud or Node target and reuse it while the composer
 * remounts within that target.
 */
export function loadProviderInventory(
  force = false,
  target: ExecutionTarget = {},
): Promise<ProviderInventorySnapshot> {
  const store = storeFor(target)
  if (!force && store.cached) return Promise.resolve(store.cached)
  if (store.inFlight) return store.inFlight

  const requestGeneration = store.generation
  const pending = Promise.all([
    api.request<ProviderProfile[]>(executionTargetPath('/providers', target), { headers: executionTargetHeaders(target) }),
    api.request<CredentialInventory>(executionTargetPath('/credentials', target), { headers: executionTargetHeaders(target) }),
  ]).then(([providers, credentials]) => ({ providers, credentials }))
  let tracked: Promise<ProviderInventorySnapshot>
  tracked = pending
    .then(snapshot => {
      if (requestGeneration !== store.generation) {
        return store.inFlight && store.inFlight !== tracked
          ? store.inFlight
          : loadProviderInventory(true, target)
      }
      store.cached = snapshot
      publish(store)
      return snapshot
    })
    .finally(() => {
      if (store.inFlight === tracked) store.inFlight = null
    })
  store.inFlight = tracked
  return tracked
}

export function resetProviderInventoryForTest() {
  resetCloudModelInventoryForTest()
  stores.clear()
}
