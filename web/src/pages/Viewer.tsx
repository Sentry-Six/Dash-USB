import { useState, useEffect, useRef, useCallback, useMemo } from "react"
import {
  Video, Play, Pause, SkipBack, SkipForward, Loader2,
  Maximize, Minimize, Trash2,
  Download, ChevronLeft, ChevronRight,
} from "lucide-react"
import { cn } from "@/lib/utils"
import type { ClipEntry, ClipGroup } from "@/lib/api"

interface VehicleProfile {
  id: string
  display_name: string
  cameras: { id: string; label: string; optional?: boolean }[]
  grid: string[][]
  filename_regex: string
  segment_seconds: number
}

const CATEGORY = "Continuous"

interface ClipSet {
  timestamp: string
  cameras: Record<string, string>
}

const SPEED_OPTIONS = [0.5, 1, 1.5, 2, 4]

/** Profile patterns use Rust regex syntax with `(?P<name>...)` named
 *  captures; JS requires `(?<name>...)`. */
function compileClipRegex(pattern: string): RegExp | null {
  try {
    return new RegExp(pattern.replace(/\(\?P</g, "(?<"))
  } catch {
    return null
  }
}

/** All cameras of a segment share the timestamp captures, which double
 *  as the sort key. */
function groupByTimestamp(files: string[], basePath: string, regex: RegExp | null): ClipSet[] {
  if (!regex) return []
  const map = new Map<string, Record<string, string>>()
  for (const f of files) {
    const m = f.match(regex)
    const g = m?.groups
    if (!g?.camera || !g.y) continue
    const ts = `${g.y}-${g.mo}-${g.d}_${g.h}-${g.mi}-${g.s}`
    if (!map.has(ts)) map.set(ts, {})
    map.get(ts)![g.camera] = `${basePath}/${f}`
  }
  return Array.from(map.entries())
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([timestamp, cameras]) => ({ timestamp, cameras }))
}

function formatTime(s: number): string {
  if (!Number.isFinite(s) || s < 0) return "0:00"
  const m = Math.floor(s / 60)
  const sec = Math.floor(s % 60)
  return `${m}:${sec.toString().padStart(2, "0")}`
}

function formatClipDate(date: string): string {
  // Segment-stamped folders: 2026-07-17_19-34-53 → Jul 17, 7:34 PM
  const match = date.match(/^(\d{4})-(\d{2})-(\d{2})_(\d{2})-(\d{2})-(\d{2})$/)
  if (match) {
    const [, y, mo, d, h, mi] = match
    const dt = new Date(+y, +mo - 1, +d, +h, +mi)
    return dt.toLocaleDateString("en-US", { month: "short", day: "numeric" }) +
      ", " + dt.toLocaleTimeString("en-US", { hour: "numeric", minute: "2-digit" })
  }
  // Continuous recordings are bucketed per day: 2026-07-17 → Fri, Jul 17
  const dateOnly = date.match(/^(\d{4})-(\d{2})-(\d{2})$/)
  if (dateOnly) {
    const [, y, mo, d] = dateOnly
    const dt = new Date(+y, +mo - 1, +d)
    return dt.toLocaleDateString("en-US", { weekday: "short", month: "short", day: "numeric" })
  }
  return date
}

export default function Viewer() {
  const [groups, setGroups] = useState<ClipGroup[]>([])
  const [loading, setLoading] = useState(true)
  const [selectedClip, setSelectedClip] = useState<ClipEntry | null>(null)
  const [clipSets, setClipSets] = useState<ClipSet[]>([])
  const [currentSetIdx, setCurrentSetIdx] = useState(0)
  const [playing, setPlaying] = useState(false)
  const [focusedCamera, setFocusedCamera] = useState<string | null>(null)
  const [activeCameras, setActiveCameras] = useState<Set<string>>(new Set())
  const [playbackSpeed, setPlaybackSpeed] = useState(1)
  const [currentTime, setCurrentTime] = useState(0)
  const [isFullscreen, setIsFullscreen] = useState(false)
  const [sidebarCollapsed, setSidebarCollapsed] = useState(() => window.innerWidth < 768)
  const [deleteConfirm, setDeleteConfirm] = useState<string | null>(null)
  const [segmentDurations, setSegmentDurations] = useState<number[]>([])
  const [profile, setProfile] = useState<VehicleProfile | null>(null)

  // Profile data defines cameras, layout, and filename parsing.
  useEffect(() => {
    fetch("/api/profile")
      .then((r) => r.json())
      .then(setProfile)
      .catch(() => {})
  }, [])

  const clipRegex = useMemo(
    () => (profile ? compileClipRegex(profile.filename_regex) : null),
    [profile]
  )
  const cameraLabels = useMemo(() => {
    const m: Record<string, string> = {}
    profile?.cameras.forEach((c) => { m[c.id] = c.label })
    return m
  }, [profile])
  // First camera in the grid (reading order) is the primary/master.
  const primaryCamera = useMemo(
    () => profile?.grid.flat().find((c) => c) ?? profile?.cameras[0]?.id ?? null,
    [profile]
  )
  const gridCols = profile ? Math.max(...profile.grid.map((r) => r.length), 1) : 2

  // Append recorded optional cameras that have no configured grid slot.
  const gridCells = useMemo(() => {
    if (!profile) return [] as string[]
    const cells = profile.grid.flat()
    const extras = profile.cameras
      .map((c) => c.id)
      .filter((id) => !cells.includes(id) && clipSets.some((s) => id in s.cameras))
    return [...cells, ...extras]
  }, [profile, clipSets])

  const currentSet = clipSets[currentSetIdx] as ClipSet | undefined
  // Track probes explicitly because fallback and measured durations can match.
  const segmentSeconds = profile?.segment_seconds ?? 300
  const probedRef = useRef<Set<number>>(new Set())

  const videoRefs = useRef<Map<string, HTMLVideoElement>>(new Map())
  const masterVideoRef = useRef<HTMLVideoElement | null>(null)
  const containerRef = useRef<HTMLDivElement>(null)
  const seekBarRef = useRef<HTMLDivElement>(null)
  const animFrameRef = useRef<number>(0)
  const pendingSeekRef = useRef<number | null>(null)

  // Hidden videos preload the next segment.
  const preloadedVideosRef = useRef<Map<string, HTMLVideoElement>>(new Map())
  const preloadedForIdxRef = useRef<number>(-1)

  // High-frequency values stay in refs to avoid per-frame renders.
  const currentTimeRef = useRef(0)
  const globalTimeRef = useRef(0)
  const playingRef = useRef(false)
  const currentSetIdxRef = useRef(0)
  const playbackSpeedRef = useRef(1)
  const lastUIUpdateRef = useRef(0)

  useEffect(() => { currentSetIdxRef.current = currentSetIdx }, [currentSetIdx])
  useEffect(() => { playbackSpeedRef.current = playbackSpeed }, [playbackSpeed])

  const priorSegmentsTime = useMemo(
    () => segmentDurations.slice(0, currentSetIdx).reduce((a, b) => a + b, 0),
    [segmentDurations, currentSetIdx]
  )
  const totalDuration = useMemo(
    () => segmentDurations.reduce((a, b) => a + b, 0),
    [segmentDurations]
  )
  const priorSegmentsTimeRef = useRef(0)
  useEffect(() => { priorSegmentsTimeRef.current = priorSegmentsTime }, [priorSegmentsTime])
  const totalDurationRef = useRef(0)
  useEffect(() => { totalDurationRef.current = totalDuration }, [totalDuration])

  const globalTime = priorSegmentsTime + currentTime

  const segmentPositions = useMemo(() => {
    if (segmentDurations.length <= 1 || totalDuration <= 0) return []
    const positions: number[] = []
    let cumulative = 0
    for (let i = 1; i < segmentDurations.length; i++) {
      cumulative += segmentDurations[i - 1]
      positions.push((cumulative / totalDuration) * 100)
    }
    return positions
  }, [segmentDurations, totalDuration])

  const CLIPS_PAGE_SIZE = 20

  useEffect(() => {
    setLoading(true)
    fetch(`/api/clips?category=${CATEGORY}&limit=${CLIPS_PAGE_SIZE}`)
      .then((r) => r.json())
      .then((data: ClipGroup[]) => {
        setGroups(data)
        setLoading(false)
      })
      .catch(() => setLoading(false))
  }, [])

  const [loadingMore, setLoadingMore] = useState(false)

  function loadMoreClips() {
    const group = groups.find((g) => g.name === CATEGORY)
    if (!group || !group.hasMore) return
    const lastClip = group.clips[group.clips.length - 1]
    if (!lastClip) return
    setLoadingMore(true)
    fetch(`/api/clips?category=${CATEGORY}&limit=${CLIPS_PAGE_SIZE}&before=${lastClip.date}`)
      .then((r) => r.json())
      .then((data: ClipGroup[]) => {
        const newGroup = data.find((g) => g.name === CATEGORY)
        if (newGroup) {
          setGroups((prev) =>
            prev.map((g) =>
              g.name === CATEGORY
                ? { ...g, clips: [...g.clips, ...newGroup.clips], hasMore: newGroup.hasMore }
                : g
            )
          )
        }
        setLoadingMore(false)
      })
      .catch(() => setLoadingMore(false))
  }

  const activeGroup = groups.find((g) => g.name === CATEGORY)

  useEffect(() => {
    if (selectedClip) {
      const sets = groupByTimestamp(selectedClip.files, selectedClip.path, clipRegex)
      setClipSets(sets)
      setCurrentSetIdx(0)
      setPlaying(false)
      setFocusedCamera(null)
      setActiveCameras(new Set(primaryCamera ? [primaryCamera] : []))
      pendingSeekRef.current = null
      setCurrentTime(0)
      currentTimeRef.current = 0
      globalTimeRef.current = 0
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- regex/primary are stable once the profile loads
  }, [selectedClip, clipRegex])

  // Probe a small initial batch, then lazily probe nearby segments.
  const EAGER_PROBE_COUNT = 6
  useEffect(() => {
    probedRef.current = new Set()
    if (!clipSets.length) { setSegmentDurations([]); return }
    const durations = new Array(clipSets.length).fill(segmentSeconds)
    setSegmentDurations([...durations])

    let cancelled = false
    const cleanups: (() => void)[] = []

    async function loadBatched(startIdx: number, endIdx: number) {
      const BATCH = 3
      for (let start = startIdx; start < endIdx; start += BATCH) {
        if (cancelled) return
        const batch = clipSets.slice(start, Math.min(start + BATCH, endIdx))
        await Promise.all(batch.map((set, j) => {
          const i = start + j
          return new Promise<void>((resolve) => {
            const url = (primaryCamera && set.cameras[primaryCamera]) || Object.values(set.cameras)[0]
            if (!url) { resolve(); return }
            const v = document.createElement("video")
            v.preload = "metadata"
            v.src = url
            v.onloadedmetadata = () => {
              if (!cancelled && Number.isFinite(v.duration)) {
                durations[i] = v.duration
                probedRef.current.add(i)
                setSegmentDurations([...durations])
              }
              resolve()
            }
            v.onerror = () => resolve()
            cleanups.push(() => { v.src = ""; v.remove() })
          })
        }))
      }
    }

    loadBatched(0, Math.min(EAGER_PROBE_COUNT, clipSets.length))
    return () => { cancelled = true; cleanups.forEach((c) => c()) }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- segmentSeconds/primaryCamera are stable once the profile loads
  }, [clipSets])

  useEffect(() => {
    if (!clipSets.length || currentSetIdx < EAGER_PROBE_COUNT - 2) return
    const probeStart = Math.max(0, currentSetIdx - 1)
    const probeEnd = Math.min(clipSets.length, currentSetIdx + 4)

    let cancelled = false
    const cleanups: (() => void)[] = []

    for (let i = probeStart; i < probeEnd; i++) {
      if (probedRef.current.has(i)) continue
      const set = clipSets[i]
      const url = (primaryCamera && set.cameras[primaryCamera]) || Object.values(set.cameras)[0]
      if (!url) continue
      const v = document.createElement("video")
      v.preload = "metadata"
      v.src = url
      v.onloadedmetadata = () => {
        if (!cancelled && Number.isFinite(v.duration)) {
          probedRef.current.add(i)
          setSegmentDurations((prev) => {
            const next = [...prev]
            next[i] = v.duration
            return next
          })
        }
      }
      cleanups.push(() => { v.src = ""; v.remove() })
    }

    return () => { cancelled = true; cleanups.forEach((c) => c()) }
    // segmentDurations would restart in-flight probes after every result.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [clipSets, currentSetIdx])

  // Drift correction: re-sync any camera >200ms off the master every 2s
  useEffect(() => {
    if (!playing) return
    const DRIFT_THRESHOLD = 0.2 // seconds
    const interval = setInterval(() => {
      const master = masterVideoRef.current
      if (!master || master.paused) return
      const masterTime = master.currentTime
      videoRefs.current.forEach((v) => {
        if (v === master || v.paused) return
        if (Math.abs(v.currentTime - masterTime) > DRIFT_THRESHOLD) {
          v.currentTime = masterTime
        }
      })
    }, 2000)
    return () => clearInterval(interval)
  }, [playing])

  const cleanupPreloaded = useCallback(() => {
    preloadedVideosRef.current.forEach((v) => { v.src = ""; v.remove() })
    preloadedVideosRef.current.clear()
    preloadedForIdxRef.current = -1
  }, [])

  // Buffer active cameras near the segment boundary.
  useEffect(() => {
    if (!playing || clipSets.length === 0) return
    const PRELOAD_WINDOW = 5 // seconds

    const checkInterval = setInterval(() => {
      const master = masterVideoRef.current
      if (!master || !Number.isFinite(master.duration)) return

      const timeRemaining = master.duration - master.currentTime
      const nextIdx = currentSetIdxRef.current + 1

      if (timeRemaining <= PRELOAD_WINDOW && nextIdx < clipSets.length) {
        if (preloadedForIdxRef.current === nextIdx) return

        cleanupPreloaded()
        preloadedForIdxRef.current = nextIdx

        const nextSet = clipSets[nextIdx]
        activeCameras.forEach((cam) => {
          const url = nextSet.cameras[cam]
          if (!url) return
          const v = document.createElement("video")
          v.preload = "auto"
          v.muted = true
          v.playsInline = true
          v.src = url
          v.style.display = "none"
          document.body.appendChild(v)
          preloadedVideosRef.current.set(cam, v)
        })
      } else if (timeRemaining > PRELOAD_WINDOW && preloadedForIdxRef.current !== -1) {
        cleanupPreloaded()
      }
    }, 1000)

    return () => {
      clearInterval(checkInterval)
    }
  }, [playing, clipSets, activeCameras, cleanupPreloaded])

  useEffect(() => {
    cleanupPreloaded()
  }, [currentSetIdx, cleanupPreloaded])

  useEffect(() => {
    return () => cleanupPreloaded()
  }, [cleanupPreloaded])

  // Prefer the profile's primary camera as the synchronization master.
  useEffect(() => {
    if (!currentSet) { masterVideoRef.current = null; return }
    const primary = primaryCamera ? videoRefs.current.get(primaryCamera) : undefined
    if (primary) { masterVideoRef.current = primary; return }
    for (const v of videoRefs.current.values()) {
      if (v) { masterVideoRef.current = v; return }
    }
    masterVideoRef.current = null
  }, [currentSet, currentSetIdx, primaryCamera])

  // Runs only during playback; React state updates are throttled to ~15fps.
  const startAnimLoop = useCallback(() => {
    const UI_INTERVAL = 66 // ~15fps for React state updates
    function tick() {
      if (!playingRef.current) return
      const master = masterVideoRef.current
      if (master) {
        currentTimeRef.current = master.currentTime
        globalTimeRef.current = priorSegmentsTimeRef.current + master.currentTime
        const now = performance.now()
        if (now - lastUIUpdateRef.current >= UI_INTERVAL) {
          lastUIUpdateRef.current = now
          setCurrentTime(master.currentTime)
        }
      }
      animFrameRef.current = requestAnimationFrame(tick)
    }
    cancelAnimationFrame(animFrameRef.current)
    animFrameRef.current = requestAnimationFrame(tick)
  }, [])

  useEffect(() => {
    playingRef.current = playing
    if (playing) startAnimLoop()
    return () => cancelAnimationFrame(animFrameRef.current)
  }, [playing, startAnimLoop])

  useEffect(() => {
    videoRefs.current.forEach((v) => { if (v) v.playbackRate = playbackSpeed })
  }, [playbackSpeed, currentSetIdx])


  const handleVideoEnded = useCallback(() => {
    setCurrentSetIdx((i) => {
      if (i < clipSets.length - 1) return i + 1
      setPlaying(false)
      return i
    })
  }, [clipSets.length])

  // Seeks are batched into a RAF callback to keep scrubbing smooth.
  const syncVideos = useCallback((time: number) => {
    requestAnimationFrame(() => {
      videoRefs.current.forEach((v) => {
        if (v) v.currentTime = time
      })
    })
    // Paused seeks need an immediate UI update.
    currentTimeRef.current = time
    if (!playingRef.current) {
      globalTimeRef.current = priorSegmentsTimeRef.current + time
      setCurrentTime(time)
    }
  }, [])

  const togglePlay = useCallback(() => {
    const wasPlaying = playingRef.current
    videoRefs.current.forEach((v) => {
      if (!v) return
      if (wasPlaying) v.pause()
      else v.play().catch(() => { })
    })
    setPlaying(!wasPlaying)
  }, [])

  const segmentDurationsRef = useRef<number[]>([])
  useEffect(() => { segmentDurationsRef.current = segmentDurations }, [segmentDurations])

  const seekToGlobal = useCallback((globalT: number) => {
    const durations = segmentDurationsRef.current
    const total = totalDurationRef.current
    const clamped = Math.max(0, Math.min(globalT, total))
    let remaining = clamped
    for (let i = 0; i < durations.length; i++) {
      if (remaining <= durations[i] + 0.05 || i === durations.length - 1) {
        const offset = Math.min(remaining, durations[i])
        if (i !== currentSetIdxRef.current) {
          pendingSeekRef.current = offset
          setCurrentSetIdx(i)
        } else {
          syncVideos(offset)
        }
        return
      }
      remaining -= durations[i]
    }
  }, [syncVideos])

  const skip = useCallback((seconds: number) => {
    seekToGlobal(globalTimeRef.current + seconds)
  }, [seekToGlobal])

  const handleSeek = useCallback((e: React.MouseEvent<HTMLDivElement>) => {
    const bar = seekBarRef.current
    const total = totalDurationRef.current
    if (!bar || total <= 0) return
    const rect = bar.getBoundingClientRect()
    const pct = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width))
    seekToGlobal(pct * total)
  }, [seekToGlobal])

  const toggleFullscreen = useCallback(() => {
    if (!containerRef.current) return
    if (document.fullscreenElement) {
      document.exitFullscreen()
    } else {
      containerRef.current.requestFullscreen()
    }
  }, [])

  useEffect(() => {
    const onFS = () => setIsFullscreen(!!document.fullscreenElement)
    document.addEventListener("fullscreenchange", onFS)
    return () => document.removeEventListener("fullscreenchange", onFS)
  }, [])

  // Stable callbacks keep this listener from re-attaching except when
  // focusedCamera changes.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return
      switch (e.key) {
        case " ":
          e.preventDefault()
          togglePlay()
          break
        case "ArrowLeft":
          e.preventDefault()
          skip(e.shiftKey ? -15 : -5)
          break
        case "ArrowRight":
          e.preventDefault()
          skip(e.shiftKey ? 15 : 5)
          break
        case "f":
          e.preventDefault()
          toggleFullscreen()
          break
        case "Escape":
          if (focusedCamera) { e.preventDefault(); setFocusedCamera(null) }
          break
      }
    }
    window.addEventListener("keydown", onKey)
    return () => window.removeEventListener("keydown", onKey)
  }, [togglePlay, skip, toggleFullscreen, focusedCamera])

  async function handleDeleteClip(clip: ClipEntry) {
    try {
      // Delete through the listed snapshot tree; /mnt/cam may be host-mounted.
      const fullPath = `/mutable/Recordings/${CATEGORY}/${clip.date}`
      await fetch(`/api/files?path=${encodeURIComponent(fullPath)}`, { method: "DELETE" })
      setGroups((prev) =>
        prev.map((g) =>
          g.name === CATEGORY
            ? { ...g, clips: g.clips.filter((c) => c.date !== clip.date) }
            : g
        )
      )
      if (selectedClip?.date === clip.date) {
        setSelectedClip(null)
        setClipSets([])
      }
      setDeleteConfirm(null)
    } catch { /* ignore */ }
  }

  function handleDownload() {
    if (!selectedClip) return
    const fullPath = `/mutable/Recordings/${CATEGORY}/${selectedClip.date}`
    window.open(`/api/files/download-zip?path=${encodeURIComponent(fullPath)}`, "_blank")
  }

  // Stable identity: playback speed is read from a ref, not a dependency.
  const setVideoRef = useCallback((cam: string) => (el: HTMLVideoElement | null) => {
    if (el) {
      videoRefs.current.set(cam, el)
      el.playbackRate = playbackSpeedRef.current
    } else {
      videoRefs.current.delete(cam)
    }
  }, [])

  const progress = totalDuration > 0 ? (globalTime / totalDuration) * 100 : 0

  const camerasToShow = focusedCamera ? [focusedCamera] : gridCells

  return (
    <div
      ref={containerRef}
      className={cn(
        "flex flex-col",
        isFullscreen ? "h-screen bg-slate-950 p-2" : "h-[calc(100vh-120px)] md:h-[calc(100vh-96px)]"
      )}
    >
      {!isFullscreen && (
        <div className="mb-3 flex items-center justify-between">
          <div>
            <h1 className="text-2xl font-bold text-slate-100">Viewer</h1>
            <p className="mt-0.5 text-sm text-slate-500">
              View all cameras simultaneously with synced playback
              <span className="ml-2 hidden text-[10px] text-slate-600 md:inline">
                Space: play/pause &middot; ←→: skip 5s &middot; Shift+←→: skip 15s &middot; F: fullscreen
              </span>
            </p>
          </div>
        </div>
      )}

      <div className={cn("mb-2 flex items-center gap-1", isFullscreen && "mb-1")}>
        <span className="rounded-lg bg-blue-500/15 px-3 py-1.5 text-sm font-medium text-blue-400">
          Recordings
          {(activeGroup?.clips.length ?? 0) > 0 && (
            <span className="ml-1.5 rounded-full bg-white/5 px-1.5 py-0.5 text-[10px] tabular-nums text-slate-500">
              {activeGroup?.clips.length}
            </span>
          )}
        </span>

        <button
          onClick={() => setSidebarCollapsed((c) => !c)}
          className="ml-auto rounded-lg p-1.5 text-slate-500 transition-colors hover:bg-white/5 hover:text-slate-300"
          title={sidebarCollapsed ? "Show clip browser" : "Hide clip browser"}
        >
          {sidebarCollapsed ? <ChevronRight className="h-4 w-4" /> : <ChevronLeft className="h-4 w-4" />}
        </button>
      </div>

      <div className="flex min-h-0 flex-1 gap-2">
        {!sidebarCollapsed && (
          <div className="glass-card flex w-56 shrink-0 flex-col overflow-hidden">
            <div className="flex-1 overflow-y-auto p-1.5">
              {loading ? (
                <div className="flex items-center justify-center p-8">
                  <Loader2 className="h-5 w-5 animate-spin text-slate-500" />
                </div>
              ) : activeGroup && activeGroup.clips.length > 0 ? (
                activeGroup.clips.map((clip) => {
                  const isSelected = selectedClip?.date === clip.date
                  return (
                    <div key={clip.date} className="group relative">
                      <button
                        onClick={() => setSelectedClip(clip)}
                        className={cn(
                          "w-full rounded-lg px-2.5 py-2 text-left transition-colors",
                          isSelected
                            ? "bg-blue-500/15 text-blue-400"
                            : "text-slate-400 hover:bg-white/5 hover:text-slate-200"
                        )}
                      >
                        <div className="text-xs font-medium">{formatClipDate(clip.date)}</div>
                        <div className="mt-0.5 flex items-center gap-1.5">
                          <span className="text-[10px] text-slate-600">
                            {clip.files.length} files
                          </span>
                        </div>
                      </button>
                      <>
                          <button
                            onClick={(e) => { e.stopPropagation(); setDeleteConfirm(clip.date) }}
                            className="absolute right-1 top-1 hidden rounded p-0.5 text-slate-600 transition-colors hover:bg-red-500/15 hover:text-red-400 group-hover:block"
                            title="Delete clip"
                          >
                            <Trash2 className="h-3 w-3" />
                          </button>
                          {deleteConfirm === clip.date && (
                            <div className="mx-1 mb-1 flex items-center gap-1 rounded-md bg-red-500/10 px-2 py-1.5">
                              <span className="flex-1 text-[10px] text-red-400">Delete this clip?</span>
                              <button
                                onClick={() => handleDeleteClip(clip)}
                                className="rounded bg-red-500/20 px-2 py-0.5 text-[10px] font-medium text-red-400 hover:bg-red-500/30"
                              >
                                Yes
                              </button>
                              <button
                                onClick={() => setDeleteConfirm(null)}
                                className="rounded bg-white/5 px-2 py-0.5 text-[10px] text-slate-400 hover:bg-white/10"
                              >
                                No
                              </button>
                            </div>
                          )}
                        </>
                    </div>
                  )
                })
              ) : (
                <div className="flex flex-col items-center justify-center py-8 text-center">
                  <Video className="mb-2 h-8 w-8 text-slate-500" />
                  <p className="text-xs text-slate-600">No recordings yet</p>
                </div>
              )}
              {activeGroup?.hasMore && (
                <button
                  onClick={loadMoreClips}
                  disabled={loadingMore}
                  className="mt-1 w-full rounded-lg px-2 py-1.5 text-[11px] font-medium text-slate-400 transition-colors hover:bg-white/5 hover:text-slate-300 disabled:opacity-50"
                >
                  {loadingMore ? (
                    <span className="flex items-center justify-center gap-1.5">
                      <Loader2 className="h-3 w-3 animate-spin" /> Loading…
                    </span>
                  ) : "Load more clips"}
                </button>
              )}
            </div>

          </div>
        )}

        <div className="flex min-h-0 flex-1 flex-col">
          {currentSet ? (
            <>
              <div
                className={cn(
                  "relative min-h-0 flex-1",
                  focusedCamera ? "" : "grid gap-0.5"
                )}
                style={focusedCamera ? undefined : {
                  gridTemplateColumns: `repeat(${gridCols}, minmax(0, 1fr))`,
                }}
              >
                {camerasToShow.map((cam, cellIdx) => {
                  if (!cam) {
                    return <div key={`spacer-${cellIdx}`} className="hidden md:block" />
                  }
                  const hasFocus = focusedCamera === cam
                  const isCamActive = activeCameras.has(cam)
                  return (
                    <div
                      key={cam}
                      className={cn(
                        "relative cursor-pointer overflow-hidden rounded-md bg-black transition-all",
                        hasFocus && "h-full w-full",
                      )}
                      onClick={() => {
                        if (!isCamActive && currentSet.cameras[cam]) {
                          setActiveCameras((prev) => new Set([...prev, cam]))
                          return
                        }
                        setFocusedCamera(hasFocus ? null : cam)
                      }}
                    >
                      {currentSet.cameras[cam] && isCamActive ? (
                        <video
                          ref={setVideoRef(cam)}
                          key={`${currentSetIdx}-${cam}`}
                          src={currentSet.cameras[cam]}
                          className="h-full w-full object-contain"
                          muted
                          playsInline
                          preload="auto"
                          onEnded={cam === primaryCamera ? handleVideoEnded : undefined}
                          onLoadedData={(e) => {
                            const v = e.currentTarget
                            v.playbackRate = playbackSpeedRef.current

                            // Target time comes from a pending seek (segment
                            // change) or the master's clock (a camera
                            // activated mid-playback).
                            let targetTime: number | null = null
                            if (pendingSeekRef.current !== null) {
                              targetTime = pendingSeekRef.current
                              if (cam === primaryCamera || !(primaryCamera && currentSet.cameras[primaryCamera])) pendingSeekRef.current = null
                            } else {
                              const master = masterVideoRef.current
                              if (master && master !== v && master.currentTime > 0.1) {
                                targetTime = master.currentTime
                              }
                            }

                            if (targetTime !== null) {
                              v.currentTime = targetTime
                            }
                            if (playingRef.current) v.play().catch(() => { })
                          }}
                        />
                      ) : currentSet.cameras[cam] && !isCamActive ? (
                        <div className="flex h-full flex-col items-center justify-center gap-1.5 bg-slate-900/80">
                          <Play className="h-5 w-5 text-slate-500" />
                          <span className="text-[10px] text-slate-500">Click to stream</span>
                        </div>
                      ) : (
                        <div className="flex h-full items-center justify-center">
                          <Video className="h-6 w-6 text-slate-500" />
                        </div>
                      )}
                      <span className="absolute bottom-1 left-1 rounded bg-black/60 px-1.5 py-0.5 text-[10px] font-medium text-slate-400">
                        {cameraLabels[cam] ?? cam}
                      </span>
                      {hasFocus && (
                        <span className="absolute right-1 top-1 rounded bg-black/60 px-1.5 py-0.5 text-[10px] text-slate-500">
                          Click to exit &middot; ESC
                        </span>
                      )}
                    </div>
                  )
                })}
              </div>

              <div className="glass-card mt-1 p-2">
                <div
                  ref={seekBarRef}
                  className="group mb-2 h-1.5 cursor-pointer rounded-full bg-white/10 transition-all hover:h-2.5"
                  onClick={handleSeek}
                  onMouseDown={(e) => {
                    handleSeek(e)
                    let lastDragTime = 0
                    const onMove = (ev: MouseEvent) => {
                      const now = performance.now()
                      if (now - lastDragTime < 33) return // ~30fps throttle
                      lastDragTime = now
                      const bar = seekBarRef.current
                      const total = totalDurationRef.current
                      if (!bar || total <= 0) return
                      const rect = bar.getBoundingClientRect()
                      const pct = Math.max(0, Math.min(1, (ev.clientX - rect.left) / rect.width))
                      seekToGlobal(pct * total)
                    }
                    const onUp = () => {
                      document.removeEventListener("mousemove", onMove)
                      document.removeEventListener("mouseup", onUp)
                    }
                    document.addEventListener("mousemove", onMove)
                    document.addEventListener("mouseup", onUp)
                  }}
                >
                  <div className="relative h-full w-full">
                    <div
                      className="h-full rounded-full bg-blue-500 transition-all"
                      style={{ width: `${progress}%` }}
                    >
                      <div className="absolute -right-1 -top-0.5 hidden h-3 w-3 rounded-full bg-blue-400 shadow-lg group-hover:block" />
                    </div>
                    {segmentPositions.map((pos, i) => (
                      <div key={i} className="absolute top-0 h-full w-px bg-white/20" style={{ left: `${pos}%` }} />
                    ))}
                  </div>
                </div>

                <div className="flex items-center gap-2">
                  <span className="w-28 text-xs tabular-nums text-slate-400">
                    {formatTime(globalTime)} / {formatTime(totalDuration)}
                  </span>
                  {segmentDurations.length > 1 && (
                    <span className="rounded bg-white/5 px-1.5 py-0.5 text-[10px] tabular-nums text-slate-500">
                      {currentSetIdx + 1}/{segmentDurations.length}
                    </span>
                  )}

                  <button
                    onClick={() => skip(-5)}
                    className="rounded-lg p-1.5 text-slate-400 transition-colors hover:bg-white/5 hover:text-slate-200"
                    title="Back 5s (← or Shift+← for 15s)"
                  >
                    <SkipBack className="h-3.5 w-3.5" />
                  </button>

                  <button
                    onClick={togglePlay}
                    className="flex h-8 w-8 items-center justify-center rounded-full bg-blue-500/20 text-blue-400 transition-colors hover:bg-blue-500/30"
                    title="Play/Pause (Space)"
                  >
                    {playing ? <Pause className="h-4 w-4" /> : <Play className="h-4 w-4 translate-x-px" />}
                  </button>

                  <button
                    onClick={() => skip(5)}
                    className="rounded-lg p-1.5 text-slate-400 transition-colors hover:bg-white/5 hover:text-slate-200"
                    title="Forward 5s (→ or Shift+→ for 15s)"
                  >
                    <SkipForward className="h-3.5 w-3.5" />
                  </button>

                  <div className="flex-1" />

                  <div className="hidden items-center gap-0.5 sm:flex">
                    {SPEED_OPTIONS.map((s) => (
                      <button
                        key={s}
                        onClick={() => setPlaybackSpeed(s)}
                        className={cn(
                          "rounded px-1.5 py-0.5 text-[10px] font-medium transition-colors",
                          playbackSpeed === s
                            ? "bg-blue-500/20 text-blue-400"
                            : "text-slate-600 hover:bg-white/5 hover:text-slate-400"
                        )}
                      >
                        {s}x
                      </button>
                    ))}
                  </div>


                  <button
                    onClick={handleDownload}
                    className="rounded-lg p-1.5 text-slate-500 transition-colors hover:bg-white/5 hover:text-slate-300"
                    title="Download clip folder"
                  >
                    <Download className="h-3.5 w-3.5" />
                  </button>

                  <button
                    onClick={toggleFullscreen}
                    className="rounded-lg p-1.5 text-slate-500 transition-colors hover:bg-white/5 hover:text-slate-300"
                    title="Fullscreen (F)"
                  >
                    {isFullscreen ? <Minimize className="h-3.5 w-3.5" /> : <Maximize className="h-3.5 w-3.5" />}
                  </button>
                </div>
              </div>
            </>
          ) : (
            <div className="glass-card flex flex-1 items-center justify-center">
              <div className="max-w-xs text-center">
                <Video className="mx-auto mb-3 h-16 w-16 text-slate-500" />
                <p className="text-sm font-medium text-slate-400">
                  {selectedClip ? "No video files found" : "Select a clip to begin playback"}
                </p>
                <p className="mt-1 text-xs text-slate-600">
                  Choose a day from the sidebar to view all cameras simultaneously with synced playback controls.
                </p>
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  )
}
