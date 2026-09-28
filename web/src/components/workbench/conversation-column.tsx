import { ComposerTakeover } from './composer-takeover'
import { canAnswerQuestion, ownsResource, ownsExecutionConfiguration, resourcePermissions } from '@/domain/resource-access'
import * as React from 'react'
import { BrandMark } from '@/components/ui/brand-mark'
import { AlertCircle, Bot, Check, ChevronDown, CircleGauge, Ellipsis, FolderOpen, LoaderCircle, Menu, RotateCcw } from 'lucide-react'
import { api } from '@/api/client'
import { ConversationScrollMemory, type ConversationView } from '@/domain/conversation-scroll-memory'
import { followAfterScroll, outerScrollReceives, touchScrollTarget, visibleOutputPosition, type ScrollIntent } from '@/domain/conversation-follow'
import { presentRuntimeError } from '@/domain/runtime-error'
import { sessionTitleGenerationPending } from '@/domain/session-title'
import { approvalDetailsSelection } from '@/domain/approval-details'
import { deriveSubagents } from '@/domain/observability'
import { useWorkspaceDisplay, WorkspaceDisplayContext } from './workspace-display-context'
import { conversationViewRegistry, primaryConversationView } from '@/plugins/conversation-registry'
import { useWorkbench } from '@/state/workbench'
import type { SessionRuntime } from '@/state/use-session-runtime'
import type { Attachment, LocalSession, PermissionPreset, SessionEvent, SubmissionDelivery, SubmissionReference } from '@/types'
import { Button } from '@/components/ui/button'
import {
  Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
} from '@/components/ui/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { downloadJson } from '@/lib/utils'
import { localizeAgentPreset } from '@/i18n/builtin-metadata'
import type { DetailsSelection } from './details-panel'
import { ConversationSession, ConversationSessionHeader } from './conversation-session'
import { HeroGlow, HeroShell, WorkspaceChip } from './empty-hero'
import { clientComposerCommand, feedbackCommandText } from './composer-catalog'
import { InputBar } from './input-bar'
import { ModelPicker, type ModelReadiness } from './model-picker'
import { SessionHeaderActions, SessionToolbar } from './session-toolbar'
import { WorkspaceHeaderActions } from './workspace-panel'
import { JobListAction } from './job-list-action'
import { SessionLineage } from './session-lineage'
import { AgentTeamSurface, AgentTeamTrigger } from './agent-team-panel'
import { SessionRenameDialog } from './session-rename-dialog'
import { SessionModelLimitDialog } from './session-model-limit-dialog'
import { useTranslate } from '@/i18n/provider'
import './builtin-conversation-views'
import css from './conversation-root.module.css'

type Runtime = SessionRuntime

const BOTTOM_ROUNDING_TOLERANCE = 1
const WIDTH_PREF_KEY = 'ternilo.conversation.content-width'
const CONTENT_MIN = 640
const CONTENT_EDGE_BUDGET = 176

function readWidthPreference(): number | null {
  const raw = localStorage.getItem(WIDTH_PREF_KEY)
  if (raw === null) return null
  const value = Number(raw)
  return Number.isFinite(value) && value > 0 ? value : null
}

function resolveContentWidth(columnWidth: number, preference: number | null): number {
  const maximum = Math.max(CONTENT_MIN, columnWidth - CONTENT_EDGE_BUDGET)
  if (preference !== null) return Math.min(Math.max(preference, CONTENT_MIN), maximum)
  return Math.max(680, Math.min(columnWidth * 0.64, 920))
}

function WidthHandle({
  side,
  onStart,
  onDrag,
  onCommit,
  onEnd,
}: {
  side: 'left' | 'right'
  onStart(): number
  onDrag(width: number): void
  onCommit(width: number): void
  onEnd(): void
}) {
  const [dragging, setDragging] = React.useState(false)
  const base = React.useRef(0)
  const origin = React.useRef(0)
  const latest = React.useRef(0)
  const frame = React.useRef<number | null>(null)
  const callbacks = React.useRef({ side, onStart, onDrag, onCommit, onEnd })
  callbacks.current = { side, onStart, onDrag, onCommit, onEnd }

  const outwardWidth = () => {
    const distance = latest.current - origin.current
    const outward = callbacks.current.side === 'right' ? distance : -distance
    return base.current + outward * 2
  }
  const cancelFrame = () => {
    if (frame.current === null) return
    cancelAnimationFrame(frame.current)
    frame.current = null
  }
  const pointerDown = React.useCallback((event: React.PointerEvent<HTMLDivElement>) => {
    event.preventDefault()
    event.currentTarget.setPointerCapture(event.pointerId)
    origin.current = event.clientX
    latest.current = event.clientX
    base.current = callbacks.current.onStart()
    setDragging(true)
  }, [])
  const pointerMove = React.useCallback((event: React.PointerEvent<HTMLDivElement>) => {
    const bounds = event.currentTarget.getBoundingClientRect()
    event.currentTarget.style.setProperty('--ternilo-width-handle-pointer-y', `${event.clientY - bounds.top}px`)
    if (!event.currentTarget.hasPointerCapture(event.pointerId)) return
    latest.current = event.clientX
    frame.current ??= requestAnimationFrame(() => {
      frame.current = null
      callbacks.current.onDrag(outwardWidth())
    })
  }, [])
  const pointerUp = React.useCallback((event: React.PointerEvent<HTMLDivElement>) => {
    if (!event.currentTarget.hasPointerCapture(event.pointerId)) return
    event.currentTarget.releasePointerCapture(event.pointerId)
    cancelFrame()
    latest.current = event.clientX
    if (latest.current !== origin.current) callbacks.current.onCommit(outwardWidth())
    setDragging(false)
    callbacks.current.onEnd()
  }, [])
  const pointerCancel = React.useCallback(() => {
    cancelFrame()
    setDragging(false)
    callbacks.current.onEnd()
  }, [])

  return (
    <div
      className={css.widthHandle}
      data-side={side}
      data-width-handle={side}
      data-dragging={dragging || undefined}
      onPointerDown={pointerDown}
      onPointerMove={pointerMove}
      onPointerUp={pointerUp}
      onPointerCancel={pointerCancel}
      onLostPointerCapture={pointerCancel}
    />
  )
}

export function ConversationColumn({
  runtime,
  selection,
  onSelect,
  onOpenMobileSidebar,
  onChooseWorkspace,
  onOpenModels,
}: {
  runtime: Runtime
  selection: DetailsSelection
  onSelect(selection: DetailsSelection): void
  onOpenMobileSidebar(): void
  onChooseWorkspace(): void
  onOpenModels(): void
}) {
  const {
    snapshot,
    sessionActivity,
    currentSession,
    currentWorkspace,
    platform,
    currentTenantRole,
    accountScope,
    presets,
    refresh,
    selectSession,
    updateSession,
    forkSession,
    forkOperation,
    archiveSession,
    notify,
  } = useWorkbench()
  const canOperate = !platform || (currentTenantRole !== null && currentTenantRole !== 'viewer')
  const permissions = resourcePermissions(currentSession?.access, canOperate)
  const configurationOwner = ownsExecutionConfiguration(currentWorkspace, currentSession, canOperate)
  const t = useTranslate('conversation')
  const builtins = useTranslate('builtins')
  const chatT = useTranslate('chat')
  const modelT = useTranslate('model')
  const workspaceT = useTranslate('workspace')
  const workspaceDisplay = useWorkspaceDisplay()
  const workspacePathLabel = workspaceDisplay.label
  const registeredViews = React.useSyncExternalStore(
    conversationViewRegistry.subscribe,
    conversationViewRegistry.getSnapshot,
    conversationViewRegistry.getSnapshot,
  )
  const primaryView = primaryConversationView(registeredViews)
  const primaryViewId = primaryView?.id ?? 'chat'
  const [view, setView] = React.useState<ConversationView>(() => primaryViewId)
  const activeView = registeredViews.find(item => item.id === view) ?? primaryView
  const activeViewId = activeView?.id ?? view
  const followsActiveTail = Boolean(activeView?.followsTail)
  const tailViewId = followsActiveTail ? activeViewId : primaryViewId
  const tailViewIdRef = React.useRef(tailViewId)
  tailViewIdRef.current = tailViewId
  const [showTailButton, setShowTailButton] = React.useState(false)
  const [renameTarget, setRenameTarget] = React.useState<LocalSession | null>(null)
  const [modelLimitSessionId, setModelLimitSessionId] = React.useState<string | null>(null)
  const [fullAccessOpen, setFullAccessOpen] = React.useState(false)
  const [fullAccessBusy, setFullAccessBusy] = React.useState(false)
  const [modelMenuOpen, setModelMenuOpen] = React.useState(false)
  const [permissionMenuOpen, setPermissionMenuOpen] = React.useState(false)
  React.useEffect(() => { setModelMenuOpen(false); setPermissionMenuOpen(false) }, [currentSession?.identity.session_id])
  const [modelReadiness, setModelReadiness] = React.useState<ModelReadiness>({
    status: 'loading', canSubmit: false, error: '', retry: () => undefined,
  })
  const rootRef = React.useRef<HTMLDivElement>(null)
  const scrollRef = React.useRef<HTMLDivElement>(null)
  const contentRef = React.useRef<HTMLDivElement>(null)
  const followsTail = React.useRef(true)
  const readerScrollIntent = React.useRef<ScrollIntent>(null)
  const touchScroll = React.useRef<{ y: number; target: EventTarget | null } | null>(null)
  const touchPointer = React.useRef<number | null>(null)
  const tailFrame = React.useRef<number | null>(null)
  const tailFrameReflowOnly = React.useRef(false)
  const pendingScrollRestore = React.useRef<{ top: number | 'tail'; width: number; height: number; output: number } | null>(null)
  const restoringScroll = React.useRef(false)
  const observedScrollTop = React.useRef(0)
  const scrollPositions = React.useRef(new ConversationScrollMemory())
  const viewRestoreFrame = React.useRef<number | null>(null)
  const currentSessionId = currentSession?.identity.session_id
  const currentSessionIdRef = React.useRef(currentSessionId)
  currentSessionIdRef.current = currentSessionId
  const visibleOutput = React.useMemo(() => visibleOutputPosition(runtime.events), [runtime.events])
  const visibleInputSeq = React.useMemo(() => runtime.events.reduce(
    (latest, event) => event.type === 'user_message' ? event.seq : latest, 0,
  ), [runtime.events])
  const historyStatsReady = React.useRef(runtime.stats !== null)
  historyStatsReady.current = runtime.stats !== null
  const outputPosition = React.useRef(visibleOutput.seq)
  outputPosition.current = visibleOutput.seq
  const outputReflowActive = React.useRef(false)
  outputReflowActive.current = runtime.busy && visibleOutput.afterBoundary
  React.useEffect(() => setModelLimitSessionId(null), [accountScope, currentSessionId, permissions.configure])
  const forkLocked = forkOperation !== null
  const forkCreatingCurrent = forkOperation?.phase === 'creating'
    && forkOperation.sourceSessionId === currentSessionId
  const forkHydratingCurrent = forkOperation?.phase === 'hydrating'
    && forkOperation.childSessionId === currentSessionId
  const lifecycleOnly = currentSession?.subagent?.transcript_kind === 'process_lifecycle'
  const titleGenerationPending = React.useMemo(
    () => sessionTitleGenerationPending(runtime.events),
    [runtime.events],
  )
  const refreshedChildSessions = React.useRef(new Set<string>())
  const runtimeError = runtime.error ? presentRuntimeError(runtime.error, chatT) : null
  const inspectApproval = React.useCallback((callId: string) => {
    const next = approvalDetailsSelection(runtime.events, callId)
    if (next) onSelect(next)
    else notify(t('approval.detailsUnavailable'), 'error')
  }, [notify, onSelect, runtime.events, t])
  const blankSession = Boolean(currentSession?.blank
    && runtime.nextBeforeSeq == null
    && runtime.events.length === 0
    && runtime.pendingSubmissions.length === 0
    && !runtime.busy
    && !runtime.error
    && !runtime.loading
    && !runtime.historyError)

  React.useEffect(() => {
    const known = new Set(snapshot.sessions.map(session => session.identity.session_id))
    const missing = deriveSubagents(runtime.events)
      .map(subagent => subagent.sessionId)
      .filter((id): id is string => Boolean(id && !known.has(id) && !refreshedChildSessions.current.has(id)))
    if (!missing.length) return
    missing.forEach(id => refreshedChildSessions.current.add(id))
    void refresh().catch(cause => {
      missing.forEach(id => refreshedChildSessions.current.delete(id))
      notify(cause instanceof Error ? cause.message : String(cause), 'error')
    })
  }, [notify, refresh, runtime.events, snapshot.sessions])

  const cloudModel = platform && currentSession?.placement === 'cloud'
  const submissionBlocked = runtime.loading || Boolean(runtime.historyError) || !modelReadiness.canSubmit
  const submissionBlockedMessage = runtime.loading
    ? chatT('chat.loadingHistory')
    : runtime.historyError
      ? chatT('chat.loadError', { message: runtime.historyError })
      : modelReadiness.status === 'error'
        ? modelT(cloudModel ? 'cloud.loadFailed' : 'provider.loadFailed', { message: modelReadiness.error })
        : modelReadiness.status === 'ready'
          ? (cloudModel ? modelReadiness.error || modelT('cloud.unavailable') : modelT('readiness.selectionDescription'))
          : modelT(cloudModel ? 'cloud.description' : 'readiness.description')

  const publishWidth = React.useCallback((root: HTMLDivElement) => {
    const columnWidth = root.offsetWidth
    root.style.setProperty('--ternilo-conversation-column-width', `${columnWidth}px`)
    const preference = readWidthPreference()
    if (preference === null) root.style.removeProperty('--ternilo-chat-user-width')
    else root.style.setProperty('--ternilo-chat-user-width', `${resolveContentWidth(columnWidth, preference)}px`)
  }, [])

  React.useEffect(() => {
    const root = rootRef.current
    if (!root) return
    const observer = new ResizeObserver(() => publishWidth(root))
    observer.observe(root)
    publishWidth(root)
    return () => observer.disconnect()
  }, [publishWidth])

  const handleStart = React.useCallback(() => {
    const root = rootRef.current
    return root ? resolveContentWidth(root.offsetWidth, readWidthPreference()) : 680
  }, [])
  const handleDrag = React.useCallback((width: number) => {
    const root = rootRef.current
    if (!root) return
    root.style.setProperty('--ternilo-chat-user-width', `${resolveContentWidth(root.offsetWidth, width)}px`)
  }, [])
  const handleCommit = React.useCallback((width: number) => {
    const root = rootRef.current
    if (!root) return
    localStorage.setItem(WIDTH_PREF_KEY, `${resolveContentWidth(root.offsetWidth, width)}`)
  }, [])
  const handleEnd = React.useCallback(() => {
    const root = rootRef.current
    if (root) publishWidth(root)
  }, [publishWidth])

  const restorePendingPosition = React.useCallback(() => {
    const saved = pendingScrollRestore.current
    const element = scrollRef.current
    if (!saved || !element) return false
    if (saved.output !== outputPosition.current || saved.width !== element.clientWidth || saved.height !== element.clientHeight) {
      pendingScrollRestore.current = null
      return false
    }
    element.scrollTop = saved.top === 'tail' ? element.scrollHeight : saved.top
    observedScrollTop.current = element.scrollTop
    if (saved.top === 'tail' ? historyStatsReady.current : element.scrollTop >= saved.top - BOTTOM_ROUNDING_TOLERANCE) pendingScrollRestore.current = null
    return true
  }, [])

  // Transcript and statistics arrive separately; finish the same history restoration once.
  React.useLayoutEffect(() => {
    if (runtime.stats !== null) restorePendingPosition()
  }, [restorePendingPosition, runtime.stats !== null])

  const updateTailButton = React.useCallback(() => {
    const element = scrollRef.current
    if (!element || !followsActiveTail || restoringScroll.current) return
    const floor = Math.max(0, element.scrollHeight - element.clientHeight)
    setShowTailButton(floor > 0 && (!followsTail.current
      || (!outputReflowActive.current && floor - element.scrollTop > BOTTOM_ROUNDING_TOLERANCE)))
  }, [followsActiveTail])

  const seatResizeRef = React.useCallback((seat: HTMLDivElement | null) => {
    if (!seat) return
    const publish = () => {
      rootRef.current?.style.setProperty('--ternilo-composer-height', `${seat.offsetHeight}px`)
      restorePendingPosition()
      updateTailButton()
    }
    const observer = new ResizeObserver(publish)
    observer.observe(seat)
    publish()
    return () => observer.disconnect()
  }, [restorePendingPosition, updateTailButton])

  const scrollToTail = React.useCallback((resumeFollowing: boolean, targetViewId = tailViewIdRef.current) => {
    if (!resumeFollowing && restoringScroll.current) return
    if (resumeFollowing) {
      followsTail.current = true
      readerScrollIntent.current = null
      setShowTailButton(false)
    }
    const element = scrollRef.current
    if (!element || (!resumeFollowing && !followsTail.current)) return
    element.scrollTop = element.scrollHeight
    setShowTailButton(false)
    observedScrollTop.current = element.scrollTop
    if (currentSessionId) scrollPositions.current.write(currentSessionId, targetViewId, element.scrollTop, followsTail.current)
  }, [currentSessionId])

  const followTail = React.useCallback((targetViewId = tailViewIdRef.current, reflowOnly = false) => {
    const requireOutput = reflowOnly && (tailFrame.current === null || tailFrameReflowOnly.current)
    tailFrameReflowOnly.current = requireOutput
    if (tailFrame.current !== null) cancelAnimationFrame(tailFrame.current)
    tailFrame.current = requestAnimationFrame(() => {
      tailFrame.current = null
      if (!requireOutput || outputReflowActive.current) scrollToTail(false, targetViewId)
    })
  }, [scrollToTail])

  const forceTail = React.useCallback((targetViewId = tailViewIdRef.current) => {
    pendingScrollRestore.current = null
    if (viewRestoreFrame.current !== null) cancelAnimationFrame(viewRestoreFrame.current)
    viewRestoreFrame.current = null
    restoringScroll.current = false
    followsTail.current = true
    readerScrollIntent.current = null
    setShowTailButton(false)
    followTail(targetViewId)
  }, [followTail])

  React.useLayoutEffect(() => () => {
    if (tailFrame.current !== null) cancelAnimationFrame(tailFrame.current)
    tailFrame.current = null
    readerScrollIntent.current = null
    touchScroll.current = null
    touchPointer.current = null
    pendingScrollRestore.current = null
  }, [currentSessionId])

  React.useLayoutEffect(() => {
    if (viewRestoreFrame.current !== null) {
      cancelAnimationFrame(viewRestoreFrame.current)
      viewRestoreFrame.current = null
    }
    setView(primaryViewId)
    followsTail.current = true
    setShowTailButton(false)
    restoringScroll.current = true
    if (runtime.loading || (currentSessionId && runtime.loadedSessionId !== currentSessionId)) return
    const frame = requestAnimationFrame(() => {
      viewRestoreFrame.current = null
      const element = scrollRef.current
      if (blankSession) {
        if (element) element.scrollTop = 0
        observedScrollTop.current = 0
      } else if (element && currentSessionId) {
        const saved = scrollPositions.current.read(currentSessionId, primaryViewId)
        if (saved === undefined || scrollPositions.current.following(currentSessionId, primaryViewId) === true) {
          scrollToTail(true)
          if (!historyStatsReady.current) pendingScrollRestore.current = {
            top: 'tail', width: element.clientWidth, height: element.clientHeight, output: outputPosition.current,
          }
        } else {
          element.scrollTop = saved
          pendingScrollRestore.current = element.scrollTop < saved - BOTTOM_ROUNDING_TOLERANCE
            ? { top: saved, width: element.clientWidth, height: element.clientHeight, output: outputPosition.current } : null
          observedScrollTop.current = element.scrollTop
          const atTail = element.scrollHeight - element.scrollTop - element.clientHeight <= BOTTOM_ROUNDING_TOLERANCE
          followsTail.current = scrollPositions.current.following(currentSessionId, primaryViewId) ?? atTail
          setShowTailButton(!followsTail.current)
        }
      } else {
        scrollToTail(true)
      }
      restoringScroll.current = false
    })
    viewRestoreFrame.current = frame
    return () => {
      cancelAnimationFrame(frame)
      if (viewRestoreFrame.current === frame) viewRestoreFrame.current = null
    }
  }, [blankSession, currentSessionId, primaryViewId, runtime.loadedSessionId, runtime.loading, scrollToTail])

  React.useLayoutEffect(() => {
    if ((visibleOutput.seq || visibleInputSeq || runtime.pendingSubmissions.length) && followsActiveTail && followsTail.current && !restoringScroll.current
      && runtime.loadedSessionId === currentSessionId) followTail()
  }, [currentSessionId, followTail, followsActiveTail, runtime.loadedSessionId, runtime.pendingSubmissions, visibleInputSeq, visibleOutput.seq])

  React.useEffect(() => {
    const content = contentRef.current
    if (!content) return
    const observer = new ResizeObserver(() => {
      if (restorePendingPosition()) return
      updateTailButton()
      if (followsActiveTail && followsTail.current && !restoringScroll.current
        && outputReflowActive.current && runtime.loadedSessionId === currentSessionId) followTail(undefined, true)
    })
    observer.observe(content)
    if (scrollRef.current) observer.observe(scrollRef.current)
    return () => observer.disconnect()
  }, [currentSessionId, followTail, followsActiveTail, restorePendingPosition, runtime.loadedSessionId, updateTailButton])

  const onScroll = () => {
    const element = scrollRef.current
    if (!element) return
    if (restoringScroll.current) {
      observedScrollTop.current = element.scrollTop
      return
    }
    const floor = Math.max(0, element.scrollHeight - element.clientHeight)
    const atTail = floor - element.scrollTop <= BOTTOM_ROUNDING_TOLERANCE
    if (followsActiveTail) {
      followsTail.current = followAfterScroll(followsTail.current, readerScrollIntent.current, observedScrollTop.current, element.scrollTop, atTail)
      setShowTailButton(floor > 0 && (!followsTail.current || (!outputReflowActive.current && !atTail)))
      if (followsTail.current && !atTail && outputReflowActive.current) followTail(undefined, true)
    } else {
      readerScrollIntent.current = null
      setShowTailButton(false)
    }
    observedScrollTop.current = element.scrollTop
    if (currentSessionId) scrollPositions.current.write(currentSessionId, view, element.scrollTop, followsTail.current)
  }

  const markReaderNavigation = React.useCallback(() => {
    if (viewRestoreFrame.current !== null) cancelAnimationFrame(viewRestoreFrame.current)
    viewRestoreFrame.current = null
    restoringScroll.current = false
    followsTail.current = false
    readerScrollIntent.current = null
    pendingScrollRestore.current = null
    setShowTailButton(followsActiveTail)
    const element = scrollRef.current
    if (element && currentSessionId) scrollPositions.current.write(currentSessionId, view, element.scrollTop, false)
  }, [currentSessionId, followsActiveTail, view])

  const markReaderScrollIntent = (delta: number, target: EventTarget | null) => {
    const element = scrollRef.current
    if (!followsActiveTail || !element || !delta || !outerScrollReceives(target, element, delta)) return
    pendingScrollRestore.current = null
    observedScrollTop.current = element.scrollTop
    // Pause before the browser moves: a queued rendering frame must not win this gesture.
    if (delta < 0) markReaderNavigation()
    readerScrollIntent.current = delta < 0 ? -1 : 1
    if (delta > 0 && element.scrollHeight - element.clientHeight - element.scrollTop <= BOTTOM_ROUNDING_TOLERANCE) {
      followsTail.current = true
      setShowTailButton(false)
      followTail()
    }
  }
  const markScrollbarDrag = (event: React.PointerEvent<HTMLDivElement>) => {
    const element = scrollRef.current
    if (!followsActiveTail || !element || event.target !== element) return
    const bounds = element.getBoundingClientRect()
    if (event.clientX >= bounds.right - (element.offsetWidth - element.clientWidth)) {
      observedScrollTop.current = element.scrollTop
      markReaderNavigation()
      readerScrollIntent.current = 0
    }
  }
  const markKeyboardScroll = (event: React.KeyboardEvent<HTMLDivElement>) => {
    if (event.defaultPrevented || event.ctrlKey || event.metaKey || event.altKey) return
    const target = event.target instanceof Element ? event.target : null
    if (target?.closest('input, textarea, select, [contenteditable="true"], [role="combobox"], [role="slider"], [role="listbox"]')) return
    if (event.key === ' ' && target?.closest('button, summary, [role="button"]')) return
    const delta = ['ArrowUp', 'PageUp', 'Home'].includes(event.key) || (event.key === ' ' && event.shiftKey) ? -1
      : ['ArrowDown', 'PageDown', 'End', ' '].includes(event.key) ? 1 : 0
    markReaderScrollIntent(delta, event.target)
  }

  const restoreViewScroll = React.useCallback((next: ConversationView) => {
    if (viewRestoreFrame.current !== null) cancelAnimationFrame(viewRestoreFrame.current)
    viewRestoreFrame.current = requestAnimationFrame(() => {
      viewRestoreFrame.current = null
      const scroller = scrollRef.current
      if (!scroller) {
        restoringScroll.current = false
        return
      }
      const target = conversationViewRegistry.getSnapshot().find(item => item.id === next)
      const saved = currentSessionId ? scrollPositions.current.read(currentSessionId, next) : undefined
      const savedFollowing = currentSessionId ? scrollPositions.current.following(currentSessionId, next) : undefined
      const restoreTail = target?.followsTail && (savedFollowing === true || saved === undefined)
      scroller.scrollTop = restoreTail ? scroller.scrollHeight : saved ?? 0
      const restoreTop = restoreTail && !historyStatsReady.current ? 'tail'
        : !restoreTail && saved !== undefined && scroller.scrollTop < saved - BOTTOM_ROUNDING_TOLERANCE ? saved : undefined
      pendingScrollRestore.current = restoreTop === undefined ? null
        : { top: restoreTop, width: scroller.clientWidth, height: scroller.clientHeight, output: outputPosition.current }
      observedScrollTop.current = scroller.scrollTop
      if (target?.followsTail) {
        const atTail = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight <= BOTTOM_ROUNDING_TOLERANCE
        followsTail.current = currentSessionId ? scrollPositions.current.following(currentSessionId, next) ?? atTail : atTail
        setShowTailButton(!followsTail.current)
      } else {
        followsTail.current = false
        setShowTailButton(false)
      }
      restoringScroll.current = false
    })
  }, [currentSessionId])

  const transitionView = React.useCallback((source: ConversationView, next: ConversationView, rememberSource = true) => {
    const element = scrollRef.current
    if (tailFrame.current !== null) cancelAnimationFrame(tailFrame.current)
    tailFrame.current = null
    readerScrollIntent.current = null
    pendingScrollRestore.current = null
    if (rememberSource && element && currentSessionId) {
      scrollPositions.current.write(currentSessionId, source, element.scrollTop, followsTail.current)
    }
    setShowTailButton(false)
    followsTail.current = false
    restoringScroll.current = true
    setView(next)
    restoreViewScroll(next)
  }, [currentSessionId, restoreViewScroll])

  const switchView = (next: ConversationView) => {
    if (next === activeViewId) return
    transitionView(view, next)
  }

  React.useLayoutEffect(() => {
    if (registeredViews.some(item => item.id === view)) return
    transitionView(view, primaryViewId, false)
  }, [primaryViewId, registeredViews, transitionView, view])

  const activateChatTail = () => {
    if (activeViewId === primaryViewId) {
      forceTail(primaryViewId)
      return
    }
    if (currentSessionId) scrollPositions.current.write(currentSessionId, primaryViewId, Number.MAX_SAFE_INTEGER, true)
    setView(primaryViewId)
    followsTail.current = true
    forceTail(primaryViewId)
  }

  const confirmRename = async (session: LocalSession, title: string) => {
    await updateSession(session.identity.session_id, { title })
    notify(t('session.renameSuccess'))
  }
  const forkCurrent = async () => {
    if (!currentSession || forkLocked) return
    try {
      await forkSession(currentSession.identity.session_id)
      notify(t('session.forkSuccess'))
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : String(cause), 'error')
    }
  }
  const archiveCurrent = async () => {
    if (!currentSession) return
    try {
      await archiveSession(currentSession.identity.session_id)
      notify(t('session.archiveSuccess'))
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : String(cause), 'error')
    }
  }
  const exportCurrent = async () => {
    if (!currentSession) return
    try {
      const exported = await api.request(`/sessions/${encodeURIComponent(currentSession.identity.session_id)}/export`)
      downloadJson(`${currentSession.title || 'ternilo-session'}.json`, exported)
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : String(cause), 'error')
    }
  }
  const applyPermissions = async (permissions: PermissionPreset) => {
    if (!currentSession) return false
    try {
      await updateSession(currentSession.identity.session_id, { permissions })
      notify(t('permission.updated'))
      return true
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : String(cause), 'error')
      return false
    }
  }
  const setPermissions = async (permissions: PermissionPreset) => {
    if (permissions !== 'full_access') {
      await applyPermissions(permissions)
      return
    }
    setFullAccessOpen(true)
  }
  const confirmFullAccess = async () => {
    setFullAccessBusy(true)
    try {
      if (await applyPermissions('full_access')) setFullAccessOpen(false)
    } finally {
      setFullAccessBusy(false)
    }
  }
  const presetLocked = !currentSession?.blank || runtime.busy || Boolean(runtime.inbox?.items.length) || runtime.pendingSubmissions.length > 0
  const setPreset = async (agentPreset: string) => {
    if (!currentSession) return
    if (!permissions.configure || presetLocked) { notify(t('session.presetLocked'), 'error'); return }
    try {
      await updateSession(currentSession.identity.session_id, { agent_preset: agentPreset })
      notify(t('session.presetUpdated'))
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : String(cause), 'error')
    }
  }
  const togglePlan = async () => {
    if (!currentSession) return
    if (runtime.busy) {
      notify(t('session.planBusy'), 'error')
      return
    }
    const mode = currentSession.mode === 'plan' ? 'execute' : 'plan'
    try {
      await updateSession(currentSession.identity.session_id, { mode })
      notify(mode === 'plan' ? t('session.planEntered') : t('session.executionEntered'))
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : String(cause), 'error')
    }
  }
  const submit = async (
    input: string,
    attachments: Attachment[],
    delivery: SubmissionDelivery,
    references: SubmissionReference[],
  ) => {
    if (!currentSession) return
    const command = clientComposerCommand(input, t)
    const feedbackText = feedbackCommandText(input)
    if (feedbackText !== null) {
      if (attachments.length || references.length) throw new Error(t('command.feedbackAttachments'))
      activateChatTail()
      await runtime.submitFeedback(feedbackText)
      return
    }
    if (command?.execution === 'mode') {
      if (input !== '/plan' && input !== '/plan off') throw new Error(t('session.planUsage'))
      if (runtime.busy) throw new Error(t('session.planBusy'))
      if (attachments.length || references.length) throw new Error(t('session.planAttachment'))
      const mode = input === '/plan off' ? 'execute' : 'plan'
      await updateSession(currentSession.identity.session_id, { mode })
      notify(mode === 'plan' ? t('session.planEntered') : t('session.executionEntered'))
      return
    }
    if (command?.execution === 'skill' && input.trim() === '/skill') {
      throw new Error(t('command.skillRequired'))
    }
    activateChatTail()
    await runtime.submit(input, attachments, command?.execution === 'direct' ? 'queue' : delivery, references)
    if (currentSessionIdRef.current === currentSessionId && tailViewIdRef.current === primaryViewId && followsTail.current) followTail(primaryViewId)
  }

  const regenerate = async (event: SessionEvent, editedInput?: string) => {
    if (!permissions.submit || lifecycleOnly || runtime.busy || runtime.inbox?.items.length || submissionBlocked || forkCreatingCurrent) return
    const skill = event.source?.kind === 'skill_invocation'
      || (event.source?.kind === 'submission' && event.source.skill_name)
    const input = editedInput ?? (skill ? event.display_content ?? event.content ?? '' : event.content ?? '')
    activateChatTail()
    await runtime.submit(input, event.attachments ?? [], 'queue', event.references ?? [], event.seq)
  }

  const preset = currentSession
    ? presets.presets.find(item => item.id === currentSession.agent_preset)
    : undefined
  const displayPreset = preset ? localizeAgentPreset(preset, builtins) : undefined
  const displayPresets = presets.presets.map(item => localizeAgentPreset(item, builtins))
  const heroControls = canOperate && currentSession ? (
    <div className={css.heroControls}>
      <WorkspaceChip
        label={currentWorkspace?.title ?? workspacePathLabel}
        onClick={onChooseWorkspace}
        t={t}
      />
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button type="button" disabled={!permissions.configure || presetLocked} title={presetLocked ? t('session.presetLocked') : undefined} className={css.heroPreset} variant="ghost" size="sm" aria-label={t('hero.agentAria', { name: displayPreset?.display_name ?? currentSession.agent_preset })}>
            <Bot />
            <span className="truncate">{displayPreset?.display_name ?? currentSession.agent_preset}</span>
            <ChevronDown />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="w-72">
          {displayPresets.map(item => (
            <DropdownMenuItem key={item.id} disabled={!permissions.configure || presetLocked} onSelect={() => void setPreset(item.id)}>
              <div className="min-w-0 flex-1">
                <div>{item.display_name}</div>
                <div className="line-clamp-2 text-xs text-muted-foreground">{item.description}</div>
              </div>
              {item.id === currentSession.agent_preset && <Check />}
            </DropdownMenuItem>
          ))}
        </DropdownMenuContent>
      </DropdownMenu>
      <AgentTeamTrigger className={css.heroTeam} label />
      {blankSession && <WorkspaceHeaderActions />}
      {currentSession.placement === 'cloud' && permissions.configure && <DropdownMenu>
        <DropdownMenuTrigger asChild><Button type="button" variant="ghost" size="icon-sm" className="shrink-0 min-h-10 min-w-10" aria-label={t('session.moreActions')}><Ellipsis /></Button></DropdownMenuTrigger>
        <DropdownMenuContent align="end"><DropdownMenuItem onSelect={() => setModelLimitSessionId(currentSession.identity.session_id)}><CircleGauge />{t('session.modelLimit')}</DropdownMenuItem></DropdownMenuContent>
      </DropdownMenu>}
    </div>
  ) : null

  const phase = blankSession ? 'hero' : 'active'
  const sessionState = !currentSession
    ? 'empty'
    : runtime.loading
      ? 'loading'
      : runtime.historyError
        ? 'error'
        : blankSession
          ? 'empty'
          : 'ready'
  const followupSubagent = async (subagentId: string, message: string) => {
    if (!currentSessionId) return
    await api.request(
      `/sessions/${encodeURIComponent(currentSessionId)}/subagents/${encodeURIComponent(subagentId)}/followup`,
      { method: 'POST', body: { message } },
    )
  }
  const stopSubagent = async (subagentId: string) => {
    if (!currentSessionId) return
    await api.request(
      `/sessions/${encodeURIComponent(currentSessionId)}/subagents/${encodeURIComponent(subagentId)}/interrupt`,
      { method: 'POST' },
    )
  }
  return (
    <WorkspaceDisplayContext.Provider value={workspaceDisplay.replacement}>
    <AgentTeamSurface
      sessionId={currentSession?.identity.session_id ?? null}
      events={runtime.events}
      sessions={snapshot.sessions}
      liveSnapshot={runtime.agentTeam}
      liveActivity={sessionActivity}
      readOnly={!permissions.submit}
      canStop={permissions.stop}
      onOpenSession={selectSession}
      onFollowup={followupSubagent}
      onStop={stopSubagent}
    >
    <main ref={rootRef} id="conversation-main" className={`${css.root} conversation-column`} data-phase={phase} data-session-state={sessionState}>
      {currentSession && (
        <ConversationSessionHeader
          session={currentSession}
          workspace={currentWorkspace}
          blank={blankSession && permissions.submit}
          view={activeViewId}
          views={registeredViews}
          viewTabsAvailable={!runtime.loading && !runtime.historyError && !blankSession}
          workspacePathLabel={workspacePathLabel}
          workspaceLocation={workspaceDisplay.path || workspaceDisplay.hint}
          projectName={workspaceDisplay.project}
          onView={switchView}
          onOpenMobileSidebar={onOpenMobileSidebar}
          actions={(
            <>
              <SessionLineage events={runtime.events} session={currentSession} sessions={snapshot.sessions} onOpenSession={selectSession} />
              <JobListAction events={runtime.events} />
              <SessionHeaderActions
                session={currentSession}
                presets={presets}
                onSetPreset={setPreset}
                onTogglePlan={togglePlan}
                readOnly={!permissions.configure && !permissions.submit}
                planLocked={runtime.busy}
                presetLocked={presetLocked}
                forkLocked={forkLocked}
                onChooseWorkspace={onChooseWorkspace}
                onRename={() => setRenameTarget(currentSession)}
                onFork={forkCurrent}
                onArchive={archiveCurrent}
                onExport={exportCurrent}
                onEditModelLimit={() => setModelLimitSessionId(currentSession.identity.session_id)}
              />
            </>
          )}
          t={t}
          workspaceSessionTitle={workspaceT('session.new')}
          displayTitle={!currentSession.blank && currentSession.title === 'New session'
            && titleGenerationPending
            ? workspaceT('session.generatingTitle')
            : undefined}
        />
      )}
      {(!currentSession || blankSession) && (
        <Button
          type="button"
          className={css.heroMobileSidebar}
          variant="ghost"
          size="icon-sm"
          onClick={onOpenMobileSidebar}
          aria-label={t('mobile.openSidebar')}
        >
          <Menu />
        </Button>
      )}
      <div
        ref={scrollRef}
        className={`${css.scrollBody} conversation-scroll`}
        data-conversation-scroll=""
        tabIndex={-1}
        onScroll={onScroll}
        onScrollEnd={event => { if (event.target === event.currentTarget) readerScrollIntent.current = null }}
        onWheelCapture={event => {
          if (!event.ctrlKey && !event.shiftKey) markReaderScrollIntent(event.deltaY, event.target)
        }}
        onTouchStart={event => {
          if (event.touches.length !== 1) {
            if (touchPointer.current !== null && event.currentTarget.hasPointerCapture(touchPointer.current)) event.currentTarget.releasePointerCapture(touchPointer.current)
            touchPointer.current = null
            touchScroll.current = null
            return
          }
          if (touchPointer.current === null) touchScroll.current = event.touches.length === 1 ? { y: event.touches[0].clientY, target: touchScrollTarget(event.target, event.currentTarget) } : null
        }}
        onTouchMove={event => {
          const previous = touchScroll.current
          if (event.touches.length !== 1) { touchScroll.current = null; return }
          if (!previous) return
          const y = event.touches[0].clientY
          markReaderScrollIntent(previous.y - y, previous.target)
          touchScroll.current = { y, target: previous.target }
        }}
        onTouchEnd={() => { touchScroll.current = null }}
        onTouchCancel={() => { touchScroll.current = null; readerScrollIntent.current = null }}
        onPointerDownCapture={event => {
          pendingScrollRestore.current = null
          markScrollbarDrag(event)
          if (event.pointerType !== 'touch' || !event.isPrimary) return
          const target = event.target instanceof Element ? event.target : null
          if (target?.closest('button, a, input, textarea, select, summary, [contenteditable="true"], [role="button"]')) return
          touchPointer.current = event.pointerId
          touchScroll.current = { y: event.clientY, target: touchScrollTarget(event.target, event.currentTarget) }
          // Streaming Markdown may replace the original touch target before its next move.
          event.currentTarget.setPointerCapture(event.pointerId)
        }}
        onPointerMoveCapture={event => {
          const previous = touchScroll.current
          if (touchPointer.current !== event.pointerId || !previous) return
          markReaderScrollIntent(previous.y - event.clientY, previous.target)
          touchScroll.current = { ...previous, y: event.clientY }
        }}
        onPointerUpCapture={event => { if (touchPointer.current === event.pointerId) { touchPointer.current = null; touchScroll.current = null } }}
        onPointerCancelCapture={event => { if (touchPointer.current === event.pointerId) touchPointer.current = null }}
        onKeyDownCapture={markKeyboardScroll}
      >
        {!currentSession ? (
          <div className={css.emptySelection} data-empty-workspace-selection="">
            <div>
              <div className={css.emptySelectionMark}><BrandMark /></div>
              <h1>{t('empty.chooseTitle')}</h1>
              <p>{t('empty.chooseDescription')}</p>
              {canOperate
                ? <Button onClick={onChooseWorkspace}><FolderOpen />{t('empty.chooseAction')}</Button>
                : <p data-viewer-empty="">{t('viewer.empty')}</p>}
            </div>
          </div>
        ) : forkCreatingCurrent ? (
          <div className={css.historyState} data-history-state="forking" role="status" aria-live="polite">
            <LoaderCircle className={css.spin} aria-hidden="true" />
            <strong>{t('session.forking')}</strong>
            <p>{t('session.forkingDescription')}</p>
          </div>
        ) : runtime.loading || runtime.loadedSessionId !== currentSessionId ? (
          <div className={css.historyState} data-history-state="loading" role="status" aria-live="polite">
            <LoaderCircle className={css.spin} aria-hidden="true" />
            <strong>{chatT(forkHydratingCurrent ? 'chat.loadingForkHistory' : 'chat.loadingHistory')}</strong>
            <p>{chatT(forkHydratingCurrent
              ? 'chat.loadingForkHistoryDescription'
              : 'chat.loadingHistoryDescription')}</p>
          </div>
        ) : runtime.historyError ? (
          <div className={css.historyState} data-history-state="error" role="alert">
            <AlertCircle aria-hidden="true" />
            <strong>{chatT('chat.loadError', { message: runtime.historyError })}</strong>
            <Button type="button" variant="outline" size="sm" onClick={runtime.retryHistory}>
              <RotateCcw />{chatT('chat.retryHistory')}
            </Button>
          </div>
        ) : !blankSession ? (
          <div ref={contentRef} className={css.sessionFlow}>
            <ConversationSession
              view={activeViewId}
              views={registeredViews.map(item => item.id)}
              contentClassName={activeView?.contentClassName}
              content={activeView?.render({
                sessionId: currentSession.identity.session_id,
                events: runtime.events,
                pendingSubmissions: runtime.pendingSubmissions,
                projection: runtime.projection,
                reloadMetadata: runtime.reloadMetadata,
                history: { hasOlder: runtime.nextBeforeSeq != null, loading: runtime.loadingOlder, error: runtime.olderHistoryError, loadOlder: runtime.loadOlderHistory },
                selection,
                onSelect,
                onReaderNavigate: markReaderNavigation,
                onRegenerate: permissions.submit && !lifecycleOnly ? regenerate : undefined,
                onEdit: permissions.submit && !lifecycleOnly ? regenerate : undefined,
                regenerateDisabled: runtime.busy || Boolean(runtime.inbox?.items.length) || submissionBlocked || forkCreatingCurrent,
              })}
            />
            {runtime.metadataWarning && (
              <div role="alert" className={css.warning}>
                <strong>{t('warning.metadataTitle')}</strong>
                <p>{runtime.metadataWarning}</p>
              </div>
            )}
            {runtimeError && (
              <div role="alert" className={css.error}>
                <strong>{runtimeError.title}</strong>
                <p>{runtimeError.message}</p>
                {runtimeError.raw !== runtimeError.message && (
                  <details>
                    <summary>{t('error.technicalDetails')}</summary>
                    <code>{runtimeError.raw}</code>
                  </details>
                )}
              </div>
            )}
          </div>
        ) : null}
        {currentSession && (
          <div ref={seatResizeRef} className={`${css.composerSeat} composer-seat`} data-composer-seat="">
            <div className={`${css.composerStack} ${blankSession ? css.composerHero : ''}`}>
              {blankSession && <HeroGlow className={css.heroGlow} />}
              {blankSession && <HeroShell t={t} />}
              {!permissions.submit ? (
                <div className={css.lifecycleNotice} data-viewer-read-only="" role="note">
                  <Bot aria-hidden="true" />
                  <div>
                    <strong>{currentSession.access ? workspaceT('sharing.noSubmit') : t('viewer.title')}</strong>
                    {!currentSession.access && <p>{t('viewer.description')}</p>}
                    {permissions.stop && runtime.busy && <Button size="sm" variant="outline" onClick={() => void runtime.cancel()}>{t('input.stop')}</Button>}
                    {permissions.configure && <div className="mt-2 flex gap-2"><SessionToolbar session={currentSession} onSetPermissions={setPermissions} /><ModelPicker compact effectiveProfile={runtime.effectiveProfile} rememberDefault={configurationOwner} onConfigureModels={configurationOwner ? onOpenModels : undefined} /></div>}
                  </div>
                </div>
              ) : lifecycleOnly ? (
                <div className={css.lifecycleNotice} data-subagent-lifecycle-only="" role="note">
                  <Bot aria-hidden="true" />
                  <div>
                    <strong>{t('subagent.lifecycleTitle')}</strong>
                    <p>{t('subagent.lifecycleDescription')}</p>
                  </div>
                </div>
              ) : !runtime.loading && !forkCreatingCurrent && (modelReadiness.status !== 'ready' || !modelReadiness.canSubmit) ? (
                <div className={css.modelNotice} data-model-onboarding="" data-state={modelReadiness.status} role={modelReadiness.status === 'error' ? 'alert' : 'status'}>
                  <div>
                    <strong>{modelReadiness.status === 'empty'
                      ? modelT('readiness.title')
                      : modelReadiness.status === 'ready'
                        ? modelT('readiness.selectionTitle')
                        : modelReadiness.status === 'error'
                          ? modelT(cloudModel ? 'cloud.loadFailed' : 'provider.loadFailed', { message: modelReadiness.error })
                          : modelT(cloudModel ? 'cloud.loading' : 'provider.loading')}</strong>
                    <p>{modelReadiness.status === 'loading'
                      ? modelT(cloudModel ? 'cloud.loadingDescription' : 'provider.loadingDescription')
                      : modelReadiness.status === 'ready'
                        ? (cloudModel ? modelReadiness.error || modelT('cloud.unavailable') : modelT('readiness.selectionDescription'))
                        : modelT(cloudModel ? 'cloud.description' : 'readiness.description')}</p>
                  </div>
                  {!permissions.configure && <p>{workspaceT('sharing.noConfigure')}</p>}
                  <div className={css.modelNoticeActions}>
                    {modelReadiness.status === 'error' && <Button type="button" size="sm" variant="outline" onClick={modelReadiness.retry}><RotateCcw />{modelT('provider.retry')}</Button>}
                    {modelReadiness.status !== 'loading' && configurationOwner && <Button type="button" size="sm" onClick={onOpenModels}>{modelT('provider.configure')}</Button>}
                  </div>
                </div>
              ) : null}
              {!permissions.submit && runtime.questions.length > 0 && <div className={css.readOnlyQuestions}><ComposerTakeover
                questions={runtime.questions}
                canAnswer={item => canAnswerQuestion(item.question, permissions, ownsResource(currentSession.access, canOperate))}
                onAnswer={runtime.answerQuestion}
                onError={message => notify(message, 'error')}
                onInspectApproval={inspectApproval}
                t={t}
              /></div>}
              {permissions.submit && !lifecycleOnly && <InputBar
                key={JSON.stringify([accountScope, currentSession.identity.session_id])}
                accountScope={accountScope}
                sessionId={currentSession.identity.session_id}
                commandCatalogRevision={`${currentSession.agent_preset}:${currentSession.mode}:${currentSession.updated_at_ms}`}
                connectionStatus={runtime.liveStatus}
                variant={blankSession ? 'hero' : 'composer'}
                busy={runtime.busy}
                execution={sessionActivity[currentSession.identity.session_id]?.execution}
                questions={runtime.questions}
                projection={runtime.projection}
                events={runtime.events}
                model={currentSession.model}
                providerTarget={{ sessionId: currentSession.identity.session_id, workspaceId: currentSession.workspace_id, placement: currentSession.placement }}
                stats={runtime.stats}
                inbox={runtime.inbox}
                sessionControls={<SessionToolbar session={currentSession} onSetPermissions={setPermissions} open={permissionMenuOpen} onOpenChange={setPermissionMenuOpen} />}
                modelControl={<ModelPicker compact effectiveProfile={runtime.effectiveProfile} disabled={!permissions.configure} rememberDefault={configurationOwner} onConfigureModels={configurationOwner ? onOpenModels : undefined} onReadinessChange={setModelReadiness} open={modelMenuOpen} onOpenChange={setModelMenuOpen} />}
                sessionMode={currentSession.mode}
                sessionActions={{
                  export: exportCurrent,
                  ...(permissions.configure ? {
                    model: () => setModelMenuOpen(true),
                    permission: () => setPermissionMenuOpen(true),
                    ...(!runtime.busy ? { plan: togglePlan } : {}),
                  } : {}),
                }}
                heroControls={blankSession ? heroControls : undefined}
                onSubmit={submit}
                onCancel={runtime.cancel}
                canStop={permissions.stop}
                canAnswerQuestion={item => canAnswerQuestion(item.question, permissions, ownsResource(currentSession.access, canOperate))}
                onAnswerQuestion={runtime.answerQuestion}
                onInspectApproval={inspectApproval}
                onEditQueueItem={runtime.editQueueItem}
                onLoadQueueItem={runtime.loadQueueItem}
                onRemoveQueueItem={runtime.removeQueueItem}
                onSteerQueueItem={runtime.steerQueueItem}
                onForceTail={activateChatTail}
                onError={message => notify(message, 'error')}
                submissionBlocked={submissionBlocked}
                submissionBlockedMessage={submissionBlockedMessage}
                t={t}
              />}
            </div>
          </div>
        )}
      </div>
      {showTailButton && (
        <button type="button" className={css.tailButton} onClick={() => forceTail()}>{t('view.backToBottom')}</button>
      )}
      {phase === 'active' && activeViewId === primaryViewId && (['left', 'right'] as const).map(side => (
        <WidthHandle
          key={side}
          side={side}
          onStart={handleStart}
          onDrag={handleDrag}
          onCommit={handleCommit}
          onEnd={handleEnd}
        />
      ))}
      <SessionRenameDialog
        session={renameTarget}
        onOpenChange={open => { if (!open) setRenameTarget(null) }}
        onRename={confirmRename}
      />
      {currentSession?.placement === 'cloud' && permissions.configure && modelLimitSessionId === currentSessionId && <SessionModelLimitDialog
        key={currentSessionId}
        limit={currentSession.model_token_limit}
        onClose={() => setModelLimitSessionId(null)}
        onSave={async model_token_limit => {
          await updateSession(currentSession.identity.session_id, { model_token_limit })
          notify(t('session.modelLimitSaved'))
        }}
      />}
      <Dialog open={permissions.configure && fullAccessOpen} onOpenChange={open => { if (!fullAccessBusy) setFullAccessOpen(open) }}>
        <DialogContent className="max-w-md" showClose={!fullAccessBusy}>
          <DialogHeader>
            <DialogTitle>{t('permission.fullAccessTitle')}</DialogTitle>
            <DialogDescription>{t('permission.fullAccessConfirm')}</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button type="button" variant="outline" disabled={fullAccessBusy} onClick={() => setFullAccessOpen(false)}>{t('permission.fullAccessCancel')}</Button>
            <Button type="button" disabled={fullAccessBusy} onClick={() => void confirmFullAccess()}>{fullAccessBusy ? t('permission.fullAccessApplying') : t('permission.fullAccessApply')}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </main>
    </AgentTeamSurface>
    </WorkspaceDisplayContext.Provider>
  )
}
