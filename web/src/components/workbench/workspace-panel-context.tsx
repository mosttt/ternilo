import * as React from 'react'
import { api } from '@/api/client'
import { initialWorkspacePanel, readWorkspacePanel, type WorkspacePanelState } from './workspace-panel-state'

export type WorkspaceApplication = { id: string; label: string; icon?: string }
export type WorkspaceInfo = { root: string; can_browse: boolean; applications: WorkspaceApplication[] }
export type WorkspaceDirectory = { path: string; entries: { name: string; kind: 'directory' | 'file' | 'other' }[]; truncated: boolean }
export type WorkspacePreview = { path: string; bytes: number; media_type: string; encoding: 'utf8' | 'base64' | 'unsupported'; content: string; truncated: boolean }

export function workspaceRequest<Result>(sessionId: string, body?: unknown, signal?: AbortSignal): Promise<Result> {
  return api.request<Result>(`/sessions/${encodeURIComponent(sessionId)}/workspace`, { ...(body ? { method: 'POST', body } : {}), signal })
}

export function useWorkspacePanel(sessionId: string | null, accountScope: string | undefined, revision: string, onOpen: () => void, notifyError: (message: string) => void) {
  const key = JSON.stringify(['ternilo.workspace-panel', accountScope ?? 'local', sessionId])
  const saved = React.useMemo(() => sessionId ? readWorkspacePanel(localStorage, key) : initialWorkspacePanel(), [key, sessionId])
  const [current, setCurrent] = React.useState({ key, state: saved })
  const state = current.key === key ? current.state : saved
  const [result, setResult] = React.useState<{ key: string; info: WorkspaceInfo | null; error: string }>({ key, info: null, error: '' })
  const [reloadIndex, reloadInfo] = React.useReducer(value => value + 1, 0)
  const infoKey = JSON.stringify([key, revision, reloadIndex])
  const info = result.key === infoKey ? result.info : null
  const error = result.key === infoKey ? result.error : ''
  const preferredKey = JSON.stringify(['ternilo.open-in-app', accountScope ?? 'local'])
  const [choice, setChoice] = React.useState(() => localStorage.getItem(preferredKey) ?? '')
  const [launching, setLaunching] = React.useState(false)
  const launchFlight = React.useRef(false)
  const triggerRef = React.useRef<HTMLButtonElement>(null)
  React.useEffect(() => setChoice(localStorage.getItem(preferredKey) ?? ''), [preferredKey])
  React.useEffect(() => {
    if (!sessionId) return
    const controller = new AbortController()
    void workspaceRequest<WorkspaceInfo>(sessionId, undefined, controller.signal)
      .then(info => { if (!controller.signal.aborted) setResult({ key: infoKey, info, error: '' }) })
      .catch(cause => { if (!controller.signal.aborted) setResult({ key: infoKey, info: null, error: cause instanceof Error ? cause.message : String(cause) }) })
    return () => controller.abort()
  }, [infoKey, sessionId])
  const update = (mutate: (state: WorkspacePanelState) => WorkspacePanelState) => {
    setCurrent(previous => {
      const state = mutate(previous.key === key ? previous.state : saved)
      if (sessionId) localStorage.setItem(key, JSON.stringify(state))
      return { key, state }
    })
  }
  const show = () => { onOpen(); update(state => ({ ...state, open: true })) }
  const hide = () => {
    update(state => ({ ...state, open: false }))
    requestAnimationFrame(() => triggerRef.current?.focus({ preventScroll: true }))
  }
  const launch = async (app: WorkspaceApplication) => {
    if (!sessionId || launchFlight.current) return
    launchFlight.current = true
    setLaunching(true)
    try {
      await workspaceRequest(sessionId, { kind: 'open', app_id: app.id })
      localStorage.setItem(preferredKey, app.id)
      setChoice(app.id)
    } finally { launchFlight.current = false; setLaunching(false) }
  }
  return { sessionId, key, state, update, info, error, reloadInfo, show, hide, triggerRef, launch, launching, choice, notifyError }
}

type WorkspacePanelContextValue = ReturnType<typeof useWorkspacePanel>
export const WorkspacePanelContext = React.createContext<WorkspacePanelContextValue | null>(null)
export function useWorkspaceSurface() { return React.useContext(WorkspacePanelContext) }
