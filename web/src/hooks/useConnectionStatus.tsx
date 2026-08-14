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

  // HTTP is authoritative because WebSockets reconnect routinely. Require two
  // failures for reconnecting and three for disconnected to absorb slow I/O.
  function evaluate() {
    if (httpOk.current) {
      if (disconnectTimer.current) {
        clearTimeout(disconnectTimer.current)
        disconnectTimer.current = null
      }
      httpFailCount.current = 0
      setState("connected")
    } else if (httpFailCount.current >= 3) {
      setState("disconnected")
    } else if (httpFailCount.current >= 2) {
      setState("reconnecting")
    }
  }

  useEffect(() => {
    wsClient.connect()
  }, [])

  useEffect(() => {
    let mounted = true
    // Prevent overlapping polls from counting one stall twice.
    let inFlight = false

    async function poll() {
      if (inFlight) return
      inFlight = true
      try {
        const controller = new AbortController()
        // Include time queued behind browser video connections.
        const timeout = setTimeout(() => controller.abort(), 15000)
        // Default priority prevents streaming traffic from indefinitely deferring health checks.
        const res = await fetch("/api/status", {
          signal: controller.signal,
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
      } finally {
        inFlight = false
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
