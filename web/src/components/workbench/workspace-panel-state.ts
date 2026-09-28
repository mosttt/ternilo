import { randomUuid } from '@/lib/random-id'

export type WorkspaceTab = { id: string; path: string | null; expanded: string[] }
export type WorkspacePane = { tabs: WorkspaceTab[]; active: string }
export type WorkspacePanelState = { open: boolean; fullscreen: boolean; panes: WorkspacePane[] }

export function fileTreeTab(): WorkspaceTab {
  return { id: randomUuid(), path: null, expanded: [] }
}

export function initialWorkspacePanel(): WorkspacePanelState {
  const tab = fileTreeTab()
  return { open: false, fullscreen: false, panes: [{ tabs: [tab], active: tab.id }] }
}

export function readWorkspacePanel(storage: Storage, key: string): WorkspacePanelState {
  try {
    const value = JSON.parse(storage.getItem(key) ?? 'null') as WorkspacePanelState | null
    if (value && typeof value.open === 'boolean' && typeof value.fullscreen === 'boolean'
      && Array.isArray(value.panes) && value.panes.length >= 1 && value.panes.length <= 2
      && value.panes.every(pane => typeof pane.active === 'string' && Array.isArray(pane.tabs) && pane.tabs.length > 0
        && pane.tabs.every(tab => typeof tab.id === 'string' && (tab.path === null || typeof tab.path === 'string')
          && Array.isArray(tab.expanded) && tab.expanded.every(path => typeof path === 'string'))
        && pane.tabs.some(tab => tab.id === pane.active))) return value
  } catch {}
  return initialWorkspacePanel()
}

export function openWorkspaceTab(state: WorkspacePanelState, paneIndex: number, path: string | null): WorkspacePanelState {
  return { ...state, open: true, panes: state.panes.map((pane, index) => {
    if (index !== paneIndex) return pane
    const existing = path === null ? undefined : pane.tabs.find(tab => tab.path === path)
    const tab = existing ?? { ...fileTreeTab(), path }
    return { tabs: existing ? pane.tabs : [...pane.tabs, tab], active: tab.id }
  }) }
}

export function closeWorkspaceTab(state: WorkspacePanelState, paneIndex: number, id: string): WorkspacePanelState {
  const panes = state.panes.map((pane, index) => {
    if (index !== paneIndex) return pane
    const tabs = pane.tabs.filter(tab => tab.id !== id)
    return { tabs, active: pane.active === id ? tabs.at(-1)?.id ?? '' : pane.active }
  }).filter(pane => pane.tabs.length > 0)
  return panes.length ? { ...state, panes } : initialWorkspacePanel()
}

export function moveWorkspaceTab(state: WorkspacePanelState, id: string, destination: number): WorkspacePanelState {
  const source = state.panes.findIndex(pane => pane.tabs.some(tab => tab.id === id))
  if (source < 0 || source === destination || !state.panes[destination]) return state
  const tab = state.panes[source]!.tabs.find(tab => tab.id === id)!
  const panes = state.panes.map((pane, index) => {
    if (index === destination) return { tabs: [...pane.tabs, tab], active: id }
    if (index !== source) return pane
    const tabs = pane.tabs.filter(item => item.id !== id)
    return { tabs, active: pane.active === id ? tabs.at(-1)?.id ?? '' : pane.active }
  }).filter(pane => pane.tabs.length)
  return { ...state, panes }
}
