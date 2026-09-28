import { describe, expect, it } from 'vitest'
import type { PendingSubmissionEcho, SessionEvent, SessionInboxSnapshot, SessionSubmission } from '@/types'
import { submissionEchoObserved, visibleInboxItems, visibleSubmissionEchoes } from './submission-echo'

const echo: PendingSubmissionEcho = {
  request_id: 'request-1', session_id: 'session-1', run_id: 'run-1',
  submission_id: 'submission-1', delivery: 'queue', input: 'hello',
  references: [], attachments: [], created_at_ms: 10,
}

function event(values: Partial<SessionEvent>): SessionEvent {
  return { seq: 1, occurred_at_ms: 20, run_id: 'run-1', type: 'user_message', ...values }
}

function inbox(items: SessionInboxSnapshot['items']): SessionInboxSnapshot {
  return { session_id: 'session-1', active_run_id: null, paused: false, items }
}

describe('submission echo reconciliation', () => {
  it('keeps an echo until the exact durable event or queue occurrence is visible', () => {
    expect(submissionEchoObserved(echo, [], inbox([]))).toBe(false)
    expect(submissionEchoObserved(echo, [event({
      run_id: 'different-run',
      source: { kind: 'submission', submission_id: 'different-submission', created_at_ms: 1, delivery: 'queue' },
    })], inbox([]))).toBe(false)
    expect(submissionEchoObserved(echo, [event({
      run_id: 'different-run',
      source: { kind: 'submission', submission_id: 'submission-1', created_at_ms: 1, delivery: 'queue' },
    })], inbox([]))).toBe(true)
    expect(submissionEchoObserved(echo, [], inbox([{
      id: 'submission-1', run_id: 'different-run', content: { kind: 'prompt', input: 'hello' },
      references: [], attachments: [], placement: 'queued', created_at_ms: 10, updated_at_ms: 10,
    }]))).toBe(true)
    expect(submissionEchoObserved(echo, [], inbox([{
      id: 'submission-1', run_id: 'run-1', content: { kind: 'prompt', input: 'hello' },
      references: [], attachments: [], placement: 'running', created_at_ms: 10, updated_at_ms: 10,
    }]))).toBe(false)
  })

  it('uses the client-minted run id before admission returns and never hides another echo', () => {
    const unadmitted = { ...echo, submission_id: undefined }
    const other = { ...unadmitted, request_id: 'request-2', run_id: 'run-2' }
    expect(visibleSubmissionEchoes([unadmitted, other], [event({ source: null })], inbox([])))
      .toEqual([other])
  })

  it('removes only steering rows consumed by their exact canonical message', () => {
    const steering: SessionSubmission = {
      id: 'steer-1', run_id: 'run-active', content: { kind: 'prompt', input: 'continue now' },
      references: [], attachments: [], placement: 'steering', created_at_ms: 10, updated_at_ms: 10,
    }
    const queued: SessionSubmission = { ...steering, id: 'queue-1', placement: 'queued' }
    const unrelated = event({
      source: { kind: 'submission', submission_id: 'other', created_at_ms: 1, delivery: 'steer' },
    })
    expect(visibleInboxItems([steering, queued], [unrelated])).toEqual([steering, queued])

    const consumed = event({
      source: { kind: 'submission', submission_id: 'steer-1', created_at_ms: 1, delivery: 'steer' },
    })
    expect(visibleInboxItems([steering, queued], [consumed])).toEqual([queued])
    const consumedQueued = event({ run_id: 'shared-batch-run',
      source: { kind: 'submission', submission_id: queued.id, created_at_ms: 1, delivery: 'queue' },
    })
    expect(visibleInboxItems([steering, queued], [consumedQueued])).toEqual([steering])
  })
})
