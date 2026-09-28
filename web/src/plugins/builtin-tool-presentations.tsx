import {
  fileToolPresentations,
  readTool, replaceTool, searchTool, writeTool,
} from './tool-presentations/file-contributions'
import { declarativeTool } from './tool-presentations/declarative-contribution'
import {
  codeTool, generalToolPresentations, genericTool, orchestrationTool, skillTool,
} from './tool-presentations/general-contributions'
import {
  shellTool, terminalTool, terminalToolPresentations,
} from './tool-presentations/terminal-contributions'
import {
  webFetchTool, webSearchTool, webToolPresentations,
} from './tool-presentations/web-contributions'
import {
  toolPresentationRegistry,
  type ToolPresentationRegistry,
} from './tool-presentation-registry'

export const builtinToolPresentations = {
  declarativeTool,
  ...fileToolPresentations,
  ...terminalToolPresentations,
  ...webToolPresentations,
  ...generalToolPresentations,
}

export function registerBuiltinToolPresentations(
  registry: ToolPresentationRegistry = toolPresentationRegistry,
) {
  const disposers = [
    registry.register(declarativeTool),
    registry.register(skillTool),
    registry.register(readTool),
    registry.register(writeTool),
    registry.register(replaceTool),
    registry.register(searchTool),
    registry.register(shellTool),
    registry.register(terminalTool),
    registry.register(webSearchTool),
    registry.register(webFetchTool),
    registry.register(codeTool),
    registry.register(orchestrationTool),
    registry.register(genericTool),
  ]
  return () => {
    for (const dispose of disposers.toReversed()) dispose()
  }
}

registerBuiltinToolPresentations()
