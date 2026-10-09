import { useState, useEffect, useRef, useCallback, useMemo } from "react"
import {
  FolderOpen,
  Upload,
  Download,
  FolderPlus,
  Trash2,
  File,
  Folder,
  ArrowLeft,
  Loader2,
  Video,
  CheckCircle,
  X,
  Search,
  ArrowUpDown,
  Check,
  HardDrive,
} from "lucide-react"
import { cn } from "@/lib/utils"
import { uploadRelativePath } from "@/lib/file-upload"

type SortOption = "name-asc" | "name-desc" | "date-newest" | "date-oldest" | "size-largest" | "size-smallest" | "type"

const SORT_LABELS: Record<SortOption, string> = {
  "name-asc": "Name (A-Z)",
  "name-desc": "Name (Z-A)",
  "date-newest": "Date (Newest)",
  "date-oldest": "Date (Oldest)",
  "size-largest": "Size (Largest)",
  "size-smallest": "Size (Smallest)",
  "type": "Type",
}

interface FileEntry {
  name: string
  path: string
  is_dir: boolean
  size: number
  // Matches files.rs serialization; date sorting otherwise degrades silently.
  mod_time: string
}

interface DriveTab {
  id: string
  base: string
  icon: "cam" | "drive"
}

const ALL_DRIVES: DriveTab[] = [
  { id: "USB Drive", base: "/mutable", icon: "drive" },
  { id: "Recordings", base: "/mutable/Recordings", icon: "cam" },
]

const TAB_ICONS: Record<DriveTab["icon"], React.ComponentType<{ className?: string }>> = {
  cam: Video,
  drive: HardDrive,
}

function formatSize(bytes: number): string {
  if (bytes === 0) return "—"
  const units = ["B", "KB", "MB", "GB"]
  const i = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1)
  return `${(bytes / Math.pow(1024, i)).toFixed(i > 0 ? 1 : 0)} ${units[i]}`
}

interface UploadProgress {
  id: number
  file: globalThis.File
  fileName: string
  destination: string
  loaded: number
  total: number
  done: boolean
  error: string | null
  conflict: boolean
}

export default function Files() {
  const [drives, setDrives] = useState<DriveTab[]>([])
  const [activeDrive, setActiveDrive] = useState<DriveTab | null>(null)
  const [currentPath, setCurrentPath] = useState("")
  const [files, setFiles] = useState<FileEntry[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const uploadRef = useRef<HTMLInputElement>(null)
  const folderUploadRef = useRef<HTMLInputElement>(null)
  const [uploads, setUploads] = useState<UploadProgress[]>([])
  const [uploading, setUploading] = useState(false)
  const [uploadError, setUploadError] = useState<string | null>(null)
  const mounted = useRef(false)
  const uploadBusy = useRef(false)
  const uploadRun = useRef(0)
  const uploadId = useRef(0)
  const activeUpload = useRef<XMLHttpRequest | null>(null)
  const currentPathRef = useRef("")
  const searchRef = useRef("")
  const listingRequest = useRef<AbortController | null>(null)
  const listingGeneration = useRef(0)
  const [dragging, setDragging] = useState(false)
  const dragCounter = useRef(0)
  const [effectiveBase, setEffectiveBase] = useState("")
  const [search, setSearch] = useState("")
  const [sortOption, setSortOption] = useState<SortOption>("name-asc")
  const [showSortMenu, setShowSortMenu] = useState(false)
  const searchTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const sortMenuRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    mounted.current = true
    return () => {
      mounted.current = false
      activeUpload.current?.abort()
      listingRequest.current?.abort()
      if (searchTimerRef.current) clearTimeout(searchTimerRef.current)
    }
  }, [])

  function changePath(path: string) {
    if (path === currentPathRef.current) {
      void fetchFiles(path, searchRef.current || undefined)
      return
    }
    currentPathRef.current = path
    listingGeneration.current++
    listingRequest.current?.abort()
    if (searchTimerRef.current) clearTimeout(searchTimerRef.current)
    setCurrentPath(path)
  }

  useEffect(() => {
    let cancelled = false
    async function loadConfig() {
      try {
        const res = await fetch("/api/config")
        const cfg = await res.json()
        if (!mounted.current || cancelled) return
        const visible: DriveTab[] = []
        visible.push(ALL_DRIVES.find(d => d.id === "USB Drive")!)
        if (cfg.has_cam === "yes") {
          visible.push(ALL_DRIVES.find(d => d.id === "Recordings")!)
        }
        // Development mode may not expose configured drives.
        const result = visible.length > 0 ? visible : ALL_DRIVES
        setDrives(result)
        setActiveDrive(result[0])
        changePath(result[0].base)
      } catch {
        if (!mounted.current || cancelled) return
        setDrives(ALL_DRIVES)
        setActiveDrive(ALL_DRIVES[0])
        changePath(ALL_DRIVES[0].base)
      }
    }
    void loadConfig()
    return () => { cancelled = true }
    // Load configuration once; directory requests have their own lifecycle.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  async function fetchFiles(path: string, searchQuery?: string) {
    if (!mounted.current || currentPathRef.current !== path) return
    listingRequest.current?.abort()
    const controller = new AbortController()
    listingRequest.current = controller
    const generation = ++listingGeneration.current
    const isCurrent = () => mounted.current && !controller.signal.aborted &&
      listingGeneration.current === generation && currentPathRef.current === path
    setLoading(true)
    setError(null)
    setSelected(new Set())
    try {
      let url = `/api/files/ls?path=${encodeURIComponent(path)}`
      if (searchQuery) url += `&search=${encodeURIComponent(searchQuery)}`
      const res = await fetch(url, { signal: controller.signal })
      const raw = await res.json().catch(() => null)
      if (!isCurrent()) return
      if (!res.ok) throw new Error(typeof raw?.error === "string" ? raw.error : "Failed to load directory")
      const data: FileEntry[] = Array.isArray(raw) ? raw : (raw?.entries ?? [])
      // Enter the named drive folder rather than adjacent hidden metadata.
      if (activeDrive && path === activeDrive.base && !searchQuery) {
        const match = data.find(e => e.is_dir && e.name === activeDrive.id)
        if (match) {
          setEffectiveBase(match.path)
          changePath(match.path)
          return
        }
      }
      setFiles(data)
    } catch (failure) {
      if (isCurrent()) {
        setError(failure instanceof Error ? failure.message : "Unable to connect")
        setFiles([])
      }
    } finally {
      if (isCurrent()) setLoading(false)
    }
  }

  useEffect(() => {
    if (currentPath) fetchFiles(currentPath, search || undefined)
    // search intentionally omitted: the debounced handler below already
    // fetches on change, so listing it here double-fetches per keystroke.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentPath])

  function handleSearchChange(value: string) {
    setSearch(value)
    searchRef.current = value
    if (searchTimerRef.current) clearTimeout(searchTimerRef.current)
    searchTimerRef.current = setTimeout(() => {
      if (currentPath) fetchFiles(currentPath, value || undefined)
    }, 300)
  }

  useEffect(() => {
    function handleClick(e: MouseEvent) {
      if (sortMenuRef.current && !sortMenuRef.current.contains(e.target as Node)) {
        setShowSortMenu(false)
      }
    }
    if (showSortMenu) document.addEventListener("mousedown", handleClick)
    return () => document.removeEventListener("mousedown", handleClick)
  }, [showSortMenu])

  // Client-side sort: directories first, name as the tiebreaker so the
  // order stays stable across refetches.
  const sortedFiles = useMemo(() => {
    const sorted = [...files]
    sorted.sort((a, b) => {
      if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1
      const nameCmp = a.name.localeCompare(b.name, undefined, { sensitivity: "base" })
      switch (sortOption) {
        case "name-asc":
          return nameCmp
        case "name-desc":
          return -nameCmp
        case "date-newest":
          return (new Date(b.mod_time).getTime() - new Date(a.mod_time).getTime()) || -nameCmp
        case "date-oldest":
          return (new Date(a.mod_time).getTime() - new Date(b.mod_time).getTime()) || nameCmp
        case "size-largest":
          return b.size - a.size
        case "size-smallest":
          return a.size - b.size
        case "type": {
          const extA = a.name.includes(".") ? a.name.split(".").pop()!.toLowerCase() : ""
          const extB = b.name.includes(".") ? b.name.split(".").pop()!.toLowerCase() : ""
          return extA.localeCompare(extB) || a.name.localeCompare(b.name, undefined, { sensitivity: "base" })
        }
        default:
          return 0
      }
    })
    return sorted
  }, [files, sortOption])

  function navigate(entry: FileEntry) {
    if (entry.is_dir) {
      changePath(entry.path)
    }
  }

  function goUp() {
    const base = effectiveBase || activeDrive?.base
    if (!activeDrive || !base || currentPath === base) return
    const parent = currentPath.split("/").slice(0, -1).join("/")
    if (parent.length < base.length) return
    changePath(parent || base)
  }

  function switchDrive(drive: DriveTab) {
    setActiveDrive(drive)
    setEffectiveBase("")
    setSearch("")
    searchRef.current = ""
    changePath(drive.base)
  }

  async function handleDelete() {
    if (selected.size === 0) return
    if (!confirm(`Delete ${selected.size} item(s)?`)) return
    for (const path of selected) {
      await fetch(`/api/files?path=${encodeURIComponent(path)}`, { method: "DELETE" })
    }
    fetchFiles(currentPath)
  }

  function uploadFileWithProgress(item: UploadProgress, run: number, overwrite = false): Promise<void> {
    return new Promise(resolve => {
      let settled = false
      let xhr: XMLHttpRequest | null = null
      const update = (patch: Partial<UploadProgress>) => {
        if (mounted.current && uploadRun.current === run) {
          setUploads(previous => previous.map(upload => upload.id === item.id ? { ...upload, ...patch } : upload))
        }
      }
      const finish = (error: string | null, conflict = false) => {
        if (settled) return
        settled = true
        if (activeUpload.current === xhr) activeUpload.current = null
        update({ done: true, error, conflict, loaded: error ? 0 : item.file.size, total: item.file.size })
        resolve()
      }
      update({ loaded: 0, total: item.file.size, done: false, error: null, conflict: false })
      try {
        const form = new FormData()
        form.append("path", item.destination)
        form.append("relative_path", item.fileName)
        form.append("overwrite", String(overwrite))
        form.append("file", item.file)
        xhr = new XMLHttpRequest()
        activeUpload.current = xhr
        xhr.open("POST", "/api/files/upload")
        xhr.upload.onprogress = event => {
          if (!settled && event.lengthComputable) update({ loaded: event.loaded, total: event.total })
        }
        xhr.onload = () => {
          const request = xhr!
          let response: { error?: unknown; path?: unknown } | null = null
          try { response = JSON.parse(request.responseText) } catch { /* Non-JSON failures need a fallback. */ }
          if (request.status < 200 || request.status >= 300) {
            const message = typeof response?.error === "string" && response.error.trim()
              ? response.error : `Upload failed (${request.status}). Please retry.`
            finish(message, request.status === 409)
          } else if (typeof response?.path !== "string") {
            finish("Could not confirm the upload. Check the folder before retrying.")
          } else finish(null)
        }
        xhr.onerror = () => finish("Connection lost. Check the folder before retrying.")
        xhr.onabort = () => finish("Upload cancelled. Check the folder before retrying.")
        xhr.ontimeout = () => finish("Upload timed out. Check the folder before retrying.")
        xhr.send(form)
      } catch (failure) {
        finish(failure instanceof Error ? `Could not start upload: ${failure.message}` : "Could not start upload. Please retry.")
      }
    })
  }

  async function processFiles(fileArr: globalThis.File[]) {
    // React state may not have rendered when a second picker/drop event arrives.
    if (!fileArr.length || uploadBusy.current || !currentPathRef.current) return
    const destination = currentPathRef.current
    let batch: UploadProgress[]
    try {
      batch = fileArr.map(file => ({
        id: ++uploadId.current, file, fileName: uploadRelativePath(file), destination,
        loaded: 0, total: file.size, done: false, error: null, conflict: false,
      }))
    } catch (failure) {
      setUploadError(failure instanceof Error ? failure.message : "Invalid upload path")
      return
    }
    uploadBusy.current = true
    const run = ++uploadRun.current
    setUploadError(null)
    setUploads(previous => [...previous, ...batch])
    setUploading(true)
    try {
      for (const item of batch) {
        if (!mounted.current || run !== uploadRun.current) break
        await uploadFileWithProgress(item, run)
      }
    } finally {
      if (run === uploadRun.current) {
        uploadBusy.current = false
        if (mounted.current) {
          setUploading(false)
          if (currentPathRef.current === destination) void fetchFiles(destination, searchRef.current || undefined)
        }
      }
    }
  }

  async function retryUpload(item: UploadProgress) {
    if (uploadBusy.current || !item.error) return
    // A lost response may mean a previous attempt succeeded. Every ordinary
    // retry is non-overwriting; only a confirmed name conflict can replace.
    const overwrite = item.conflict
    if (overwrite && !confirm(`Replace ${item.fileName} in ${item.destination}?`)) return
    uploadBusy.current = true
    const run = ++uploadRun.current
    setUploading(true)
    try {
      await uploadFileWithProgress(item, run, overwrite)
    } finally {
      if (run === uploadRun.current) {
        uploadBusy.current = false
        if (mounted.current) {
          setUploading(false)
          if (currentPathRef.current === item.destination) void fetchFiles(item.destination, searchRef.current || undefined)
        }
      }
    }
  }

  function cancelUploads() {
    if (!uploadBusy.current) return
    uploadRun.current++
    uploadBusy.current = false
    activeUpload.current?.abort()
    activeUpload.current = null
    setUploading(false)
    setUploads(previous => previous.map(item => item.done ? item : {
      ...item, done: true, loaded: 0, error: "Upload cancelled. Check the folder before retrying.", conflict: false,
    }))
  }

  async function handleUpload(event: React.ChangeEvent<HTMLInputElement>) {
    const fileArr = Array.from(event.target.files ?? [])
    // Allow selecting the same file again after a failed or cancelled attempt.
    event.target.value = ""
    await processFiles(fileArr)
  }

  const handleDragEnter = useCallback((e: React.DragEvent) => {
    e.preventDefault()
    e.stopPropagation()
    dragCounter.current++
    if (e.dataTransfer.items?.length) setDragging(true)
  }, [])

  const handleDragLeave = useCallback((e: React.DragEvent) => {
    e.preventDefault()
    e.stopPropagation()
    dragCounter.current--
    if (dragCounter.current === 0) setDragging(false)
  }, [])

  const handleDragOver = useCallback((e: React.DragEvent) => {
    e.preventDefault()
    e.stopPropagation()
  }, [])

  async function handleDrop(event: React.DragEvent) {
    event.preventDefault()
    event.stopPropagation()
    setDragging(false)
    dragCounter.current = 0
    if (uploadBusy.current) return
    const items = Array.from(event.dataTransfer.items ?? [])
    if (items.some(item => item.webkitGetAsEntry?.()?.isDirectory)) {
      setUploadError("Choose Upload Folder to preserve a folder's structure.")
      return
    }
    const dropped = Array.from(event.dataTransfer.files)
    if (!dropped.length) {
      setUploadError("No files were available from this drop. Use Upload or Upload Folder instead.")
      return
    }
    await processFiles(dropped)
  }

  function handleDownloadSelected() {
    if (selected.size === 0) return
    const form = document.createElement("form")
    form.method = "POST"
    form.action = "/api/files/download-zip-multi"
    form.style.display = "none"
    const input = document.createElement("input")
    input.type = "hidden"
    input.name = "paths"
    input.value = JSON.stringify(Array.from(selected))
    form.appendChild(input)
    document.body.appendChild(form)
    form.submit()
    form.remove()
  }

  async function handleNewFolder() {
    const name = prompt("Folder name:")
    if (!name) return
    await fetch("/api/files/mkdir", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ path: `${currentPath}/${name}` }),
    })
    fetchFiles(currentPath)
  }

  if (!activeDrive) {
    return (
      <div className="flex items-center justify-center p-8">
        <Loader2 className="h-5 w-5 animate-spin text-slate-500" />
      </div>
    )
  }

  const base = effectiveBase || activeDrive.base
  const relativePath = currentPath.replace(base, "") || "/"

  return (
    <div className="flex h-[calc(100vh-120px)] flex-col space-y-4 md:h-[calc(100vh-96px)]">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold text-slate-100">Files</h1>
          <p className="mt-1 text-sm text-slate-500">
            Manage dashcam clips and media files
          </p>
        </div>
        <div className="flex flex-wrap gap-2">
          <button
            onClick={handleNewFolder}
            className="glass-card glass-card-hover flex items-center gap-1.5 px-3 py-1.5 text-sm text-slate-400 transition-colors hover:text-slate-200"
          >
            <FolderPlus className="h-4 w-4" />
            New Folder
          </button>
          <button
            onClick={() => uploadRef.current?.click()}
            disabled={uploading}
            className={cn(
              "glass-card glass-card-hover flex items-center gap-1.5 px-3 py-1.5 text-sm transition-colors",
              uploading ? "text-slate-600 cursor-not-allowed" : "text-slate-400 hover:text-slate-200"
            )}
          >
            {uploading ? <Loader2 className="h-4 w-4 animate-spin" /> : <Upload className="h-4 w-4" />}
            {uploading ? "Uploading..." : "Upload"}
          </button>
          <input ref={uploadRef} aria-label="Choose files to upload" type="file" multiple className="hidden" onChange={handleUpload} />
          <button
            onClick={() => folderUploadRef.current?.click()}
            disabled={uploading}
            className={cn(
              "glass-card glass-card-hover flex items-center gap-1.5 px-3 py-1.5 text-sm transition-colors",
              uploading ? "text-slate-600 cursor-not-allowed" : "text-slate-400 hover:text-slate-200"
            )}
          >
            <FolderOpen className="h-4 w-4" />
            Upload Folder
          </button>
          {/* @ts-expect-error webkitdirectory is non-standard but supported in all major browsers */}
          <input ref={folderUploadRef} aria-label="Choose folder to upload" type="file" multiple webkitdirectory="" className="hidden" onChange={handleUpload} />
        </div>
      </div>

      <div className="flex flex-wrap gap-1">
        {drives.map((drive) => (
          <button
            key={drive.id}
            onClick={() => switchDrive(drive)}
            className={cn(
              "flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-sm font-medium transition-colors",
              activeDrive.id === drive.id
                ? "bg-blue-500/15 text-blue-400"
                : "text-slate-500 hover:bg-white/5 hover:text-slate-300"
            )}
          >
            {(() => { const Icon = TAB_ICONS[drive.icon]; return <Icon className="h-3.5 w-3.5" /> })()}
            {drive.id}
          </button>
        ))}
      </div>

      <div className="flex items-center gap-2">
        <div className="relative flex-1">
          <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-slate-600" />
          <input
            type="text"
            value={search}
            onChange={(e) => handleSearchChange(e.target.value)}
            placeholder="Search files..."
            className="w-full rounded-lg border border-white/10 bg-white/5 py-1.5 pl-8 pr-8 text-sm text-slate-300 placeholder-slate-600 outline-none transition focus:border-blue-500/50 focus:ring-1 focus:ring-blue-500/25"
          />
          {search && (
            <button
              onClick={() => handleSearchChange("")}
              className="absolute right-2 top-1/2 -translate-y-1/2 rounded p-0.5 text-slate-600 hover:text-slate-400"
            >
              <X className="h-3.5 w-3.5" />
            </button>
          )}
        </div>
        <div className="relative" ref={sortMenuRef}>
          <button
            onClick={() => setShowSortMenu(!showSortMenu)}
            className={cn(
              "glass-card glass-card-hover flex items-center gap-1.5 whitespace-nowrap px-3 py-1.5 text-sm transition-colors",
              showSortMenu ? "text-blue-400" : "text-slate-400 hover:text-slate-200"
            )}
          >
            <ArrowUpDown className="h-3.5 w-3.5" />
            <span className="hidden sm:inline">{SORT_LABELS[sortOption]}</span>
          </button>
          {showSortMenu && (
            <div className="absolute right-0 top-full z-20 mt-1 w-44 overflow-hidden rounded-lg border border-white/10 bg-slate-900 shadow-xl">
              {(Object.keys(SORT_LABELS) as SortOption[]).map((opt) => (
                <button
                  key={opt}
                  onClick={() => { setSortOption(opt); setShowSortMenu(false) }}
                  className={cn(
                    "flex w-full items-center justify-between px-3 py-2 text-left text-sm transition-colors hover:bg-white/5",
                    sortOption === opt ? "text-blue-400" : "text-slate-400"
                  )}
                >
                  {SORT_LABELS[opt]}
                  {sortOption === opt && <Check className="h-3.5 w-3.5" />}
                </button>
              ))}
            </div>
          )}
        </div>
      </div>

      {uploadError && <p role="alert" className="text-sm text-red-400">{uploadError}</p>}
      {uploads.length > 0 && (
        <div className="glass-card max-h-72 shrink-0 space-y-2 overflow-y-auto p-3" aria-label="Upload progress">
          <div className="flex items-center justify-between">
            <p className="text-xs font-medium text-slate-300" role="status">
              {uploading ? "Uploading files..." : uploads.some(item => item.error) ? "Some files were not uploaded" : (
                <span className="flex items-center gap-1.5">
                  <CheckCircle className="h-3.5 w-3.5 text-emerald-400" />Upload complete
                </span>
              )}
            </p>
            {uploading ? (
              <button type="button" onClick={cancelUploads} className="rounded border border-white/10 px-2 py-1 text-xs text-slate-300">Cancel uploads</button>
            ) : (
              <button type="button" aria-label="Dismiss upload results" onClick={() => setUploads([])} className="rounded p-0.5 text-slate-600 hover:text-slate-400">
                <X className="h-3.5 w-3.5" />
              </button>
            )}
          </div>
          {uploads.map(item => {
            const pct = item.total > 0 ? Math.min(100, Math.round((item.loaded / item.total) * 100)) : item.done && !item.error ? 100 : 0
            return (
              <div key={item.id} className="space-y-1">
                <div className="flex items-center justify-between gap-2 text-[11px]">
                  <span className="truncate text-slate-400" title={item.fileName}>{item.fileName}</span>
                  <span className={cn("tabular-nums", item.error ? "text-red-400" : item.done ? "text-emerald-400" : "text-slate-500")}>
                    {item.error ? "Not uploaded" : item.done ? "Done" : `${pct}%`}
                  </span>
                </div>
                <p className="truncate text-[10px] text-slate-500" title={item.destination}>To {item.destination}</p>
                {item.error ? (
                  <div className="flex flex-wrap items-center gap-2">
                    <p role="alert" className="text-xs text-red-300">{item.error}</p>
                    <button type="button" disabled={uploading} onClick={() => void retryUpload(item)}
                      className="rounded border border-white/10 px-2 py-1 text-xs text-slate-300 disabled:opacity-50">
                      {item.conflict ? "Replace existing file" : "Retry"}
                    </button>
                  </div>
                ) : (
                  <div className="h-1 overflow-hidden rounded-full bg-slate-800" role="progressbar" aria-label={`Upload ${item.fileName}`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct}>
                    <div className={cn("h-full rounded-full transition-all duration-300", item.done ? "bg-emerald-500" : "bg-blue-500")} style={{ width: `${pct}%` }} />
                  </div>
                )}
              </div>
            )
          })}
          {uploading && uploads.length > 1 && (
            <p className="border-t border-white/5 pt-2 text-[11px] text-slate-500">
              {uploads.filter(item => item.done).length}/{uploads.length} files finished
            </p>
          )}
        </div>
      )}

      <div
        className={cn("glass-card flex min-h-0 flex-1 flex-col overflow-hidden relative", dragging && "ring-2 ring-blue-500/50")}
        onDragEnter={handleDragEnter}
        onDragLeave={handleDragLeave}
        onDragOver={handleDragOver}
        onDrop={handleDrop}
      >
        {dragging && (
          <div className="absolute inset-0 z-30 flex items-center justify-center bg-slate-900/80 backdrop-blur-sm">
            <div className="flex flex-col items-center gap-2 text-blue-400">
              <Upload className="h-10 w-10" />
              <p className="text-sm font-medium">Drop files here; use Upload Folder for folders</p>
            </div>
          </div>
        )}
        <div className="flex items-center justify-between border-b border-white/5 px-3 py-2">
          <div className="flex items-center gap-2">
            {currentPath !== base && (
              <button
                onClick={goUp}
                className="rounded p-1 text-slate-500 hover:bg-white/5 hover:text-slate-300"
              >
                <ArrowLeft className="h-4 w-4" />
              </button>
            )}
            <p className="font-mono text-sm text-slate-400">{relativePath}</p>
          </div>
          {selected.size > 0 && (
            <span className="rounded-full bg-blue-500/20 px-2 py-0.5 text-[10px] font-semibold text-blue-400">
              {selected.size} selected
            </span>
          )}
        </div>

        <div className="flex-1 overflow-y-auto">
          {loading ? (
            <div className="flex items-center justify-center p-8">
              <Loader2 className="h-5 w-5 animate-spin text-slate-500" />
            </div>
          ) : error ? (
            <div className="flex flex-col items-center justify-center p-8">
              <FolderOpen className="mb-2 h-10 w-10 text-slate-500" />
              <p className="text-sm text-slate-500">{error}</p>
            </div>
          ) : sortedFiles.length === 0 ? (
            <div className="flex flex-col items-center justify-center p-8">
              {(() => { const Icon = TAB_ICONS[activeDrive.icon]; return <Icon className="mb-2 h-10 w-10 text-slate-500" /> })()}
              <p className="text-sm text-slate-500">{search ? "No matching files" : "Empty folder"}</p>
              <p className="mt-1 text-xs text-slate-600">
                {search ? "Try a different search term" : activeDrive.icon === "cam" ? "No clips in this folder" : "Upload files to get started"}
              </p>
            </div>
          ) : (
            <table className="w-full text-sm">
              <tbody>
                {sortedFiles.map((f) => (
                  <tr
                    key={f.path}
                    className={cn(
                      "cursor-pointer border-b border-white/5 transition-colors hover:bg-white/5",
                      selected.has(f.path) && "bg-blue-500/10"
                    )}
                    onClick={() => {
                      if (f.is_dir) {
                        navigate(f)
                      } else {
                        setSelected((prev) => {
                          const next = new Set(prev)
                          if (next.has(f.path)) next.delete(f.path)
                          else next.add(f.path)
                          return next
                        })
                      }
                    }}
                  >
                    <td className="w-8 px-2 py-3">
                      <input
                        type="checkbox"
                        checked={selected.has(f.path)}
                        onChange={() => {
                          setSelected((prev) => {
                            const next = new Set(prev)
                            if (next.has(f.path)) next.delete(f.path)
                            else next.add(f.path)
                            return next
                          })
                        }}
                        onClick={(e) => e.stopPropagation()}
                        className="h-3.5 w-3.5 cursor-pointer rounded border-slate-600 accent-blue-500"
                      />
                    </td>
                    <td className="px-1 py-3">
                      {f.is_dir ? (
                        <Folder className="h-4 w-4 text-blue-400" />
                      ) : (
                        <File className="h-4 w-4 text-slate-500" />
                      )}
                    </td>
                    <td className="min-w-0 truncate py-3 text-slate-300">{f.name}</td>
                    <td className="hidden px-3 py-3 text-right text-xs text-slate-600 sm:table-cell">
                      {f.mod_time ? new Date(f.mod_time).toLocaleDateString() : ""}
                    </td>
                    <td className="px-3 py-3 text-right text-xs text-slate-600">
                      {f.is_dir ? "" : formatSize(f.size)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      </div>

      {selected.size > 0 && (
        <div className="fixed bottom-6 left-1/2 z-50 -translate-x-1/2 md:left-[calc(50%+7rem)]">
          <div className="glass-card flex items-center gap-3 border border-blue-500/20 bg-slate-900/95 px-4 py-3 shadow-2xl backdrop-blur-xl animate-in slide-in-from-bottom-2 fade-in duration-200">
            <span className="rounded-full bg-blue-500/20 px-2.5 py-1 text-xs font-semibold text-blue-400">
              {selected.size} selected
            </span>
            <div className="h-4 w-px bg-white/10" />
            <button
              onClick={handleDownloadSelected}
              className="flex items-center gap-2 rounded-lg bg-blue-500/15 px-3 py-2 text-sm font-medium text-blue-400 transition-colors hover:bg-blue-500/25"
            >
              <Download className="h-4 w-4" />
              Download
            </button>
            <button
              onClick={handleDelete}
              className="flex items-center gap-2 rounded-lg bg-red-500/10 px-3 py-2 text-sm font-medium text-red-400 transition-colors hover:bg-red-500/20"
            >
              <Trash2 className="h-4 w-4" />
              Delete
            </button>
            <button
              onClick={() => setSelected(new Set())}
              className="rounded-lg p-2 text-slate-500 transition-colors hover:bg-white/5 hover:text-slate-300"
              title="Clear selection"
            >
              <X className="h-4 w-4" />
            </button>
          </div>
        </div>
      )}
    </div>
  )
}
