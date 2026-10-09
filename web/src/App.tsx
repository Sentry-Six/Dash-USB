import { useEffect, useState, lazy, Suspense } from "react"
import { BrowserRouter, Routes, Route } from "react-router-dom"
import { Loader2, AlertTriangle } from "lucide-react"
import { AppShell } from "@/components/layout/AppShell"
import { SetupWizard } from "@/components/setup/SetupWizard"
import { SetupProgress } from "@/components/setup/SetupProgress"
import { AuthProvider, useAuth } from "@/hooks/useAuth"
import { ErrorBoundary } from "@/components/ErrorBoundary"

// One chunk per route keeps heavy deps (xterm, via Terminal) out of the
// first paint, and keeps the shell off the unauthenticated Login path.
const Dashboard = lazy(() => import("@/pages/Dashboard"))
const Viewer = lazy(() => import("@/pages/Viewer"))
const Files = lazy(() => import("@/pages/Files"))
const Logs = lazy(() => import("@/pages/Logs"))
const Settings = lazy(() => import("@/pages/Settings"))
const Support = lazy(() => import("@/pages/Support"))
const Terminal = lazy(() => import("@/pages/Terminal"))
const Notifications = lazy(() => import("@/pages/Notifications"))
const Snapshots = lazy(() => import("@/pages/Snapshots"))
const Login = lazy(() => import("@/pages/Login"))

type AppState = "loading" | "setup" | "configuring" | "finalizing" | "ready" | "setup_error"

type SetupErrorInfo = { kind?: string; message?: string }

export default function App() {
  return (
    <ErrorBoundary fallback={(error) => <CrashScreen error={error} />}>
      <AuthProvider>
        <AppContent />
      </AuthProvider>
    </ErrorBoundary>
  )
}

/** Last-resort fallback for uncaught root render errors. */
function CrashScreen({ error }: { error: Error }) {
  return (
    <div className="flex h-screen items-center justify-center bg-slate-950 p-4">
      <div className="flex w-full max-w-md flex-col items-center gap-4 rounded-2xl border border-white/10 bg-white/[0.03] p-8 text-center">
        <h2 className="text-lg font-semibold text-slate-100">Something went wrong</h2>
        <p className="break-all text-xs text-slate-500">{String(error?.message ?? error)}</p>
        <button
          onClick={() => window.location.reload()}
          className="rounded-lg bg-blue-500/15 px-4 py-2 text-sm font-medium text-blue-400 transition-colors hover:bg-blue-500/25"
        >
          Reload
        </button>
      </div>
    </div>
  )
}

function AppContent() {
  const [appState, setAppState] = useState<AppState>("loading")
  const [setupError, setSetupError] = useState<SetupErrorInfo | null>(null)
  const { state: authState, login } = useAuth()

  useEffect(() => {
    let cancelled = false
    async function checkStatus() {
      try {
        const res = await fetch("/api/setup/status")
        const data = await res.json()
        if (cancelled) return
        if (data.setup_finished) {
          setAppState("ready")
        } else if (data.error) {
          // Replace the running spinner with the recoverable error state.
          setSetupError(data.error)
          setAppState("setup_error")
        } else if (data.setup_running) {
          setAppState("configuring")
        } else {
          setAppState("setup")
        }
      } catch {
        if (!cancelled) setAppState("ready")
      }
    }
    checkStatus()
    return () => { cancelled = true }
  }, [])

  // The finished marker precedes the final reboot; wait for that reboot.
  useEffect(() => {
    if (appState !== "configuring") return
    const interval = setInterval(async () => {
      try {
        const res = await fetch("/api/setup/status")
        const data = await res.json()
        if (data.setup_finished) {
          setAppState("finalizing")
        } else if (data.error) {
          // Surface stopped setup instead of spinning indefinitely.
          setSetupError(data.error)
          setAppState("setup_error")
        }
      } catch {
        // Server rebooting: keep polling.
      }
    }, 3000)
    return () => clearInterval(interval)
  }, [appState])

  // Require an outage before accepting a response as post-reboot.
  useEffect(() => {
    if (appState !== "finalizing") return
    let wentDown = false
    const id = setInterval(async () => {
      try {
        const res = await fetch("/api/setup/status")
        if (res.ok && wentDown) {
          setAppState("ready")
        }
      } catch {
        wentDown = true
      }
    }, 2000)
    return () => clearInterval(id)
  }, [appState])

  if (appState === "loading") {
    return (
      <div className="flex h-screen items-center justify-center bg-slate-950">
        <div className="h-6 w-6 animate-spin rounded-full border-2 border-blue-500 border-t-transparent" />
      </div>
    )
  }

  if (appState === "setup_error") {
    const isConfig = setupError?.kind === "config"
    return (
      <div className="flex min-h-screen items-center justify-center bg-slate-950 p-4">
        <div className="flex w-full max-w-lg flex-col items-center gap-6 rounded-2xl border border-amber-500/20 bg-white/[0.03] p-10 text-center">
          <div className="flex h-16 w-16 items-center justify-center rounded-full bg-amber-500/20">
            <AlertTriangle className="h-8 w-8 text-amber-400" />
          </div>
          <div>
            <h2 className="text-xl font-semibold text-slate-100">
              {isConfig ? "Setup needs a configuration fix" : "Setup hit a problem"}
            </h2>
            <p className="mt-2 text-sm text-slate-400">
              {setupError?.message || "Setup could not finish."}
            </p>
            <p className="mt-4 text-xs text-slate-600">
              {isConfig
                ? "Adjust your settings and re-run setup. Your recordings are not affected."
                : "Retry setup, or open the configuration to make changes."}
            </p>
          </div>
          <div className="flex flex-wrap items-center justify-center gap-3">
            <button
              onClick={() => { setSetupError(null); setAppState("setup") }}
              className="rounded-lg bg-blue-500/15 px-4 py-2 text-sm font-medium text-blue-400 transition-colors hover:bg-blue-500/25"
            >
              Open setup to fix
            </button>
            {!isConfig && (
              <button
                onClick={async () => {
                  setSetupError(null)
                  setAppState("configuring")
                  try {
                    await fetch("/api/setup/run", { method: "POST" })
                  } catch {
                    /* poll loop will re-surface any error */
                  }
                }}
                className="rounded-lg border border-white/10 px-4 py-2 text-sm font-medium text-slate-300 transition-colors hover:bg-white/[0.06]"
              >
                Retry setup
              </button>
            )}
          </div>
        </div>
      </div>
    )
  }

  if (appState === "configuring") {
    return (
      <div className="flex h-screen items-center justify-center bg-slate-950">
        <div className="flex w-full max-w-lg flex-col items-center gap-6 rounded-2xl border border-white/10 bg-white/[0.03] p-10 text-center">
          <div className="flex h-16 w-16 items-center justify-center rounded-full bg-blue-500/20">
            <Loader2 className="h-8 w-8 animate-spin text-blue-400" />
          </div>
          <div>
            <h2 className="text-xl font-semibold text-slate-100">Setting Up Dash USB</h2>
            <p className="mt-2 text-sm text-slate-400">
              Setup is in progress. The device will reboot several times — this is normal.
            </p>
            <p className="mt-4 text-xs text-slate-600">
              This page will automatically refresh when setup is complete.
              Do not power off the device. This may take 10–20 minutes.
            </p>
          </div>
          <SetupProgress />
        </div>
      </div>
    )
  }

  // Hold until the network drops and recovers after final setup.
  if (appState === "finalizing") {
    return (
      <div className="flex h-screen items-center justify-center bg-slate-950">
        <div className="flex w-full max-w-lg flex-col items-center gap-6 rounded-2xl border border-white/10 bg-white/[0.03] p-10 text-center">
          <div className="flex h-16 w-16 items-center justify-center rounded-full bg-emerald-500/20">
            <Loader2 className="h-8 w-8 animate-spin text-emerald-400" />
          </div>
          <div>
            <h2 className="text-xl font-semibold text-slate-100">Almost Done!</h2>
            <p className="mt-2 text-sm text-slate-400">
              Setup complete. Rebooting one last time to apply everything — this page will
              redirect automatically once Dash USB is back online.
            </p>
          </div>
        </div>
      </div>
    )
  }

  if (appState === "setup") {
    return (
      <div className="min-h-screen bg-slate-950 p-4">
        <SetupWizard onClose={() => setAppState("ready")} />
      </div>
    )
  }

  if (authState === "loading") {
    return (
      <div className="flex h-screen items-center justify-center bg-slate-950">
        <div className="h-6 w-6 animate-spin rounded-full border-2 border-blue-500 border-t-transparent" />
      </div>
    )
  }

  if (authState === "unauthenticated") {
    return (
      <Suspense fallback={<RouteFallback />}>
        <Login onLogin={login} />
      </Suspense>
    )
  }

  return (
    <BrowserRouter>
      <Suspense fallback={<RouteFallback />}>
        <Routes>
          <Route element={<AppShell />}>
            <Route path="/" element={<Dashboard />} />
            <Route path="/viewer" element={<Viewer />} />
            <Route path="/files" element={<Files />} />
            <Route path="/logs" element={<Logs />} />
            <Route path="/support" element={<Support />} />
            <Route path="/terminal" element={<Terminal />} />
            <Route path="/notifications" element={<Notifications />} />
            <Route path="/snapshots" element={<Snapshots />} />
            <Route path="/settings" element={<Settings />} />
          </Route>
        </Routes>
      </Suspense>
    </BrowserRouter>
  )
}

function RouteFallback() {
  return (
    <div className="flex h-screen items-center justify-center bg-slate-950">
      <Loader2 className="h-6 w-6 animate-spin text-blue-400" />
    </div>
  )
}
