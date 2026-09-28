import * as React from 'react'
import { LoaderCircle } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { cn } from '@/lib/utils'
import styles from './settings-layout.module.css'

export function SectionHeader({
  title,
  description,
  action,
}: {
  title: string
  description: string
  action?: React.ReactNode
}) {
  return (
    <header className="mb-7 flex flex-wrap items-start justify-between gap-4">
      <div className="min-w-0">
        <h2 className="break-words text-xl font-semibold tracking-tight sm:text-2xl">{title}</h2>
        <p className="mt-2 max-w-2xl text-sm leading-relaxed text-muted-foreground">{description}</p>
      </div>
      {action}
    </header>
  )
}

export function GroupHeader({ title, description }: { title: string; description?: string }) {
  return (
    <div className="mb-4">
      <h3 className="text-sm font-semibold">{title}</h3>
      {description ? <p className="mt-1 text-xs leading-relaxed text-muted-foreground">{description}</p> : null}
    </div>
  )
}

export function SettingRow({
  title,
  description,
  children,
}: {
  title: string
  description?: string
  children: React.ReactNode
}) {
  return (
    <div className="flex flex-col gap-3 border-b py-4 last:border-b-0 sm:flex-row sm:items-center">
      <div className="min-w-0 flex-1">
        <div className="text-sm font-medium">{title}</div>
        {description ? <p className="mt-1 text-xs leading-relaxed text-muted-foreground">{description}</p> : null}
      </div>
      <div className="min-w-0 shrink-0 [&>[data-slot=select]]:w-full sm:[&>[data-slot=select]]:min-w-40">{children}</div>
    </div>
  )
}

export interface SettingsTab {
  id: string
  label: string
}

export function SettingsTabs({
  label,
  tabs,
  active,
  onChange,
}: {
  label: string
  tabs: readonly SettingsTab[]
  active: string
  onChange(id: string): void
}) {
  const refs = React.useRef<Array<HTMLButtonElement | null>>([])
  return (
    <div className={styles.tabs} role="tablist" aria-label={label}>
      {tabs.map((tab, index) => (
        <button
          ref={(element) => { refs.current[index] = element }}
          type="button"
          className={styles.tab}
          role="tab"
          id={`settings-tab-${tab.id}`}
          aria-controls={`settings-panel-${tab.id}`}
          aria-selected={tab.id === active}
          tabIndex={tab.id === active ? 0 : -1}
          key={tab.id}
          onClick={() => onChange(tab.id)}
          onKeyDown={(event) => {
            let next = index
            if (event.key === 'ArrowRight') next = (index + 1) % tabs.length
            else if (event.key === 'ArrowLeft') next = (index - 1 + tabs.length) % tabs.length
            else if (event.key === 'Home') next = 0
            else if (event.key === 'End') next = tabs.length - 1
            else return
            event.preventDefault()
            const item = tabs[next]
            if (!item) return
            onChange(item.id)
            refs.current[next]?.focus()
          }}
        >
          {tab.label}
        </button>
      ))}
    </div>
  )
}

export function SettingsTabPanel({
  id,
  active,
  children,
}: {
  id: string
  active: string
  children: React.ReactNode
}) {
  return (
    <div
      id={`settings-panel-${id}`}
      role="tabpanel"
      aria-labelledby={`settings-tab-${id}`}
      hidden={id !== active}
      className="pt-5"
    >
      {children}
    </div>
  )
}

export function ActionDialog({
  open,
  title,
  description,
  cancelLabel,
  confirmLabel,
  busyLabel,
  busy = false,
  destructive = false,
  error,
  children,
  onOpenChange,
  onConfirm,
  onCancelWhileBusy,
}: {
  open: boolean
  title: string
  description?: string
  cancelLabel: string
  confirmLabel: string
  busyLabel?: string
  busy?: boolean
  destructive?: boolean
  error?: string
  children?: React.ReactNode
  onOpenChange(open: boolean): void
  onConfirm(): void
  onCancelWhileBusy?(): void
}) {
  return (
    <Dialog open={open} onOpenChange={(next) => { if (!busy) onOpenChange(next); else if (!next) onCancelWhileBusy?.() }}>
      <DialogContent className={cn(styles.settingsDialog, 'max-w-md')} aria-describedby={description ? undefined : ''} data-settings-dialog="">
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          {description ? <DialogDescription>{description}</DialogDescription> : null}
        </DialogHeader>
        {children}
        {error ? <p className="text-sm text-destructive" role="alert">{error}</p> : null}
        <DialogFooter>
          <Button variant="outline" disabled={busy && !onCancelWhileBusy} onClick={() => busy ? onCancelWhileBusy?.() : onOpenChange(false)}>{cancelLabel}</Button>
          <Button
            className={cn(destructive && 'text-destructive hover:text-destructive')}
            variant={destructive ? 'outline' : 'default'}
            disabled={busy}
            onClick={onConfirm}
          >
            {busy ? <LoaderCircle className="animate-spin" /> : null}
            {busy ? (busyLabel ?? confirmLabel) : confirmLabel}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
