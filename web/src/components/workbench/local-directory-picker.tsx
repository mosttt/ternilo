import * as React from 'react'
import {
  Check, ChevronRight, Edit3, Folder, FolderOpen, FolderSearch, Plus,
} from 'lucide-react'
import { api } from '@/api/client'
import {
  directoryDraftParts,
  directoryParentPath,
  directorySeparator,
  displayDirectoryCrumbs,
  sameDirectoryPath,
  visibleDirectoryEntries,
} from '@/domain/directory-browser'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import type { DirectoryEntry, DirectoryListing } from '@/types'
import { Button } from '@/components/ui/button'
import {
  Dialog, DialogContent, DialogDescription, DialogTitle,
} from '@/components/ui/dialog'
import styles from './local-directory-picker.module.css'

const SLOW_SCAN_DELAY_MS = 300
const PARENT_LEG_WAIT_MS = 200
const DRAFT_PREVIEW_DEBOUNCE_MS = 250

interface ScannedDirectory {
  directory: string
  landed: string
}

interface BrowserView {
  parent: DirectoryListing | null
  selected: DirectoryEntry | null
  child: DirectoryListing | null
}

export interface DirectoryBrowserProps {
  open: boolean
  initialPath?: string
  title?: string
  description?: string
  confirmLabel?: string
  listDirectory(path?: string, signal?: AbortSignal): Promise<DirectoryListing>
  createDirectory(path: string, name: string): Promise<string>
  onOpen(path: string): void | Promise<void>
  onClose(): void
  busy?: boolean
  onNativePick?: (() => void | Promise<void>) | undefined
}

function failureText(error: unknown): string {
  if (error !== null && typeof error === 'object' && 'rpcError' in error) {
    const rpcError = error.rpcError
    if (rpcError !== null && typeof rpcError === 'object' && 'message' in rpcError
      && typeof rpcError.message === 'string') return rpcError.message
  }
  return error instanceof Error ? error.message : String(error)
}

function DirectoryColumn({
  listing,
  selectedPath,
  showHidden,
  prefix,
  disabled,
  pathEditing,
  onPick,
}: {
  listing: DirectoryListing
  selectedPath: string | null
  showHidden: boolean
  prefix: string | null
  disabled: boolean
  pathEditing: boolean
  onPick(entry: DirectoryEntry): void
}) {
  const t = useTranslate('workspace')
  const entries = visibleDirectoryEntries(listing.entries, selectedPath, showHidden, prefix)
  return (
    <div className={styles.column} role="list" aria-label={t('browser.directoryAria', { path: listing.path })}>
      {entries.map((entry) => {
        const selected = entry.path === selectedPath
        return (
          <span className={styles.rowSeat} role="listitem" key={entry.path}>
            <button
              type="button"
              className={`${styles.row} ${selected ? styles.rowSelected : ''}`}
              aria-current={selected || undefined}
              disabled={disabled}
              onMouseDown={pathEditing ? event => event.preventDefault() : undefined}
              onClick={() => onPick(entry)}
            >
              {selected
                ? <FolderOpen className={styles.rowIconSelected} aria-hidden="true" />
                : <Folder className={styles.rowIcon} aria-hidden="true" />}
              <span className={styles.rowName}>{entry.name}</span>
              <ChevronRight className={styles.rowChevron} aria-hidden="true" />
            </button>
          </span>
        )
      })}
      {entries.length === 0 && <p className={styles.empty}>{t('browser.empty')}</p>}
    </div>
  )
}

export function DirectoryBrowser({
  open,
  initialPath,
  title,
  description,
  confirmLabel,
  listDirectory,
  createDirectory,
  onOpen,
  onClose,
  busy = false,
  onNativePick,
}: DirectoryBrowserProps) {
  const t = useTranslate('workspace')
  const [view, setView] = React.useState<BrowserView>({ parent: null, selected: null, child: null })
  const [loading, setLoading] = React.useState(false)
  const [slowScan, setSlowScan] = React.useState(false)
  const [scanWindow, setScanWindow] = React.useState(0)
  const [error, setError] = React.useState<string | null>(null)
  const [pathDraft, setPathDraft] = React.useState<string | null>(null)
  const [showHidden, setShowHidden] = React.useState(false)
  const [folderDraft, setFolderDraft] = React.useState<string | null>(null)
  const [creatingFolder, setCreatingFolder] = React.useState(false)
  const [createError, setCreateError] = React.useState<string | null>(null)
  const [opening, setOpening] = React.useState(false)
  const requestSeq = React.useRef(0)
  const scanController = React.useRef<AbortController | null>(null)
  const openGeneration = React.useRef(0)
  const composingRef = React.useRef(false)
  const crumbTrailRef = React.useRef<HTMLSpanElement | null>(null)
  const millerRowRef = React.useRef<HTMLDivElement | null>(null)
  const pathInputRef = React.useRef<HTMLInputElement | null>(null)
  const editZoneRef = React.useRef<HTMLButtonElement | null>(null)
  const newFolderButtonRef = React.useRef<HTMLButtonElement | null>(null)
  const previewSuspended = React.useRef(false)
  const scanned = React.useRef<ScannedDirectory | null>(null)
  const refocusPathInput = React.useRef(false)
  const refocusPick = React.useRef(false)
  const refocusEditZone = React.useRef(false)
  const viewRef = React.useRef(view)

  React.useEffect(() => { viewRef.current = view }, [view])
  React.useEffect(() => () => {
    requestSeq.current += 1
    openGeneration.current += 1
    scanController.current?.abort()
  }, [])

  const compositionGuard = {
    onCompositionStart: () => { composingRef.current = true },
    onCompositionEnd: () => { composingRef.current = false },
  }

  const supersede = React.useCallback((): number => {
    scanController.current?.abort()
    scanController.current = null
    return ++requestSeq.current
  }, [])

  const restartSlowScanWindow = React.useCallback(() => {
    setSlowScan(false)
    setScanWindow(value => value + 1)
  }, [])

  const launchListing = React.useCallback((path: string | undefined) => {
    const seq = supersede()
    const controller = new AbortController()
    scanController.current = controller
    restartSlowScanWindow()
    return { seq, scan: listDirectory(path, controller.signal) }
  }, [listDirectory, restartSlowScanWindow, supersede])

  const continueScan = React.useCallback((path: string) => {
    const controller = new AbortController()
    scanController.current = controller
    restartSlowScanWindow()
    return listDirectory(path, controller.signal)
  }, [listDirectory, restartSlowScanWindow])

  const land = React.useCallback((path: string | undefined, options: { closeEditor: boolean; announce: boolean }) => {
    const { seq, scan } = launchListing(path)
    setLoading(true)
    if (options.announce) setError(null)
    const settle = () => {
      setLoading(false)
      if (options.closeEditor) {
        setPathDraft(null)
      } else {
        setError(null)
        refocusPathInput.current = true
      }
    }
    scan.then((target) => {
      if (seq !== requestSeq.current) return
      if (!options.closeEditor && path !== undefined) scanned.current = { directory: path, landed: target.path }
      let landed = false
      const landSingle = () => {
        if (landed || seq !== requestSeq.current) return
        landed = true
        setView({ parent: target, selected: null, child: null })
        settle()
      }
      if (displayDirectoryCrumbs(target, '').length < 2) {
        landSingle()
        return
      }
      const parentPath = directoryParentPath(target)
      if (parentPath === null) {
        landSingle()
        return
      }
      continueScan(parentPath).then((parentLevel) => {
        if (seq !== requestSeq.current) return
        const selected = parentLevel.entries.find(entry => sameDirectoryPath(parentLevel, entry.path, target.path))
        if (selected === undefined) {
          landSingle()
          return
        }
        landed = true
        setView({ parent: parentLevel, selected, child: target })
        settle()
      }, () => landSingle())
      if (options.closeEditor) window.setTimeout(landSingle, PARENT_LEG_WAIT_MS)
    }, (reason: unknown) => {
      if (seq !== requestSeq.current) return
      setLoading(false)
      if (options.announce) setError(failureText(reason))
    })
  }, [continueScan, launchListing])

  const navigate = React.useCallback((path?: string) => {
    land(path, { closeEditor: true, announce: true })
  }, [land])

  const select = React.useCallback((entry: DirectoryEntry) => {
    const { seq, scan } = launchListing(entry.path)
    if (pathDraft !== null) refocusPick.current = true
    setPathDraft(null)
    setView(current => ({ ...current, selected: entry, child: null }))
    setLoading(true)
    setError(null)
    scan.then((child) => {
      if (seq !== requestSeq.current) return
      setView(current => ({ ...current, child }))
      setLoading(false)
    }, (reason: unknown) => {
      if (seq !== requestSeq.current) return
      setLoading(false)
      setError(failureText(reason))
      setView(current => ({ ...current, selected: null, child: null }))
      refocusEditZone.current = true
    })
  }, [launchListing, pathDraft])

  const advance = React.useCallback((entry: DirectoryEntry) => {
    const child = view.child
    if (child === null) return
    setView({ parent: child, selected: null, child: null })
    select(entry)
  }, [select, view.child])

  const previewDraftLevel = React.useCallback((directory: string) => {
    land(directory, { closeEditor: false, announce: false })
  }, [land])

  const cancelPathEdit = React.useCallback(() => {
    supersede()
    setLoading(false)
    setPathDraft(null)
    setError(null)
    if (view.child === null) setView(current => ({ ...current, selected: null }))
    if (view.parent === null) navigate(initialPath)
  }, [initialPath, navigate, supersede, view.child, view.parent])

  React.useEffect(() => {
    openGeneration.current += 1
    if (open) {
      setView({ parent: null, selected: null, child: null })
      setCreatingFolder(false)
      setShowHidden(false)
      setOpening(false)
      scanned.current = null
      previewSuspended.current = false
      navigate(initialPath)
      return
    }
    supersede()
    setLoading(false)
    setSlowScan(false)
    setError(null)
    setPathDraft(null)
    setFolderDraft(null)
    setCreateError(null)
    setOpening(false)
    refocusPick.current = false
    refocusEditZone.current = false
  }, [initialPath, navigate, open, supersede])

  React.useEffect(() => {
    if (!loading) {
      setSlowScan(false)
      return
    }
    const timer = window.setTimeout(() => setSlowScan(true), SLOW_SCAN_DELAY_MS)
    return () => window.clearTimeout(timer)
  }, [loading, scanWindow])

  React.useEffect(() => {
    if (pathDraft === null) return
    const timer = window.setTimeout(() => {
      if (previewSuspended.current) return
      const current = viewRef.current.child ?? viewRef.current.parent
      if (current === null) return
      const { directory, prefix } = directoryDraftParts(current, pathDraft, scanned.current)
      if (directory === null || prefix !== null) return
      previewDraftLevel(directory)
    }, DRAFT_PREVIEW_DEBOUNCE_MS)
    return () => window.clearTimeout(timer)
  }, [pathDraft, previewDraftLevel])

  const crumbSource = view.child ?? view.parent
  const typedPrefix = crumbSource === null || pathDraft === null
    ? null
    : directoryDraftParts(crumbSource, pathDraft, scanned.current).prefix
  const crumbs = crumbSource === null ? [] : displayDirectoryCrumbs(crumbSource, t('browser.home'))
  const crumbTail = crumbs.at(-1)?.path
  React.useEffect(() => {
    const trail = crumbTrailRef.current
    if (trail !== null) trail.scrollLeft = trail.scrollWidth
  }, [crumbTail])
  const childPath = view.child?.path
  React.useEffect(() => {
    const row = millerRowRef.current
    if (row !== null && childPath !== undefined) row.scrollLeft = row.scrollWidth
  }, [childPath])

  React.useEffect(() => {
    if (refocusPathInput.current) {
      refocusPathInput.current = false
      if (document.activeElement === document.body) pathInputRef.current?.focus()
    }
    if (pathDraft !== null) return
    if (refocusPick.current) {
      refocusPick.current = false
      refocusEditZone.current = false
      const selected = millerRowRef.current?.querySelector<HTMLButtonElement>('button[aria-current="true"]')
      selected?.focus()
      return
    }
    if (refocusEditZone.current) {
      refocusEditZone.current = false
      if (document.activeElement === document.body) editZoneRef.current?.focus()
    }
  })

  const targetPath = view.selected?.path ?? view.parent?.path ?? null
  const targetName = view.selected?.name
    ?? (view.parent === null ? '' : (displayDirectoryCrumbs(view.parent, t('browser.home')).at(-1)?.name ?? view.parent.path))

  const confirmCreate = () => {
    if (targetPath === null || folderDraft === null || creatingFolder || folderDraft.trim() === '') return
    const name = folderDraft
    setCreatingFolder(true)
    setCreateError(null)
    const generation = openGeneration.current
    createDirectory(targetPath, name).then((createdPath) => {
      if (generation !== openGeneration.current) return
      setCreatingFolder(false)
      setFolderDraft(null)
      const { seq, scan } = launchListing(targetPath)
      setLoading(true)
      setError(null)
      scan.then((level) => {
        if (seq !== requestSeq.current) return
        setView({ parent: level, selected: null, child: null })
        setLoading(false)
        refocusPick.current = true
        select({ name, path: createdPath, hidden: false })
      }, (reason: unknown) => {
        if (seq !== requestSeq.current) return
        setLoading(false)
        setError(failureText(reason))
      })
    }, (reason: unknown) => {
      if (generation !== openGeneration.current) return
      setCreatingFolder(false)
      setCreateError(failureText(reason))
    })
  }

  const confirmOpen = () => {
    if (targetPath === null || loading || opening || pathDraft !== null) return
    setOpening(true)
    setError(null)
    Promise.resolve(onOpen(targetPath)).catch((reason: unknown) => {
      setError(failureText(reason))
    }).finally(() => setOpening(false))
  }

  const confirmNative = () => {
    if (onNativePick === undefined || opening) return
    setOpening(true)
    setError(null)
    Promise.resolve(onNativePick()).catch((reason: unknown) => {
      setError(failureText(reason))
    }).finally(() => setOpening(false))
  }

  if (!open) return null
  const draftPending = pathDraft !== null
  const parentInert = busy || opening || folderDraft !== null
  const dialogTitle = title ?? t('browser.title')
  const dialogDescription = description ?? t('browser.description')

  return (
    <Dialog
      open={open}
      onOpenChange={next => {
        if (!next && folderDraft === null && !busy && !opening) onClose()
      }}
    >
      <DialogContent
        showClose={false}
        className={styles.dialog}
        onEscapeKeyDown={(event) => {
          if (pathDraft !== null || folderDraft !== null || busy || opening) event.preventDefault()
        }}
        onPointerDownOutside={(event) => {
          if (folderDraft !== null || busy || opening) event.preventDefault()
        }}
      >
        <div
          className={styles.editorScope}
          inert={folderDraft !== null ? true : undefined}
          aria-hidden={folderDraft !== null ? true : undefined}
          onKeyDown={(event) => {
            if (event.key !== 'Escape' || pathDraft === null) return
            event.stopPropagation()
            refocusEditZone.current = document.activeElement === pathInputRef.current
            cancelPathEdit()
          }}
          onBlur={(event) => {
            if (pathDraft === null || !document.hasFocus()) return
            const card = event.currentTarget.closest('[role="dialog"]')
            if (card === null) return
            if (event.relatedTarget instanceof Node && card.contains(event.relatedTarget)) return
            refocusEditZone.current = false
            cancelPathEdit()
          }}
        >
          <header className={styles.header}>
            <DialogTitle className={styles.title}>{dialogTitle}</DialogTitle>
            <DialogDescription className={styles.description}>{dialogDescription}</DialogDescription>
            <div className={styles.crumbBar}>
              {pathDraft === null ? (
                <>
                  <span className={styles.crumbTrail} role="navigation" aria-label={t('browser.editPath')} ref={crumbTrailRef}>
                    {crumbs.map((crumb, index) => (
                      <span className={styles.crumbSeat} key={crumb.path}>
                        {index > 0 && <ChevronRight className={styles.crumbChevron} aria-hidden="true" />}
                        <button
                          type="button"
                          className={styles.crumb}
                          disabled={parentInert}
                          onClick={() => navigate(crumb.path)}
                        >
                          {crumb.name}
                        </button>
                      </span>
                    ))}
                  </span>
                  <button
                    type="button"
                    className={styles.crumbEditZone}
                    aria-label={t('browser.editPath')}
                    title={t('browser.editPath')}
                    disabled={parentInert}
                    ref={editZoneRef}
                    onClick={() => {
                      supersede()
                      setLoading(false)
                      previewSuspended.current = false
                      if (view.parent === null) {
                        setPathDraft('')
                        return
                      }
                      const base = view.selected?.path ?? view.parent.path
                      const separator = directorySeparator(view.parent)
                      setPathDraft(base.endsWith(separator) ? base : `${base}${separator}`)
                    }}
                  >
                    <Edit3 aria-hidden="true" />
                  </button>
                </>
              ) : (
                <input
                  className={styles.pathInput}
                  value={pathDraft}
                  aria-label={t('browser.editPath')}
                  placeholder={t('browser.pathPlaceholder')}
                  autoFocus
                  ref={pathInputRef}
                  disabled={parentInert}
                  onChange={(event) => {
                    supersede()
                    setLoading(false)
                    previewSuspended.current = false
                    setPathDraft(event.target.value)
                  }}
                  {...compositionGuard}
                  onKeyDown={(event) => {
                    if (event.key !== 'Enter' || composingRef.current) return
                    event.preventDefault()
                    if (pathDraft.trim() === '') return
                    refocusEditZone.current = true
                    previewSuspended.current = true
                    navigate(pathDraft)
                  }}
                />
              )}
            </div>
          </header>

          <div className={styles.content}>
            <div className={styles.millerRow} ref={millerRowRef}>
              {view.parent !== null && (
                <DirectoryColumn
                  listing={view.parent}
                  selectedPath={view.selected?.path ?? null}
                  showHidden={showHidden}
                  prefix={view.child === null ? typedPrefix : null}
                  disabled={parentInert}
                  pathEditing={draftPending}
                  onPick={select}
                />
              )}
              {view.selected !== null && <span className={styles.divider} aria-hidden="true" />}
              {view.selected !== null && view.child !== null && (
                <DirectoryColumn
                  listing={view.child}
                  selectedPath={null}
                  showHidden={showHidden}
                  prefix={typedPrefix}
                  disabled={parentInert}
                  pathEditing={draftPending}
                  onPick={advance}
                />
              )}
            </div>
            {loading && slowScan && <div className={`${styles.status} ${styles.loadingFloat}`} role="status">{t('browser.loading')}</div>}
            {(view.parent?.truncated === true || view.child?.truncated === true)
              && <div className={styles.status} role="status">{t('browser.truncated')}</div>}
            {error !== null && <div className={styles.error} role="alert">{error}</div>}
          </div>

          <footer className={styles.footerBar}>
            <Button
              type="button"
              variant="outline"
              className={styles.footerControl}
              ref={newFolderButtonRef}
              disabled={view.parent === null || loading || parentInert || draftPending}
              onClick={() => {
                setFolderDraft('')
                setCreateError(null)
              }}
            >
              <Plus aria-hidden="true" />{t('browser.newFolder')}
            </Button>
            <button
              type="button"
              className={`${styles.showHiddenToggle} ${showHidden ? styles.showHiddenToggleActive : ''}`}
              aria-pressed={showHidden}
              disabled={parentInert}
              onMouseDown={draftPending ? event => event.preventDefault() : undefined}
              onClick={() => setShowHidden(value => !value)}
            >
              {t('browser.showHidden')}
              {showHidden && <Check aria-hidden="true" />}
            </button>
            {onNativePick !== undefined && (
              <Button
                type="button"
                variant="ghost"
                className={styles.footerControl}
                disabled={loading || parentInert || draftPending}
                onClick={confirmNative}
              >
                <FolderSearch aria-hidden="true" />{t('browser.native')}
              </Button>
            )}
            <span className={styles.footerGap} />
            <Button type="button" variant="outline" className={styles.footerAction} disabled={parentInert} onClick={onClose}>
              {t('browser.cancel')}
            </Button>
            <Button
              type="button"
              className={styles.footerAction}
              disabled={targetPath === null || loading || parentInert || draftPending}
              onClick={confirmOpen}
            >
              {confirmLabel ?? t('browser.open')}
            </Button>
          </footer>
        </div>

        <Dialog
          open={folderDraft !== null}
          onOpenChange={next => {
            if (!next && !creatingFolder) setFolderDraft(null)
          }}
        >
          <DialogContent
            showClose={false}
            className={styles.createDialog}
            onEscapeKeyDown={event => {
              if (creatingFolder) event.preventDefault()
            }}
            onPointerDownOutside={event => {
              if (creatingFolder) event.preventDefault()
            }}
            onCloseAutoFocus={(event) => {
              event.preventDefault()
              if (!refocusPick.current) newFolderButtonRef.current?.focus()
            }}
          >
            <div className={styles.createBody}>
              <DialogTitle className={styles.createTitle}>{t('browser.newFolder')}</DialogTitle>
              <DialogDescription className={styles.createIn}>{t('browser.createIn', { name: targetName })}</DialogDescription>
              <input
                className={styles.createInput}
                value={folderDraft ?? ''}
                aria-label={t('browser.folderName')}
                placeholder={t('browser.untitledFolder')}
                autoFocus
                disabled={creatingFolder}
                onChange={event => setFolderDraft(event.target.value)}
                {...compositionGuard}
                onKeyDown={(event) => {
                  if (event.key === 'Enter' && !composingRef.current) {
                    event.preventDefault()
                    confirmCreate()
                  }
                  if (event.key === 'Escape') {
                    event.stopPropagation()
                    if (!creatingFolder) setFolderDraft(null)
                  }
                }}
              />
              {createError !== null && <div className={styles.error} role="alert">{createError}</div>}
              <div className={styles.createActions}>
                <Button type="button" variant="outline" disabled={creatingFolder} onClick={() => setFolderDraft(null)}>
                  {t('browser.cancel')}
                </Button>
                <Button
                  type="button"
                  disabled={creatingFolder || folderDraft === null || folderDraft.trim() === ''}
                  onClick={confirmCreate}
                >
                  {t('browser.create')}
                </Button>
              </div>
            </div>
          </DialogContent>
        </Dialog>
      </DialogContent>
    </Dialog>
  )
}

function directoryQuery(path?: string): string {
  return path === undefined ? '' : `?${new URLSearchParams({ path })}`
}

export function LocalDirectoryPicker({
  open,
  onOpenChange,
  apiBase = '/directories',
  initialPath,
  onChoosePath,
  title,
  description,
  confirmLabel,
}: {
  open: boolean
  onOpenChange(open: boolean): void
  apiBase?: string
  initialPath?: string
  onChoosePath?(path: string): Promise<void>
  title?: string
  description?: string
  confirmLabel?: string
}) {
  const { createWorkspace, createSession, notify } = useWorkbench()
  const t = useTranslate('workspace')
  const listDirectory = React.useCallback((path?: string, signal?: AbortSignal) => (
    api.request<DirectoryListing>(`${apiBase}${directoryQuery(path)}`, { signal })
  ), [apiBase])
  const createDirectory = React.useCallback(async (parent: string, name: string) => {
    const created = await api.request<{ path: string }>(apiBase, {
      method: 'POST', body: { parent, name },
    })
    return created.path
  }, [apiBase])
  const choosePath = React.useCallback(async (path: string) => {
    if (onChoosePath !== undefined) {
      await onChoosePath(path)
      onOpenChange(false)
      return
    }
    const workspace = await createWorkspace(path)
    await createSession(workspace.workspace_id)
    onOpenChange(false)
    notify(t('browser.opened', { name: workspace.title }))
  }, [createSession, createWorkspace, notify, onChoosePath, onOpenChange, t])
  const nativeInvoke = window.__TAURI__?.core?.invoke
  const nativePick = nativeInvoke !== undefined && onChoosePath === undefined
    ? async () => {
      const selected = await nativeInvoke<string | null>('pick_workspace')
      if (selected !== null) await choosePath(selected)
    }
    : undefined

  return (
    <DirectoryBrowser
      open={open}
      initialPath={initialPath}
      title={title}
      description={description}
      confirmLabel={confirmLabel}
      listDirectory={listDirectory}
      createDirectory={createDirectory}
      onOpen={choosePath}
      onClose={() => onOpenChange(false)}
      onNativePick={nativePick}
    />
  )
}
