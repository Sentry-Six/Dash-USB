import { Component, type ReactNode } from "react"
import { AlertTriangle, RotateCw } from "lucide-react"

interface Props {
  children: ReactNode
  /** Rendered in place of children after a render error. Receives the error
   *  and a reset callback that re-attempts rendering the children. */
  fallback: (error: Error, reset: () => void) => ReactNode
}

interface State {
  error: Error | null
}

/** Contain render failures so the root remains recoverable. */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null }

  static getDerivedStateFromError(error: Error): State {
    return { error }
  }

  reset = () => this.setState({ error: null })

  render() {
    if (this.state.error) {
      return this.props.fallback(this.state.error, this.reset)
    }
    return this.props.children
  }
}

/** Degrade a failed settings section to one retryable card. */
export function SectionErrorBoundary({ children }: { children: ReactNode }) {
  return (
    <ErrorBoundary
      fallback={(error, reset) => (
        <div className="glass-card flex flex-col gap-2 p-3.5">
          <div className="flex items-center gap-2 text-xs font-medium text-amber-400">
            <AlertTriangle className="h-3.5 w-3.5 shrink-0" />
            This card hit an error
          </div>
          <p className="break-all text-[11px] text-slate-500">
            {String(error?.message ?? error)}
          </p>
          <button
            type="button"
            onClick={reset}
            className="inline-flex w-fit items-center gap-1.5 rounded-md border border-white/10 bg-white/5 px-2.5 py-1 text-xs text-slate-200 transition-colors hover:border-white/20"
          >
            <RotateCw className="h-3 w-3" /> Retry
          </button>
        </div>
      )}
    >
      {children}
    </ErrorBoundary>
  )
}
