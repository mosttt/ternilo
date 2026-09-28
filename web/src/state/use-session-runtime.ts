import * as React from 'react'
import { api } from '@/api/client'
import { live } from '@/api/live'
import { isServerAccessPaused } from '@/auth/server'
import { useTranslate } from '@/i18n/provider'
import { randomUuid } from '@/lib/random-id'
import { invalidateFileInventory } from '@/domain/file-inventory'
import { SessionController, type SessionRuntime } from './session-controller'
import { useWorkbench } from './workbench'
import { optimisticInputAuthor, useInputViewer } from './input-viewer'

export type { SessionRuntime } from './session-controller'

export function useSessionRuntime(): SessionRuntime {
  const {
    currentSessionId,
    refresh,
    notify,
    acceptLiveWorkbench,
    acceptLiveActivity,
    loading: workbenchLoading,
    authRequired,
    currentTenantId,
    accountScope,
    pauseAccess,
  } = useWorkbench()
  React.useEffect(() => live.onFrame(frame => {
    if (frame.type === 'error' && isServerAccessPaused(frame)) pauseAccess()
    if (frame.type === 'event_batch' && frame.events.some(event => ['user_message', 'turn_finished', 'turn_failed', 'turn_cancelled'].includes(event.type))) invalidateFileInventory()
  }), [pauseAccess])
  const t = useTranslate('app')
  const viewer = useInputViewer()
  const dependencies = React.useRef({ refresh, notify, acceptLiveWorkbench, acceptLiveActivity, t, viewer })
  dependencies.current = { refresh, notify, acceptLiveWorkbench, acceptLiveActivity, t, viewer }
  const [controller] = React.useState(() => new SessionController({
    api,
    live,
    acceptWorkbench: (state, revision, activity) => dependencies.current.acceptLiveWorkbench(state, revision, activity),
    acceptActivity: activity => dependencies.current.acceptLiveActivity(activity),
    refresh: () => dependencies.current.refresh(),
    notify: (message, kind) => dependencies.current.notify(message, kind),
    labels: () => ({
      metadata: [
        dependencies.current.t('metadata.stats'),
        dependencies.current.t('metadata.projection'),
        dependencies.current.t('metadata.questions'),
        dependencies.current.t('metadata.plugins'),
      ],
      skillNameError: dependencies.current.t('error.skillName'),
      runStopping: dependencies.current.t('run.stopping'),
    }),
    inputAuthor: () => optimisticInputAuthor(dependencies.current.viewer),
    randomId: randomUuid,
    now: () => Date.now(),
  }))
  const snapshot = React.useSyncExternalStore(controller.subscribe, controller.getSnapshot, controller.getSnapshot)

  React.useLayoutEffect(() => {
    controller.start()
    return () => controller.dispose()
  }, [controller])

  React.useLayoutEffect(() => {
    controller.setTarget(
      currentSessionId,
      !workbenchLoading && !authRequired,
      authRequired ? 'signed-out' : accountScope ?? currentTenantId ?? 'host',
    )
  }, [accountScope, authRequired, controller, currentSessionId, currentTenantId, workbenchLoading])

  React.useEffect(() => {
    if (workbenchLoading || authRequired) {
      live.stop()
      return
    }
    live.stop()
    live.start()
    return () => live.stop()
  }, [accountScope, authRequired, currentTenantId, workbenchLoading])

  return React.useMemo(() => ({
    ...snapshot,
    submit: controller.submit,
    submitFeedback: controller.submitFeedback,
    editQueueItem: controller.editQueueItem,
    loadQueueItem: controller.loadQueueItem,
    removeQueueItem: controller.removeQueueItem,
    steerQueueItem: controller.steerQueueItem,
    cancel: controller.cancel,
    answerQuestion: controller.answerQuestion,
    reloadMetadata: controller.reloadMetadata,
    retryHistory: controller.retryHistory,
  }), [controller, snapshot])
}
