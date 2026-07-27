import { createContext, useContext, useEffect, useRef, useState } from "react"
import { wsClient } from "@/lib/ws"

export type ConnectionState = "connected" | "reconnecting" | "disconnected"

interface ConnectionContextValue {
  state: ConnectionState
  retry: () => void
}

const ConnectionContext = createContext<ConnectionContextValue>({
  state: "connected",
  retry: () => {},
})

export function useConnectionStatus() {
  return useContext(ConnectionContext)
}

export function ConnectionProvider({ children }: { children: React.ReactNode }) {
  const [state, setState] = useState<ConnectionState>("connected")
  const disconnectTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const httpOk = useRef(true)
  const httpFailCount = useRef(0)

  // HTTP is the primary connectivity signal. WebSockets cycle on their own
  // (server timeouts, keepalive) without meaning anything is wrong, so
  // "reconnecting" and "disconnected" only follow failing HTTP polls.
  function evaluate() {
    if (httpOk.current) {
      if (disconnectTimer.current) {
        clearTimeout(disconnectTimer.current)
        disconnectTimer.current = null
      }
      httpFailCount.current = 0
      setState("connected")
    } else if (httpFailCount.current >= 3) {
      // Repeated HTTP failures mean it is genuinely gone.
      setState("disconnected")
    } else {
      // First HTTP failure: show reconnecting and give it time.
      setState("reconnecting")
    }
  }

  // Ensure WebSocket stays connected (it handles its own reconnection)
  useEffect(() => {
    wsClient.connect()
  }, [])

  useEffect(() => {
    let mounted = true

    async function poll() {
      try {
        const controller = new AbortController()
        const timeout = setTimeout(() => controller.abort(), 10000)
        const res = await fetch("/api/status", {
          signal: controller.signal,
          priority: "low",
        } as RequestInit)
        clearTimeout(timeout)
        if (mounted) {
          httpOk.current = res.ok
          if (res.ok) httpFailCount.current = 0
          else httpFailCount.current++
          evaluate()
        }
      } catch {
        if (mounted) {
          httpOk.current = false
          httpFailCount.current++
          evaluate()
        }
      }
    }

    poll()
    const iv = setInterval(poll, 8000)
    return () => { mounted = false; clearInterval(iv) }
  }, [])

  function retry() {
    wsClient.reconnect()
    setState("reconnecting")
    fetch("/api/status")
      .then((res) => {
        httpOk.current = res.ok
        if (res.ok) httpFailCount.current = 0
        evaluate()
      })
      .catch(() => {
        httpOk.current = false
        httpFailCount.current++
        evaluate()
      })
  }

  return (
    <ConnectionContext.Provider value={{ state, retry }}>
      {children}
    </ConnectionContext.Provider>
  )
}
