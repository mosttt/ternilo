import type { ComponentProps } from 'react'

export function BrandMark(props: ComponentProps<'svg'>) {
  return (
    <svg viewBox="0 0 32 32" fill="none" aria-hidden="true" {...props}>
      <path d="M9 6h8a7 7 0 0 1 0 14h-2" stroke="currentColor" strokeWidth="3.2" strokeLinecap="round" />
      <path d="M23 26h-8a7 7 0 0 1 0-14h2" stroke="currentColor" strokeWidth="3.2" strokeLinecap="round" />
      <path d="m11 21 10-10" stroke="currentColor" strokeWidth="3.2" strokeLinecap="round" />
    </svg>
  )
}
