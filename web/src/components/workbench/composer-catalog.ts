import * as React from 'react'
import { api, ApiError } from '@/api/client'
import type { LiveConnectionStatus } from '@/api/live-client'
import type {
  ReferenceCandidate, ReferenceCandidateSnapshot, SessionCommandCatalog, SkillCatalogSnapshot, SkillSummary,
  SubmissionReference,
} from '@/types'
import type { ConversationKey } from '@/i18n/resources/conversation'
import type { Translate } from '@/i18n/runtime'

export type ComposerCommand = {
  value: string
  description: string
  inputHint?: string
  images: boolean
  execution: 'direct' | 'feedback' | 'mode' | 'skill' | 'action'
}

export type ComposerSessionAction = 'model' | 'permission' | 'export' | 'plan'

const SESSION_COMMANDS = ['/compact', '/export', '/feedback', '/goal', '/permission', '/plan', '/model']

export function composerCommandSurface(command: ComposerCommand): 'command' | 'tool' {
  return SESSION_COMMANDS.includes(command.value) ? 'command' : 'tool'
}

export function composerCommandLabel(command: ComposerCommand): string {
  return composerCommandSurface(command) === 'tool' ? `.${command.value.slice(1)}` : command.value
}

export function composerMenuCommands(commands: readonly ComposerCommand[], surface: 'command' | 'tool', query: string): ComposerCommand[] {
  const needle = query.toLocaleLowerCase()
  const matches = commands.filter(command => composerCommandSurface(command) === surface
    && `${command.value.slice(1)} ${command.description}`.toLocaleLowerCase().includes(needle))
  return surface === 'command'
    ? matches.sort((left, right) => SESSION_COMMANDS.indexOf(left.value) - SESSION_COMMANDS.indexOf(right.value))
    : matches
}

const CLIENT_COMMANDS: ReadonlyArray<{
  value: string
  description: ConversationKey
  inputHint?: string
  images: boolean
  execution: 'feedback' | 'mode' | 'skill' | 'action'
}> = [
  { value: '/export', description: 'command.export', images: false, execution: 'action' },
  { value: '/model', description: 'command.model', images: false, execution: 'action' },
  { value: '/permission', description: 'command.permission', images: false, execution: 'action' },
  { value: '/feedback', description: 'command.feedback', inputHint: '<text>', images: false, execution: 'feedback' },
  { value: '/plan', description: 'command.plan', images: false, execution: 'mode' },
  { value: '/skill', description: 'command.skill', inputHint: '<name> [task]', images: true, execution: 'skill' },
]

export function clientComposerCommands(t: Translate<'conversation'>): ComposerCommand[] {
  return CLIENT_COMMANDS.map(command => ({ ...command, description: t(command.description) }))
}

export function mergeComposerCommands(
  snapshot: SessionCommandCatalog | null,
  t: Translate<'conversation'>,
): ComposerCommand[] {
  const skillsAvailable = snapshot?.commands.some(command => command.name === 'skills') ?? false
  const client = clientComposerCommands(t).filter(command => command.execution !== 'skill' || skillsAvailable)
  const clientNames = new Set(client.map(command => command.value.slice(1)))
  const direct = (snapshot?.commands ?? []).map(command => {
    if (clientNames.has(command.name)) throw new Error(`direct command /${command.name} collides with a Web contribution`)
    return {
      value: `/${command.name}`,
      description: command.name === 'compact' ? t('command.compact') : command.name === 'goal' ? t('command.goal') : command.description,
      ...(command.input?.hint ? { inputHint: command.input.hint } : {}),
      images: command.input?.images ?? false,
      execution: 'direct' as const,
    }
  })
  return [...direct, ...client].sort((left, right) => left.value.localeCompare(right.value))
}

export function composerCommand(input: string, commands: readonly ComposerCommand[]): ComposerCommand | null {
  const token = input.trim().match(/^[/.][^\s]+/)?.[0]
  if (!token) return null
  const canonical = `/${token.slice(1)}`
  return commands.find(command => command.value === canonical
    && (token.startsWith('/') || composerCommandSurface(command) === 'tool')) ?? null
}

export function composerCommandInput(input: string, command: ComposerCommand | null): string {
  return command && input.startsWith('.') ? `/${input.slice(1)}` : input
}

export function clientComposerCommand(input: string, t: Translate<'conversation'>): ComposerCommand | null {
  return composerCommand(input, clientComposerCommands(t))
}

export function modelIndependentComposerCommand(
  input: string,
  commands: readonly ComposerCommand[],
): ComposerCommand | null {
  const command = composerCommand(input, commands)
  if (command?.value === '/goal') {
    const action = input.trim().split(/\s+/)[1]
    if (action && !['edit', 'complete', 'blocked'].includes(action)) return null
  }
  return command && command.execution !== 'skill' ? command : null
}

export function useCommandCatalog(
  sessionId: string,
  compositionRevision: string | number,
  t: Translate<'conversation'>,
  connectionStatus: LiveConnectionStatus = 'ready',
): {
  commands: ComposerCommand[]
  loading: boolean
  error: string
  reload(): void
  read(): Promise<ComposerCommand[]>
} {
  const compositionKey = `${sessionId}\u0000${compositionRevision}`
  const [cached, setCached] = React.useState<{ key: string; snapshot: SessionCommandCatalog } | null>(null)
  const [loading, setLoading] = React.useState(true)
  const [error, setError] = React.useState('')
  const [revision, reload] = React.useReducer(value => value + 1, 0)
  const reader = React.useRef<(() => Promise<SessionCommandCatalog>) | null>(null)
  const previousConnection = React.useRef(connectionStatus)

  React.useEffect(() => {
    const recovered = connectionStatus === 'ready' && previousConnection.current !== 'ready'
    previousConnection.current = connectionStatus
    if (recovered && error && !loading) reload()
  }, [connectionStatus, error, loading])

  React.useEffect(() => {
    const controller = new AbortController()
    const requestCatalog = () => api.request<SessionCommandCatalog>(
      `/sessions/${encodeURIComponent(sessionId)}/commands`,
      { signal: controller.signal },
    )
    let pending: Promise<SessionCommandCatalog> | null = null
    const read = () => {
      if (pending) return pending
      setLoading(true)
      setError('')
      pending = requestCatalog()
        .catch(async cause => {
          if (!(cause instanceof ApiError) || cause.status !== 503) throw cause
          await new Promise(resolve => setTimeout(resolve, 100))
          if (controller.signal.aborted) throw cause
          return requestCatalog()
        })
        .then(next => {
          if (controller.signal.aborted) throw new DOMException('Catalog request was cancelled', 'AbortError')
          if (next.session_id !== sessionId) throw new Error(`command catalog belongs to ${next.session_id}, expected ${sessionId}`)
          setCached({ key: compositionKey, snapshot: next })
          return next
        })
        .catch(cause => {
          if (!controller.signal.aborted) {
            setCached(null)
            setError(cause instanceof Error ? cause.message : String(cause))
          }
          throw cause
        })
        .finally(() => {
          if (!controller.signal.aborted) setLoading(false)
          pending = null
        })
      return pending
    }
    reader.current = read
    void read().catch(() => undefined)
    return () => {
      controller.abort()
      if (reader.current === read) reader.current = null
    }
  }, [compositionKey, compositionRevision, revision, sessionId])

  let commands: ComposerCommand[]
  let catalogError = error
  try {
    commands = mergeComposerCommands(cached?.key === compositionKey ? cached.snapshot : null, t)
  } catch (cause) {
    commands = clientComposerCommands(t)
    if (!catalogError) catalogError = cause instanceof Error ? cause.message : String(cause)
  }
  const read = async () => {
    if (!reader.current) throw new DOMException('Catalog request was cancelled', 'AbortError')
    return mergeComposerCommands(await reader.current(), t)
  }
  return { commands, loading, error: catalogError, reload, read }
}

/** Returns the raw `/feedback` suffix, or null when the input is not this command. */
export function feedbackCommandText(input: string): string | null {
  const match = /^\/feedback(?:\s([\s\S]*))?$/.exec(input)
  return match ? match[1] ?? '' : null
}

export type ComposerReference = {
  id: string
  kind: 'file' | 'session'
  label: string
  detail: string
  fileKind?: 'file' | 'directory'
  reference: SubmissionReference
}

export function composerReference(candidate: ReferenceCandidate): ComposerReference {
  if (candidate.kind === 'file') {
    return {
      id: `file:${candidate.path}`,
      kind: 'file',
      label: candidate.label,
      detail: candidate.path,
      fileKind: candidate.file_kind,
      reference: { kind: 'file', path: candidate.path, file_kind: candidate.file_kind },
    }
  }
  return {
    id: `session:${candidate.session_id}`,
    kind: 'session',
    label: candidate.label,
    detail: candidate.same_workspace ? '' : candidate.workspace,
    reference: { kind: 'session', session_id: candidate.session_id, label: candidate.label },
  }
}

export function useReferenceCandidates(
  sessionId: string,
  directory: string,
  query: string,
  enabled: boolean,
): {
  references: ComposerReference[]
  loading: boolean
  error: string
  reload(): void
} {
  const [snapshot, setSnapshot] = React.useState<ReferenceCandidateSnapshot | null>(null)
  const [loading, setLoading] = React.useState(false)
  const [error, setError] = React.useState('')
  const [revision, reload] = React.useReducer(value => value + 1, 0)

  React.useEffect(() => {
    if (!enabled) return
    const controller = new AbortController()
    const params = new URLSearchParams({ directory, query })
    setLoading(true)
    setError('')
    void api.request<ReferenceCandidateSnapshot>(
      `/sessions/${encodeURIComponent(sessionId)}/references?${params.toString()}`,
      { signal: controller.signal },
    ).then(setSnapshot).catch(cause => {
      if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : String(cause))
    }).finally(() => {
      if (!controller.signal.aborted) setLoading(false)
    })
    return () => controller.abort()
  }, [directory, enabled, query, revision, sessionId])

  return {
    references: snapshot?.candidates.map(composerReference) ?? [],
    loading,
    error,
    reload,
  }
}

export function useSkillCatalog(
  sessionId: string,
  compositionRevision: string | number,
  enabled: boolean,
  connectionStatus: LiveConnectionStatus = 'ready',
): {
  skills: SkillSummary[]
  complete: boolean | undefined
  loading: boolean
  error: string
  reload(): void
} {
  const compositionKey = `${sessionId}\u0000${compositionRevision}`
  const [cached, setCached] = React.useState<{ key: string; skills: SkillSummary[]; complete: boolean } | null>(null)
  const [loading, setLoading] = React.useState(true)
  const [error, setError] = React.useState('')
  const [revision, reload] = React.useReducer(value => value + 1, 0)
  const previousConnection = React.useRef(connectionStatus)

  React.useEffect(() => {
    const recovered = connectionStatus === 'ready' && previousConnection.current !== 'ready'
    previousConnection.current = connectionStatus
    if (recovered && enabled && error && !loading) reload()
  }, [connectionStatus, enabled, error, loading])

  React.useEffect(() => {
    if (!enabled) {
      setCached(null)
      setLoading(false)
      setError('')
      return
    }
    const controller = new AbortController()
    setLoading(true)
    setError('')
    const requestCatalog = () => api.request<SkillCatalogSnapshot>(
      `/sessions/${encodeURIComponent(sessionId)}/skills`,
      { signal: controller.signal },
    )
    void requestCatalog()
      .catch(async cause => {
        if (!(cause instanceof ApiError) || cause.status !== 503) throw cause
        await new Promise(resolve => setTimeout(resolve, 100))
        if (controller.signal.aborted) throw cause
        return requestCatalog()
      })
      .then(snapshot => setCached({
        key: compositionKey,
        skills: snapshot.skills.filter(skill => skill.invocation.user_invocable),
        complete: snapshot.complete,
      }))
      .catch(cause => {
        if (!controller.signal.aborted) {
          setCached(null)
          setError(cause instanceof Error ? cause.message : String(cause))
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false)
      })
    return () => controller.abort()
  }, [compositionKey, compositionRevision, enabled, revision, sessionId])

  return { skills: cached?.key === compositionKey ? cached.skills : [],
    complete: cached?.key === compositionKey ? cached.complete : undefined, loading, error, reload }
}
