import { ChevronDown, Folder, FolderOpen } from 'lucide-react'
import { useId } from 'react'
import { BrandMark } from '@/components/ui/brand-mark'
import type { RefObject } from 'react'
import type { Translate } from '@/i18n/runtime'
import css from './hero-shell.module.css'

type HeroTranslate = Translate<'conversation'>

export function workspaceLabel(path: string): string {
  const normalized = path.replace(/[\\/]+$/, '')
  const label = normalized.split(/[\\/]/).filter(Boolean).at(-1)
  return label || path
}

export function WorkspaceChip({
  buttonRef,
  label,
  menuOpen = false,
  onClick,
  t,
}: {
  buttonRef?: RefObject<HTMLButtonElement | null>
  label?: string
  menuOpen?: boolean
  onClick?: () => void
  t: HeroTranslate
}) {
  const Icon = label === undefined ? Folder : FolderOpen
  return (
    <button
      ref={buttonRef}
      type="button"
      className={css.workspace}
      aria-label={label === undefined ? t('hero.chooseWorkspace') : t('hero.switchWorkspace')}
      aria-haspopup="dialog"
      aria-expanded={menuOpen}
      onClick={onClick}
    >
      <Icon className={css.folder} size={16} />
      <span className={css.workspaceLabel}>{label ?? t('hero.chooseWorkspace')}</span>
      <ChevronDown className={css.chevron} size={12} />
    </button>
  )
}

export function HeroGlow({ className }: { className?: string }) {
  const glowFilterId = `ternilo-empty-glow-${useId().replace(/:/g, '')}`
  return (
    <svg className={className} viewBox="0 0 1051 468" fill="none" aria-hidden="true">
      <defs>
        <filter
          id={glowFilterId}
          x="0"
          y="0"
          width="1051"
          height="468"
          filterUnits="userSpaceOnUse"
          colorInterpolationFilters="sRGB"
        >
          <feFlood floodOpacity="0" result="BackgroundImageFix" />
          <feBlend mode="normal" in="SourceGraphic" in2="BackgroundImageFix" result="shape" />
          <feGaussianBlur stdDeviation="50" result="effect1_foregroundBlur" />
        </filter>
      </defs>
      <g filter={`url(#${glowFilterId})`}>
        <ellipse cx="525.5" cy="234" rx="425.5" ry="134" fill="var(--accent-color)" fillOpacity="0.035" />
      </g>
    </svg>
  )
}

export function HeroShell({ t }: { t: HeroTranslate }) {
  return (
    <div className={css.root} data-new-session-hero="">
      <div className={css.stack}>
        <div className={css.headline}>
          <span className={css.markHitbox} aria-hidden="true">
            <span className={css.mark}><BrandMark /></span>
          </span>
          <span className={css.headlineText}>{t('hero.headline')}</span>
        </div>
      </div>
    </div>
  )
}
