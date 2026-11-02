import { createContext, useContext, useState, useEffect, type ReactNode } from "react"

type AuthState = "loading" | "authenticated" | "unauthenticated"

interface AuthContextValue {
  state: AuthState
  authRequired: boolean
  login: (username: string, password: string) => Promise<string | null>
  logout: () => Promise<void>
}

const AuthContext = createContext<AuthContextValue>({
  state: "loading",
  authRequired: false,
  login: async () => null,
  logout: async () => {},
})

export function useAuth() {
  return useContext(AuthContext)
}

export function AuthProvider({ children }: { children: ReactNode }) {
  const [state, setState] = useState<AuthState>("loading")
  const [authRequired, setAuthRequired] = useState(false)

  async function login(username: string, password: string): Promise<string | null> {
    try {
      const res = await fetch("/api/auth/login", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ username, password }),
      })
      if (res.ok) {
        setState("authenticated")
        return null
      }
      const data = await res.json()
      return data.error || "Invalid username or password"
    } catch {
      return "Connection failed. Please try again."
    }
  }

  async function logout() {
    await fetch("/api/auth/logout", { method: "POST" }).catch(() => {})
    setState("unauthenticated")
  }

  useEffect(() => {
    let stopped = false
    // State updates run only in the request callbacks, never synchronously.
    const checkAuth = () => {
      fetch("/api/auth/check")
        .then((res) => res.json())
        .then((data) => {
          if (stopped) return
          setAuthRequired(data.auth_required)
          setState(data.authenticated || !data.auth_required ? "authenticated" : "unauthenticated")
        })
        .catch(() => {
          // Connectivity handling, rather than auth UI, reports an unavailable server.
          if (!stopped) setState("authenticated")
        })
    }
    checkAuth()
    // Server restarts invalidate the in-memory session store.
    const iv = setInterval(checkAuth, 10_000)
    return () => {
      stopped = true
      clearInterval(iv)
    }
  }, [])

  return (
    <AuthContext.Provider value={{ state, authRequired, login, logout }}>
      {children}
    </AuthContext.Provider>
  )
}
