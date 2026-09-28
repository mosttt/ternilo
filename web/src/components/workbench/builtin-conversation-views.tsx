import { FileClock, MessageSquare } from 'lucide-react'
import { registerConversationView } from '@/plugins/conversation-registry'
import { ChatView } from './chat-view'
import { TrajectoryView } from './trajectory-view'
import css from './conversation-root.module.css'

registerConversationView({
  id: 'chat',
  order: 10,
  icon: MessageSquare,
  primary: true,
  followsTail: true,
  label: t => t('view.chat'),
  openLabel: t => t('view.backToChat'),
  render: context => (
    <ChatView
      key={context.sessionId}
      sessionId={context.sessionId}
      events={context.events}
      history={context.history}
      pendingSubmissions={context.pendingSubmissions}
      projection={context.projection}
      reloadMetadata={context.reloadMetadata}
      selection={context.selection}
      onSelect={context.onSelect}
      onReaderNavigate={context.onReaderNavigate}
      onRegenerate={context.onRegenerate}
      onEdit={context.onEdit}
      regenerateDisabled={context.regenerateDisabled}
    />
  ),
})

registerConversationView({
  id: 'trajectory',
  order: 20,
  icon: FileClock,
  label: t => t('view.trajectory'),
  openLabel: t => t('view.openTrajectory'),
  contentClassName: css.trajectoryContent,
  render: context => (
    <TrajectoryView
      sessionId={context.sessionId}
      events={context.events}
      history={context.history}
      selection={context.selection}
      onSelect={context.onSelect}
    />
  ),
})
