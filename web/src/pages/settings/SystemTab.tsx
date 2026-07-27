import { useEffect, useState } from "react"
import {
  Download,
  Settings as SettingsIcon,
  Wand2,
  ShieldCheck,
  Check,
  X,
  Loader2,
  FileText,
} from "lucide-react"
import { PrefCard, PrefGrid } from "@/components/settings/PrefCard"
import { ConfigBackupSection } from "@/components/settings/sections/ConfigBackupSection"
import { StorageRepairCard } from "@/components/settings/sections/StorageRepairCard"
import { cn } from "@/lib/utils"

interface Props {
  onOpenRawConfig: () => void
  onOpenWizard: () => void
  /** App version, written into the export file header. */
  version?: string | null
  /** Device hostname, written into the export file header. */
  hostname?: string | null
}

/**
 * Settings > System: the admin and maintenance surface. Config backup,
 * export and raw config; the setup wizard and resources; the analytics
 * opt-in and its disclosure.
 */
export function SystemTab({ onOpenRawConfig, onOpenWizard, version, hostname }: Props) {
  // Writes a bash-sourceable .conf: active settings as `export KEY='value'`,
  // defaults commented out. Web UI preferences (the JSON kv-store at
  // /mutable/.dashusb_preferences.json) are appended as `# preference:` lines
  // so sourcing the file cannot pollute the bash namespace. Single quotes in
  // values use the '\'' escape so the file stays valid bash.
  async function exportConfig(): Promise<void> {
    try {
      const [configRes, prefsRes] = await Promise.all([
        fetch("/api/setup/config"),
        fetch("/api/config/preference"),
      ])
      if (!configRes.ok) throw new Error("Failed to read config")
      const config = (await configRes.json()) as Record<
        string,
        { value: string; active: boolean }
      >
      const prefs = prefsRes.ok
        ? ((await prefsRes.json()) as Record<string, unknown>)
        : {}

      const now = new Date().toISOString()
      const ver = version || "unknown"
      const host = hostname || "dashusb"
      const escape = (s: string) => (s ?? "").replace(/'/g, "'\\''")

      let content = ""
      content += `# dashusb.conf — exported from Dash USB UI\n`
      content += `# Exported:  ${now}\n`
      content += `# Hostname:  ${host}\n`
      content += `# Version:   ${ver}\n`
      content += `#\n`
      content += `# This file is bash-sourceable. Active settings are 'export' lines;\n`
      content += `# inactive/default values are commented out for reference.\n`
      content += `\n`
      content += `# === Setup configuration ===\n`

      const keys = Object.keys(config).sort()
      for (const k of keys) {
        const e = config[k]
        const v = escape(e.value ?? "")
        if (e.active) {
          content += `export ${k}='${v}'\n`
        } else {
          content += `# export ${k}='${v}'\n`
        }
      }

      const prefKeys = Object.keys(prefs).sort()
      if (prefKeys.length > 0) {
        content += `\n`
        content += `# === Web UI preferences (Dash USB Rusty) ===\n`
        content += `# Managed via the web UI; stored in /mutable/.dashusb_preferences.json.\n`
        content += `# Listed here for export completeness — these are NOT sourced by bash.\n`
        for (const k of prefKeys) {
          const v = prefs[k]
          content += `# preference: ${k} = ${JSON.stringify(v)}\n`
        }
      }

      const blob = new Blob([content], { type: "text/plain" })
      const url = URL.createObjectURL(blob)
      const a = document.createElement("a")
      a.href = url
      a.download = "dashusb.conf"
      a.click()
      URL.revokeObjectURL(url)
    } catch {
      /* ignore */
    }
  }

  return (
    <PrefGrid min={300}>
      {/* Config Backup and Configuration share one masonry column so the
          Configuration card sits directly under Config Backup. The
          height-balanced grid will not reliably stack two separate siblings. */}
      <div className="flex flex-col gap-2.5">
        <ConfigBackupSection />
        <PrefCard
          icon={<SettingsIcon className="h-3.5 w-3.5" />}
          halo="slate"
          title="Configuration"
        >
          <p className="t-xs">
            Edit individual settings keys directly, or export your full active
            configuration as a single shell-format file — handy for migrating to a
            new Pi or sharing a recipe.
          </p>
          <div className="flex flex-wrap gap-2">
            <button
              onClick={onOpenRawConfig}
              className="rounded-lg border border-white/10 bg-white/5 px-3 py-1.5 text-xs font-medium text-slate-300 transition-colors hover:bg-white/10"
            >
              <SettingsIcon className="mr-1.5 inline h-3.5 w-3.5" />
              Open editor
            </button>
            <button
              onClick={exportConfig}
              className="rounded-lg border border-white/10 bg-white/5 px-3 py-1.5 text-xs font-medium text-slate-300 transition-colors hover:bg-white/10"
            >
              <Download className="mr-1.5 inline h-3.5 w-3.5" />
              Download dashusb.conf
            </button>
          </div>
        </PrefCard>
      </div>

      <StorageRepairCard />

      <PrefCard
        icon={<Wand2 className="h-3.5 w-3.5" />}
        halo="accent"
        title="Setup Wizard"
      >
        <p className="t-xs">
          Re-run the first-time setup wizard to reconfigure WiFi, drives, time zones and units.
          Safe to run any time — your existing config is the starting point.
        </p>
        <button
          onClick={onOpenWizard}
          className="self-start rounded-lg bg-blue-500/15 px-3 py-1.5 text-xs font-medium text-blue-400 transition-colors hover:bg-blue-500/25"
        >
          Launch Wizard
        </button>
        <div className="tile-divider" />
        <p className="section-label">Resources</p>
        <div className="flex flex-col gap-1">
          <a
            href="https://github.com/Sentry-Six/Dash-USB"
            target="_blank"
            rel="noopener noreferrer"
            className="t-sm text-blue-400 hover:text-blue-300"
          >
            GitHub repository ↗
          </a>
          <a
            href="https://discord.gg/9QZEzVwdnt"
            target="_blank"
            rel="noopener noreferrer"
            className="t-sm text-violet-400 hover:text-violet-300"
          >
            Discord community ↗
          </a>
        </div>
      </PrefCard>

      <PrivacyCards />
    </PrefGrid>
  )
}

/**
 * Settings > System > Privacy. Reviewing the disclosure and flipping the
 * analytics opt-in must stay available at any time: this is the GDPR Art. 21
 * right-to-object mechanism for legitimate-interest processing, by automated
 * means and with no email required.
 */
function PrivacyCards() {
  const [choice, setChoice] = useState<boolean | null>(null)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [loaded, setLoaded] = useState(false)

  useEffect(() => {
    fetch("/api/config/preference?key=analytics_opt_in")
      .then((r) => r.json())
      .then((data) => {
        if (typeof data?.value === "boolean") setChoice(data.value)
        setLoaded(true)
      })
      .catch(() => setLoaded(true))
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
      setError(`Couldn't save: ${e instanceof Error ? e.message : String(e)}`)
    } finally {
      setSaving(false)
    }
  }

  return (
    <>
      <PrefCard
        icon={<ShieldCheck className="h-3.5 w-3.5" />}
        halo="accent"
        title="Analytics opt-in"
      >
        <p className="t-xs">
          When opted in, daily update checks include a one-way hashed device
          ID (derived from your board serial) so we can count unique installs
          without double-counting reinstalls. When opted out, nothing
          identifying is sent on update checks.
        </p>

        <div className="mt-1 flex flex-col gap-2 sm:flex-row">
          <button
            type="button"
            disabled={saving || !loaded}
            onClick={() => persist(true)}
            className={cn(
              "flex flex-1 items-center justify-center gap-2 rounded-lg border px-3 py-2 text-xs font-medium transition-colors disabled:opacity-50",
              choice === true
                ? "border-emerald-400/60 bg-emerald-500/15 text-emerald-200"
                : "border-white/10 bg-white/[0.02] text-slate-300 hover:border-white/20 hover:bg-white/[0.05]"
            )}
          >
            {saving && choice !== true ? (
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
            ) : (
              <Check className="h-3.5 w-3.5" />
            )}
            Opted in
          </button>
          <button
            type="button"
            disabled={saving || !loaded}
            onClick={() => persist(false)}
            className={cn(
              "flex flex-1 items-center justify-center gap-2 rounded-lg border px-3 py-2 text-xs font-medium transition-colors disabled:opacity-50",
              choice === false
                ? "border-slate-400/60 bg-slate-500/15 text-slate-200"
                : "border-white/10 bg-white/[0.02] text-slate-300 hover:border-white/20 hover:bg-white/[0.05]"
            )}
          >
            {saving && choice !== false ? (
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
            ) : (
              <X className="h-3.5 w-3.5" />
            )}
            Opted out
          </button>
        </div>

        {choice === null && loaded && !error && (
          <p className="mt-2 text-[11px] text-slate-500">
            Not set. Default is opted out — no choice means no tracking.
          </p>
        )}
        {error && <p className="mt-2 text-[11px] text-rose-400">{error}</p>}
      </PrefCard>

      <PrefCard
        icon={<FileText className="h-3.5 w-3.5" />}
        halo="slate"
        title="What we send, and when"
      >
        <div className="divide-y divide-white/5">
          <FlowRow
            when="Daily update check"
            what="Version, architecture, board model"
            note="Device identifier only if opted in above."
          />
          <FlowRow
            when="Once per install"
            what="Empty ping (no body, no identifier)"
            note="Anonymous gross-install counter."
          />
          <FlowRow
            when="iOS push pairing"
            what="A random pairing ID"
            note="Not tied to your hardware."
          />
        </div>
        <div className="tile-divider" />
        <div className="flex flex-col gap-1">
          <a
            href="https://sentry-six.com/privacy"
            target="_blank"
            rel="noopener noreferrer"
            className="t-sm text-blue-400 hover:text-blue-300"
          >
            Full privacy policy ↗
          </a>
          <a
            href="https://github.com/Sentry-Six/Dash-USB/wiki/Privacy"
            target="_blank"
            rel="noopener noreferrer"
            className="t-sm text-blue-400 hover:text-blue-300"
          >
            Wiki: what each flow does ↗
          </a>
        </div>
      </PrefCard>
    </>
  )
}

function FlowRow({
  when,
  what,
  note,
}: {
  when: string
  what: string
  note?: string
}) {
  return (
    <div className="py-2">
      <p className="text-xs font-semibold text-slate-300">{when}</p>
      <p className="mt-0.5 text-[11px] text-slate-400">{what}</p>
      {note && (
        <p className="mt-0.5 text-[11px] italic text-slate-500/80">{note}</p>
      )}
    </div>
  )
}
