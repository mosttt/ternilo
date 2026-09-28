import * as React from 'react'
import { ChevronRight, CornerDownLeft, Download, FileText, Folder, ListTodo, MessageSquareText, Minimize2, RotateCcw, Shield, Sparkles, Target, TerminalSquare, type LucideIcon } from 'lucide-react'
import type { Translate } from '@/i18n/runtime'
import { cn } from '@/lib/utils'
import type { SkillSummary } from '@/types'
import { composerCommandLabel, composerCommandSurface, type ComposerCommand, type ComposerReference } from './composer-catalog'
import css from './composer-menu.module.css'

const SESSION_COMMAND_ICONS: Record<string, LucideIcon> = {
  '/compact': Minimize2,
  '/export': Download,
  '/feedback': MessageSquareText,
  '/goal': Target,
  '/permission': Shield,
  '/plan': ListTodo,
  '/model': Sparkles,
}

export type ComposerMenuRow =
  | { key: string; kind: 'command'; command: ComposerCommand }
  | { key: string; kind: 'skill'; skill: SkillSummary }
  | { key: string; kind: 'reference'; reference: ComposerReference }

export function ComposerMenu({
  id,
  rows,
  active,
  loading = false,
  error = '',
  emptyMessage,
  onRetry,
  onActive,
  onPick,
  directory = '',
  onDirectory,
  t,
}: {
  id: string
  rows: ComposerMenuRow[]
  active: number
  loading?: boolean
  error?: string
  emptyMessage?: string
  onRetry?(): void
  onActive(index: number): void
  onPick(row: ComposerMenuRow, action?: 'pick' | 'drill'): void
  directory?: string
  onDirectory?(directory: string): void
  t: Translate<'conversation'>
}) {
  const viewport = React.useRef<HTMLDivElement>(null)
  const activeKey = rows[active]?.key
  React.useLayoutEffect(() => {
    const menu = viewport.current
    const selected = menu?.querySelector<HTMLElement>('[aria-selected="true"]')
    if (!menu || !selected) return
    const bounds = menu.getBoundingClientRect()
    const row = selected.getBoundingClientRect()
    if (row.top < bounds.top) menu.scrollTop -= bounds.top - row.top
    else if (row.bottom > bounds.bottom) menu.scrollTop += row.bottom - bounds.bottom
  }, [active, activeKey])

  return (
    <div className={css.menu} data-composer-menu="">
      <div ref={viewport} id={id} className={css.viewport} role="listbox" aria-label={t('command.menu')}>
      {onDirectory && directory && (
        <div className={css.crumbs} aria-label={t('reference.location')}>
          <button type="button" onMouseDown={event => { event.preventDefault(); onDirectory('') }}>{t('reference.root')}</button>
          {directory.split('/').filter(Boolean).map((segment, index, parts) => {
            const path = parts.slice(0, index + 1).join('/')
            return <span key={path}><ChevronRight /><button type="button" onMouseDown={event => { event.preventDefault(); onDirectory(path) }}>{segment}</button></span>
          })}
        </div>
      )}
      {loading && <div className={css.status}>{t('composer.catalogLoading')}</div>}
      {error && (
        <div className={css.error} role="alert">
          <span>{error}</span>
          {onRetry && <button type="button" onClick={onRetry}><RotateCcw />{t('composer.retry')}</button>}
        </div>
      )}
      {!loading && !error && rows.length === 0 && <div className={css.status}>{emptyMessage ?? t('composer.noMatches')}</div>}
      {rows.map((row, index) => {
        const previous = rows[index - 1]
        const referenceSectionChanged = row.kind === 'reference'
          && previous?.kind === 'reference'
          && previous.reference.kind !== row.reference.kind
        const section = index === 0 || previous?.kind !== row.kind || referenceSectionChanged
          ? row.kind === 'command'
            ? t(composerCommandSurface(row.command) === 'tool' ? 'composer.tools' : 'composer.commands')
            : row.kind === 'skill'
              ? t('composer.skills')
              : row.reference.kind === 'file'
                ? t('composer.files')
                : t('composer.sessions')
          : null
        const Icon = row.kind === 'command'
          ? SESSION_COMMAND_ICONS[row.command.value] ?? TerminalSquare
          : row.kind === 'skill'
            ? Sparkles
            : row.reference.kind === 'file'
              ? row.reference.fileKind === 'directory' ? Folder : FileText
              : MessageSquareText
        const drillable = row.kind === 'reference'
          && row.reference.kind === 'file'
          && row.reference.fileKind === 'directory'
        return (
          <div key={row.key}>
            {section && <div className={css.section}>{section}</div>}
            <div className={cn(css.itemRow, index === active && css.active)} onMouseMove={() => onActive(index)}>
              <button
                id={`${id}-${index}`}
                type="button"
                role="option"
                tabIndex={-1}
                aria-selected={index === active}
                title={row.kind === 'command' ? row.command.description : undefined}
                className={css.item}
                onMouseDown={event => {
                  event.preventDefault()
                  onPick(row)
                }}
              >
                <Icon className={css.icon} />
                {row.kind === 'command' ? (
                  <>
                    <code>{composerCommandLabel(row.command)}</code>
                    {row.command.inputHint && <span className={css.hint}>{row.command.inputHint}</span>}
                    <span className={css.description}>{row.command.description}</span>
                  </>
                ) : row.kind === 'skill' ? (
                  <>
                    <span className={css.name}>{row.skill.name}</span>
                    <span className={css.description}>{row.skill.description || row.skill.when_to_use}</span>
                    <span className={css.badge}>{row.skill.provider}</span>
                  </>
                ) : (
                  <>
                    <span className={css.name}>{row.reference.label}</span>
                    <span className={css.description}>{row.reference.detail}</span>
                    <span className={css.badge}>{row.reference.kind === 'file'
                      ? row.reference.fileKind === 'directory' ? t('composer.folderBadge') : t('composer.fileBadge')
                      : t('composer.sessionBadge')}</span>
                  </>
                )}
                <CornerDownLeft className={cn(css.accept, index !== active && css.inactiveAccept)} aria-hidden="true" />
              </button>
              {drillable && (
                <button
                  type="button"
                  className={css.drill}
                  aria-label={t('reference.openFolder', { name: row.reference.label })}
                  onMouseDown={event => {
                    event.preventDefault()
                    event.stopPropagation()
                    onPick(row, 'drill')
                  }}
                >
                  <ChevronRight />
                </button>
              )}
            </div>
          </div>
        )
      })}
      </div>
      <div className={css.help} aria-hidden="true">{t('composer.commandHelp')}</div>
    </div>
  )
}
