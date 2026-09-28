import { describe, expect, it, vi } from 'vitest'
import { closeWorkspaceTab, fileTreeTab, initialWorkspacePanel, moveWorkspaceTab, openWorkspaceTab, readWorkspacePanel } from './workspace-panel-state'

describe('personal workspace tabs', () => {
  it('initializes and opens tabs without the secure-context randomUUID API', () => {
    const native = vi.spyOn(crypto, 'randomUUID').mockImplementation(() => { throw new Error('native UUID must not be used') })
    vi.stubGlobal('crypto', { getRandomValues: crypto.getRandomValues.bind(crypto) })
    try {
      const panel = openWorkspaceTab(initialWorkspacePanel(), 0, 'proof.txt')
      expect(panel.panes[0]!.tabs).toHaveLength(2)
      for (const tab of panel.panes[0]!.tabs) expect(tab.id).toMatch(/^[\da-f]{8}-[\da-f]{4}-4[\da-f]{3}-[89ab][\da-f]{3}-[\da-f]{12}$/)
      expect(new Set(panel.panes[0]!.tabs.map(tab => tab.id)).size).toBe(2)
      expect(native).not.toHaveBeenCalled()
    } finally {
      vi.unstubAllGlobals()
      native.mockRestore()
    }
  })
  it('keeps document identity when reopening or moving, closes an empty pane and resets the last tab', () => {
    const initial = initialWorkspacePanel()
    const opened = openWorkspaceTab(initial, 0, 'src/main.rs')
    const reopened = openWorkspaceTab(opened, 0, 'src/main.rs')
    expect(reopened.panes[0]!.tabs).toHaveLength(2)
    expect(reopened.panes[0]!.active).toBe(opened.panes[0]!.active)
    const tree = fileTreeTab()
    const split = { ...reopened, panes: [...reopened.panes, { tabs: [tree], active: tree.id }] }
    const moved = moveWorkspaceTab(split, tree.id, 0)
    expect(moved.panes).toHaveLength(1)
    expect(moved.panes[0]!.tabs.at(-1)).toBe(tree)
    expect(moveWorkspaceTab(moved, 'external-drop', 0)).toBe(moved)
    let closed = moved
    for (const tab of moved.panes[0]!.tabs) closed = closeWorkspaceTab(closed, 0, tab.id)
    expect(closed.open).toBe(false)
    expect(closed.panes[0]!.tabs[0]!.path).toBeNull()
  })

  it('restores only the selected account/session and discards invalid saved state', () => {
    const panel = openWorkspaceTab(initialWorkspacePanel(), 0, 'private.md')
    const values = new Map<string, string>()
    const storage = { getItem: (key: string) => values.get(key) ?? null } as Storage
    values.set('owner/session', JSON.stringify(panel))
    expect(readWorkspacePanel(storage, 'owner/session')).toEqual(panel)
    expect(readWorkspacePanel(storage, 'peer/session').open).toBe(false)
    values.set('owner/session', JSON.stringify({ ...panel, panes: [{ tabs: [], active: 'missing' }] }))
    expect(readWorkspacePanel(storage, 'owner/session').open).toBe(false)
  })
})
