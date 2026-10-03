import * as React from 'react'

/** Ignore delayed hover requests after the pointer and focus have left the row. */
export function useSidebarHover() {
  const [open, setOpen] = React.useState(false)
  const target = React.useRef<HTMLDivElement>(null)
  const pointer = React.useRef(false)
  const change = (next: boolean) => {
    if (!next || pointer.current || target.current?.contains(document.activeElement)) setOpen(next)
  }
  return { open, change, trigger: {
    ref: target,
    onPointerEnter: (event: React.PointerEvent) => { if (event.pointerType !== 'touch') pointer.current = true },
    onPointerLeave: () => { pointer.current = false },
  } }
}
