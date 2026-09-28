import * as React from 'react'
import { ArrowUp, AtSign, FilePlus2, LoaderCircle, Paperclip, Plus, Square, X } from 'lucide-react'
import type {
  Attachment,
  ModelSelection,
  PendingQuestion,
  SessionEvent,
  SessionInboxSnapshot,
  SessionProjection,
  SessionStats,
  SessionSubmission,
  SubmissionDelivery,
  UserQuestionAnswer,
} from '@/types'
import type { Translate } from '@/i18n/runtime'
import { useTranslate } from '@/i18n/provider'
import {
  committedComposerDraft,
  readComposerDraft,
  restoredFailedDrafts,
  writeComposerDraft,
} from '@/domain/composer-draft'
import { cn } from '@/lib/utils'
import { resolveComposerDelivery, useBusyEnterBehavior } from '@/domain/composer-preference'
import { attachmentFromFile } from '@/domain/attachment-ingest'
import {
  ComposerEditHistory,
  composerTriggerAt,
  replaceComposerRange,
  textareaSelection,
  type ComposerEditSnapshot,
  type ComposerSelection,
  type ComposerTrigger,
} from '@/domain/composer-edit'
import { imageDataSource, ImageLightbox } from './message-attachments'
import { visibleInboxItems } from '@/domain/submission-echo'
import { currentTurnActivity } from '@/domain/turn-activity'
import {
  composerCommand,
  composerCommandInput,
  composerCommandLabel,
  composerMenuCommands,
  modelIndependentComposerCommand,
  type ComposerCommand,
  type ComposerReference,
  type ComposerSessionAction,
  useCommandCatalog,
  useReferenceCandidates,
  useSkillCatalog,
} from './composer-catalog'
import { ComposerContextMeter } from './composer-context-meter'
import { ComposerDropOverlay } from './composer-drop-overlay'
import { ComposerMenu, type ComposerMenuRow } from './composer-menu'
import { ComposerProjectionDock } from './composer-projection-dock'
import { ComposerTakeover } from './composer-takeover'
import { QueueDock } from './queue-dock'
import { SessionStatsLine } from './session-stats-line'
import type { ExecutionTarget } from '@/domain/execution-target'
import type { LiveConnectionStatus, SessionLiveActivity } from '@/api/live-client'
import css from './input-bar.module.css'

function menuTriggerKey(trigger: ComposerTrigger | null, draft: string, launcherOpen: boolean): string {
  return trigger ? `${trigger.kind}:${trigger.query}:${trigger.range.start}:${trigger.range.end}:${draft}:${launcherOpen}` : ''
}

function touchComposer() {
  return window.innerWidth <= 760 || window.matchMedia('(pointer: coarse)').matches
}

export interface InputBarProps {
  sessionId: string
  accountScope?: string
  commandCatalogRevision?: string | number
  connectionStatus?: LiveConnectionStatus
  variant?: 'hero' | 'composer'
  busy: boolean
  execution?: SessionLiveActivity['execution']
  questions: PendingQuestion[]
  projection: SessionProjection | null
  events: SessionEvent[]
  model: ModelSelection
  providerTarget?: ExecutionTarget
  referenceCandidates?: ComposerReference[]
  onSubmit(input: string, attachments: Attachment[], delivery: SubmissionDelivery, references: ComposerReference['reference'][]): Promise<void>
  onCancel(): Promise<void>
  canStop?: boolean
  canAnswerQuestion?(item: PendingQuestion): boolean
  onAnswerQuestion(id: string, answer: UserQuestionAnswer): Promise<void>
  onInspectApproval(callId: string): void
  onForceTail(): void
  onError(message: string): void
  submissionBlocked?: boolean
  submissionBlockedMessage?: string
  sessionControls: React.ReactNode
  modelControl: React.ReactNode
  sessionActions?: Partial<Record<ComposerSessionAction, () => void | Promise<void>>>
  sessionMode?: 'execute' | 'plan'
  heroControls?: React.ReactNode
  stats: SessionStats | null
  inbox: SessionInboxSnapshot | null
  onEditQueueItem(id: string, input: string, expectedUpdatedAtMs: number): Promise<void>
  onLoadQueueItem(id: string): Promise<SessionSubmission | undefined>
  onRemoveQueueItem(id: string): Promise<void>
  onSteerQueueItem(id: string): Promise<void>
  t: Translate<'conversation'>
}

export function InputBar({
  sessionId,
  accountScope,
  commandCatalogRevision = '',
  connectionStatus = 'ready',
  variant = 'composer',
  busy,
  execution,
  questions,
  projection,
  events,
  model,
  providerTarget = {},
  referenceCandidates,
  onSubmit,
  onCancel,
  canStop = true,
  canAnswerQuestion,
  onAnswerQuestion,
  onInspectApproval,
  onForceTail,
  onError,
  submissionBlocked = false,
  submissionBlockedMessage = '',
  sessionControls,
  modelControl,
  sessionActions = {},
  sessionMode = 'execute',
  heroControls,
  stats,
  inbox,
  onEditQueueItem,
  onLoadQueueItem,
  onRemoveQueueItem,
  onSteerQueueItem,
  t,
}: InputBarProps) {
  const chatT = useTranslate('chat')
  const [draft, setDraft] = React.useState(() => readComposerDraft(window.localStorage, sessionId, accountScope))
  const [selection, setSelection] = React.useState<ComposerSelection>(() => ({ start: draft.length, end: draft.length }))
  const [attachments, setAttachments] = React.useState<Attachment[]>([])
  const [references, setReferences] = React.useState<ComposerReference[]>([])
  const [composing, setComposing] = React.useState(false)
  const [menuIndex, setMenuIndex] = React.useState(0)
  const [launcherOpen, setLauncherOpen] = React.useState(false)
  const menuId = React.useId()
  const commandAnchor = React.useRef<HTMLDivElement>(null)
  const [dismissedTrigger, setDismissedTrigger] = React.useState('')
  const [previewAttachment, setPreviewAttachment] = React.useState<Attachment | null>(null)
  const draftRef = React.useRef(draft)
  const selectionRef = React.useRef(selection)
  const attachmentsRef = React.useRef(attachments)
  const referencesRef = React.useRef(references)
  const editHistory = React.useRef(new ComposerEditHistory<ComposerReference>())
  const nextAttempt = React.useRef(0)
  const failedDrafts = React.useRef(new Map<number, string>())
  const failedAttachments = React.useRef(new Map<number, Attachment[]>())
  const failedReferences = React.useRef(new Map<number, ComposerReference[]>())
  const restoredDraft = React.useRef<string | null>(null)
  const inputRef = React.useRef<HTMLTextAreaElement>(null)
  const inputScrollRef = React.useRef<HTMLDivElement>(null)
  const fileRef = React.useRef<HTMLInputElement>(null)
  const busyEnter = useBusyEnterBehavior()
  const commandCatalog = useCommandCatalog(sessionId, commandCatalogRevision, t, connectionStatus)
  const skillsAvailable = commandCatalog.commands.some(command => command.value === '/skills')
  const skillCatalog = useSkillCatalog(sessionId, commandCatalogRevision, skillsAvailable, connectionStatus)
  const typedTrigger = composerTriggerAt(draft, selection)
  const trigger = launcherOpen
    ? { kind: 'command' as const, query: '', range: typedTrigger?.kind === 'command' ? typedTrigger.range : { start: 0, end: 0 } }
    : typedTrigger
  const referenceQuery = trigger?.kind === 'reference' ? trigger.query : ''
  const referenceSlash = referenceQuery.lastIndexOf('/')
  const referenceDirectory = referenceSlash < 0 ? '' : referenceQuery.slice(0, referenceSlash)
  const referenceFragment = referenceSlash < 0 ? referenceQuery : referenceQuery.slice(referenceSlash + 1)
  const triggerKey = menuTriggerKey(trigger, draft, launcherOpen)
  const referenceCatalog = useReferenceCandidates(
    sessionId,
    referenceDirectory,
    referenceFragment,
    trigger?.kind === 'reference' && referenceCandidates === undefined,
  )
  const availableReferences = referenceCandidates ?? referenceCatalog.references
  const visibleInbox = visibleInboxItems(inbox?.items ?? [], events)
  const menuRows = React.useMemo<ComposerMenuRow[]>(() => {
    if (!trigger) return []
    const query = trigger.query.toLocaleLowerCase()
    if (trigger.kind === 'skill') {
      return skillCatalog.skills
        .filter(skill => `${skill.name} ${skill.description} ${skill.when_to_use ?? ''}`.toLocaleLowerCase().includes(query))
        .slice(0, 16)
        .map(skill => ({ key: `skill:${skill.name}`, kind: 'skill', skill }))
    }
    if (trigger.kind === 'reference') {
      return availableReferences
        .filter(reference => !references.some(selected => selected.id === reference.id))
        .filter(reference => `${reference.label} ${reference.detail}`.toLocaleLowerCase().includes(query))
        .slice(0, 18)
        .map(reference => ({ key: reference.id, kind: 'reference', reference }))
    }
    return composerMenuCommands(commandCatalog.commands, trigger.kind, query)
      .filter(command => command.execution !== 'action' || sessionActions[command.value.slice(1) as ComposerSessionAction])
      .filter(command => command.execution !== 'mode' || sessionActions.plan)
      .map(command => command.execution === 'mode' ? { ...command, description: t(sessionMode === 'plan' ? 'session.toExecution' : 'session.toPlan') } : command)
      .map(command => ({ key: `command:${command.value}`, kind: 'command', command }))
  }, [availableReferences, commandCatalog.commands, references, sessionActions, sessionMode, skillCatalog.skills, t, trigger])
  const menuOpen = Boolean(trigger && !composing && dismissedTrigger !== triggerKey
    && (trigger.kind !== 'tool' || !trigger.query || menuRows.length > 0))
  const activeIndex = Math.min(menuIndex, Math.max(0, menuRows.length - 1))
  const empty = draft.trim() === '' && attachments.length === 0 && references.length === 0
  const modelIndependentDraft = modelIndependentComposerCommand(draft, commandCatalog.commands) !== null
  const waitingForExecution = React.useMemo(() => {
    const activity = currentTurnActivity(events)
    const queuedWait = execution && execution.phase !== 'running'
      && visibleInbox.some(item => item.placement === 'running' && item.run_id === execution.run_id)
    return busy && (Boolean(queuedWait) || (activity !== null && activity.phase !== 'running'))
  }, [busy, events, execution, visibleInbox])
  const primaryStops = busy && empty
  const pendingCatalogSubmit = React.useRef(false)
  const submissionContext = React.useRef(0)
  React.useEffect(() => () => { submissionContext.current += 1 }, [sessionId, commandCatalogRevision])
  const catalogMenu = menuOpen ? trigger?.kind : null
  const previousCatalogMenu = React.useRef<typeof catalogMenu>(null)

  React.useEffect(() => {
    const opened = catalogMenu !== previousCatalogMenu.current
    previousCatalogMenu.current = catalogMenu
    if (!opened) return
    if ((catalogMenu === 'command' || catalogMenu === 'tool' || catalogMenu === 'skill') && commandCatalog.error) commandCatalog.reload()
    else if (catalogMenu === 'skill' && !skillCatalog.loading && (skillCatalog.error || skillCatalog.complete === false)) skillCatalog.reload()
  }, [catalogMenu, commandCatalog.error, commandCatalog.reload, skillCatalog.complete, skillCatalog.error, skillCatalog.loading, skillCatalog.reload])

  React.useEffect(() => setMenuIndex(0), [triggerKey])
  React.useEffect(() => {
    if (!menuOpen) return
    const dismiss = (event: PointerEvent) => {
      if (event.target instanceof Node && commandAnchor.current?.contains(event.target)) return
      setLauncherOpen(false)
      setDismissedTrigger(triggerKey)
    }
    document.addEventListener('pointerdown', dismiss, true)
    return () => document.removeEventListener('pointerdown', dismiss, true)
  }, [menuOpen, triggerKey])
  React.useEffect(() => writeComposerDraft(window.localStorage, sessionId, draft, accountScope), [accountScope, draft, sessionId])

  React.useEffect(() => {
    const element = inputScrollRef.current
    if (!element) return
    const onWheel = (event: WheelEvent) => {
      const host = element.closest<HTMLElement>('[data-conversation-scroll]')
      if (!host || event.deltaY === 0) return
      const atTop = element.scrollTop <= 0
      const atEnd = element.scrollTop + element.clientHeight >= element.scrollHeight - 1
      if ((event.deltaY < 0 && !atTop) || (event.deltaY > 0 && !atEnd)) return
      event.preventDefault()
      host.scrollTop += event.deltaY
    }
    element.addEventListener('wheel', onWheel, { passive: false })
    return () => element.removeEventListener('wheel', onWheel)
  }, [])

  const changeSelection = (next: ComposerSelection) => {
    selectionRef.current = next
    setSelection(current => current.start === next.start && current.end === next.end ? current : next)
  }

  const changeDraft = (
    value: string,
    readerEdit = false,
    nextSelection: ComposerSelection = { start: value.length, end: value.length },
  ) => {
    setLauncherOpen(false)
    draftRef.current = value
    changeSelection(nextSelection)
    if (readerEdit) {
      restoredDraft.current = null
      setDismissedTrigger('')
    }
    setDraft(value)
  }

  const changeAttachments = (update: Attachment[] | ((current: Attachment[]) => Attachment[])) => {
    const next = typeof update === 'function' ? update(attachmentsRef.current) : update
    attachmentsRef.current = next
    setAttachments(next)
  }

  const changeReferences = (update: ComposerReference[] | ((current: ComposerReference[]) => ComposerReference[])) => {
    const next = typeof update === 'function' ? update(referencesRef.current) : update
    referencesRef.current = next
    setReferences(next)
  }

  const composerSnapshot = (): ComposerEditSnapshot<ComposerReference> => ({
    draft: draftRef.current,
    references: [...referencesRef.current],
    selection: { ...selectionRef.current },
  })

  const restoreSelection = (next: ComposerSelection) => {
    requestAnimationFrame(() => {
      const input = inputRef.current
      if (!input) return
      input.focus({ preventScroll: true })
      input.setSelectionRange(next.start, next.end)
      changeSelection(next)
    })
  }

  const commitComposerEdit = (
    nextDraft: string,
    nextReferences: ComposerReference[],
    nextSelection: ComposerSelection,
  ) => {
    editHistory.current.record(composerSnapshot())
    changeDraft(nextDraft, true, nextSelection)
    changeReferences(nextReferences)
    restoreSelection(nextSelection)
  }

  const applyHistory = (direction: 'undo' | 'redo'): boolean => {
    const next = direction === 'undo'
      ? editHistory.current.undo(composerSnapshot())
      : editHistory.current.redo(composerSnapshot())
    if (!next) return false
    changeDraft(next.draft, true, next.selection)
    changeReferences([...next.references])
    restoreSelection(next.selection)
    return true
  }

  const recordReaderEdit = (inputType: string) => {
    const group = ['insertText', 'insertCompositionText', 'deleteContentBackward', 'deleteContentForward'].includes(inputType)
      ? inputType
      : undefined
    editHistory.current.record(composerSnapshot(), group)
  }

  const invokeSessionAction = async (command: ComposerCommand, range?: ComposerSelection) => {
    const action = sessionActions[command.value.slice(1) as ComposerSessionAction]
    if (!action) { onError(t('command.unavailable')); return }
    const previous = draftRef.current
    const next = range ? replaceComposerRange(previous, range, '') : null
    if (next) changeDraft(next.draft, false, next.selection)
    setLauncherOpen(false)
    setDismissedTrigger(triggerKey)
    try {
      await action()
    } catch (cause) {
      if (next && draftRef.current === next.draft) changeDraft(previous)
      onError(cause instanceof Error ? cause.message : String(cause))
    }
  }

  const send = async (delivery: SubmissionDelivery = 'queue') => {
    if (delivery === 'steer' && busy && !canStop) return
    if (pendingCatalogSubmit.current) return
    const submittedDraft = draftRef.current
    let command = composerCommand(submittedDraft, commandCatalog.commands)
    const slashCandidate = /^[/.][a-z][\w!-]*(?:\s|$)/i.test(submittedDraft.trim())
    if (slashCandidate && !command && (commandCatalog.loading || commandCatalog.error)) {
      const context = submissionContext.current
      const currentAttachments = attachmentsRef.current
      const currentReferences = referencesRef.current
      pendingCatalogSubmit.current = true
      try {
        const commands = await commandCatalog.read()
        if (context !== submissionContext.current || draftRef.current !== submittedDraft
          || attachmentsRef.current !== currentAttachments || referencesRef.current !== currentReferences) return
        command = composerCommand(submittedDraft, commands)
      } catch (cause) {
        if (context === submissionContext.current && !(cause instanceof DOMException && cause.name === 'AbortError')) {
          onError(t('command.catalogUnavailable', { error: cause instanceof Error ? cause.message : String(cause) }))
        }
        return
      } finally { pendingCatalogSubmit.current = false }
    }
    if (command?.execution === 'action' || (command?.execution === 'mode' && submittedDraft.trim() === command.value && sessionActions.plan)) {
      if (submittedDraft.trim() !== command.value) { onError(t('command.noArguments', { command: command.value })); return }
      await invokeSessionAction(command, { start: 0, end: submittedDraft.length })
      return
    }
    if (submissionBlocked && !modelIndependentComposerCommand(submittedDraft, command ? [command] : [])) {
      if (submissionBlockedMessage) onError(submissionBlockedMessage)
      return
    }
    if (command && command.execution !== 'skill' && (
      referencesRef.current.length > 0 || (attachmentsRef.current.length > 0 && !command.images)
    )) {
      onError(command.execution === 'feedback' ? t('command.feedbackAttachments') : t('command.directAttachments', { command: command.value }))
      return
    }
    const input = composerCommandInput(submittedDraft.trim(), command)
    const currentAttachments = attachmentsRef.current
    const currentReferences = referencesRef.current
    if (!input && !currentAttachments.length && !currentReferences.length) return
    const attempt = ++nextAttempt.current
    failedDrafts.current.clear()
    failedAttachments.current.clear()
    failedReferences.current.clear()
    restoredDraft.current = null
    onForceTail()
    const pending = onSubmit(
      input,
      currentAttachments,
      command?.execution === 'direct' ? 'queue' : delivery,
      currentReferences.map(reference => reference.reference),
    )
    if (touchComposer()) inputRef.current?.blur()
    editHistory.current.reset()
    changeDraft(committedComposerDraft(draftRef.current, submittedDraft))
    changeAttachments(current => current.filter(attachment => !currentAttachments.includes(attachment)))
    changeReferences(current => current.filter(reference => !currentReferences.includes(reference)))
    try {
      await pending
    } catch (cause) {
      failedDrafts.current.set(attempt, submittedDraft)
      failedAttachments.current.set(attempt, currentAttachments)
      failedReferences.current.set(attempt, currentReferences)
      if (draftRef.current === '' || draftRef.current === restoredDraft.current) {
        const restored = restoredFailedDrafts(failedDrafts.current)
        restoredDraft.current = restored
        changeDraft(restored)
      }
      const orderedAttachments = [...failedAttachments.current.entries()]
        .sort(([left], [right]) => left - right)
        .flatMap(([, items]) => items)
      changeAttachments(current => [
        ...orderedAttachments,
        ...current.filter(attachment => !orderedAttachments.includes(attachment)),
      ])
      const orderedReferences = [...failedReferences.current.entries()]
        .sort(([left], [right]) => left - right)
        .flatMap(([, items]) => items)
      changeReferences(current => [
        ...orderedReferences,
        ...current.filter(reference => !orderedReferences.some(restored => restored.id === reference.id)),
      ])
      onError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      if (!touchComposer()) requestAnimationFrame(() => inputRef.current?.focus({ preventScroll: true }))
    }
  }

  const addFiles = async (files: FileList | readonly File[] | null) => {
    if (!files) return
    if (attachmentsRef.current.length + files.length > 5) {
      onError(t('attachment.tooMany', { limit: 5 }))
      return
    }
    try {
      const incoming: Attachment[] = []
      for (const file of files) incoming.push(await attachmentFromFile(file, t))
      if ([...attachmentsRef.current, ...incoming].reduce((total, item) => total + item.content.length, 0) > 12 * 1024 * 1024) {
        throw new Error(t('attachment.totalTooLarge', { limit: '12 MiB' }))
      }
      changeAttachments(current => [...current, ...incoming])
    } catch (cause) {
      onError(cause instanceof Error ? cause.message : String(cause))
    }
    if (fileRef.current) fileRef.current.value = ''
  }

  const pasteImages = (event: React.ClipboardEvent<HTMLTextAreaElement>) => {
    const images = [...event.clipboardData.files].filter(file => file.type.startsWith('image/'))
    if (!images.length) return
    if (!event.clipboardData.getData('text/plain')) event.preventDefault()
    void addFiles(images)
  }

  const navigateReferenceDirectory = (directory: string) => {
    if (trigger?.kind !== 'reference') return
    const edit = replaceComposerRange(draftRef.current, trigger.range, `@${directory ? `${directory}/` : ''}`)
    commitComposerEdit(edit.draft, referencesRef.current, edit.selection)
    setDismissedTrigger('')
  }

  const pickMenuRow = (row: ComposerMenuRow, action: 'pick' | 'drill' = 'pick') => {
    if (!trigger) return
    if (row.kind === 'command') {
      if (row.command.execution === 'action' || (row.command.execution === 'mode' && sessionActions.plan)) {
        void invokeSessionAction(row.command, trigger.range)
        return
      }
      const suffix = draftRef.current.slice(trigger.range.end)
      const value = `${composerCommandLabel(row.command)}${(row.command.inputHint || suffix) && !/^\s/.test(suffix) ? ' ' : ''}`
      const edit = replaceComposerRange(draftRef.current, trigger.range, value)
      commitComposerEdit(edit.draft, referencesRef.current, edit.selection)
    } else if (row.kind === 'skill') {
      const suffix = draftRef.current.slice(trigger.range.end)
      const value = `.skill ${row.skill.name}${/^\s/.test(suffix) ? '' : ' '}`
      const edit = replaceComposerRange(draftRef.current, trigger.range, value)
      commitComposerEdit(edit.draft, referencesRef.current, edit.selection)
    } else {
      if (row.reference.kind === 'file' && row.reference.fileKind === 'directory' && action === 'drill') {
        navigateReferenceDirectory(row.reference.detail)
        requestAnimationFrame(() => inputRef.current?.focus({ preventScroll: true }))
        return
      }
      const edit = replaceComposerRange(draftRef.current, trigger.range, '')
      const nextReferences = referencesRef.current.some(reference => reference.id === row.reference.id)
        ? referencesRef.current
        : [...referencesRef.current, row.reference]
      commitComposerEdit(edit.draft, nextReferences, edit.selection)
    }
    if (row.kind === 'command' && row.command.execution === 'skill') setDismissedTrigger('')
    else setDismissedTrigger(menuTriggerKey(composerTriggerAt(draftRef.current, selectionRef.current), draftRef.current, false))
    requestAnimationFrame(() => inputRef.current?.focus({ preventScroll: true }))
  }

  const steerWholeQueue = async () => {
    if (busy && !canStop) return
    const queued = (inbox?.items ?? []).filter(item => item.placement === 'queued')
    if (queued[0]) await onSteerQueueItem(queued[0].id)
  }

  const stop = () => {
    if (!canStop) return
    void onCancel().catch(cause => onError(cause instanceof Error ? cause.message : String(cause)))
  }

  const onPrimary = () => {
    if (primaryStops) stop()
    else void send('queue')
  }

  const placeholder = busy
    ? busyEnter === 'queue' ? t('placeholder.steerQueue') : t('placeholder.steerPreferred')
    : variant === 'hero' ? t('placeholder.hero') : t('placeholder.default')

  return (
    <div className={cn(css.root, variant === 'hero' && css.hero)} data-input-bar="">
      <div className={css.contextStack}>
        <ComposerProjectionDock
          projection={projection}
          busy={busy}
          onGoalCommand={async command => {
            if (submissionBlocked && !modelIndependentComposerCommand(command, commandCatalog.commands)) {
              throw new Error(submissionBlockedMessage)
            }
            onForceTail()
            await onSubmit(command, [], 'queue', [])
          }}
          t={t}
        />
        <ComposerTakeover canAnswer={canAnswerQuestion} questions={questions} onAnswer={onAnswerQuestion} onError={onError} onInspectApproval={onInspectApproval} t={t} />
        <QueueDock
          key={JSON.stringify([accountScope, sessionId])}
          canRemove={canStop}
          items={visibleInbox}
          running={busy}
          onEdit={onEditQueueItem}
          onLoad={onLoadQueueItem}
          onRemove={onRemoveQueueItem}
          onSteer={onSteerQueueItem}
          onError={onError}
          t={t}
        />
      </div>
      {heroControls}
      {questions.length > 0 ? (
        variant !== 'hero' ? <SessionStatsLine stats={stats} t={t} /> : null
      ) : (
      <div ref={commandAnchor} className={css.commandAnchor}>
        {menuOpen && (
          <ComposerMenu
            id={menuId}
            rows={menuRows}
            active={activeIndex}
            loading={trigger?.kind === 'skill' ? skillCatalog.loading : trigger?.kind === 'reference' ? referenceCatalog.loading : commandCatalog.loading}
            error={trigger?.kind === 'skill' ? skillCatalog.error : trigger?.kind === 'reference' ? referenceCatalog.error : commandCatalog.error}
            emptyMessage={trigger?.kind === 'skill' && skillCatalog.complete === false ? t('composer.skillsNotReady') : undefined}
            onRetry={trigger?.kind === 'reference' ? referenceCatalog.reload : trigger?.kind === 'skill' ? skillCatalog.reload : commandCatalog.reload}
            onActive={setMenuIndex}
            onPick={pickMenuRow}
            directory={trigger?.kind === 'reference' ? referenceDirectory : ''}
            onDirectory={trigger?.kind === 'reference' ? navigateReferenceDirectory : undefined}
            t={t}
          />
        )}
        <form
          className={`${css.card} composer-shell`}
          data-composer-card=""
          data-busy={busy || undefined}
          data-submit-blocked={submissionBlocked || undefined}
          onSubmit={event => { event.preventDefault(); onPrimary() }}
        >
          {references.length > 0 && (
            <div className={css.references} aria-label={t('reference.pending')}>
              {references.map(reference => (
                <span key={reference.id} className={css.reference} data-kind={reference.kind}>
                  <AtSign />
                  <span title={reference.detail}>{reference.label}</span>
                  <button type="button" aria-label={t('reference.remove', { name: reference.label })} onClick={() => commitComposerEdit(
                    draftRef.current,
                    referencesRef.current.filter(item => item.id !== reference.id),
                    selectionRef.current,
                  )}>
                    <X />
                  </button>
                </span>
              ))}
            </div>
          )}
          {attachments.length > 0 && (
            <div className={css.attachments} aria-label={t('attachment.pending')}>
              {attachments.map((attachment, index) => {
                const image = imageDataSource(attachment)
                return (
                  <span key={`${attachment.name}-${index}`} className={css.attachment}>
                    {image
                      ? (
                        <button type="button" className={css.attachmentPreview} aria-label={t('attachment.preview', { name: attachment.name })} onClick={() => setPreviewAttachment(attachment)}>
                          <img src={image} alt="" />
                        </button>
                      )
                      : <FilePlus2 className={css.attachmentFile} />}
                    <span className={css.attachmentName}>{attachment.name}</span>
                    <button type="button" className={css.attachmentRemove} aria-label={t('attachment.remove', { name: attachment.name })} onClick={() => changeAttachments(current => current.filter((_, itemIndex) => itemIndex !== index))}>
                      <X />
                    </button>
                  </span>
                )
              })}
            </div>
          )}
          <div ref={inputScrollRef} className={css.scroll} data-input-scroll="">
            <textarea
              ref={inputRef}
              className={`${css.input} composer-editor`}
              data-composer-input=""
              value={draft}
              rows={variant === 'hero' ? 2 : 1}
              aria-busy={busy}
              placeholder={placeholder}
              aria-label={t('input.aria')}
              aria-autocomplete="list"
              aria-haspopup="listbox"
              aria-controls={menuOpen ? menuId : undefined}
              aria-activedescendant={menuOpen && menuRows.length ? `${menuId}-${activeIndex}` : undefined}
              onChange={event => {
                const inputType = (event.nativeEvent as InputEvent).inputType || 'input'
                recordReaderEdit(inputType)
                changeDraft(event.target.value, true, textareaSelection(event.target))
              }}
              onSelect={event => changeSelection(textareaSelection(event.currentTarget))}
              onClick={event => changeSelection(textareaSelection(event.currentTarget))}
              onKeyUp={event => changeSelection(textareaSelection(event.currentTarget))}
              onCompositionStart={() => setComposing(true)}
              onCompositionEnd={event => {
                setComposing(false)
                changeSelection(textareaSelection(event.currentTarget))
              }}
              onBeforeInput={event => {
                const inputType = (event.nativeEvent as InputEvent).inputType
                if (inputType !== 'historyUndo' && inputType !== 'historyRedo') return
                event.preventDefault()
                applyHistory(inputType === 'historyUndo' ? 'undo' : 'redo')
              }}
              onPaste={pasteImages}
              onKeyDown={event => {
                if (composing || event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) return
                const undo = (event.ctrlKey || event.metaKey) && !event.altKey && event.key.toLocaleLowerCase() === 'z' && !event.shiftKey
                const redo = ((event.ctrlKey || event.metaKey) && !event.altKey && event.key.toLocaleLowerCase() === 'z' && event.shiftKey)
                  || ((event.ctrlKey || event.metaKey) && !event.altKey && event.key.toLocaleLowerCase() === 'y')
                if (!event.nativeEvent.isComposing && (undo || redo)) {
                  event.preventDefault()
                  applyHistory(undo ? 'undo' : 'redo')
                } else if (menuOpen && menuRows.length && (event.key === 'ArrowDown' || event.key === 'ArrowUp')) {
                  event.preventDefault()
                  event.stopPropagation()
                  setMenuIndex((activeIndex + (event.key === 'ArrowDown' ? 1 : -1) + menuRows.length) % menuRows.length)
                } else if (menuOpen && menuRows.length && event.key === 'Tab' && !event.shiftKey) {
                  event.preventDefault()
                  event.stopPropagation()
                  const row = menuRows[activeIndex]
                  if (row) pickMenuRow(row, row.kind === 'reference' && row.reference.kind === 'file' && row.reference.fileKind === 'directory' ? 'drill' : 'pick')
                } else if (menuOpen && menuRows.length && event.key === 'Enter' && !event.shiftKey) {
                  event.preventDefault()
                  event.stopPropagation()
                  const row = menuRows[activeIndex]
                  if (row) pickMenuRow(row)
                } else if (menuOpen && (event.key === 'Escape' || (event.key === 'Tab' && event.shiftKey))) {
                  event.preventDefault()
                  event.stopPropagation()
                  setLauncherOpen(false)
                  setDismissedTrigger(triggerKey)
                } else if (event.key === 'Backspace' && draft === '' && references.length > 0) {
                  event.preventDefault()
                  commitComposerEdit(draftRef.current, referencesRef.current.slice(0, -1), selectionRef.current)
                } else if (event.key === 'Enter' && !event.shiftKey && !event.nativeEvent.isComposing && event.nativeEvent.keyCode !== 229) {
                  event.preventDefault()
                  if (event.repeat) return
                  const delivery = resolveComposerDelivery(busy, event.ctrlKey || event.metaKey, busyEnter)
                  if (!draft.trim() && attachments.length === 0 && references.length === 0 && delivery === 'steer') {
                    void steerWholeQueue().catch(cause => onError(cause instanceof Error ? cause.message : String(cause)))
                  } else {
                    void send(delivery)
                  }
                }
              }}
            />
          </div>
          <div className={css.row}>
            <div className={css.tools}>
              <input ref={fileRef} type="file" multiple hidden onChange={event => void addFiles(event.target.files)} />
              <button type="button" className={`${css.add} ${css.commandLauncher}`} aria-label={t('composer.commandEntry')} title={t('composer.commandEntry')} aria-haspopup="listbox" aria-expanded={menuOpen && trigger?.kind === 'command'} onClick={() => {
                if (touchComposer()) inputRef.current?.blur()
                else inputRef.current?.focus({ preventScroll: true })
                if (menuOpen && trigger?.kind === 'command') {
                  setLauncherOpen(false)
                  setDismissedTrigger(triggerKey)
                } else {
                  setDismissedTrigger('')
                  setLauncherOpen(true)
                }
              }}><Plus size={16} /></button>
              <button type="button" className={css.add} aria-label={t('attachment.add')} onClick={() => fileRef.current?.click()}>
                <Paperclip size={16} />
              </button>
              <div className={css.modes}>{sessionControls}</div>
            </div>
            <div className={css.trailing}>
              {modelControl}
              <ComposerContextMeter model={model} events={events} target={providerTarget} t={t} />
              {busy && !empty && (
                <button
                  type="button"
                  className={css.secondaryStop}
                  aria-label={t('input.stop')}
                  title={t('input.stop')}
                  disabled={!canStop}
                  onClick={stop}
                >
                  <Square size={13} fill="currentColor" aria-hidden="true" />
                </button>
              )}
              <button
                type="submit"
                className={css.primary}
                aria-label={primaryStops ? t('input.stop') : t('input.send')}
                title={primaryStops ? t('input.stop') : t('input.send')}
                disabled={primaryStops ? !canStop : empty || (submissionBlocked && !modelIndependentDraft)}
              >
                {primaryStops
                  ? waitingForExecution ? <Square size={13} fill="currentColor" aria-hidden="true" /> : <LoaderCircle className={css.spinner} aria-hidden="true" />
                  : <ArrowUp size={18} />}
              </button>
            </div>
          </div>
        </form>
        {variant !== 'hero' && <SessionStatsLine stats={stats} t={t} />}
      </div>
      )}
      <ComposerDropOverlay disabled={questions.length > 0} onFiles={files => { void addFiles(files) }} t={t} />
      {previewAttachment && imageDataSource(previewAttachment) && (
        <ImageLightbox
          image={{ attachmentIndex: 0, name: previewAttachment.name || t('image.label'), source: imageDataSource(previewAttachment) ?? '' }}
          position={0}
          count={1}
          onClose={() => setPreviewAttachment(null)}
          onPrevious={() => {}}
          onNext={() => {}}
          t={chatT}
        />
      )}
    </div>
  )
}
