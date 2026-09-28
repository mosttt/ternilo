import * as React from 'react'
import { cn } from '@/lib/utils'
import { ChoiceSelect } from './choice-select'

export function Input({ className, ...props }: React.ComponentProps<'input'>) {
  return <input className={cn('flex h-10 w-full rounded-lg border border-input bg-card px-3 py-1 text-sm outline-none transition-[border,box-shadow] placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/30 disabled:cursor-not-allowed disabled:opacity-50', className)} {...props} />
}

export function Textarea({ className, ...props }: React.ComponentProps<'textarea'>) {
  return <textarea className={cn('flex min-h-24 w-full resize-y rounded-lg border border-input bg-card px-3 py-2 text-sm outline-none transition-[border,box-shadow] placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/30 disabled:cursor-not-allowed disabled:opacity-50', className)} {...props} />
}

function optionText(children: React.ReactNode): string {
  return React.Children.toArray(children).map(child => typeof child === 'string' || typeof child === 'number' ? String(child) : React.isValidElement<{ children?: React.ReactNode }>(child) ? optionText(child.props.children) : '').join('')
}

export function Select({ children, value, defaultValue, onValueChange, ...props }: Omit<React.ComponentProps<typeof ChoiceSelect>, 'options' | 'value' | 'onValueChange'> & {
  children: React.ReactNode; value?: string | number; defaultValue?: string | number; onValueChange?(value: string): void
}) {
  const options = React.Children.toArray(children).filter((child): child is React.ReactElement<React.ComponentProps<'option'>> => React.isValidElement(child) && child.type === 'option')
    .map(option => ({ value: String(option.props.value ?? optionText(option.props.children)), label: optionText(option.props.children), disabled: option.props.disabled }))
  const [uncontrolled, setUncontrolled] = React.useState(() => String(defaultValue ?? options.find(option => !option.disabled)?.value ?? ''))
  return <ChoiceSelect {...props} options={options} value={value === undefined ? uncontrolled : String(value)} onValueChange={next => { if (value === undefined) setUncontrolled(next); onValueChange?.(next) }} />
}

export function Label({ className, ...props }: React.ComponentProps<'label'>) {
  return <label className={cn('text-sm font-medium leading-none peer-disabled:cursor-not-allowed peer-disabled:opacity-70', className)} {...props} />
}

export function Field({ className, ...props }: React.ComponentProps<'div'>) {
  return <div className={cn('grid content-start gap-2.5', className)} {...props} />
}

export function FieldDescription({ className, ...props }: React.ComponentProps<'p'>) {
  return <p className={cn('text-xs leading-relaxed text-muted-foreground', className)} {...props} />
}
