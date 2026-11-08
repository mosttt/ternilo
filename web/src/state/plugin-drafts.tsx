import * as React from 'react'

export interface PluginDraft {
  draft: Record<string, unknown>
  texts: Record<string, string>
  rawText: string
  rawInvalid: boolean
  touched: Set<string>
}

type StoredDraft = { baseline: string; value: PluginDraft; pendingSource?: string }

export function pluginConfigBaseline(config: Record<string, unknown>) {
  return JSON.stringify(config, (_key, value: unknown) => value && typeof value === 'object' && !Array.isArray(value)
    ? Object.fromEntries(Object.entries(value).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0)) : value)
}
const PluginDraftContext = React.createContext<{
  locks: Set<string>
  locksRef: React.RefObject<Set<string>>
  setLocks: React.Dispatch<React.SetStateAction<Set<string>>>
  drafts: Record<string, StoredDraft>
  setDrafts: React.Dispatch<React.SetStateAction<Record<string, StoredDraft>>>
} | null>(null)

function initialStore(scope: string) {
  return { scope, drafts: {} as Record<string, StoredDraft>, locks: new Set<string>(), locksRef: { current: new Set<string>() } }
}

// Reset only plugin state on account changes, preserving in-flight authentication UI.
export function PluginDraftProvider({ children, scope = '' }: { children: React.ReactNode; scope?: string }) {
  const [state, setState] = React.useState(() => initialStore(scope))
  if (state.scope !== scope) setState(initialStore(scope))
  const setDrafts: React.Dispatch<React.SetStateAction<Record<string, StoredDraft>>> = action => {
    setState(current => current.locksRef !== state.locksRef ? current : {
      ...current, drafts: typeof action === 'function' ? action(current.drafts) : action,
    })
  }
  const setLocks: React.Dispatch<React.SetStateAction<Set<string>>> = action => {
    setState(current => current.locksRef !== state.locksRef ? current : {
      ...current, locks: typeof action === 'function' ? action(current.locks) : action,
    })
  }
  return <PluginDraftContext.Provider value={{ drafts: state.drafts, setDrafts, locks: state.locks, locksRef: state.locksRef, setLocks }}>{children}</PluginDraftContext.Provider>
}

export function usePluginDraft(key: string | undefined, baseline: string, initial: PluginDraft) {
  const shared = React.useContext(PluginDraftContext)
  const [local, setLocal] = React.useState<StoredDraft>(() => ({ baseline, value: initial }))
  const stored = key && shared ? shared.drafts[key] : local
  const matchesSource = (entry: StoredDraft | undefined) => entry?.baseline === baseline || entry?.pendingSource === baseline
  const current = stored && matchesSource(stored) ? stored : { baseline, value: initial }
  const value = current.value
  const acceptedBaseline = current.baseline
  const update = (action: React.SetStateAction<PluginDraft>) => {
    const apply = (previous: StoredDraft | undefined): StoredDraft => {
      const current = previous && matchesSource(previous) ? previous : { baseline, value: initial }
      return { ...current, value: typeof action === 'function' ? action(current.value) : action }
    }
    if (key && shared) shared.setDrafts(current => ({ ...current, [key]: apply(current[key]) }))
    else setLocal(apply)
  }
  // A completed write must not replace edits made from another entry point while it was pending.
  const settle = (next: PluginDraft, savedBaseline?: string) => {
    const unchanged = (previous: StoredDraft | undefined) => previous
      ? matchesSource(previous) && previous.value === value : stored === undefined
    // Remember the accepted write until its asynchronous profile projection catches up.
    const accepted = { baseline: savedBaseline ?? baseline, value: next,
      ...(savedBaseline && savedBaseline !== baseline ? { pendingSource: baseline } : {}) }
    if (key && shared) shared.setDrafts(current => {
      return unchanged(current[key]) ? { ...current, [key]: accepted } : current
    })
    else setLocal(current => unchanged(current) ? accepted : current)
  }
  React.useEffect(() => {
    if (!stored || (stored.baseline === baseline && !stored.pendingSource) || stored.pendingSource === baseline) return
    const synchronize = (current: StoredDraft): StoredDraft => {
      if (current.baseline === baseline && current.pendingSource) return { baseline, value: current.value }
      // An edit can commit after this render but before its baseline effect runs.
      return current === stored ? { baseline, value: initial } : current
    }
    if (key && shared) shared.setDrafts(current => {
      const previous = current[key]
      if (!previous) return current
      const next = synchronize(previous)
      return next === previous ? current : { ...current, [key]: next }
    })
    else setLocal(synchronize)
  }, [baseline, key, stored?.baseline, stored?.pendingSource])
  return [value, update, settle, acceptedBaseline] as const
}

// A save may outlive a dialog. Share its lock with the next configuration entry point.
export function usePluginWriteLock(key: string) {
  const shared = React.useContext(PluginDraftContext)
  const localRef = React.useRef(new Set<string>())
  const [local, setLocal] = React.useState(new Set<string>())
  const ref = shared?.locksRef ?? localRef
  const setLocks = shared?.setLocks ?? setLocal
  const start = () => {
    if (ref.current.has(key)) return false
    ref.current.add(key)
    setLocks(new Set(ref.current))
    return true
  }
  const finish = () => {
    ref.current.delete(key)
    setLocks(new Set(ref.current))
  }
  return [(shared?.locks ?? local).has(key), start, finish] as const
}
