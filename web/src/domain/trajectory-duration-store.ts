import type { TrajectoryTimelineMode } from './trajectory-timeline'

export const TRAJECTORY_DURATION_STORAGE_KEY = 'ternilo.trajectory.duration-mode'

export function readTrajectoryDurationMode(storage: Pick<Storage, 'getItem'> | undefined): TrajectoryTimelineMode {
  return storage?.getItem(TRAJECTORY_DURATION_STORAGE_KEY) === 'actual' ? 'actual' : 'duration'
}

export function writeTrajectoryDurationMode(storage: Pick<Storage, 'setItem'> | undefined, mode: TrajectoryTimelineMode) {
  storage?.setItem(TRAJECTORY_DURATION_STORAGE_KEY, mode)
}
