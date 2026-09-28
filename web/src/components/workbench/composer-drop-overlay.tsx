import * as React from 'react'
import { createPortal } from 'react-dom'
import { ImagePlus } from 'lucide-react'
import type { Translate } from '@/i18n/runtime'
import css from './composer-drop-overlay.module.css'

function carriesFiles(event: DragEvent): boolean {
  return [...event.dataTransfer?.types ?? []].includes('Files')
}

export function ComposerDropOverlay({
  disabled,
  onFiles,
  t,
}: {
  disabled: boolean
  onFiles(files: File[]): void
  t: Translate<'conversation'>
}) {
  const [visible, setVisible] = React.useState(false)
  const depth = React.useRef(0)
  const onFilesRef = React.useRef(onFiles)
  onFilesRef.current = onFiles

  React.useEffect(() => {
    const enter = (event: DragEvent) => {
      if (!carriesFiles(event)) return
      event.preventDefault()
      depth.current += 1
      setVisible(true)
    }
    const over = (event: DragEvent) => {
      if (!carriesFiles(event)) return
      event.preventDefault()
      if (event.dataTransfer) event.dataTransfer.dropEffect = disabled ? 'none' : 'copy'
    }
    const leave = (event: DragEvent) => {
      if (!carriesFiles(event)) return
      depth.current = Math.max(0, depth.current - 1)
      if (depth.current === 0) setVisible(false)
    }
    const drop = (event: DragEvent) => {
      if (!carriesFiles(event)) return
      event.preventDefault()
      depth.current = 0
      setVisible(false)
      if (!disabled) onFilesRef.current([...event.dataTransfer?.files ?? []])
    }
    const reset = () => {
      depth.current = 0
      setVisible(false)
    }
    document.addEventListener('dragenter', enter)
    document.addEventListener('dragover', over)
    document.addEventListener('dragleave', leave)
    document.addEventListener('drop', drop)
    window.addEventListener('blur', reset)
    return () => {
      document.removeEventListener('dragenter', enter)
      document.removeEventListener('dragover', over)
      document.removeEventListener('dragleave', leave)
      document.removeEventListener('drop', drop)
      window.removeEventListener('blur', reset)
    }
  }, [disabled])

  if (!visible) return null
  return createPortal(
    <div className={css.mask} role="status" data-drop-overlay="">
      <div className={css.card} data-disabled={disabled || undefined}>
        <ImagePlus aria-hidden="true" />
        <strong>{disabled ? t('drop.disabled') : t('drop.title')}</strong>
        {!disabled && <span>{t('drop.description')}</span>}
      </div>
    </div>,
    document.body,
  )
}
