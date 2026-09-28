import * as React from 'react'
import { ChevronRight, Circle } from 'lucide-react'
import { DropdownMenu as DropdownPrimitive } from 'radix-ui'
import { cn } from '@/lib/utils'

export function DropdownMenu({ modal = false, ...props }: React.ComponentProps<typeof DropdownPrimitive.Root>) {
  return <DropdownPrimitive.Root modal={modal} {...props} />
}
export const DropdownMenuTrigger = DropdownPrimitive.Trigger
export const DropdownMenuGroup = DropdownPrimitive.Group
export const DropdownMenuPortal = DropdownPrimitive.Portal
export const DropdownMenuSub = DropdownPrimitive.Sub
export const DropdownMenuRadioGroup = DropdownPrimitive.RadioGroup

export function DropdownMenuContent({ className, sideOffset = 6, ...props }: React.ComponentProps<typeof DropdownPrimitive.Content>) {
  return (
    <DropdownPrimitive.Portal>
      <DropdownPrimitive.Content
        data-ternilo-dismiss-layer=""
        sideOffset={sideOffset}
        className={cn('z-50 min-w-48 overflow-hidden rounded-lg border bg-popover p-1 text-popover-foreground shadow-xl outline-none data-[state=closed]:animate-out data-[state=open]:animate-in data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0 data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95', className)}
        {...props}
      />
    </DropdownPrimitive.Portal>
  )
}

export function DropdownMenuItem({ className, inset, ...props }: React.ComponentProps<typeof DropdownPrimitive.Item> & { inset?: boolean }) {
  return <DropdownPrimitive.Item data-slot="dropdown-menu-item" className={cn('relative flex cursor-default select-none items-center gap-2 rounded-md px-2 py-1.5 text-sm outline-none data-[disabled]:pointer-events-none data-[disabled]:opacity-50 data-[highlighted]:bg-accent data-[highlighted]:text-accent-foreground [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*=size-])]:size-4', inset && 'pl-8', className)} {...props} />
}

export function DropdownMenuRadioItem({ className, children, ...props }: React.ComponentProps<typeof DropdownPrimitive.RadioItem>) {
  return (
    <DropdownPrimitive.RadioItem data-slot="dropdown-menu-radio-item" className={cn('relative flex cursor-default select-none items-center rounded-md py-1.5 pl-8 pr-2 text-sm outline-none data-[highlighted]:bg-accent', className)} {...props}>
      <span className="absolute left-2 flex size-4 items-center justify-center"><DropdownPrimitive.ItemIndicator><Circle className="size-2 fill-current" /></DropdownPrimitive.ItemIndicator></span>
      {children}
    </DropdownPrimitive.RadioItem>
  )
}

export function DropdownMenuLabel({ className, inset, ...props }: React.ComponentProps<typeof DropdownPrimitive.Label> & { inset?: boolean }) {
  return <DropdownPrimitive.Label className={cn('px-2 py-1.5 text-xs font-medium text-muted-foreground', inset && 'pl-8', className)} {...props} />
}

export function DropdownMenuSeparator({ className, ...props }: React.ComponentProps<typeof DropdownPrimitive.Separator>) {
  return <DropdownPrimitive.Separator className={cn('-mx-1 my-1 h-px bg-border', className)} {...props} />
}

export function DropdownMenuSubTrigger({ className, inset, children, onPointerLeave, ...props }: React.ComponentProps<typeof DropdownPrimitive.SubTrigger> & { inset?: boolean }) {
  return <DropdownPrimitive.SubTrigger data-slot="dropdown-menu-sub-trigger" className={cn('flex cursor-default select-none items-center rounded-md px-2 py-1.5 text-sm outline-none data-[state=open]:bg-accent data-[highlighted]:bg-accent', inset && 'pl-8', className)} onPointerLeave={event => {
    onPointerLeave?.(event)
    const submenuId = event.currentTarget.getAttribute('aria-controls')
    // Moving into the submenu must not focus its parent and dismiss it.
    if (submenuId && event.relatedTarget instanceof Node && document.getElementById(submenuId)?.contains(event.relatedTarget)) event.preventDefault()
  }} {...props}>{children}<ChevronRight className="ml-auto size-4" /></DropdownPrimitive.SubTrigger>
}

export function DropdownMenuSubContent({ className, ...props }: React.ComponentProps<typeof DropdownPrimitive.SubContent>) {
  return <DropdownPrimitive.SubContent data-ternilo-dismiss-layer="" className={cn('z-50 min-w-40 overflow-hidden rounded-lg border bg-popover p-1 shadow-xl', className)} {...props} />
}
