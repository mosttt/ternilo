import { copyText } from '@/lib/clipboard'
import * as React from 'react'
import { Braces, Check, Copy, FileJson2 } from 'lucide-react'
import type { Translate } from '@/i18n/runtime'
import css from './json-tree.module.css'

const OBJECT_PREVIEW_LIMIT = 4
const ARRAY_PREVIEW_LIMIT = 5
const PREVIEW_DEPTH_LIMIT = 2

type JsonPath = readonly (number | string)[]

function isContainer(value: unknown): value is Record<string, unknown> | unknown[] {
  return typeof value === 'object' && value !== null && !(value instanceof Date)
}

function entriesOf(value: Record<string, unknown> | unknown[]) {
  return Array.isArray(value)
    ? value.map((item, index) => [String(index), item] as const)
    : Object.entries(value)
}

function brackets(value: Record<string, unknown> | unknown[]) {
  return Array.isArray(value) ? ['[', ']'] as const : ['{', '}'] as const
}

function rawText(value: unknown): string {
  if (typeof value === 'string') return value
  const encoded = JSON.stringify(value, null, 2)
  return encoded ?? String(value)
}

function pathText(path: JsonPath): string {
  return path.reduce<string>((result, part) => {
    if (typeof part === 'number') return `${result}[${part}]`
    return /^[A-Za-z_$][\w$]*$/.test(part)
      ? `${result}.${part}`
      : `${result}[${JSON.stringify(part)}]`
  }, '$')
}

function pathId(path: JsonPath): string {
  return path.map(part => typeof part === 'number' ? `n${part}` : `s${part.length}:${part}`).join('/')
}

function primitive(value: unknown) {
  if (value === null) return <span className={css.keyword}>null</span>
  if (typeof value === 'string') return <span className={css.string}>{JSON.stringify(value)}</span>
  if (typeof value === 'number') return <span className={css.number}>{String(value)}</span>
  if (typeof value === 'boolean') return <span className={css.keyword}>{String(value)}</span>
  return <span className={css.other}>{String(value)}</span>
}

function preview(value: unknown, depth: number): React.ReactNode {
  if (!isContainer(value)) return primitive(value)
  const values = entriesOf(value)
  const array = Array.isArray(value)
  const limit = array ? ARRAY_PREVIEW_LIMIT : OBJECT_PREVIEW_LIMIT
  const [open, close] = brackets(value)
  return <>
    <span className={css.punctuation}>{open}</span>
    {depth >= PREVIEW_DEPTH_LIMIT
      ? <span className={css.ellipsis}>…</span>
      : values.slice(0, limit).map(([key, item], index) => <React.Fragment key={key}>
        {index > 0 && <span className={css.punctuation}>, </span>}
        {!array && <><span className={css.previewKey}>{key}</span><span className={css.punctuation}>: </span></>}
        {preview(item, depth + 1)}
      </React.Fragment>)}
    {depth < PREVIEW_DEPTH_LIMIT && values.length > limit && <span className={css.ellipsis}>, …</span>}
    <span className={css.punctuation}>{close}</span>
  </>
}

function moveExpander(button: HTMLButtonElement, direction: -1 | 1 | 'first' | 'last') {
  const tree = button.closest<HTMLElement>('[role="tree"]')
  if (!tree) return
  const expanders = [...tree.querySelectorAll<HTMLButtonElement>('[data-json-expander]')]
  if (expanders.length === 0) return
  const current = expanders.indexOf(button)
  const index = direction === 'first' ? 0
    : direction === 'last' ? expanders.length - 1
      : (current + direction + expanders.length) % expanders.length
  expanders[index]?.focus()
}

function JsonNode({
  field,
  value,
  path,
  last,
  activeId,
  onActive,
  t,
}: {
  field: string
  value: unknown
  path: JsonPath
  last: boolean
  activeId: string | null
  onActive(id: string): void
  t: Translate<'chat'>
}) {
  const groupId = React.useId()
  const [expanded, setExpanded] = React.useState(false)
  const container = isContainer(value)
  const values = container ? entriesOf(value) : []
  const expandable = values.length > 0
  const id = pathId(path)
  const label = field === '' ? '""' : field

  const onKeyDown = (event: React.KeyboardEvent<HTMLButtonElement>) => {
    if (event.key === 'ArrowRight' || event.key === 'ArrowLeft') {
      event.preventDefault()
      setExpanded(event.key === 'ArrowRight')
    } else if (event.key === 'ArrowUp' || event.key === 'ArrowDown') {
      event.preventDefault()
      moveExpander(event.currentTarget, event.key === 'ArrowUp' ? -1 : 1)
    } else if (event.key === 'Home' || event.key === 'End') {
      event.preventDefault()
      moveExpander(event.currentTarget, event.key === 'Home' ? 'first' : 'last')
    }
  }

  return <li className={css.node} role="treeitem" aria-expanded={expandable ? expanded : undefined}>
    <div className={css.row} data-json-path={pathText(path)}>
      {expandable
        ? <button
          type="button"
          className={css.expander}
          data-json-expander=""
          aria-label={t(expanded ? 'details.json.collapseNode' : 'details.json.expandNode', { path: pathText(path) })}
          aria-expanded={expanded}
          aria-controls={expanded ? groupId : undefined}
          tabIndex={activeId === id ? 0 : -1}
          onFocus={() => onActive(id)}
          onClick={() => setExpanded(current => !current)}
          onKeyDown={onKeyDown}
        ><span className={expanded ? css.chevronOpen : css.chevron} /></button>
        : <span className={css.expanderSpacer} />}
      <span className={css.key}>{label}:</span>
      {container ? preview(value, 0) : primitive(value)}
      {!last && <span className={css.punctuation}>,</span>}
    </div>
    {expanded && container && <ul id={groupId} className={css.children} role="group">
      {values.map(([key, item], index) => <JsonNode
        key={key}
        field={key}
        value={item}
        path={[...path, Array.isArray(value) ? index : key]}
        last={index === values.length - 1}
        activeId={activeId}
        onActive={onActive}
        t={t}
      />)}
    </ul>}
  </li>
}

export function JsonTree({ value, label, t }: {
  value: Record<string, unknown> | unknown[]
  label: string
  t: Translate<'chat'>
}) {
  const values = entriesOf(value)
  const firstExpandable = values.findIndex(([, item]) => isContainer(item) && entriesOf(item).length > 0)
  const first = values[firstExpandable]
  const initialId = first ? pathId([Array.isArray(value) ? firstExpandable : first[0]]) : null
  const [activeId, setActiveId] = React.useState<string | null>(initialId)
  const [open, close] = brackets(value)

  return <div
    className={css.treeViewport}
    data-json-tree=""
    tabIndex={0}
    aria-label={t('details.json.treeLabel', { label })}
  >
    <div className={css.treeBody} role="tree" aria-label={t('details.json.treeLabel', { label })}>
      <div className={css.rootBracket} aria-hidden="true">{open}</div>
      <ul className={css.rootChildren} role="group">
        {values.map(([key, item], index) => <JsonNode
          key={key}
          field={key}
          value={item}
          path={[Array.isArray(value) ? index : key]}
          last={index === values.length - 1}
          activeId={activeId}
          onActive={setActiveId}
          t={t}
        />)}
      </ul>
      <div className={css.rootBracket} aria-hidden="true">{close}</div>
    </div>
  </div>
}

export function JsonInspector({ value, label, t }: {
  value: unknown
  label: string
  t: Translate<'chat'>
}) {
  const structured = isContainer(value)
  const text = rawText(value)
  const timer = React.useRef<ReturnType<typeof setTimeout> | null>(null)
  const [showRaw, setShowRaw] = React.useState(false)
  const [copyState, setCopyState] = React.useState<'idle' | 'copied' | 'failed'>('idle')

  // The parent deliberately keys inspectors by the selected record. Streaming
  // updates replace `value` while that record is still selected, so resetting
  // local view state on every value identity change makes the raw/tree toggle
  // unusable during a live response.
  React.useEffect(() => () => {
    if (timer.current) clearTimeout(timer.current)
  }, [])

  const copy = async () => {
    try {
      await copyText(text)
      setCopyState('copied')
    } catch {
      setCopyState('failed')
    }
    if (timer.current) clearTimeout(timer.current)
    timer.current = setTimeout(() => setCopyState('idle'), 1_500)
  }

  const copyLabel = copyState === 'copied' ? t('details.json.copied')
    : copyState === 'failed' ? t('details.json.copyFailed') : t('details.json.copyFull')

  return <div className={css.inspector} data-json-inspector="" data-json-mode={showRaw || !structured ? 'raw' : 'tree'}>
    <div className={css.toolbar}>
      {structured && <button
        type="button"
        className={css.action}
        aria-pressed={showRaw}
        onClick={() => setShowRaw(current => !current)}
      >{showRaw ? <Braces /> : <FileJson2 />}<span>{t(showRaw ? 'details.json.showTree' : 'details.json.showRaw')}</span></button>}
      <button type="button" className={css.action} data-state={copyState} aria-label={copyLabel} onClick={() => void copy()}>
        {copyState === 'copied' ? <Check /> : <Copy />}<span>{copyLabel}</span>
      </button>
    </div>
    {structured && !showRaw
      ? <JsonTree value={value} label={label} t={t} />
      : <pre className={css.raw} data-json-raw="" tabIndex={0}>{text}</pre>}
  </div>
}
