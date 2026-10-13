import * as React from 'react'

export interface PluginDraft {
  draft: Record<string, unknown>
  texts: Record<string, string>
  rawText: string
  rawInvalid: boolean
  touched: Set<string>
}

type StoredDraft = { baseline: string; value: PluginDraft }
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
  const value = stored?.baseline === baseline ? stored.value : initial
  const update = (action: React.SetStateAction<PluginDraft>) => {
    const apply = (previous: StoredDraft | undefined): StoredDraft => {
      const current = previous?.baseline === baseline ? previous.value : initial
      return { baseline, value: typeof action === 'function' ? action(current) : action }
    }
    if (key && shared) shared.setDrafts(current => ({ ...current, [key]: apply(current[key]) }))
    else setLocal(apply)
  }
  React.useEffect(() => {
    if (stored && stored.baseline !== baseline) update(initial)
  }, [baseline, key, stored?.baseline])
  return [value, update] as const
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
