import * as React from 'react'
import { createPortal } from 'react-dom'
import type { ChatTurn } from '@/domain/chat-turns'
import type { Translate } from '@/i18n/runtime'
import css from './turn-navigator.module.css'

function itemAtPointer(turns: ChatTurn[], rail: HTMLElement, scrollTop: number, clientY: number) {
  const bounds = rail.getBoundingClientRect()
  const inset = 6
  const index = Math.max(0, Math.min(turns.length - 1, Math.round((clientY - bounds.top + scrollTop - inset) / 10)))
  return turns[index]
}

export function TurnNavigator({ turns, rootRef, onReaderNavigate, t }: {
  turns: ChatTurn[]
  rootRef: React.RefObject<HTMLDivElement | null>
  onReaderNavigate(): void
  t: Translate<'chat'>
}) {
  const [activeTurn, setActiveTurn] = React.useState<number | null>(null)
  const [previewTurn, setPreviewTurn] = React.useState<number | null>(null)
  const [frame, setFrame] = React.useState<{ host: HTMLElement; top: number; height: number } | null>(null)
  const [railScroll, setRailScroll] = React.useState(0)
  const railRef = React.useRef<HTMLDivElement>(null)
  const pointerInside = React.useRef(false)
  const previewId = React.useId()
  const naturalHeight = (turns.length - 1) * 10 + 12
  const navigate = React.useCallback((turn: ChatTurn) => {
    const root = rootRef.current
    const scroller = root?.closest<HTMLElement>('.conversation-scroll')
    const row = [...(root?.querySelectorAll<HTMLElement>('[data-chat-run-id]') ?? [])].find(candidate => candidate.dataset.chatRunId === turn.runId)
    if (!root || !scroller || !row) return
    onReaderNavigate()
    scroller.scrollTop += row.getBoundingClientRect().top - scroller.getBoundingClientRect().top - 24
    setActiveTurn(turn.number)
  }, [onReaderNavigate, rootRef])

  React.useEffect(() => {
    const root = rootRef.current
    const scroller = root?.closest<HTMLElement>('.conversation-scroll')
    const host = root?.closest<HTMLElement>('.conversation-column')
    if (!root || !scroller || !host) return
    const composer = scroller.querySelector<HTMLElement>('[data-composer-seat]')
    const measure = () => {
      const top = scroller.getBoundingClientRect().top - host.getBoundingClientRect().top
      const height = Math.max(0, scroller.clientHeight - (composer?.offsetHeight ?? 0))
      setFrame(current => current?.host === host && current.top === top && current.height === height ? current : { host, top, height })
    }
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(scroller)
    if (composer) observer.observe(composer)
    return () => observer.disconnect()
  }, [rootRef])

  React.useEffect(() => {
    const root = rootRef.current
    const scroller = root?.closest<HTMLElement>('.conversation-scroll')
    if (!root || !scroller || turns.length < 2) return
    let frame = 0
    const update = () => {
      cancelAnimationFrame(frame)
      frame = requestAnimationFrame(() => {
        const top = scroller.getBoundingClientRect().top + 72
        const rows = [...root.querySelectorAll<HTMLElement>('[data-chat-turn]')]
        let current: HTMLElement | undefined = rows[0]
        for (const row of rows) {
          if (row.getBoundingClientRect().top > top) break
          current = row
        }
        if (scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight <= 1) current = rows.at(-1)
        const value = current ? Number(current.dataset.chatTurn) : null
        if (value !== null && Number.isFinite(value)) setActiveTurn(value)
      })
    }
    update()
    scroller.addEventListener('scroll', update, { passive: true })
    const observer = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(update)
    observer?.observe(root)
    observer?.observe(scroller)
    window.addEventListener('resize', update)
    return () => { cancelAnimationFrame(frame); observer?.disconnect(); scroller.removeEventListener('scroll', update); window.removeEventListener('resize', update) }
  }, [rootRef, turns])

  React.useEffect(() => {
    const rail = railRef.current
    const index = turns.findIndex(turn => turn.number === activeTurn)
    if (!rail || index < 0 || pointerInside.current) return
    const top = index * 10 + 6
    if (top < rail.scrollTop + 6 || top > rail.scrollTop + rail.clientHeight - 6) {
      rail.scrollTop = Math.max(0, top - rail.clientHeight / 2)
    }
  }, [activeTurn, turns])

  if (turns.length < 2) return null
  const preview = turns.find(turn => turn.number === previewTurn)
  const position = (turn: ChatTurn) => turns.length <= 1 ? 0 : turns.indexOf(turn) / (turns.length - 1) * 100
  const height = Math.min(naturalHeight, 420, Math.max(0, (frame?.height ?? 484) - 64))
  const content = <div className={css.layer} data-turn-navigator=""><div className={css.slot} style={{ top: frame ? frame.top + frame.height / 2 : undefined }}>
    <nav
      className={css.rail}
      style={{ height }}
      aria-label={t('chat.turnNavigation.label')}
      onClick={event => { const turn = itemAtPointer(turns, event.currentTarget, railRef.current?.scrollTop ?? 0, event.clientY); if (turn) navigate(turn) }}
      onPointerMove={event => setPreviewTurn(itemAtPointer(turns, event.currentTarget, railRef.current?.scrollTop ?? 0, event.clientY)?.number ?? null)}
      onPointerEnter={() => { pointerInside.current = true }}
      onPointerLeave={() => { pointerInside.current = false; setPreviewTurn(null) }}
    >
      <div ref={railRef} className={css.scroller} onScroll={event => setRailScroll(event.currentTarget.scrollTop)}>
      <div className={css.marks} style={{ height: naturalHeight }}>
      {turns.map((turn, index) => <button
        type="button"
        className={css.mark}
        data-turn-navigator-mark=""
        data-turn-position={position(turn)}
        data-active={turn.number === activeTurn || undefined}
        data-preview={turn.number === previewTurn || undefined}
        style={{ top: index * 10 + 6 }}
        aria-label={t('chat.turnNavigation.jump', { turn: turn.number })}
        aria-current={turn.number === activeTurn ? 'true' : undefined}
        aria-describedby={turn.number === previewTurn ? previewId : undefined}
        key={turn.runId}
        onClick={event => { event.stopPropagation(); navigate(turn) }}
        onFocus={() => setPreviewTurn(turn.number)}
        onBlur={() => setPreviewTurn(null)}
      />)}
      </div>
      </div>
      {preview && <div id={previewId} className={css.preview} data-turn-navigator-preview="" role="tooltip" style={{ top: Math.max(0, Math.min(height - 60, turns.indexOf(preview) * 10 + 6 - railScroll - 30)) }}>
        <strong>{preview.prompt || t('chat.turnNavigation.turn', { turn: preview.number })}</strong>
        {preview.response && <span>{preview.response}</span>}
      </div>}
    </nav>
  </div></div>
  return frame ? createPortal(content, frame.host) : content
}
