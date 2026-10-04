import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { ServerSetup } from './components/workbench/server-setup'
import { App } from './app/App'
import { applyContentFontSize, readContentFontSize } from './domain/content-font'
import { installVisualViewportVariables } from './domain/visual-viewport'
import { installScrollbarActivity } from './domain/scrollbar'
import { installDesktopContextMenu } from './domain/desktop-context-menu'
import './styles/tokens.css'
import './styles/app.css'
import './styles/markdown.css'

const root = document.getElementById('root')
if (root === null) throw new Error('Ternilo workbench: missing #root')

applyContentFontSize(readContentFontSize(localStorage))
installVisualViewportVariables()
installScrollbarActivity()
installDesktopContextMenu()

createRoot(root).render(
  <StrictMode>
    {window.__TERNILO_BOOT__?.setup ? <ServerSetup /> : <App />}
  </StrictMode>,
)
