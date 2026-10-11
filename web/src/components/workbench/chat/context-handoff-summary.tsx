import { handoffFields, type ContextHandoff } from '@/domain/context-handoff'
import type { Translate } from '@/i18n/runtime'

const labels = {
  constraints: 'handoff.constraints', decisions: 'handoff.decisions', completed: 'handoff.completed', pending: 'handoff.pending',
  files: 'handoff.files', verification: 'handoff.verification', risks: 'handoff.risks',
} as const

export function ContextHandoffSummary({ handoff, t }: { handoff: ContextHandoff; t: Translate<'chat'> }) {
  return <div className="grid min-w-0 gap-4 text-xs leading-relaxed [overflow-wrap:anywhere]" data-context-handoff="">
    <section><h4 className="mb-1 font-medium">{t('handoff.objective')}</h4><p className="whitespace-pre-wrap">{handoff.objective}</p></section>
    {handoffFields.filter(key => handoff[key].length).map(key => <section key={key} data-handoff-section={key}>
      <h4 className="mb-1 font-medium">{t(labels[key])}</h4>
      <ul className="list-disc space-y-1 pl-4">{handoff[key].map((item, index) => <li key={index} className="whitespace-pre-wrap">{item}</li>)}</ul>
    </section>)}
  </div>
}
