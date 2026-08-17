import { ClipboardEvent as ReactClipboardEvent, ComponentPropsWithoutRef, FormEvent, KeyboardEvent, useEffect, useMemo, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { emitTo, listen } from '@tauri-apps/api/event'
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'

type Role = 'user' | 'assistant'
type Mode = 'browser' | 'flight'

type Message = {
  id: string
  role: Role
  text: string
  streaming?: boolean
  failed?: boolean
  image?: ImageAttachment
}

type ImageAttachment = { name: string, dataUrl: string }
type ConversationSummary = { id: string, title: string, updatedAt?: number }
type AccountSummary = { name: string, plan: string }

type StreamEvent = {
  kind: 'bridge_ready' | 'account' | 'probe' | 'session_ready' | 'session_login_required' | 'enter_flight' | 'flight_active' | 'assistant_start' | 'conversation' | 'conversation_loading' | 'conversation_list' | 'conversation_list_error' | 'history' | 'command_suggestions' | 'delta' | 'complete' | 'title' | 'error'
  text?: string
  conversationId?: string
  title?: string
  error?: string
  messages?: Array<{ role: Role, text: string }>
}

const newId = () => crypto.randomUUID()
const CHAT_WINDOW_LABEL = 'chatgpt-session'
const QUICK_FLIGHT_WINDOW_LABEL = 'flight-quick'
const MAX_IMAGE_BYTES = 10 * 1024 * 1024
const HISTORY_LIMITS = [2, 5, 10, 20, 50] as const
const SPLIT_WIDTHS = [
  { label: '固定宽度 (1000px)', value: 1000 },
  { label: '中等宽度 (1200px)', value: 1200 },
  { label: '全屏平分', value: 0 }
] as const
const WINDOW_SIZES = [
  { label: '标准 (1400×900)', width: 1400, height: 900 },
  { label: '紧凑 (1280×768)', width: 1280, height: 768 },
  { label: '宽屏 (1600×900)', width: 1600, height: 900 },
  { label: '全高清 (1920×1080)', width: 1920, height: 1080 }
] as const

const initialHistoryLimit = (() => {
  const saved = Number(window.localStorage.getItem('flight-history-limit'))
  return HISTORY_LIMITS.includes(saved as typeof HISTORY_LIMITS[number]) ? saved : 2
})()

const initialSplitWidth = (() => {
  const saved = Number(window.localStorage.getItem('flight-split-width'))
  return saved || 1000
})()

const initialWindowSize = (() => {
  const saved = window.localStorage.getItem('flight-window-size')
  if (saved) {
    const parsed = JSON.parse(saved)
    return { width: parsed.width || 1400, height: parsed.height || 900 }
  }
  return { width: 1400, height: 900 }
})()

const readImage = (file: File) => new Promise<ImageAttachment>((resolve, reject) => {
  const reader = new FileReader()
  reader.onerror = () => reject(new Error('无法读取图片'))
  reader.onload = () => resolve({ name: file.name || 'clipboard-image.png', dataUrl: String(reader.result) })
  reader.readAsDataURL(file)
})

const mergeDelta = (current: string, incoming: string) => {
  if (!current || !incoming) return current + incoming
  if (current.endsWith(incoming)) return current
  if (incoming.startsWith(current)) return incoming
  return current + incoming
}

function CodeBlock({ className, children, ...props }: ComponentPropsWithoutRef<'code'>) {
  const [copied, setCopied] = useState(false)
  const language = /language-([\w+-]+)/.exec(className ?? '')?.[1]
  const source = String(children).replace(/\n$/, '')

  if (!className) return <code {...props}>{children}</code>

  async function copyCode() {
    await navigator.clipboard?.writeText(source)
    setCopied(true)
    window.setTimeout(() => setCopied(false), 1400)
  }

  return (
    <span className="code-block">
      <span className="code-block-head"><span>{language || 'code'}</span><button type="button" onClick={() => void copyCode()}>{copied ? '已复制' : '复制'}</button></span>
      <code className={className} {...props}>{children}</code>
    </span>
  )
}

function MarkdownContent({ value }: { value: string }) {
  return <ReactMarkdown remarkPlugins={[remarkGfm]} components={{ code: CodeBlock }}>{value}</ReactMarkdown>
}

export default function App() {
  const [mode, setMode] = useState<Mode>('browser')
  const [messages, setMessages] = useState<Message[]>([])
  const [draft, setDraft] = useState('')
  const [attachment, setAttachment] = useState<ImageAttachment>()
  const [commandOptions, setCommandOptions] = useState<string[]>([])
  const [commandSelected, setCommandSelected] = useState(false)
  const [commandLabel, setCommandLabel] = useState<string>()
  const [splitView, setSplitView] = useState(false)
  const [connected, setConnected] = useState(false)
  const [pickingHistory, setPickingHistory] = useState(false)
  const [sending, setSending] = useState(false)
  const [status, setStatus] = useState('先打开网页登录')
  const [conversationId, setConversationId] = useState<string>()
  const [title, setTitle] = useState('未命名会话')
  const [historyLimit, setHistoryLimit] = useState<number>(initialHistoryLimit)
  const [splitWidth, setSplitWidth] = useState<number>(initialSplitWidth)
  const [windowSize, setWindowSize] = useState(initialWindowSize)
  const [conversationList, setConversationList] = useState<ConversationSummary[]>([])
  const [historyDrawerOpen, setHistoryDrawerOpen] = useState(false)
  const [historyQuery, setHistoryQuery] = useState('')
  const [historyLoading, setHistoryLoading] = useState(false)
  const [historyError, setHistoryError] = useState<string>()
  const [account, setAccount] = useState<AccountSummary>()
  const [accountError, setAccountError] = useState<string>()
  const [conversationSwitching, setConversationSwitching] = useState(false)
  const transcriptRef = useRef<HTMLDivElement>(null)
  const textareaRef = useRef<HTMLTextAreaElement>(null)
  const pendingConversationRef = useRef<string>()
  const newConversationPendingRef = useRef(false)
  const autoEnterRef = useRef(false)

  useEffect(() => {
    window.localStorage.setItem('flight-history-limit', String(historyLimit))
  }, [historyLimit])

  useEffect(() => {
    window.localStorage.setItem('flight-split-width', String(splitWidth))
  }, [splitWidth])

  useEffect(() => {
    window.localStorage.setItem('flight-window-size', JSON.stringify(windowSize))
  }, [windowSize])

  useEffect(() => {
    const textarea = textareaRef.current
    if (!textarea) return

    const adjustHeight = () => {
      textarea.style.height = 'auto'
      textarea.style.height = `${textarea.scrollHeight}px`
    }

    adjustHeight()
  }, [draft])

  useEffect(() => {
    const stop = listen<StreamEvent>('flight://stream', ({ payload }) => {
      if (payload.kind === 'bridge_ready') {
        setConnected(true)
        if (!payload.conversationId && newConversationPendingRef.current) {
          newConversationPendingRef.current = false
          setConversationSwitching(false)
          setStatus('新会话已在网页后台就绪')
          return
        }
        if (payload.conversationId && pendingConversationRef.current === payload.conversationId) {
          pendingConversationRef.current = undefined
          setConversationSwitching(false)
          setStatus('会话已在网页后台就绪')
        }
        return
      }

      if (payload.kind === 'probe') {
        // Probe events are diagnostics only. They must not overwrite the
        // visible conversation/split status or imply that input is blocked.
        return
      }

      if (payload.kind === 'account') {
        try {
          const next = JSON.parse(payload.text ?? '{}')
          if (typeof next?.name === 'string' && typeof next?.plan === 'string' && next.name && next.plan) {
            setAccount({ name: next.name, plan: next.plan })
            setAccountError(undefined)
          } else if (typeof next?.error === 'string') {
            setAccountError(next.error)
          }
        } catch {
          setAccountError('网页返回格式异常')
        }
        return
      }

      if (payload.kind === 'session_ready') {
        setConnected(true)
        if (!autoEnterRef.current) {
          autoEnterRef.current = true
          setStatus('已恢复网页登录，正在进入飞行模式…')
          void enterFlightMode()
        }
        return
      }

      if (payload.kind === 'session_login_required') {
        setConnected(false)
        setStatus('网页登录已失效，请打开登录页重新登录')
        return
      }

      if (payload.kind === 'enter_flight') {
        void enterFlightMode()
        return
      }

      if (payload.kind === 'flight_active') {
        setMode('flight')
        setPickingHistory(false)
        setConnected(true)
        setStatus('已连接')
        return
      }

      if (payload.kind === 'conversation' && payload.conversationId) {
        setConversationId(payload.conversationId)
        setConnected(true)
        return
      }

      if (payload.kind === 'conversation_loading' && payload.conversationId) {
        pendingConversationRef.current = payload.conversationId
        setConversationSwitching(true)
        setConversationId(payload.conversationId)
        setStatus('正在读取会话，并在后台打开网页…')
        return
      }

      if (payload.kind === 'conversation_list') {
        try {
          const list = JSON.parse(payload.text ?? '[]')
          const conversations = Array.isArray(list) ? list.filter((item): item is ConversationSummary => (
            typeof item?.id === 'string' && typeof item?.title === 'string'
          )) : []
          setConversationList(conversations)
          setHistoryError(undefined)
          setHistoryLoading(false)
        } catch {
          setConversationList([])
          setHistoryError('历史会话列表数据格式异常')
          setHistoryLoading(false)
        }
        return
      }

      if (payload.kind === 'conversation_list_error') {
        setHistoryLoading(false)
        setHistoryError(payload.error || '无法读取历史会话列表')
        return
      }

      if (payload.kind === 'history') {
        const history = (payload.messages ?? []).filter((message) => message.text.trim())
        if (history.length) {
          setMessages(history.map((message) => ({ id: newId(), role: message.role, text: message.text })))
        }
        if (payload.conversationId) setConversationId(payload.conversationId)
        if (payload.conversationId && pendingConversationRef.current === payload.conversationId) {
          scheduleBridgeRecovery()
          pendingConversationRef.current = undefined
          setConversationSwitching(false)
        }
        setSending(false)
        setConnected(true)
        setStatus(history.length ? `已回显最新 ${history.length} 条历史消息` : '未读取到完整历史，保留当前已捕获内容')
        return
      }

      if (payload.kind === 'command_suggestions') {
        try {
          const options = JSON.parse(payload.text ?? '[]')
          setCommandOptions(Array.isArray(options) ? options.filter((option): option is string => typeof option === 'string') : [])
        } catch {
          setCommandOptions([])
        }
        return
      }

      if (payload.kind === 'title' && payload.title) {
        setTitle(payload.title)
        return
      }

      if (payload.kind === 'assistant_start') {
        setConnected(true)
        setStatus('生成中')
        return
      }

      if (payload.kind === 'delta' && payload.text) {
        setMessages((current) => {
          const last = current.at(-1)
          if (last?.role === 'assistant' && last.streaming) {
            return [...current.slice(0, -1), { ...last, text: mergeDelta(last.text, payload.text ?? '') }]
          }
          return [...current, { id: newId(), role: 'assistant', text: payload.text ?? '', streaming: true }]
        })
        return
      }

      if (payload.kind === 'complete') {
        setMessages((current) => current.map((message) => (
          message.streaming ? { ...message, streaming: false } : message
        )))
        setSending(false)
        setStatus('完成')
        return
      }

      if (payload.kind === 'error') {
        setMessages((current) => current.map((message) => (
          message.streaming ? { ...message, streaming: false, failed: true } : message
        )))
        setSending(false)
        setStatus(payload.error || '网页端返回异常')
      }
    })

    return () => { void stop.then((unlisten) => unlisten()) }
  }, [])

  useEffect(() => {
    if (mode === 'flight' && conversationList.length === 0) void refreshConversationList()
  }, [mode])

  useEffect(() => {
    void bootstrapWebSession()
  }, [])

  useEffect(() => {
    transcriptRef.current?.scrollTo({ top: transcriptRef.current.scrollHeight, behavior: 'smooth' })
  }, [messages])

  useEffect(() => {
    if (mode !== 'flight' || sending) {
      setCommandOptions([])
      return
    }
    if (commandSelected) {
      const timer = window.setTimeout(() => {
        void invoke('update_command_suffix', { text: draft }).catch((error) => setStatus(`无法同步命令文字：${String(error)}`))
      }, 90)
      return () => window.clearTimeout(timer)
    }
    if (!/^[\s]*[@/]/.test(draft)) {
      setCommandOptions([])
      return
    }
    const timer = window.setTimeout(() => {
      void invoke('update_command_draft', { text: draft }).catch(() => setCommandOptions([]))
    }, 130)
    return () => window.clearTimeout(timer)
  }, [commandSelected, draft, mode, sending])

  const canSend = useMemo(() => mode === 'flight' && (draft.trim().length > 0 || Boolean(attachment) || commandSelected) && !sending && !conversationSwitching, [attachment, commandSelected, conversationSwitching, draft, mode, sending])

  async function openLogin() {
    try {
      const existing = await WebviewWindow.getByLabel(CHAT_WINDOW_LABEL)
      if (existing) {
        await existing.show()
        await existing.setFocus()
      } else {
        const chatWindow = new WebviewWindow(CHAT_WINDOW_LABEL, {
          url: 'https://chatgpt.com/',
          title: 'ChatGPT · 登录与网页会话',
          width: 980,
          height: 720,
          minWidth: 720,
          minHeight: 520,
          center: true,
        })
        await new Promise<void>((resolve, reject) => {
          void chatWindow.once('tauri://created', () => resolve())
          void chatWindow.once('tauri://error', (event) => reject(event.payload))
        })
      }
      setMode('browser')
      setStatus('网页登录窗口已打开')
      // ChatGPT can redirect through several documents before its final page.
      // Retry injection so the controls land on that final authenticated document.
      for (const delay of [700, 1800, 3600, 6500]) {
        window.setTimeout(() => {
          void invoke('prepare_web_bridge').catch((error) => {
            if (delay === 6500) setStatus(`无法注入网页按钮：${String(error)}`)
          })
        }, delay)
      }
    } catch (error) {
      setStatus(`无法打开网页：${String(error)}`)
    }
  }

  async function bootstrapWebSession() {
    try {
      const existing = await WebviewWindow.getByLabel(CHAT_WINDOW_LABEL)
      if (!existing) {
        const chatWindow = new WebviewWindow(CHAT_WINDOW_LABEL, {
          url: 'https://chatgpt.com/',
          title: 'ChatGPT · 登录与网页会话',
          width: 980,
          height: 720,
          minWidth: 720,
          minHeight: 520,
          center: true,
          visible: false,
          focus: false,
        })
        await new Promise<void>((resolve, reject) => {
          void chatWindow.once('tauri://created', () => resolve())
          void chatWindow.once('tauri://error', (event) => reject(event.payload))
        })
      }
      setStatus('正在后台恢复网页登录…')
      for (const delay of [700, 1800, 3600, 6500]) {
        window.setTimeout(() => { void invoke('prepare_web_bridge').catch(() => {}) }, delay)
      }
    } catch (error) {
      setStatus(`无法恢复网页登录：${String(error)}`)
    }
  }

  async function showQuickFlightControl() {
    const existing = await WebviewWindow.getByLabel(QUICK_FLIGHT_WINDOW_LABEL)
    if (existing) {
      await existing.show()
      await emitTo(QUICK_FLIGHT_WINDOW_LABEL, 'flight://quick-reset')
      return
    }
    const quickWindow = new WebviewWindow(QUICK_FLIGHT_WINDOW_LABEL, {
      url: '/?quick-flight',
      title: '进入 Flight',
      width: 174,
      height: 58,
      minWidth: 174,
      minHeight: 58,
      maxWidth: 174,
      maxHeight: 58,
      x: 18,
      y: 18,
      decorations: false,
      resizable: false,
      alwaysOnTop: true,
      skipTaskbar: true,
      focus: false,
    })
    await new Promise<void>((resolve, reject) => {
      void quickWindow.once('tauri://created', () => resolve())
      void quickWindow.once('tauri://error', (event) => reject(event.payload))
    })
  }

  async function hideQuickFlightControl() {
    const quickWindow = await WebviewWindow.getByLabel(QUICK_FLIGHT_WINDOW_LABEL)
    if (quickWindow) await quickWindow.hide()
  }

  async function enterFlightMode() {
    try {
      if (splitView) {
        await invoke('set_split_view', { enabled: false, width: splitWidth })
        setSplitView(false)
      }
      // 检查网页窗口是否存在
      const existing = await WebviewWindow.getByLabel(CHAT_WINDOW_LABEL)
      if (!existing) {
        // 网页窗口不存在，需要重新打开
        setStatus('网页窗口已关闭，正在重新打开...')
        await openLogin()
        setStatus('请在网页窗口登录后，再次点击"进入飞行模式"')
        return
      }
      await invoke('enter_flight_mode', { historyLimit })
      await hideQuickFlightControl()
      setMode('flight')
      setPickingHistory(false)
      setConnected(true)
      setStatus('已连接')
    } catch (error) {
      setStatus(`请先完成网页登录：${String(error)}`)
    }
  }

  async function exitFlightMode() {
    try {
      if (splitView) {
        await invoke('set_split_view', { enabled: false, width: splitWidth })
        setSplitView(false)
      }
      // 不调用后端的 exit_flight_mode，因为它会打开网页
      // 只在前端切换到 browser 模式即可
      setMode('browser')
      setPickingHistory(false)
      setMessages([])
      setConversationId(undefined)
      setTitle('未命名会话')
      setStatus('可调整设置后重新进入')
    } catch (error) {
      setStatus(`无法退出飞行模式：${String(error)}`)
    }
  }

  async function restartApp() {
    try {
      await invoke('restart_app', { width: windowSize.width, height: windowSize.height })
    } catch (error) {
      setStatus(`重启失败：${String(error)}`)
    }
  }

  async function openHistoryPicker() {
    try {
      if (splitView) {
        await invoke('set_split_view', { enabled: false })
        setSplitView(false)
      }
      await invoke('exit_flight_mode')
      await showQuickFlightControl()
      setMode('browser')
      setPickingHistory(true)
      setMessages([])
      setConversationId(undefined)
      setTitle('正在选择历史会话')
      setStatus('请在网页侧边栏选择历史会话，完成后回到这里继续飞行')
    } catch (error) {
      setStatus(`无法显示网页：${String(error)}`)
    }
  }

  function scheduleBridgeRecovery() {
    for (const delay of [700, 1800, 3600, 6500]) {
      window.setTimeout(() => {
        void invoke('prepare_web_bridge').catch((error) => {
          if (delay === 6500) setStatus(`网页会话准备失败：${String(error)}`)
        })
      }, delay)
    }
  }

  async function refreshConversationList() {
    setHistoryLoading(true)
    setHistoryError(undefined)
    try {
      await invoke('load_conversation_list')
    } catch (error) {
      setHistoryLoading(false)
      setHistoryError(`无法读取历史会话列表：${String(error)}`)
    }
  }

  async function openConversationList() {
    setHistoryDrawerOpen(true)
    if (conversationList.length === 0) await refreshConversationList()
  }

  async function selectConversation(conversation: ConversationSummary) {
    if (conversationSwitching || conversation.id === conversationId) {
      setHistoryDrawerOpen(false)
      return
    }
    try {
      pendingConversationRef.current = conversation.id
      setConversationSwitching(true)
      setHistoryDrawerOpen(false)
      setMessages([])
      setConversationId(conversation.id)
      setTitle(conversation.title)
      setStatus('正在读取最新两条历史消息…')
      await invoke('select_conversation', { conversationId: conversation.id })
      scheduleBridgeRecovery()
    } catch (error) {
      pendingConversationRef.current = undefined
      setConversationSwitching(false)
      setStatus(`无法打开该会话：${String(error)}`)
    }
  }

  async function createConversation() {
    try {
      newConversationPendingRef.current = true
      pendingConversationRef.current = undefined
      setConversationSwitching(true)
      setStatus('正在创建新会话，并等待网页就绪…')
      await invoke('new_conversation')
      setPickingHistory(false)
      setMessages([])
      setConversationId(undefined)
      setTitle('未命名会话')
      scheduleBridgeRecovery()
    } catch (error) {
      newConversationPendingRef.current = false
      setConversationSwitching(false)
      setStatus(`无法创建新会话：${String(error)}`)
    }
  }

  function beginMessage(text: string, image?: ImageAttachment) {
    setMessages((current) => [
      ...current,
      { id: newId(), role: 'user', text, image },
      { id: newId(), role: 'assistant', text: '', streaming: true },
    ])
    setDraft('')
    setAttachment(undefined)
    setSending(true)
    setStatus('已交给网页发送')
  }

  async function sendToWeb(text: string, image?: ImageAttachment) {
    const displayText = commandLabel ? `@ ${commandLabel}${text ? `\n${text}` : ''}` : text
    beginMessage(displayText, image)
    try {
      await invoke('send_message', { text, image, preserveComposer: commandSelected })
      setCommandSelected(false)
      setCommandLabel(undefined)
      setCommandOptions([])
    } catch (error) {
      setMessages((current) => current.map((message) => (
        message.streaming ? { ...message, streaming: false, failed: true, text: '网页端未能接收这条消息。' } : message
      )))
      setSending(false)
      setStatus(`发送失败：${String(error)}`)
    }
  }

  async function send(event: FormEvent) {
    event.preventDefault()
    const text = draft.trim()
    if ((!text && !attachment && !commandSelected) || sending) return
    await sendToWeb(text, attachment)
  }

  async function pasteImage(event: ReactClipboardEvent<HTMLTextAreaElement>) {
    const file = [...event.clipboardData.items]
      .find((item) => item.kind === 'file' && item.type.startsWith('image/'))
      ?.getAsFile()
    if (!file) return
    event.preventDefault()
    if (file.size > MAX_IMAGE_BYTES) {
      setStatus('图片不能超过 10MB')
      return
    }
    try {
      const image = await readImage(file)
      setAttachment(image)
      setStatus('图片已粘贴，发送后将交给网页上传')
    } catch (error) {
      setStatus(`无法粘贴图片：${String(error)}`)
    }
  }

  async function selectCommand(index: number) {
    try {
      const label = commandOptions[index]
      await invoke('select_command_option', { text: label })
      setCommandSelected(true)
      setCommandLabel(label)
      setDraft('')
      setCommandOptions([])
      setStatus('已选择网页命令，直接发送即可执行')
    } catch (error) {
      setStatus(`无法选择命令：${String(error)}`)
    }
  }

  async function clearSelectedCommand() {
    if (!commandSelected) return
    try {
      // The webpage owns the actual command tag, so remove it there first.
      // Passing draft preserves any text the user typed after the tag.
      await invoke('clear_command_selection', { text: draft })
      setCommandSelected(false)
      setCommandLabel(undefined)
      setCommandOptions([])
      setStatus('已移除网页命令标签')
    } catch (error) {
      setStatus(`无法移除命令标签：${String(error)}`)
    }
  }

  async function toggleSplitView() {
    const previous = splitView
    const enabled = !previous
    // Reflect a close immediately. The native hide of a busy ChatGPT WebView
    // is deliberately allowed to finish in the background.
    setSplitView(enabled)
      setStatus(enabled ? '开启分屏…' : '返回飞行…')
    try {
      await invoke('set_split_view', { enabled, width: splitWidth })
      setStatus(enabled ? '分屏已开启' : '已返回')
    } catch (error) {
      setSplitView(previous)
      setStatus(`无法切换分屏：${String(error)}`)
    }
  }

  function onComposerKeyDown(event: KeyboardEvent<HTMLTextAreaElement>, action: () => void) {
    // A selected @ / slash command is rendered as a chip outside the textarea.
    // Match the webpage behavior: when its suffix is empty, Backspace removes
    // that command instead of doing nothing in the textarea.
    if (event.key === 'Backspace' && commandSelected && !draft) {
      event.preventDefault()
      void clearSelectedCommand()
      return
    }
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault()
      action()
    }
  }

  return (
    <main className="app-shell">
      <header className="topbar">
        <div className="topbar-left">
          <div className="wordmark" aria-label="Flight Chat">
            <span className="wordmark-mark">F</span>
            <span>Flight</span>
          </div>
          <span className="topbar-status" title={sending ? '正在接收网页回复' : status}>{sending ? '回复中' : status}</span>
          <span className={`connection ${connected ? 'is-live' : ''}`}><i />{connected ? '在线' : '离线'}</span>
          {connected && <span className={`account-summary ${accountError ? 'has-error' : ''}`} title={account ? `${account.name} · ${account.plan}` : accountError}>{account ? `${account.name} · ${account.plan}` : accountError ? '账户未识别' : '读取账户…'}</span>}
        </div>
        <div className="topbar-center">
          <span className="eyebrow">当前会话</span>
          <strong>{title}</strong>
        </div>
        <div className="topbar-actions">
          {mode === 'flight' ? (
            <>
              <button className="button quiet" onClick={() => void createConversation()}>新会话</button>
              <button className={`button quiet ${splitView ? 'is-active' : ''}`} onClick={() => void toggleSplitView()}>{splitView ? '关闭分屏' : '分屏预览'}</button>
              <button className={`button quiet ${historyDrawerOpen ? 'is-active' : ''}`} onClick={() => void openConversationList()}>会话列表</button>
            </>
          ) : (
            <>
              <button className="button quiet" onClick={() => void openLogin()}>打开登录页</button>
            </>
          )}
        </div>
      </header>

      {mode === 'browser' ? (
        <section className="landing" aria-labelledby="landing-title">
          <div>
            <p className="eyebrow">Flight mode / 01</p>
            <h1 id="landing-title">{pickingHistory ? '从历史里回来，继续专注对话' : '把网页留在后台，把注意力留给对话'}</h1>
            <p className="landing-copy">{pickingHistory ? '网页窗口已显示。请在 ChatGPT 左侧历史列表选择会话；选择完成后，点击"继续飞行"隐藏网页。' : '先在独立网页窗口登录 ChatGPT。验证完成后，返回这里进入飞行模式；后续发送与流式回复仍由真实网页会话处理。'}</p>
          </div>
          <div className="landing-actions">
            <button className="button primary" onClick={() => void openLogin()}>{pickingHistory ? '回到网页会话' : '打开网页登录'}</button>
            <button className="text-button" onClick={() => void enterFlightMode()}>{pickingHistory ? '继续飞行' : '进入飞行模式'} <span>↗</span></button>
          </div>
          <div className="landing-settings">
            <div className="setting-item">
              <span className="setting-label">历史回显</span>
              <div className="setting-control">
                <select value={historyLimit} onChange={(event) => setHistoryLimit(Number(event.target.value))}>
                  {HISTORY_LIMITS.map((limit) => <option key={limit} value={limit}>最新 {limit} 条消息</option>)}
                </select>
                <p className="setting-hint">设置会自动保存，并在下次进入飞行模式时生效</p>
              </div>
            </div>
            <div className="setting-item">
              <span className="setting-label">分屏宽度</span>
              <div className="setting-control">
                <select value={splitWidth} onChange={(event) => setSplitWidth(Number(event.target.value))}>
                  {SPLIT_WIDTHS.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
                </select>
                <p className="setting-hint">设置点击"分屏预览"时每个窗口的宽度</p>
              </div>
            </div>
            <div className="setting-item">
              <span className="setting-label">窗口大小</span>
              <div className="setting-control">
                <div className="setting-options">
                  {WINDOW_SIZES.map((size) => (
                    <label key={`${size.width}x${size.height}`} className="setting-option">
                      <input
                        type="radio"
                        name="windowSize"
                        checked={windowSize.width === size.width && windowSize.height === size.height}
                        onChange={() => setWindowSize({ width: size.width, height: size.height })}
                      />
                      <span>{size.label}</span>
                    </label>
                  ))}
                </div>
                <div className="setting-action">
                  <button className="button quiet" onClick={() => void restartApp()}>应用</button>
                </div>
              </div>
            </div>
          </div>
          <div className="status-line"><span>待机</span><span>{status}</span></div>
        </section>
      ) : (
        <section className="conversation" aria-label="飞行模式对话">
          {historyDrawerOpen && (
            <aside className="conversation-drawer" aria-label="历史会话">
              <div className="conversation-drawer-head"><strong>历史会话</strong><span>{historyLoading ? '加载中…' : `${conversationList.length} 条`}</span><button type="button" onClick={() => setHistoryDrawerOpen(false)} aria-label="关闭会话列表">×</button></div>
              <input value={historyQuery} onChange={(event) => setHistoryQuery(event.target.value)} placeholder="搜索会话标题" aria-label="搜索会话标题" />
              <div className="conversation-list">
                {conversationList.filter((item) => item.title.toLowerCase().includes(historyQuery.trim().toLowerCase())).map((item) => (
                  <button type="button" key={item.id} className={item.id === conversationId ? 'is-current' : ''} onClick={() => void selectConversation(item)} disabled={conversationSwitching} title={item.title}>{item.title}</button>
                ))}
                {!historyLoading && conversationList.length === 0 && historyError && <p>{historyError}</p>}
                {!historyLoading && conversationList.length === 0 && !historyError && <p>没有可用的历史会话。</p>}
              </div>
              <button className="drawer-refresh" type="button" onClick={() => void refreshConversationList()} disabled={historyLoading}>刷新列表</button>
            </aside>
          )}
          <div className="thread">
            <div className={`transcript ${messages.length === 0 ? 'is-empty' : ''}`} ref={transcriptRef}>
              {messages.length === 0 ? (
                <div className="empty-state">
                  <span className="empty-index">01</span>
                  <h1>开始一段干净的对话。</h1>
                  <p>内容会由已登录网页发送；这里仅保留轻量、实时的阅读界面。</p>
                </div>
              ) : messages.map((message) => (
                <article className={`message ${message.role} ${message.failed ? 'failed' : ''}`} key={message.id}>
                  <span className="message-label">{message.role === 'user' ? '你' : 'ChatGPT'}</span>
                  <div className="message-content">
                    {message.image && <img className="message-image" src={message.image.dataUrl} alt={message.image.name} />}
                    {message.text ? <MarkdownContent value={message.text} /> : (message.streaming && <span className="typing" aria-label="正在生成"><i /><i /><i /></span>)}
                    {message.streaming && message.text && <span className="cursor" aria-hidden="true" />}
                  </div>
                </article>
              ))}
            </div>
            <form className="composer" onSubmit={(event) => void send(event)}>
              {commandOptions.length > 0 && <div className="command-menu" role="listbox">{commandOptions.map((option, index) => <button type="button" key={`${option}-${index}`} onClick={() => void selectCommand(index)}><span>{option}</span><b>↵</b></button>)}</div>}
              {attachment && <div className="attachment-preview"><img src={attachment.dataUrl} alt="待发送图片" /><span>{attachment.name}</span><button type="button" onClick={() => setAttachment(undefined)} aria-label="移除图片">×</button></div>}
              {commandLabel && (
                <div className="command-chip">
                  <span className="command-chip-mark">@</span>
                  <span className="command-chip-label">{commandLabel}</span>
                  <button type="button" onClick={() => void clearSelectedCommand()} aria-label="移除命令标签">×</button>
                </div>
              )}
              <div className="composer-input-wrapper">
                <textarea
                  ref={textareaRef}
                  aria-label="输入消息"
                  value={draft}
                  onChange={(event) => setDraft(event.target.value)}
                  onPaste={(event) => void pasteImage(event)}
                  onKeyDown={(event) => onComposerKeyDown(event, () => void send(event as unknown as FormEvent))}
                  placeholder="写下你想说的，或直接粘贴图片…"
                  rows={1}
                  disabled={sending || conversationSwitching}
                />
                <button className="send" type="submit" disabled={!canSend} aria-label="发送消息">↑</button>
              </div>
              <p>Enter 发送　·　Shift + Enter 换行</p>
            </form>
          </div>
        </section>
      )}
      {mode === 'flight' && (
        <button className="floating-settings-button" onClick={() => void exitFlightMode()} aria-label="返回设置页" title="返回设置页">
          ⚙
        </button>
      )}
    </main>
  )
}
