import { createContext, useCallback, useContext, useEffect, useRef, useState } from "react"
import { createLivenessProbe } from "@/lib/liveness"
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
  const retryRef = useRef<(() => void) | null>(null)
  const retry = useCallback(() => retryRef.current?.(), [])

  useEffect(() => {
    const probe = createLivenessProbe()
    let mounted = true
    let failures = 0
    let interval: ReturnType<typeof setInterval> | null = null
    let active: { controller: AbortController; timeout: ReturnType<typeof setTimeout> } | null = null

    function cancelProbe() {
      const previous = active
      active = null
      if (previous) {
        clearTimeout(previous.timeout)
        previous.controller.abort()
      }
    }

    async function poll(force = false) {
      if (!mounted || (!force && (document.hidden || active))) return
      // Manual retry supersedes the old request; its late result cannot undo it.
      cancelProbe()
      const controller = new AbortController()
      const request = { controller, timeout: setTimeout(() => controller.abort(), 15000) }
      active = request
      let ok = false
      try {
        ok = (await probe(controller.signal)).ok
      } catch {
        // A timeout/network failure is counted below only for the active probe.
      } finally {
        clearTimeout(request.timeout)
        if (mounted && active === request) {
          active = null
          // HTTP is authoritative: WebSockets can reconnect during healthy I/O.
          // Two failures show reconnecting; three show disconnected.
          failures = ok ? 0 : failures + 1
          if (ok) setState("connected")
          else if (failures >= 3) setState("disconnected")
          else if (failures >= 2) setState("reconnecting")
        }
      }
    }

    function stopPolling() {
      if (interval !== null) clearInterval(interval)
      interval = null
      cancelProbe()
    }

    function onVisibilityChange() {
      stopPolling()
      if (!document.hidden) {
        void poll()
        interval = setInterval(() => { void poll() }, 8000)
      }
    }

    wsClient.connect()
    retryRef.current = () => {
      wsClient.reconnect()
      setState("reconnecting")
      void poll(true)
    }
    document.addEventListener("visibilitychange", onVisibilityChange)
    onVisibilityChange()
    return () => {
      mounted = false
      retryRef.current = null
      document.removeEventListener("visibilitychange", onVisibilityChange)
      stopPolling()
      wsClient.disconnect()
    }
  }, [])

  return (
    <ConnectionContext.Provider value={{ state, retry }}>
      {children}
    </ConnectionContext.Provider>
  )
}
