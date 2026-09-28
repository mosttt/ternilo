import * as React from 'react'
import { Bot, ChevronDown, ExternalLink, GitBranch, UsersRound } from 'lucide-react'
import { addressableSubagentSession, deriveSubagents, type SubagentView } from '@/domain/observability'
import { useTranslate } from '@/i18n/provider'
import type { LocalSession, SessionEvent } from '@/types'
import { useOpenAgentTeam } from './agent-team-panel'
import css from './session-lineage.module.css'

function statusLabel(status: SubagentView['status'], t: ReturnType<typeof useTranslate<'observability'>>) {
  return t(`lineage.status.${status}` as const)
}

export function SessionLineage({ events, session, sessions, onOpenSession }: {
  events: readonly SessionEvent[]
  session: LocalSession
  sessions: readonly LocalSession[]
  onOpenSession(id: string): void
}) {
  const t = useTranslate('observability')
  const openTeam = useOpenAgentTeam()
  const subagents = React.useMemo(() => deriveSubagents(events), [events])
  const children = sessions.filter(item => item.parent_session_id === session.identity.session_id && item.archived_at_ms == null)
  const byId = React.useMemo(() => new Map(
    sessions
      .filter(item => item.archived_at_ms == null)
      .map(item => [item.identity.session_id, item]),
  ), [sessions])
  const ancestors = React.useMemo(() => {
    const chain: LocalSession[] = []
    let parentId = session.parent_session_id
    while (parentId) {
      const parent = byId.get(parentId)
      if (!parent) break
      chain.unshift(parent)
      parentId = parent.parent_session_id
    }
    return chain
  }, [byId, session.parent_session_id])
  const [open, setOpen] = React.useState(false)
  const root = React.useRef<HTMLDivElement>(null)
  const trigger = React.useRef<HTMLButtonElement>(null)

  React.useEffect(() => {
    if (!open) return
    const close = (event: PointerEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false)
    }
    document.addEventListener('pointerdown', close)
    return () => document.removeEventListener('pointerdown', close)
  }, [open])

  if (!subagents.length && !children.length && !ancestors.length) return null
  const countLabel = subagents.length
    ? t(subagents.length === 1 ? 'lineage.trigger.one' : 'lineage.trigger.other', { count: subagents.length })
    : t('lineage.trigger.sessions')
  const unaddressable = subagents.filter(item => !addressableSubagentSession(item, sessions))
  const addressedIds = new Set(
    subagents
      .map(item => addressableSubagentSession(item, sessions)?.identity.session_id)
      .filter((id): id is string => Boolean(id)),
  )
  const renderDescendants = (parentId: string, depth: number): React.ReactNode => sessions
    .filter(item => item.parent_session_id === parentId && item.archived_at_ms == null)
    .map(child => <React.Fragment key={child.identity.session_id}>
      <button
        type="button"
        className={css.sessionRow}
        role="treeitem"
        aria-level={depth}
        data-lineage-session={child.identity.session_id}
        style={{ paddingInlineStart: `${8 + (depth - 1) * 16}px` }}
        aria-label={t('lineage.openSession', { name: child.title })}
        onClick={() => { setOpen(false); onOpenSession(child.identity.session_id) }}
      >
        <GitBranch aria-hidden="true" /><span>{child.title}</span>
      </button>
      {renderDescendants(child.identity.session_id, depth + 1)}
    </React.Fragment>)

  return <div
    ref={root}
    className={css.root}
    data-session-lineage=""
    onKeyDown={event => {
      if (event.key !== 'Escape' || !open) return
      event.preventDefault()
      setOpen(false)
      trigger.current?.focus()
    }}
  >
    <button ref={trigger} type="button" className={css.trigger} aria-expanded={open} aria-label={countLabel} onClick={() => setOpen(value => !value)}>
      <GitBranch aria-hidden="true" />
      <span>{countLabel}</span>
      <ChevronDown data-open={open || undefined} aria-hidden="true" />
    </button>
    {open && <div className={css.menu} role="tree" aria-label={t('lineage.title')}>
      {ancestors.length > 0 && <section className={css.group}>
        <strong>{t('lineage.parent')}</strong>
        {ancestors.map((ancestor, index) => <button
          type="button"
          className={css.sessionRow}
          role="treeitem"
          aria-level={index + 1}
          data-lineage-ancestor={ancestor.identity.session_id}
          style={{ paddingInlineStart: `${8 + index * 16}px` }}
          key={ancestor.identity.session_id}
          onClick={() => { setOpen(false); onOpenSession(ancestor.identity.session_id) }}
        >
          <GitBranch aria-hidden="true" /><span>{ancestor.title}</span>
        </button>)}
      </section>}
      {subagents.length > 0 && <section className={css.group}>
        <strong>{t('lineage.title')}</strong>
        {subagents.map(item => {
          const addressed = addressableSubagentSession(item, sessions)
          const content = <>
            <span className={css.dot} data-status={item.status} aria-hidden="true" />
            <span className={css.itemCopy}><span>{item.label}</span><small>{item.task || t('lineage.emptyTask')}</small></span>
            <span className={css.itemStatus}>{statusLabel(item.status, t)}</span>
          </>
          if (openTeam) return <React.Fragment key={item.id}>
            <div className={css.item} role="treeitem">
              {content}
              <button type="button" className={css.itemAction} aria-label={t('team.openFor', { name: item.label })} onClick={() => { setOpen(false); openTeam(item.id) }}><UsersRound aria-hidden="true" /></button>
              {addressed && <button type="button" className={css.itemAction} aria-label={t('lineage.open', { name: item.label })} onClick={() => { setOpen(false); onOpenSession(addressed.identity.session_id) }}><ExternalLink aria-hidden="true" /></button>}
            </div>
            {addressed && renderDescendants(addressed.identity.session_id, 2)}
          </React.Fragment>
          return addressed
            ? <React.Fragment key={item.id}><button type="button" className={css.item} role="treeitem" aria-label={t('lineage.open', { name: item.label })} onClick={() => { setOpen(false); onOpenSession(addressed.identity.session_id) }}>{content}</button>{renderDescendants(addressed.identity.session_id, 2)}</React.Fragment>
            : <div className={css.item} role="treeitem" aria-disabled="true" key={item.id}>{content}</div>
        })}
      </section>}
      {children.some(child => !addressedIds.has(child.identity.session_id)) && <section className={css.group}>
        <strong>{t('lineage.localChildren')}</strong>
        {children.filter(child => !addressedIds.has(child.identity.session_id)).map(child => <React.Fragment key={child.identity.session_id}>
          <button type="button" className={css.sessionRow} role="treeitem" aria-level={1} data-lineage-session={child.identity.session_id} aria-label={t('lineage.openSession', { name: child.title })} onClick={() => { setOpen(false); onOpenSession(child.identity.session_id) }}>
            <GitBranch aria-hidden="true" /><span>{child.title}</span>
          </button>
          {renderDescendants(child.identity.session_id, 2)}
        </React.Fragment>)}
      </section>}
      {unaddressable.some(item => item.sessionId && item.transcriptKind === 'conversation') && <p className={css.readOnly}><Bot aria-hidden="true" />{t('subagent.sessionUnavailable')}</p>}
      {unaddressable.some(item => !item.sessionId || item.transcriptKind !== 'conversation') && <p className={css.readOnly}><Bot aria-hidden="true" />{t('lineage.readOnly')}</p>}
    </div>}
  </div>
}
