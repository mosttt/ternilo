import * as React from 'react'

interface Props {
  children: React.ReactNode
  sessionId?: string
  view: string
  enabled: boolean
  loading: boolean
  statsReady: boolean
  firstSeq?: number
  scrollRef: React.RefObject<HTMLDivElement | null>
  following(): boolean
  onRestore(): void
}

type Snapshot = { tail: true } | {
  tail: false
  element: HTMLElement
  messageSeq?: string
  blockIndex: number
  top: number
}

/** Read the old DOM immediately before a history page changes the transcript. */
export class HistoryScrollPreserver extends React.Component<Props, object, Snapshot | null> {
  private restoredTop: number | undefined
  private roundingError = 0

  getSnapshotBeforeUpdate(previous: Props): Snapshot | null {
    const { sessionId, view, enabled, loading, statsReady, firstSeq, scrollRef, following } = this.props
    if (!enabled || previous.sessionId !== sessionId || previous.view !== view
      || previous.firstSeq === undefined
      || (!(loading || previous.loading) && (previous.statsReady || !statsReady))
      || (previous.firstSeq === firstSeq && previous.loading === loading && previous.statsReady === statsReady)) return null
    const scroll = scrollRef.current
    if (!scroll) return null
    if (scroll.scrollTop !== this.restoredTop) this.roundingError = 0
    if (following() && scroll.scrollHeight - scroll.clientHeight - scroll.scrollTop <= 1) return { tail: true }
    const viewport = scroll.getBoundingClientRect()
    const visibleBottom = scroll.querySelector('[data-composer-seat]')?.getBoundingClientRect().top ?? viewport.bottom
    for (const selector of ['[data-chat-message-seq] .markdown-body > *', '[data-chat-message-seq]', '[data-chat-anchor-key]']) {
      const element = [...scroll.querySelectorAll<HTMLElement>(selector)].find(row => {
        const rect = row.getBoundingClientRect()
        return rect.height > 0 && rect.bottom > viewport.top && rect.top < visibleBottom
      })
      if (!element) continue
      const message = element.closest<HTMLElement>('[data-chat-message-seq]')
      return {
        tail: false, element, messageSeq: message?.dataset.chatMessageSeq,
        blockIndex: message ? [...message.querySelectorAll('.markdown-body > *')].indexOf(element) : -1,
        top: element.getBoundingClientRect().top - viewport.top,
      }
    }
    return null
  }

  componentDidUpdate(_previous: Props, _state: object, snapshot: Snapshot | null) {
    const scroll = this.props.scrollRef.current
    if (!snapshot || !scroll) return
    if (snapshot.tail) {
      scroll.scrollTop = scroll.scrollHeight
      this.roundingError = 0
    } else {
      const message = snapshot.messageSeq === undefined ? null
        : [...scroll.querySelectorAll<HTMLElement>('[data-chat-message-seq]')].find(row => row.dataset.chatMessageSeq === snapshot.messageSeq)
      const element = snapshot.element.isConnected ? snapshot.element
        : snapshot.blockIndex >= 0 ? message?.querySelectorAll<HTMLElement>('.markdown-body > *')[snapshot.blockIndex] : message
      if (element && element.getBoundingClientRect().height > 0) {
        const target = scroll.scrollTop + element.getBoundingClientRect().top - scroll.getBoundingClientRect().top - snapshot.top + this.roundingError
        scroll.scrollTop = target
        // Preserve fractional offsets across repeated pages despite integer scrollTop rounding.
        this.roundingError = target - scroll.scrollTop
      }
    }
    this.restoredTop = scroll.scrollTop
    this.props.onRestore()
  }

  render() { return this.props.children }
}
