import { useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'

export default function QuickFlightControl() {
  const [entering, setEntering] = useState(false)
  const [error, setError] = useState('')

  useEffect(() => {
    const stop = listen('flight://quick-reset', () => {
      setEntering(false)
      setError('')
    })
    return () => { void stop.then((unlisten) => unlisten()) }
  }, [])

  async function enterFlight() {
    if (entering) return
    setEntering(true)
    setError('')
    try {
      // This window is independent from the ChatGPT webview, so it remains
      // clickable even while that page is busy rendering a long conversation.
      await invoke('enter_flight_mode', { historyLimit: 2 })
      await getCurrentWindow().hide()
    } catch (reason) {
      setError(String(reason))
      setEntering(false)
    }
  }

  return (
    <main className="quick-flight">
      <button type="button" onClick={() => void enterFlight()} disabled={entering}>
        <span>✈</span>{entering ? '正在进入…' : '进入 Flight'}
      </button>
      {error && <p title={error}>进入失败，重试</p>}
    </main>
  )
}
