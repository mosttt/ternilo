export interface TrajectoryFollowState {
  pinned: boolean
  observedScrollTop: number
}

export interface TrajectoryScrollSnapshot {
  scrollTop: number
  scrollHeight: number
  clientHeight: number
  readerIntent: boolean
}

export interface TrajectoryFollowDecision extends TrajectoryFollowState {
  followTail: boolean
}

export const TRAJECTORY_TAIL_THRESHOLD = 24

/** Distinguishes reader scrolling from scroll events caused by streamed layout growth. */
export function updateTrajectoryFollow(
  state: TrajectoryFollowState,
  snapshot: TrajectoryScrollSnapshot,
): TrajectoryFollowDecision {
  const floor = Math.max(0, snapshot.scrollHeight - snapshot.clientHeight)
  const atTail = floor - snapshot.scrollTop <= TRAJECTORY_TAIL_THRESHOLD + 1
  const movedUp = snapshot.scrollTop < state.observedScrollTop - 0.5
  if (atTail) return { pinned: true, observedScrollTop: snapshot.scrollTop, followTail: false }
  if (!state.pinned || snapshot.readerIntent || movedUp) {
    return { pinned: false, observedScrollTop: snapshot.scrollTop, followTail: false }
  }
  return { pinned: true, observedScrollTop: snapshot.scrollTop, followTail: true }
}
