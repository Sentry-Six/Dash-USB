import { useEffect, useState } from "react"
import { ShieldCheck, Check, X, Loader2 } from "lucide-react"
import type { StepProps } from "../SetupWizard"
import { cn } from "@/lib/utils"

/**
 * Privacy disclosure and analytics opt-in. Two GDPR requirements ride on
 * this step:
 *
 * 1. Article 13 transparency at the point of collection: the disclosure
 *    table must list every outbound data flow before the user can click
 *    past this screen.
 *
 * 2. Article 21 right to object by automated means: the opt-in requires
 *    explicit affirmative action, so no pre-ticked default (CJEU Planet49,
 *    Art. 4(11)), and both buttons keep equal visual weight to avoid the
 *    dark-pattern asymmetry flagged by EDPB Guidelines 03/2022.
 *
 * The opt-in writes `analytics_opt_in` immediately on click, independent of
 * the wizard's Apply flow, so the choice survives backing out of the wizard
 * and the next update check already honours it.
 */
export function PrivacyStep(_props: StepProps) {
  const [choice, setChoice] = useState<boolean | null>(null)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)

  // Load existing value on mount so re-running setup shows the current state.
  useEffect(() => {
    fetch("/api/config/preference?key=analytics_opt_in")
      .then((r) => r.json())
      .then((data) => {
        if (typeof data?.value === "boolean") setChoice(data.value)
      })
      .catch(() => {
        // Pref not set yet. Leave it null so neither button is highlighted
        // and the choice stays explicit.
      })
  }, [])

  async function persist(value: boolean) {
    setSaving(true)
    setError(null)
    try {
      const res = await fetch("/api/config/preference", {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ key: "analytics_opt_in", value }),
      })
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
      setChoice(value)
    } catch (e) {
      setError(`Couldn't save preference: ${e instanceof Error ? e.message : String(e)}`)
    } finally {
      setSaving(false)
    }
  }

  return (
    <div className="flex flex-col items-center py-6">
      <div className="mb-6 flex h-20 w-20 items-center justify-center rounded-2xl bg-emerald-500/15">
        <ShieldCheck className="h-10 w-10 text-emerald-400" />
      </div>

      <h2 className="text-center text-2xl font-bold text-slate-100">
        Privacy
      </h2>
      <p className="mt-3 max-w-xl text-center text-sm leading-relaxed text-slate-400">
        Before going further, here's everything Dash USB sends from your
        device and when — so you know what's leaving your network before it
        does.
      </p>

      {/* Article 13 transparency at the point of collection. */}
      <div className="mt-8 w-full max-w-2xl rounded-xl border border-white/10 bg-white/[0.02] p-5">
        <p className="mb-3 text-xs font-semibold uppercase tracking-wider text-slate-400">
          What we send, when, and why
        </p>
        <div className="divide-y divide-white/5">
          <FlowRow
            when="Daily update check"
            what="Software version, CPU architecture, board model"
            why="Detect vulnerable builds, ship compatible binaries"
            note="No device identifier sent unless you opt in below."
          />
          <FlowRow
            when="Once per install"
            what="Empty ping (no body, no identifier)"
            why="Count gross install volume on the server"
            note="Anonymous. There's nothing to opt out of."
          />
          <FlowRow
            when="If you enable iOS push notifications"
            what="A randomly-generated device pairing ID"
            why="Routing push notifications to your phone"
            note="Not tied to your hardware. Cleared when you unpair."
          />
        </div>
        <p className="mt-4 text-[11px] leading-relaxed text-slate-500">
          Full policy:{" "}
          <a
            href="https://sentry-six.com/privacy"
            target="_blank"
            rel="noopener noreferrer"
            className="text-slate-400 underline hover:text-slate-300"
          >
            sentry-six.com/privacy
          </a>
          . Source code:{" "}
          <a
            href="https://github.com/Sentry-Six/Dash-USB"
            target="_blank"
            rel="noopener noreferrer"
            className="text-slate-400 underline hover:text-slate-300"
          >
            github.com/Sentry-Six/Dash-USB
          </a>
          .
        </p>
      </div>

      {/* Explicit affirmative action; must never be pre-ticked. */}
      <div className="mt-6 w-full max-w-2xl rounded-xl border border-white/10 bg-white/[0.02] p-5">
        <p className="text-sm font-semibold text-slate-200">
          Help us count new installs?
        </p>
        <p className="mt-2 text-xs leading-relaxed text-slate-400">
          If you opt in, daily update checks will include a one-way hashed
          device ID (derived from your board's serial number) so we can tell
          how many unique devices are running each version, without double-
          counting reinstalls. You can change this any time in Settings →
          Privacy.
        </p>

        <div className="mt-4 flex flex-col gap-2 sm:flex-row">
          <button
            type="button"
            disabled={saving}
            onClick={() => persist(true)}
            className={cn(
              "flex flex-1 items-center justify-center gap-2 rounded-lg border px-4 py-3 text-sm font-medium transition-colors disabled:opacity-50",
              choice === true
                ? "border-emerald-400/60 bg-emerald-500/15 text-emerald-200"
                : "border-white/10 bg-white/[0.02] text-slate-300 hover:border-white/20 hover:bg-white/[0.05]"
            )}
          >
            {saving && choice !== true ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <Check className="h-4 w-4" />
            )}
            Yes, count me
          </button>
          <button
            type="button"
            disabled={saving}
            onClick={() => persist(false)}
            className={cn(
              "flex flex-1 items-center justify-center gap-2 rounded-lg border px-4 py-3 text-sm font-medium transition-colors disabled:opacity-50",
              choice === false
                ? "border-slate-400/60 bg-slate-500/15 text-slate-200"
                : "border-white/10 bg-white/[0.02] text-slate-300 hover:border-white/20 hover:bg-white/[0.05]"
            )}
          >
            {saving && choice !== false ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <X className="h-4 w-4" />
            )}
            No thanks
          </button>
        </div>

        {choice === null && !error && (
          <p className="mt-3 text-[11px] text-slate-500">
            You can leave this unanswered and continue — no choice means no
            tracking. Default is opted out.
          </p>
        )}
        {choice !== null && !error && (
          <p className="mt-3 text-[11px] text-emerald-300/70">
            Saved. You can change this any time in Settings → Privacy.
          </p>
        )}
        {error && (
          <p className="mt-3 text-[11px] text-rose-400">
            {error}
          </p>
        )}
      </div>
    </div>
  )
}

function FlowRow({
  when,
  what,
  why,
  note,
}: {
  when: string
  what: string
  why: string
  note?: string
}) {
  return (
    <div className="grid grid-cols-1 gap-1 py-3 sm:grid-cols-[140px_1fr]">
      <div className="text-xs font-semibold text-slate-300">{when}</div>
      <div>
        <p className="text-xs text-slate-300">{what}</p>
        <p className="mt-0.5 text-[11px] text-slate-500">{why}</p>
        {note && (
          <p className="mt-1 text-[11px] italic text-slate-500/80">{note}</p>
        )}
      </div>
    </div>
  )
}
