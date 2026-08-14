import { useState, useCallback, useEffect, useRef } from "react"
import { ChevronLeft, ChevronRight, Check, Loader2, AlertCircle, AlertTriangle } from "lucide-react"
import { cn } from "@/lib/utils"
import { SetupProgress } from "./SetupProgress"
import { WelcomeStep } from "./steps/WelcomeStep"
import { PrivacyStep } from "./steps/PrivacyStep"
import { NetworkStep } from "./steps/NetworkStep"
import { StorageStep } from "./steps/StorageStep"
import { ArchiveStep } from "./steps/ArchiveStep"
import { NotificationsStep } from "./steps/NotificationsStep"
import { SecurityStep } from "./steps/SecurityStep"
import { AdvancedStep } from "./steps/AdvancedStep"
import { ReviewStep } from "./steps/ReviewStep"

interface SetupFormData {
  [key: string]: string
}

interface StepDef {
  id: string
  title: string
  component: React.ComponentType<StepProps>
}

export interface StepProps {
  data: SetupFormData
  onChange: (key: string, value: string) => void
  onBatchChange: (updates: Record<string, string>) => void
  setupAlreadyFinished: boolean
}

function storageError(data: SetupFormData): string | null {
  // Zero disables the image, which is invalid for this setup flow.
  const cam = parseFloat(data.CAM_SIZE ?? "0")
  if (!Number.isFinite(cam) || cam <= 0) {
    return "Dashcam drive size must be greater than 0 GB."
  }
  // GM refuses drives under 64 GB (needs 64 GB total / 32 GB available).
  if (cam < 64) {
    return "GM requires a dashcam drive of at least 64 GB."
  }
  return null
}

function archiveError(data: SetupFormData): string | null {
  const system = data.ARCHIVE_SYSTEM ?? "cifs"
  if (system === "none") return null
  if (system === "cifs") {
    if (!data.ARCHIVE_SERVER?.trim()) return "Archive Server is required."
    if (!data.SHARE_NAME?.trim()) return "Share Name is required."
    if (!data.SHARE_USER?.trim()) return "Username is required."
    if (!data.SHARE_PASSWORD?.trim()) return "Password is required."
  } else if (system === "rsync") {
    if (!data.RSYNC_SERVER?.trim()) return "Server is required."
    if (!data.RSYNC_USER?.trim()) return "Username is required."
    if (!data.RSYNC_PATH?.trim()) return "Remote Path is required."
  } else if (system === "rclone") {
    if (!data.RCLONE_DRIVE?.trim()) return "Remote Name is required."
    if (!data.RCLONE_PATH?.trim()) return "Remote Path is required."
    // Rclone remote names cannot serve as archiveloop's connectivity host.
    if (!data.ARCHIVE_SERVER?.trim()) return "Archive Server (for connectivity check) is required for rclone."
  } else if (system === "nfs") {
    if (!data.ARCHIVE_SERVER?.trim()) return "NFS Server is required."
    if (!data.SHARE_NAME?.trim()) return "Export Path is required."
  }
  return null
}

function notificationsError(data: SetupFormData): string | null {
  // Reject partially configured providers.
  const requiredPerProvider: [string, string[]][] = [
    ["Pushover", ["PUSHOVER_USER_KEY", "PUSHOVER_APP_KEY"]],
    ["Gotify", ["GOTIFY_DOMAIN", "GOTIFY_APP_TOKEN"]],
    ["Discord", ["DISCORD_WEBHOOK_URL"]],
    ["Telegram", ["TELEGRAM_CHAT_ID", "TELEGRAM_BOT_TOKEN"]],
    ["IFTTT", ["IFTTT_EVENT_NAME", "IFTTT_KEY"]],
    ["Slack", ["SLACK_WEBHOOK_URL"]],
    ["Signal", ["SIGNAL_URL", "SIGNAL_FROM_NUM", "SIGNAL_TO_NUM"]],
    ["Matrix", ["MATRIX_SERVER_URL", "MATRIX_USERNAME", "MATRIX_PASSWORD", "MATRIX_ROOM"]],
    ["AWS SNS", ["AWS_REGION", "AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SNS_TOPIC_ARN"]],
    ["Webhook", ["WEBHOOK_URL"]],
  ]
  for (const [label, fields] of requiredPerProvider) {
    const hasAny = fields.some((f) => (data[f] ?? "").trim() !== "")
    const hasAll = fields.every((f) => (data[f] ?? "").trim() !== "")
    if (hasAny && !hasAll) return `Complete all required fields for ${label}.`
  }
  return null
}

function securityError(data: SetupFormData): string | null {
  // Authentication requires both credentials or neither.
  const u = data.WEB_USERNAME?.trim() ?? ""
  const p = data.WEB_PASSWORD?.trim() ?? ""
  if (u && !p) return "Web Password is required when a Web Username is set."
  if (p && !u) return "Web Username is required when a Web Password is set."
  return null
}

function getStepError(stepIdx: number, data: SetupFormData): string | null {
  switch (stepIdx) {
    case 3: return storageError(data)
    case 4: return archiveError(data)
    case 5: return notificationsError(data)
    case 6: return securityError(data)
    default: return null
  }
}

// These changes recreate the camera image but preserve backingfiles snapshots.
const DESTRUCTIVE_SIZE_KEYS: Record<string, string> = {
  CAM_SIZE: "Dashcam drive (live clips inside)",
}

interface DestructiveChange {
  key: string
  label: string
  reason: string
}

function normalizeSizeValue(val: string | undefined): string {
  if (!val) return "0"
  return val.replace(/G$/i, "").trim() || "0"
}

function detectDestructiveChanges(
  current: SetupFormData,
  original: SetupFormData | undefined,
): DestructiveChange[] {
  if (!original) return []

  const changes: DestructiveChange[] = []

  // A new DATA_DRIVE selects a physical disk that setup will format.
  const oldDataDrive = (original.DATA_DRIVE ?? "").trim()
  const newDataDrive = (current.DATA_DRIVE ?? "").trim()
  if (oldDataDrive && newDataDrive && oldDataDrive !== newDataDrive) {
    changes.push({
      key: "DATA_DRIVE",
      label: `External data drive: ${newDataDrive}`,
      reason:
        `DATA_DRIVE changed from ${oldDataDrive} to ${newDataDrive}. ` +
        `The new drive will be formatted (everything currently on it will be lost). ` +
        `Your old drive (${oldDataDrive}) will be left untouched and unmounted — ` +
        `disconnect it before re-running setup if it's still plugged in.`,
    })
  }

  for (const [key, label] of Object.entries(DESTRUCTIVE_SIZE_KEYS)) {
    const newVal = normalizeSizeValue(current[key])
    const oldVal = normalizeSizeValue(original[key])
    if (newVal !== oldVal) {
      // Image files are recreated because their filesystems are not resized.
      const reason =
        key === "CAM_SIZE"
          ? `CAM_SIZE changed from ${oldVal || "0"}G to ${newVal}G. Live clips currently inside the dashcam drive will be lost. Snapshots (in /backingfiles/snapshots) and other drives are not affected.`
          : `Size changed from ${oldVal || "0"}G to ${newVal}G — only this drive's image will be recreated. Other drives and snapshots are not affected.`
      changes.push({ key, label, reason })
    }
  }

  return changes
}

const steps: StepDef[] = [
  { id: "welcome", title: "Welcome", component: WelcomeStep },
  // Show the data-flow disclosure before setup can send outbound traffic.
  { id: "privacy", title: "Privacy", component: PrivacyStep },
  { id: "network", title: "Network", component: NetworkStep },
  { id: "storage", title: "Storage", component: StorageStep },
  { id: "archive", title: "Archive", component: ArchiveStep },
  { id: "notifications", title: "Notifications", component: NotificationsStep },
  { id: "security", title: "Security", component: SecurityStep },
  { id: "advanced", title: "Advanced", component: AdvancedStep },
  { id: "review", title: "Review", component: ReviewStep },
]

interface SetupWizardProps {
  initialData?: SetupFormData
  onClose: () => void
}

type SetupPhase = "wizard" | "applying" | "running" | "rebooting" | "finalizing" | "complete" | "error"

export function SetupWizard({ initialData, onClose }: SetupWizardProps) {
  const [currentStep, setCurrentStep] = useState(0)
  // Persist UI defaults even when the config does not yet contain them.
  const defaults: SetupFormData = {
    // GM requires a FAT32 drive of at least 64 GB.
    CAM_SIZE: "64",
    ARCHIVE_SYSTEM: "cifs",
    TEMPERATURE_UNIT: "C",
    ARCHIVE_RECORDINGS: "true",
    TEMPERATURE_POSTARCHIVE: "true",
    RTC_BATTERY_ENABLED: "false",
    RTC_TRICKLE_CHARGE: "false",
  }
  const [formData, setFormData] = useState<SetupFormData>({ ...defaults, ...(initialData ?? {}) })
  // Lets Apply read a value committed by blur in the same click.
  const formDataRef = useRef<SetupFormData>(formData)
  const [saving, setSaving] = useState(false)
  const [saveError, setSaveError] = useState<string | null>(null)
  const [phase, setPhase] = useState<SetupPhase>("wizard")
  const [setupMessage, setSetupMessage] = useState("")
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null)
  const originalDataRef = useRef<SetupFormData | undefined>(initialData)
  const [destructiveWarning, setDestructiveWarning] = useState<DestructiveChange[] | null>(null)
  const [isRestoreFlow, setIsRestoreFlow] = useState(false)
  // Distinguish reconfiguration from first-time setup messaging.
  const [setupAlreadyFinished, setSetupAlreadyFinished] = useState(false)
  const [spaceRejection, setSpaceRejection] = useState<string | null>(null)

  // Must update during render; an effect runs too late after Apply's blur.
  // eslint-disable-next-line react-hooks/refs
  formDataRef.current = formData

  const handleChange = useCallback((key: string, value: string) => {
    setFormData((prev) => ({ ...prev, [key]: value }))
  }, [])

  const handleBatchChange = useCallback((updates: Record<string, string>) => {
    setFormData((prev) => ({ ...prev, ...updates }))
    // Compare post-restore edits against restored values, not fresh defaults.
    if (updates._restore_baseline === "true") {
      const baseline = { ...updates }
      delete baseline._restore_baseline
      originalDataRef.current = { ...(originalDataRef.current ?? {}), ...baseline }
      setIsRestoreFlow(true)
    }
  }, [])

  // Detect completed installations for non-destructive reconfiguration copy.
  useEffect(() => {
    let cancelled = false
    fetch("/api/setup/status")
      .then((r) => r.json())
      .then((data) => {
        if (cancelled) return
        setSetupAlreadyFinished(Boolean(data?.setup_finished))
      })
      .catch(() => { /* Assume a fresh install when status is unavailable. */ })
    return () => { cancelled = true }
  }, [])


  useEffect(() => {
    if (phase !== "running" && phase !== "rebooting") return
    pollRef.current = setInterval(async () => {
      try {
        const res = await fetch("/api/setup/status")
        const data = await res.json()
        if (data.error) {
          // Do not mistake a stopped setup for a reboot disconnect.
          setPhase("error")
          setSetupMessage(data.error.message || "Setup failed. Check the log below.")
          if (pollRef.current) clearInterval(pollRef.current)
        } else if (data.setup_finished) {
          // Keep finalizing until the last reboot returns.
          setPhase("finalizing")
          setSetupMessage("Dash USB has finished setting up. The device is now rebooting one last time...")
          if (pollRef.current) clearInterval(pollRef.current)
        } else if (data.setup_running && phase === "rebooting") {
          // Recover from service/I/O disconnects that were not reboots.
          setPhase("running")
          setSetupMessage("Setup is running. The device will reboot several times during this process — this is normal.")
        } else if (!data.setup_running && phase === "running") {
          setPhase("rebooting")
          setSetupMessage("System is rebooting to continue setup. This page will reconnect automatically.")
        }
      } catch {
        // Unreachability is expected during setup reboots.
        if (phase !== "rebooting") {
          setPhase("rebooting")
          setSetupMessage("Waiting for device to come back online after reboot...")
        }
      }
    }, 3000)
    return () => { if (pollRef.current) clearInterval(pollRef.current) }
  }, [phase])

  // Require a confirmed outage before treating a response as post-reboot.
  useEffect(() => {
    if (phase !== "finalizing") return
    let wentDown = false
    const poll = setInterval(async () => {
      try {
        const res = await fetch("/api/setup/status")
        if (res.ok && wentDown) {
          setPhase("complete")
          setSetupMessage("Setup completed successfully! Your device is ready.")
          clearInterval(poll)
        }
      } catch {
        wentDown = true
        setSetupMessage("Waiting for Dash USB to come back online after final reboot...")
      }
    }, 3000)
    return () => clearInterval(poll)
  }, [phase])

  // Stream progress with reconnects while polling remains the fallback.
  useEffect(() => {
    if (phase !== "running" && phase !== "applying" && phase !== "rebooting") return
    let ws: WebSocket | null = null
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null
    let backoff = 2000
    let cancelled = false

    function connect() {
      if (cancelled) return
      try {
        const protocol = window.location.protocol === "https:" ? "wss:" : "ws:"
        ws = new WebSocket(`${protocol}//${window.location.host}/api/ws`)
        ws.onopen = () => { backoff = 2000 }
        ws.onmessage = (event) => {
          try {
            const msg = JSON.parse(event.data)
            if (msg.type === "setup_status") {
              const d = msg.data
              if (d.status === "running") {
                setPhase("running")
                setSetupMessage("Running setup... This may take several minutes.")
              } else if (d.status === "complete") {
                setPhase("finalizing")
                setSetupMessage("Dash USB has finished setting up. The device is now rebooting one last time...")
              } else if (d.status === "rebooting") {
                setPhase("rebooting")
                setSetupMessage(d.message || "System is rebooting to continue setup...")
              } else if (d.status === "error") {
                setPhase("error")
                setSetupMessage(d.error || "Setup failed. Check logs for details.")
              }
            }
          } catch { /* ignore parse errors */ }
        }
        ws.onclose = () => {
          if (cancelled) return
          reconnectTimer = setTimeout(() => {
            backoff = Math.min(backoff * 1.5, 15000)
            connect()
          }, backoff)
        }
      } catch { /* ws not available */ }
    }

    connect()
    return () => {
      cancelled = true
      if (reconnectTimer) clearTimeout(reconnectTimer)
      ws?.close()
    }
  }, [phase])

  const StepComponent = steps[currentStep].component
  const currentStepError = getStepError(currentStep, formData)

  async function doApply(dataToSave: SetupFormData) {
    setSaving(true)
    setSaveError(null)
    setSpaceRejection(null)
    try {
      const sizeFields = new Set(["CAM_SIZE", "INCREASE_ROOT_SIZE"])
      const configData: Record<string, string> = Object.fromEntries(
        Object.entries(dataToSave)
          .filter(([k, v]) => !k.startsWith("_") && v !== "")
          .map(([k, v]) => {
            if (sizeFields.has(k) && /^\d+$/.test(v)) {
              // Match backend size parsing when blur has not added a unit.
              return [k, v + "G"]
            }
            if ((k === "TEMPERATURE_WARNING" || k === "TEMPERATURE_CAUTION") && v && !v.includes("000")) {
              const num = parseFloat(v)
              if (!isNaN(num)) return [k, String(Math.round(num * 1000))]
            }
            return [k, v]
          })
      )

      // Derive enable flags from provider fields at apply time.
      const notificationEnableMap: Record<string, string[]> = {
        PUSHOVER_ENABLED: ["PUSHOVER_USER_KEY", "PUSHOVER_APP_KEY"],
        GOTIFY_ENABLED: ["GOTIFY_DOMAIN", "GOTIFY_APP_TOKEN"],
        DISCORD_ENABLED: ["DISCORD_WEBHOOK_URL"],
        TELEGRAM_ENABLED: ["TELEGRAM_CHAT_ID", "TELEGRAM_BOT_TOKEN"],
        IFTTT_ENABLED: ["IFTTT_EVENT_NAME", "IFTTT_KEY"],
        SLACK_ENABLED: ["SLACK_WEBHOOK_URL"],
        SIGNAL_ENABLED: ["SIGNAL_URL", "SIGNAL_FROM_NUM", "SIGNAL_TO_NUM"],
        MATRIX_ENABLED: ["MATRIX_SERVER_URL", "MATRIX_USERNAME", "MATRIX_PASSWORD", "MATRIX_ROOM"],
        SNS_ENABLED: ["AWS_REGION", "AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SNS_TOPIC_ARN"],
        WEBHOOK_ENABLED: ["WEBHOOK_URL"],
        NTFY_ENABLED: ["NTFY_URL"],
      }
      for (const [enableField, fields] of Object.entries(notificationEnableMap)) {
        configData[enableField] = fields.some((k) => (dataToSave[k] ?? "").trim() !== "") ? "true" : "false"
      }

      // Preflight mounted storage; fresh installs return checked=false.
      try {
        const pfRes = await fetch("/api/setup/preflight", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(configData),
        })
        if (pfRes.ok) {
          const pf = await pfRes.json()
          if (pf?.ok === false && pf?.error) {
            setSpaceRejection(pf.error)
            setPhase("wizard")
            setSaving(false)
            return
          }
        }
      } catch { /* network blip: the real apply path will surface any error */ }

      const res = await fetch("/api/setup/config", {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(configData),
      })
      if (!res.ok) throw new Error("Failed to save configuration")

      // Backup location is a preference, not shell configuration.
      if (dataToSave._BACKUP_LOCATION) {
        await fetch("/api/config/preference", {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ key: "backup_location", value: dataToSave._BACKUP_LOCATION }),
        }).catch(() => {}) // best-effort
      }

      setPhase("applying")
      setSetupMessage("Configuration saved. Starting setup...")

      const runRes = await fetch("/api/setup/run", { method: "POST" })
      if (!runRes.ok) {
        const err = await runRes.json()
        throw new Error(err.error || "Failed to start setup")
      }

      setPhase("running")
      setSetupMessage("Setup is running. The device will reboot several times during this process — this is normal.")
    } catch (err) {
      setSaveError(err instanceof Error ? err.message : "Unknown error")
      setPhase("wizard")
    } finally {
      setSaving(false)
    }
  }

  async function handleApply() {
    // Flush SizeInput's blur commit before reading formDataRef.
    if (document.activeElement instanceof HTMLElement) {
      document.activeElement.blur()
    }
    await new Promise<void>((r) => requestAnimationFrame(() => r()))
    const data = formDataRef.current

    const firstInvalidIdx = steps.findIndex((_, i) => getStepError(i, data) !== null)
    if (firstInvalidIdx !== -1) {
      setCurrentStep(firstInvalidIdx)
      setSaveError(getStepError(firstInvalidIdx, data))
      return
    }

    const changes = detectDestructiveChanges(data, originalDataRef.current)
    if (changes.length > 0) {
      setDestructiveWarning(changes)
      return
    }

    doApply(data)
  }

  function handleApplyAll() {
    setDestructiveWarning(null)
    doApply(formData)
  }

  function handleSkipDestructive() {
    if (!destructiveWarning || !originalDataRef.current) return
    const safeData = { ...formData }
    for (const change of destructiveWarning) {
      safeData[change.key] = originalDataRef.current[change.key] ?? ""
    }
    setDestructiveWarning(null)
    doApply(safeData)
  }

  const isLast = currentStep === steps.length - 1
  const isFirst = currentStep === 0

  if (destructiveWarning) {
    return (
      <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
        <div className="glass-card setup-wizard-glass flex w-full max-w-lg flex-col gap-5 p-8">
          <div className="flex items-start gap-4">
            <div className="flex h-12 w-12 shrink-0 items-center justify-center rounded-full bg-amber-500/20">
              <AlertTriangle className="h-6 w-6 text-amber-400" />
            </div>
            <div>
              <h2 className="text-lg font-semibold text-slate-100">
                {isRestoreFlow ? "Drive Sizes Changed From Backup" : "Data Will Be Deleted"}
              </h2>
              <p className="mt-1 text-sm text-slate-400">
                {isRestoreFlow
                  ? "You changed drive sizes from what was in your backup. This will cause the SSD to be reformatted, which will erase all existing footage and data on the affected drives."
                  : "The following changes require drive images to be recreated. All data on the affected drives will be permanently lost."}
              </p>
            </div>
          </div>

          <div className="rounded-lg border border-amber-500/20 bg-amber-500/5 p-4">
            <ul className="space-y-3">
              {destructiveWarning.map((change) => (
                <li key={change.key} className="flex flex-col gap-0.5">
                  <span className="text-sm font-medium text-slate-200">{change.label}</span>
                  <span className="text-xs text-slate-400">{change.reason}</span>
                </li>
              ))}
            </ul>
          </div>

          <div className="flex flex-col gap-2 sm:flex-row sm:justify-end">
            <button
              onClick={() => setDestructiveWarning(null)}
              className="rounded-lg border border-white/10 bg-white/5 px-4 py-2 text-sm font-medium text-slate-300 transition-colors hover:bg-white/10"
            >
              Cancel
            </button>
            <button
              onClick={handleSkipDestructive}
              className="rounded-lg border border-blue-500/30 bg-blue-500/10 px-4 py-2 text-sm font-medium text-blue-400 transition-colors hover:bg-blue-500/20"
            >
              {isRestoreFlow ? "Restore Backup Sizes" : "Skip Data-Affecting Changes"}
            </button>
            <button
              onClick={handleApplyAll}
              className="rounded-lg bg-red-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-red-600"
            >
              {isRestoreFlow ? "Continue & Reformat" : "Delete Data & Apply All"}
            </button>
          </div>
        </div>
      </div>
    )
  }

  if (phase !== "wizard") {
    const isInProgress = phase === "applying" || phase === "running" || phase === "rebooting" || phase === "finalizing"
    return (
      <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
        <div className="glass-card setup-wizard-glass flex w-full max-w-2xl flex-col gap-6 p-8 lg:max-w-5xl">
          {isInProgress ? (
            <>
              <div className="text-center">
                <h2 className="text-xl font-semibold text-slate-100">
                  {phase === "finalizing" ? "Almost Done!" : "Setting Up Dash USB"}
                </h2>
                <p className="mt-2 text-sm text-slate-400">{setupMessage}</p>
                {phase !== "finalizing" && (
                  <p className="mt-2 text-xs text-slate-600">
                    The device will reboot multiple times — this is normal. Do not power off.
                  </p>
                )}
                {phase === "finalizing" && (
                  <p className="mt-2 text-xs text-slate-600">
                    Performing final reboot. This page will redirect automatically.
                  </p>
                )}
              </div>
              <SetupProgress phase={phase} />
            </>
          ) : phase === "complete" ? (
            <>
              <div className="text-center">
                <h2 className="text-xl font-semibold text-slate-100">
                  Setup Complete!
                </h2>
                <p className="mt-2 text-sm text-slate-400">{setupMessage}</p>
              </div>
              <SetupProgress complete phase="complete" />
              <div className="flex justify-center">
                <button
                  onClick={onClose}
                  className="rounded-xl bg-blue-500 px-6 py-2.5 text-sm font-medium text-white transition-colors hover:bg-blue-600"
                >
                  Go to Dashboard
                </button>
              </div>
            </>
          ) : (
            <>
              <div className="text-center">
                <div className="mx-auto mb-3 flex h-14 w-14 items-center justify-center rounded-full bg-red-500/20">
                  <AlertCircle className="h-7 w-7 text-red-400" />
                </div>
                <h2 className="text-xl font-semibold text-slate-100">Setup Error</h2>
                <p className="mt-2 text-sm text-red-400">{setupMessage}</p>
              </div>
              <SetupProgress phase="error" />
              <div className="flex justify-center gap-3">
                <button
                  onClick={() => { setPhase("wizard"); setCurrentStep(steps.length - 1) }}
                  className="rounded-xl border border-white/10 bg-white/5 px-4 py-2.5 text-sm font-medium text-slate-300 transition-colors hover:bg-white/10"
                >
                  Back to Wizard
                </button>
                <button
                  onClick={handleApply}
                  className="rounded-xl bg-blue-500 px-4 py-2.5 text-sm font-medium text-white transition-colors hover:bg-blue-600"
                >
                  Retry
                </button>
              </div>
            </>
          )}
        </div>
      </div>
    )
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
      <div className="glass-card setup-wizard-glass relative flex h-[90vh] w-full max-w-3xl flex-col overflow-hidden">
        <div className="shrink-0 border-b border-white/5 px-6 py-4">
          <div className="mb-3 flex items-center justify-between">
            <h2 className="text-lg font-semibold text-slate-100">
              Setup Wizard
            </h2>
            <button
              onClick={onClose}
              className="rounded-lg px-3 py-1 text-sm text-slate-500 hover:bg-white/5 hover:text-slate-300"
            >
              Cancel
            </button>
          </div>

          <div className="flex gap-1">
            {steps.map((step, i) => (
              <button
                key={step.id}
                onClick={() => {
                  if (i > currentStep) {
                    for (let s = 0; s < i; s++) {
                      if (getStepError(s, formData) !== null) {
                        setCurrentStep(s)
                        return
                      }
                    }
                  }
                  setCurrentStep(i)
                }}
                className="group flex-1"
                title={step.title}
              >
                <div
                  className={cn(
                    "h-1 rounded-full transition-all",
                    i === currentStep
                      ? "bg-blue-400"
                      : i < currentStep && getStepError(i, formData) !== null
                        ? "bg-red-500/70"
                        : i < currentStep
                          ? "bg-blue-500"
                          : "bg-slate-800"
                  )}
                />
                <p
                  className={cn(
                    "mt-1 hidden text-[10px] font-medium sm:block",
                    i === currentStep ? "text-slate-200" : i < currentStep ? "text-slate-400" : "text-slate-500"
                  )}
                >
                  {step.title}
                </p>
              </button>
            ))}
          </div>
        </div>

        <div className="flex-1 overflow-y-auto px-6 py-5">
          <StepComponent
            data={formData}
            onChange={handleChange}
            onBatchChange={handleBatchChange}
            setupAlreadyFinished={setupAlreadyFinished}
          />
        </div>

        <div className="shrink-0 border-t border-white/5 px-6 py-4">
          {/* Reconfiguration without image-recreating changes preserves data. */}
          {isLast
            && setupAlreadyFinished
            // Apply handlers need the event-time baseline held in this ref.
            // eslint-disable-next-line react-hooks/refs
            && detectDestructiveChanges(formData, originalDataRef.current).length === 0
            && !saveError
            && !currentStepError
            && !spaceRejection && (
              <div className="mb-3 rounded-lg border border-emerald-500/30 bg-emerald-500/10 px-3 py-2 text-xs text-emerald-300">
                <span className="font-medium">Your data is safe.</span>{" "}
                Setup will only update settings — the dashcam drive,
                snapshots, and other drives will be preserved.
              </div>
            )}
          {spaceRejection && (
            <div className="mb-3 rounded-lg border border-amber-500/30 bg-amber-500/10 p-3 text-xs">
              <p className="font-medium text-amber-300">Not enough free space</p>
              <p className="mt-1 text-slate-300">{spaceRejection}</p>
              <a
                href="/snapshots"
                className="mt-2 inline-block text-amber-300 underline hover:text-amber-200"
              >
                Open snapshot management →
              </a>
            </div>
          )}
          {saveError && (
            <p className="mb-2 text-sm text-red-400">{saveError}</p>
          )}
          {currentStepError && (
            <p className="mb-2 text-sm text-red-400">{currentStepError}</p>
          )}
          <div className="flex items-center justify-between">
            <button
              onClick={() => setCurrentStep((s) => s - 1)}
              disabled={isFirst}
              className={cn(
                "flex items-center gap-1.5 rounded-lg px-4 py-2 text-sm font-medium transition-colors",
                isFirst
                  ? "text-slate-600"
                  : "text-slate-400 hover:bg-white/5 hover:text-slate-200"
              )}
            >
              <ChevronLeft className="h-4 w-4" />
              Back
            </button>

            <span className="text-xs text-slate-600">
              {currentStep + 1} / {steps.length}
            </span>

            {isLast ? (
              <button
                onClick={handleApply}
                disabled={saving}
                className="flex items-center gap-1.5 rounded-lg bg-blue-500 px-5 py-2 text-sm font-medium text-white transition-colors hover:bg-blue-600 disabled:opacity-50"
              >
                {saving ? (
                  <Loader2 className="h-4 w-4 animate-spin" />
                ) : (
                  <Check className="h-4 w-4" />
                )}
                Apply & Run Setup
              </button>
            ) : (
              <button
                onClick={() => setCurrentStep((s) => s + 1)}
                disabled={!!currentStepError}
                className="flex items-center gap-1.5 rounded-lg bg-blue-500/20 px-4 py-2 text-sm font-medium text-blue-400 transition-colors hover:bg-blue-500/30 disabled:opacity-40 disabled:cursor-not-allowed"
              >
                Next
                <ChevronRight className="h-4 w-4" />
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  )
}
