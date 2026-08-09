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
  //
  // Hysteresis: one failed poll is noise — a status handler held up by a busy
  // disk, or this fetch queuing behind video streams on the browser's
  // per-host connection limit. Two consecutive failures show "reconnecting",
  // three show "disconnected". Flashing the banner on a single slow poll
  // trained users to ignore it.
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
    } else if (httpFailCount.current >= 2) {
      setState("reconnecting")
    }
  }

  // Ensure WebSocket stays connected (it handles its own reconnection)
  useEffect(() => {
    wsClient.connect()
  }, [])

  useEffect(() => {
    let mounted = true
    // The abort timeout outlives the 8s interval, so without this guard a
    // slow window runs overlapping polls — double-counting a single stall as
    // two consecutive failures (and holding two connection slots).
    let inFlight = false

    async function poll() {
      if (inFlight) return
      inFlight = true
      try {
        const controller = new AbortController()
        // 15s, not 10s: a poll that queues behind video streams counts its
        // queue time here too, so a shorter deadline reports a healthy
        // device as gone.
        const timeout = setTimeout(() => controller.abort(), 15000)
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
