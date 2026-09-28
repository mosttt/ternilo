import { copyText } from '@/lib/clipboard'
import * as React from 'react'
import { Check, Copy, FileDiff, FileText, Globe2, Search, Sparkles, TerminalSquare } from 'lucide-react'
import type { BuiltinToolPresentation } from '@/domain/tool-presentation'
import { safeWebUrl } from '@/domain/tool-presentation'
import type { Translate } from '@/i18n/runtime'
import { parseAnsiLines } from '@/lib/ansi'
import { cn } from '@/lib/utils'
import css from './tool-call-tree.module.css'

type ChatTranslate = Translate<'chat'>

function AnsiOutput({ value, label }: { value: string; label: string }) {
  const lines = React.useMemo(() => parseAnsiLines(value), [value])
  return <section className={css.terminalSection}>
    <span className={css.resultLabel}>{label}</span>
    <pre className={css.terminalOutput} data-tool-terminal-output="" data-tool-scroll="" tabIndex={0} role="region" aria-label={label}>{lines.map((line, lineIndex) => <React.Fragment key={lineIndex}>{line.map((span, spanIndex) => <span style={span.style} key={spanIndex}>{span.text}</span>)}{lineIndex < lines.length - 1 ? '\n' : ''}</React.Fragment>)}</pre>
  </section>
}

export function ReadToolResult({ view, t }: {
  view: Extract<BuiltinToolPresentation, { kind: 'read' }>
  t: ChatTranslate
}) {
  const lines = view.content.split('\n')
  return <div className={css.resultCard} data-tool-view="read">
    <header className={css.resultHeader}><FileText /><code>{view.path}</code><span>{t('tool.lines', { start: view.startLine, end: view.endLine ?? view.startLine + Math.max(0, lines.length - 1), total: view.totalLines ?? '?' })}</span></header>
    <ol className={css.readLines} start={view.startLine} data-tool-scroll="" tabIndex={0} aria-label={t('tool.read')}>{lines.map((line, index) => <li value={view.startLine + index} key={index}><code>{line || ' '}</code></li>)}</ol>
  </div>
}

export function DiffToolResult({ view, t }: {
  view: Extract<BuiltinToolPresentation, { kind: 'diff' }>
  t: ChatTranslate
}) {
  return <div className={css.resultCard} data-tool-view="diff">
    <header className={css.resultHeader}><FileDiff /><code>{view.path}</code><span>{view.operation === 'write' ? t('tool.created') : t('tool.replaced')}</span></header>
    <pre className={css.diffLines} data-tool-scroll="" tabIndex={0} role="region" aria-label={view.operation === 'write' ? t('tool.created') : t('tool.replaced')}>{view.before.split('\n').map((line, index) => <span className={css.diffLine} data-kind="remove" key={`remove-${index}`}><b>-</b>{line || ' '}</span>)}{view.after.split('\n').map((line, index) => <span className={css.diffLine} data-kind="add" key={`add-${index}`}><b>+</b>{line || ' '}</span>)}</pre>
  </div>
}

export function SearchToolResult({ view, t }: {
  view: Extract<BuiltinToolPresentation, { kind: 'search' }>
  t: ChatTranslate
}) {
  const partial = view.truncated || view.warnings.length > 0
  return <div className={css.resultCard} data-tool-view="search" data-partial={partial || undefined}>
    <header className={css.resultHeader}><Search /><span>{view.mode === 'glob' ? t('tool.globResults') : t('tool.searchResults')}</span><code>{view.query}</code><span>{t('tool.resultCount', { count: view.entries.length })}</span></header>
    {view.entries.length ? <ol className={css.searchResults} data-tool-scroll="" tabIndex={0} aria-label={view.mode === 'glob' ? t('tool.globResults') : t('tool.searchResults')}>{view.entries.map((entry, index) => <li key={`${entry.path}:${entry.line ?? ''}:${index}`}><code>{entry.path}{entry.line == null ? '' : `:${entry.line}${entry.column == null ? '' : `:${entry.column}`}`}</code>{entry.preview && <span>{entry.preview}</span>}</li>)}</ol> : <p className={css.empty}>{t(partial ? 'tool.noCollectedResults' : 'tool.noResults')}</p>}
    {partial && <div className={css.searchNotice} data-tool-search-notice="" role="note" aria-label={t('tool.partialResults')}>
      <p>{t(view.truncated ? 'tool.searchTruncated' : 'tool.partialResults')}</p>
      {view.warnings.length > 0 && <ul>{view.warnings.map((warning, index) => <li key={index}>{warning}</li>)}</ul>}
    </div>}
  </div>
}

export function TerminalToolResult({ view, t }: {
  view: Extract<BuiltinToolPresentation, { kind: 'terminal' }>
  t: ChatTranslate
}) {
  const failed = view.timedOut || (view.exitCode != null && view.exitCode !== 0)
  const [copyState, setCopyState] = React.useState<'idle' | 'copied' | 'failed'>('idle')
  const copyLabel = copyState === 'copied' ? t('message.copied') : t('tool.copyCommand')
  const copyCommand = async () => {
    try {
      await copyText(view.command)
      setCopyState('copied')
    } catch {
      setCopyState('failed')
    }
  }
  return <div className={cn(css.resultCard, css.terminalCard)} data-tool-view="terminal" data-error={failed || undefined}>
    <header className={cn(css.resultHeader, css.terminalHeader)}>
      <TerminalSquare aria-hidden />
      <code data-tool-terminal-command=""><span aria-hidden>$ </span>{view.command}</code>
      <div className={css.terminalActions}>
        <span>{view.timedOut ? t('tool.timedOut') : view.exitCode == null ? t('event.completed') : t('tool.exitCode', { code: view.exitCode })}</span>
        <button type="button" className={css.copyCommand} aria-label={copyLabel} title={copyLabel} onClick={() => void copyCommand()}>{copyState === 'copied' ? <Check aria-hidden /> : <Copy aria-hidden />}</button>
      </div>
    </header>
    {copyState === 'failed' && <p className={css.copyError} role="alert">{t('message.copyFailed')}</p>}
    {view.stdout && <AnsiOutput label="stdout" value={view.stdout} />}
    {view.stderr && <AnsiOutput label="stderr" value={view.stderr} />}
    {!view.stdout && !view.stderr && <p className={css.empty}>{t('tool.noOutput')}</p>}
  </div>
}

function SafeLink({ url, children }: { url: string; children: React.ReactNode }) {
  const href = safeWebUrl(url)
  return href ? <a href={href} target="_blank" rel="noopener noreferrer">{children}</a> : <span>{children}</span>
}

export function WebSearchToolResult({ view, t }: {
  view: Extract<BuiltinToolPresentation, { kind: 'web-search' }>
  t: ChatTranslate
}) {
  return <div className={css.resultCard} data-tool-view="web-search">
    <header className={css.resultHeader}><Globe2 /><span>{t('tool.webSearch')}</span><code>{view.query}</code><span>{t('tool.resultCount', { count: view.sources.length })}</span></header>
    {view.sources.length ? <ol className={css.webResults} data-tool-scroll="" tabIndex={0} aria-label={t('tool.webSearch')}>{view.sources.map((source, index) => <li key={`${source.url}:${index}`}><SafeLink url={source.url}>{source.title}</SafeLink>{source.snippet && <p>{source.snippet}</p>}{source.engine && <span>{source.engine}</span>}</li>)}</ol> : <p className={css.empty}>{t('tool.noResults')}</p>}
  </div>
}

export function WebFetchToolResult({ view, t }: {
  view: Extract<BuiltinToolPresentation, { kind: 'web-fetch' }>
  t: ChatTranslate
}) {
  return <div className={css.resultCard} data-tool-view="web-fetch">
    <header className={css.resultHeader}><Globe2 /><SafeLink url={view.url}>{view.url}</SafeLink><span>HTTP {view.statusCode} · {view.contentType}</span></header>
    <pre className={css.webBody} data-tool-scroll="" tabIndex={0} role="region" aria-label={t('tool.webFetch')}>{view.body || t('tool.emptyResponse')}</pre>
  </div>
}

export function SkillToolResult({ view, t }: {
  view: Extract<BuiltinToolPresentation, { kind: 'skill' }>
  t: ChatTranslate
}) {
  return <section className={cn(css.resultCard, css.skillResult)} data-tool-view="skill" data-error={view.error || undefined} aria-label={t('tool.skillInstructions')}>
    <header className={css.resultHeader}><Sparkles /><span>{t('tool.skillInstructions')}</span><code>{view.name}</code></header>
    <pre className={css.skillInstructions} data-tool-scroll="" tabIndex={0} role="region" aria-label={t('tool.skillInstructions')}>{view.content || t('tool.emptyResult')}</pre>
  </section>
}

export function GenericToolResult({ view, t }: {
  view: Extract<BuiltinToolPresentation, { kind: 'generic' }>
  t: ChatTranslate
}) {
  return <pre className={cn(css.resultCard, css.genericResult)} data-tool-view="generic" data-tool-scroll="" data-error={view.error || undefined} tabIndex={0} role="region" aria-label={t('details.output')}>{view.content || t('tool.emptyResult')}</pre>
}
