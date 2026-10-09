import { createContext, useContext, useId, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react"
import { createPortal } from "react-dom"
import { X } from "lucide-react"
import { cn } from "@/lib/utils"

interface ModalProps {
  title: ReactNode
  onClose: () => void
  /** Set false to suppress the close button, backdrop and Escape dismissal. */
  dismissable?: boolean
  size?: "sm" | "md" | "lg"
  footer?: ReactNode
  children: ReactNode
  className?: string
}

const SIZE_MAX = { sm: "420px", md: "560px", lg: "768px" }
interface DialogToken { parent: DialogToken | null }
const ParentDialog = createContext<DialogToken | null>(null)
interface Layer { node: HTMLDivElement; token: DialogToken; returnFocus: HTMLElement | null }
interface Layers {
  items: Layer[]
  inert: Map<HTMLElement, boolean>
  overflow: string
  overflowPriority: string
  observer: MutationObserver | null
}
const documents = new WeakMap<Document, Layers>()

function layersFor(document: Document): Layers {
  let layers = documents.get(document)
  if (!layers) {
    layers = { items: [], inert: new Map(), overflow: "", overflowPriority: "", observer: null }
    documents.set(document, layers)
  }
  return layers
}

function syncLayers(document: Document, layers: Layers) {
  const active = layers.items.at(-1)?.node
  if (!active) {
    for (const [element, inert] of layers.inert) element.inert = inert
    layers.inert.clear()
    return
  }
  for (const element of Array.from(document.body.children) as HTMLElement[]) {
    if (!layers.inert.has(element)) layers.inert.set(element, element.inert)
    element.inert = element !== active
  }
  layers.items.forEach((layer, index) => { layer.node.style.zIndex = String(2000 + index) })
}

function focusable(layer: HTMLElement): HTMLElement[] {
  return Array.from(layer.querySelectorAll<HTMLElement>(
    'button, a[href], area[href], input, select, textarea, summary, iframe, [contenteditable="true"], [tabindex]',
  )).filter(element => {
    if (element.matches(':disabled, input[type="hidden"]') || element.closest('[hidden], [inert]')) return false
    if (element.tabIndex < 0 && !(element.isContentEditable && !element.hasAttribute('tabindex'))) return false
    for (let ancestor: HTMLElement | null = element; ancestor; ancestor = ancestor.parentElement) {
      const style = layer.ownerDocument.defaultView!.getComputedStyle(ancestor)
      if (style.display === "none" || style.visibility === "hidden" || style.visibility === "collapse") return false
      if (ancestor.tagName === "DETAILS" && !ancestor.hasAttribute("open")) {
        const summary = Array.from(ancestor.children).find(child => child.tagName === "SUMMARY")
        if (!summary?.contains(element)) return false
      }
      if (ancestor === layer) break
    }
    return true
  }).sort((a, b) => (a.tabIndex > 0 ? a.tabIndex : Number.MAX_SAFE_INTEGER) - (b.tabIndex > 0 ? b.tabIndex : Number.MAX_SAFE_INTEGER))
}

function focusFirst(node: HTMLElement) {
  ;(focusable(node)[0] ?? node).focus()
}

export function Modal({ title, onClose, dismissable = true, size = "md", footer, children, className }: ModalProps) {
  const titleId = useId()
  const parent = useContext(ParentDialog)
  const token = useMemo(() => ({ parent }), [parent])
  const layer = useRef<HTMLDivElement>(null)
  const latest = useRef({ onClose, dismissable })
  // Capture before a descendant's autoFocus runs during the portal commit.
  const [returnFocus] = useState(() => document.activeElement as HTMLElement | null)
  useLayoutEffect(() => { latest.current = { onClose, dismissable } })
  useLayoutEffect(() => {
    const node = layer.current!
    const document = node.ownerDocument
    const window = document.defaultView!
    const layers = layersFor(document)
    const isTop = () => layers.items.at(-1)?.node === node
    if (!layers.items.length) {
      layers.overflow = document.body.style.getPropertyValue("overflow")
      layers.overflowPriority = document.body.style.getPropertyPriority("overflow")
      document.body.style.setProperty("overflow", "hidden")
      layers.observer = new window.MutationObserver(() => syncLayers(document, layers))
      layers.observer.observe(document.body, { childList: true })
    }
    const entry: Layer = { node, token, returnFocus }
    // A nested portal's effect can run before its parent's on initial mount.
    // Insert that parent underneath its descendants; later dialogs go on top.
    const firstChild = layers.items.findIndex(item => {
      for (let ancestor = item.token.parent; ancestor; ancestor = ancestor.parent) {
        if (ancestor === token) return true
      }
      return false
    })
    layers.items.splice(firstChild < 0 ? layers.items.length : firstChild, 0, entry)
    syncLayers(document, layers)
    if (isTop()) focusFirst(node)

    function onKey(event: KeyboardEvent) {
      if (!isTop() || event.defaultPrevented) return
      const target = event.target as HTMLElement | null
      // Native select popups own Escape. Custom dropdowns may consume Escape
      // or Tab before this bubbling listener sees the event.
      if (event.key === "Escape" && target?.closest?.("select")) return
      if ((event.key === "Escape" || event.key === "Tab") && target?.closest?.('[data-select-popup], [data-select-trigger][aria-expanded="true"]')) return
      if (event.key === "Escape") {
        event.preventDefault()
        event.stopImmediatePropagation()
        if (latest.current.dismissable) latest.current.onClose()
      } else if (event.key === "Tab") {
        const elements = focusable(node)
        const first = elements[0]
        const last = elements.at(-1)
        if (!first) {
          event.preventDefault()
          node.focus()
        } else if (!node.contains(document.activeElement) || document.activeElement === node) {
          event.preventDefault()
          ;(event.shiftKey ? last : first)?.focus()
        } else if (event.shiftKey && document.activeElement === first) {
          event.preventDefault()
          last?.focus()
        } else if (!event.shiftKey && document.activeElement === last) {
          event.preventDefault()
          first.focus()
        }
      }
    }
    function onFocus(event: FocusEvent) {
      if (isTop() && !node.contains(event.target as Node)) focusFirst(node)
    }
    function blockBackground(event: MouseEvent) {
      if (isTop() && !node.contains(event.target as Node)) {
        event.preventDefault()
        event.stopImmediatePropagation()
      }
    }
    window.addEventListener("keydown", onKey)
    document.addEventListener("focusin", onFocus)
    document.addEventListener("click", blockBackground, true)
    return () => {
      const wasTop = isTop()
      window.removeEventListener("keydown", onKey)
      document.removeEventListener("focusin", onFocus)
      document.removeEventListener("click", blockBackground, true)
      layers.items = layers.items.filter(item => item !== entry)
      // Removing an entire nested stack must restore its original opener,
      // not a button in a parent dialog which is also being removed.
      for (const remaining of layers.items) {
        if (remaining.returnFocus && node.contains(remaining.returnFocus)) remaining.returnFocus = entry.returnFocus
      }
      if (!layers.items.length) {
        layers.observer?.disconnect()
        layers.observer = null
        document.body.style.setProperty("overflow", layers.overflow, layers.overflowPriority)
      }
      syncLayers(document, layers)
      const top = layers.items.at(-1)?.node
      if (!wasTop && top?.contains(document.activeElement)) return
      const previous = entry.returnFocus
      if (previous?.isConnected && !previous.closest("[inert]") && (!top || top.contains(previous))) previous.focus()
      else if (top) focusFirst(top)
    }
  }, [token, returnFocus])

  return createPortal(
    <ParentDialog.Provider value={token}>
      <div ref={layer} className="modal-shell" role="dialog" aria-modal="true" aria-labelledby={titleId} tabIndex={-1}
        onClick={event => {
          if (event.target === event.currentTarget && layersFor(event.currentTarget.ownerDocument).items.at(-1)?.node === event.currentTarget && dismissable) onClose()
        }}>
        <div className={cn("glass-card modal-card", className)} style={{ maxWidth: SIZE_MAX[size] }}>
          <div className="modal-header">
            <h2 id={titleId} className="modal-title">{title}</h2>
            {dismissable && <button type="button" className="modal-close" onClick={onClose} aria-label="Close">
              <X className="h-4 w-4" />
            </button>}
          </div>
          <div className="modal-body">{children}</div>
          {footer && <div className="border-t border-white/5 px-4 py-3">{footer}</div>}
        </div>
      </div>
    </ParentDialog.Provider>,
    document.body,
  )
}
