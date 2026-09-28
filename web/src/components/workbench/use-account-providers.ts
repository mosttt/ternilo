import * as React from 'react'
import { loadProviderInventory, peekProviderInventory, subscribeProviderInventory } from '@/domain/provider-inventory'

export function useAccountProviders(enabled: boolean, tenantId?: string) {
  const target = React.useMemo(() => ({ tenantId }), [tenantId])
  const [inventory, setInventory] = React.useState(() => enabled ? peekProviderInventory(target) : null)
  const [error, setError] = React.useState('')
  const generation = React.useRef(0)
  const load = React.useCallback(async (force = false) => {
    if (!enabled) return
    const current = ++generation.current
    try {
      const value = await loadProviderInventory(force, target)
      if (current === generation.current) { setInventory(value); setError('') }
    } catch (cause) {
      if (current === generation.current) setError(cause instanceof Error ? cause.message : String(cause))
    }
  }, [enabled, target])
  React.useEffect(() => {
    setInventory(enabled ? peekProviderInventory(target) : null)
    if (!enabled) return
    const unsubscribe = subscribeProviderInventory(value => { setInventory(value); if (!value) void load() }, target)
    void load()
    return () => { generation.current += 1; unsubscribe() }
  }, [enabled, load, target])
  return { inventory, error, load }
}
