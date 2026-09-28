import * as React from 'react'
import { Check, ChevronDown, ChevronUp } from 'lucide-react'
import { Select as SelectPrimitive } from 'radix-ui'
import { cn } from '@/lib/utils'
import { usePathname } from '@/app/navigation'

export function ChoiceSelect({ value, onValueChange, options, placeholder, disabled, label, className, name, required, ...props }: Omit<React.ComponentProps<typeof SelectPrimitive.Trigger>, 'value' | 'defaultValue' | 'onChange'> & {
  value: string; onValueChange(value: string): void; placeholder?: string; label?: string; name?: string; required?: boolean
  options: { value: string; label: string; description?: string; disabled?: boolean }[]
}) {
  const emptyItem = React.useId()
  const pathname = usePathname()
  const empty = options.find(option => option.value === '')
  return <SelectPrimitive.Root key={pathname} value={value} onValueChange={next => { if (next) onValueChange(next === emptyItem ? '' : next) }} disabled={disabled} name={name} required={required}>
    <SelectPrimitive.Trigger aria-label={label} data-slot="select" data-choice-value={value} className={cn('flex min-h-10 w-full min-w-0 items-center justify-between gap-3 rounded-lg border border-input bg-[var(--surface-layer-3)] px-3 py-2 text-left text-sm outline-none transition-colors hover:border-[var(--border-strong)] focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/30 disabled:cursor-not-allowed disabled:opacity-50 [&>span:first-child]:truncate', className)} {...props}>
      <SelectPrimitive.Value placeholder={empty?.label ?? placeholder} /><SelectPrimitive.Icon><ChevronDown className="size-4 shrink-0 text-muted-foreground" /></SelectPrimitive.Icon>
    </SelectPrimitive.Trigger>
    <SelectPrimitive.Portal><SelectPrimitive.Content position="popper" align="start" sideOffset={5} collisionPadding={12} className="z-[200] max-h-[min(360px,var(--radix-select-content-available-height))] min-w-[var(--radix-select-trigger-width)] max-w-[min(480px,calc(100vw-24px))] overflow-hidden rounded-lg border border-[var(--border-subtle)] bg-popover p-1 text-popover-foreground shadow-[var(--shadow-lv2)]" data-ternilo-dismiss-layer="" data-choice-menu="">
      <SelectPrimitive.ScrollUpButton className="flex justify-center py-1"><ChevronUp className="size-4" /></SelectPrimitive.ScrollUpButton>
      <SelectPrimitive.Viewport>{options.map(option => <SelectPrimitive.Item key={option.value} value={option.value || emptyItem} data-choice-option={option.value} data-selected={value === option.value || undefined} aria-selected={value === option.value} disabled={option.disabled} textValue={option.label} className="relative flex min-h-10 cursor-default items-center gap-3 rounded-md py-2 pr-8 pl-2 text-sm outline-none data-[selected]:bg-accent/60 data-[highlighted]:bg-accent data-[highlighted]:text-accent-foreground data-[disabled]:opacity-45">
        <div className="min-w-0 break-words"><SelectPrimitive.ItemText>{option.label}</SelectPrimitive.ItemText>{option.description && <p className="mt-1 text-xs text-muted-foreground">{option.description}</p>}</div>
        {value === option.value && <Check className="absolute right-2 size-4" />}
      </SelectPrimitive.Item>)}</SelectPrimitive.Viewport>
      <SelectPrimitive.ScrollDownButton className="flex justify-center py-1"><ChevronDown className="size-4" /></SelectPrimitive.ScrollDownButton>
    </SelectPrimitive.Content></SelectPrimitive.Portal>
  </SelectPrimitive.Root>
}
