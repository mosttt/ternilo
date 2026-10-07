import * as React from 'react'
import { Switch as SwitchPrimitive } from 'radix-ui'
import { cn } from '@/lib/utils'

export function Switch({ className, ...props }: React.ComponentProps<typeof SwitchPrimitive.Root>) {
  return (
    <SwitchPrimitive.Root data-slot="switch" className={cn('peer group/switch inline-flex h-10 w-10 shrink-0 cursor-pointer items-center justify-center rounded-md outline-none focus-visible:ring-[3px] focus-visible:ring-ring/45 disabled:cursor-not-allowed disabled:opacity-50', className)} {...props}>
      <span data-slot="switch-track" className="pointer-events-none inline-flex h-5 w-9 shrink-0 items-center rounded-full border-2 border-transparent bg-input shadow-xs transition-colors group-data-[state=checked]/switch:bg-primary">
        <SwitchPrimitive.Thumb className="block size-4 rounded-full bg-background shadow-sm ring-0 transition-transform data-[state=checked]:translate-x-4 data-[state=unchecked]:translate-x-0" />
      </span>
    </SwitchPrimitive.Root>
  )
}
