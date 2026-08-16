import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import App from './App'
import QuickFlightControl from './QuickFlightControl'
import './styles.css'

const isQuickFlightControl = new URLSearchParams(window.location.search).has('quick-flight')

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    {isQuickFlightControl ? <QuickFlightControl /> : <App />}
  </StrictMode>,
)
