import { useEffect, useState } from "react"

/**
 * Current installed version of Dash USB, from `/api/system/version`.
 * Returns `null` until the first fetch settles, then the version string
 * (or "unknown" if the endpoint failed).
 */
export function useVersion(): string | null {
  const [version, setVersion] = useState<string | null>(null)

  useEffect(() => {
    let cancelled = false
    fetch("/api/system/version")
      .then((r) => r.json())
      .then((data) => {
        if (!cancelled) setVersion(data.version || "unknown")
      })
      .catch(() => {
        if (!cancelled) setVersion("unknown")
      })
    return () => {
      cancelled = true
    }
  }, [])

  return version
}
