import * as React from 'react'
import { updateTrajectoryFollow, type TrajectoryFollowState } from '@/domain/trajectory-follow'

const persistedFollow = new Map<string, boolean>()

export function useTrajectoryTail(sessionId: string, revision: number) {
  const rootRef = React.useRef<HTMLDivElement>(null)
  const stateRef = React.useRef<TrajectoryFollowState>({ pinned: persistedFollow.get(sessionId) ?? false, observedScrollTop: 0 })
  const scrollRef = React.useRef<HTMLElement | null>(null)
  const touchingRef = React.useRef(false)

  const scrollToTail = React.useCallback(() => {
    const scroller = scrollRef.current
    if (!scroller || !stateRef.current.pinned || touchingRef.current) return
    scroller.scrollTop = scroller.scrollHeight
    stateRef.current.observedScrollTop = scroller.scrollTop
  }, [])

  React.useLayoutEffect(() => {
    const root = rootRef.current
    const scroller = root?.closest<HTMLElement>('.conversation-scroll') ?? null
    scrollRef.current = scroller
    stateRef.current = {
      pinned: persistedFollow.get(sessionId) ?? false,
      observedScrollTop: scroller?.scrollTop ?? 0,
    }
    if (!root || !scroller) return

    touchingRef.current = false
    let readerIntent = false
    let intentFrame = 0
    let followFrame = 0
    const clearIntentSoon = () => {
      cancelAnimationFrame(intentFrame)
      intentFrame = requestAnimationFrame(() => { readerIntent = false })
    }
    const markIntent = () => {
      readerIntent = true
      clearIntentSoon()
    }
    const onTouchStart = () => {
      touchingRef.current = true
      markIntent()
      cancelAnimationFrame(followFrame)
    }
    const onTouchEnd = () => { touchingRef.current = false }
    const onPointerDown = (event: PointerEvent) => {
      const bounds = scroller.getBoundingClientRect()
      if (event.clientX >= bounds.right - 18) markIntent()
    }
    const onKeyDown = (event: KeyboardEvent) => {
      if (['ArrowUp', 'ArrowDown', 'PageUp', 'PageDown', 'Home', 'End', ' '].includes(event.key)) markIntent()
    }
    const scheduleFollow = () => {
      cancelAnimationFrame(followFrame)
      if (!touchingRef.current) followFrame = requestAnimationFrame(scrollToTail)
    }
    const onScroll = () => {
      const decision = updateTrajectoryFollow(stateRef.current, {
        scrollTop: scroller.scrollTop,
        scrollHeight: scroller.scrollHeight,
        clientHeight: scroller.clientHeight,
        readerIntent: touchingRef.current || readerIntent,
      })
      stateRef.current = decision
      persistedFollow.set(sessionId, decision.pinned)
      readerIntent = false
      if (decision.followTail) scheduleFollow()
    }

    scroller.addEventListener('scroll', onScroll)
    scroller.addEventListener('wheel', markIntent, { passive: true })
    scroller.addEventListener('touchstart', onTouchStart, { passive: true })
    scroller.addEventListener('touchend', onTouchEnd, { passive: true })
    scroller.addEventListener('touchcancel', onTouchEnd, { passive: true })
    scroller.addEventListener('pointerdown', onPointerDown)
    scroller.addEventListener('keydown', onKeyDown)
    const observer = new ResizeObserver(() => {
      if (stateRef.current.pinned) scheduleFollow()
    })
    observer.observe(root)

    let secondFrame = 0
    const firstFrame = requestAnimationFrame(() => {
      secondFrame = requestAnimationFrame(scrollToTail)
    })
    return () => {
      persistedFollow.set(sessionId, stateRef.current.pinned)
      cancelAnimationFrame(firstFrame)
      cancelAnimationFrame(secondFrame)
      cancelAnimationFrame(intentFrame)
      cancelAnimationFrame(followFrame)
      observer.disconnect()
      scroller.removeEventListener('scroll', onScroll)
      scroller.removeEventListener('wheel', markIntent)
      scroller.removeEventListener('touchstart', onTouchStart)
      scroller.removeEventListener('touchend', onTouchEnd)
      scroller.removeEventListener('touchcancel', onTouchEnd)
      scroller.removeEventListener('pointerdown', onPointerDown)
      scroller.removeEventListener('keydown', onKeyDown)
      scrollRef.current = null
    }
  }, [scrollToTail, sessionId])

  React.useLayoutEffect(() => {
    if (!stateRef.current.pinned) return
    const frame = requestAnimationFrame(scrollToTail)
    return () => cancelAnimationFrame(frame)
  }, [revision, scrollToTail])

  return rootRef
}
