/** Copy during a user action, including browsers without the secure clipboard API. */
export async function copyText(value: string): Promise<void> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(value)
      return
    }
  } catch {
    // A denied async API can still permit copying a user-selected DOM value.
  }
  const active = document.activeElement instanceof HTMLElement ? document.activeElement : null
  const selection = document.getSelection()
  const ranges = selection ? Array.from({ length: selection.rangeCount }, (_, index) => selection.getRangeAt(index).cloneRange()) : []
  const field = active instanceof HTMLInputElement || active instanceof HTMLTextAreaElement ? active : null
  const start = field?.selectionStart
  const end = field?.selectionEnd
  const input = document.createElement('textarea')
  input.value = value
  input.readOnly = true
  input.tabIndex = -1
  input.style.cssText = 'position:fixed;top:0;left:0;width:1px;height:1px;padding:0;border:0;opacity:0;font-size:16px'
  // Stay inside the active dialog's focus scope, including mobile dialogs.
  ;(active?.closest('[role="dialog"], [role="alertdialog"]') ?? document.body).append(input)
  try {
    input.focus({ preventScroll: true })
    input.select()
    input.setSelectionRange(0, value.length)
    if (!document.execCommand?.('copy')) throw new Error('Browser denied clipboard access')
  } finally {
    input.remove()
    active?.focus({ preventScroll: true })
    if (selection) {
      selection.removeAllRanges()
      for (const range of ranges) selection.addRange(range)
    }
    if (field && start != null && end != null) field.setSelectionRange(start, end)
  }
}
