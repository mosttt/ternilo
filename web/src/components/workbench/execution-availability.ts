import { createContext, useContext } from 'react'

export type ExecutionAvailability = 'live' | 'offline' | 'reconnecting'
export const ExecutionAvailabilityContext = createContext<ExecutionAvailability>('live')
export const useExecutionAvailability = () => useContext(ExecutionAvailabilityContext)
