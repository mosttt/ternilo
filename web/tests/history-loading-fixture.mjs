export function history() {
  const events = []
  const add = (type, fields = {}) => events.push({ seq: events.length, occurred_at_ms: 1_700_000_000_000 + events.length,
    run_id: 'history-fixture', type, ...fields })
  add('turn_started')
  add('user_message', { content: 'Load the complete long reasoning history' })
  add('step_started', { step: 1 })
  for (let index = 0; index < 30_000; index++) add('assistant_reasoning_delta', { step: 1, delta: `Reason ${index}\n` })
  add('assistant_message_delta', { step: 1, delta: 'History preserved.' })
  add('turn_failed', { message: 'History fixture interrupted after receiving output' })
  return events
}

