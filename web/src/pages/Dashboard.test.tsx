import assert from "node:assert/strict"
import test from "node:test"
import { act, createElement } from "react"
import { MemoryRouter } from "react-router-dom"
import { Window } from "happy-dom"

async function dashboardFixture(overrides: Record<string, unknown> = {}) {
  const win = new Window({ url: "http://localhost/" })
  const descriptors = ["window", "document", "navigator", "localStorage", "WebSocket", "IS_REACT_ACT_ENVIRONMENT"].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  class TestSocket { static OPEN = 1; readyState = 1; close() {}; send() {} }
  for (const [key, value] of Object.entries({ window: win, document: win.document, navigator: win.navigator, localStorage: win.localStorage, WebSocket: TestSocket, IS_REACT_ACT_ENVIRONMENT: true })) {
    Object.defineProperty(globalThis, key, { configurable: true, value })
  }
  const oldFetch = globalThis.fetch
  const writes: { path: string; body: unknown }[] = []
  const failed = new Set<string>()
  const fixtures: Record<string, unknown> = {
    "/api/status": { cpu_temp: "55000", num_snapshots: "86", snapshot_oldest: "1770000000", snapshot_newest: "1770000300", total_space: "1000000", free_space: "45000", uptime: "60", drives_active: "yes", udc_state: "configured", wifi_ssid: "Test network", wifi_strength: "60/70", wifi_ip: "192.0.2.1", ether_ip: "", ether_speed: "", fan_speed: "2000", supply_voltage: 5.09602, storage_health: { state: "healthy", message: "Storage managed automatically" } },
    "/api/archive/status": { phase: "archiving", current: 300, total: 3200, eta_seconds: 6200, eta_state: "running", cycle: { id: "fixture-cycle", cancelling: false } },
    "/api/status/storage": { cam_size: 100000, snapshots_size: 855000, total_space: 1000000, free_space: 45000 },
    "/api/system/wifi-firmware": { eligible: false, supported_board: false, up_to_date: false, target_version: "7.45.286", install: { state: "idle" } },
  }
  Object.assign(fixtures, overrides)
  globalThis.fetch = async (input, init) => {
    const path = String(input)
    if (init?.method === "POST") writes.push({ path, body: init.body ? JSON.parse(String(init.body)) : null })
    if (failed.has(path)) return Response.json({ error: "Fixture request failed" }, { status: 503 })
    return Response.json(fixtures[path] ?? {})
  }
  const { createRoot } = await import("react-dom/client")
  const { default: Dashboard } = await import("./Dashboard.tsx")
  const container = win.document.createElement("div")
  win.document.body.append(container)
  const root = createRoot(container as unknown as HTMLElement)
  await act(async () => root.render(createElement(MemoryRouter, null, createElement(Dashboard))))
  return {
    win, container, fixtures, failed, writes,
    refresh: () => act(async () => { win.document.dispatchEvent(new win.Event("visibilitychange")) }),
    cleanup: async () => {
      await act(async () => root.unmount())
      globalThis.fetch = oldFetch
      for (const [key, descriptor] of descriptors) {
        if (descriptor) Object.defineProperty(globalThis, key, descriptor)
        else Reflect.deleteProperty(globalThis, key)
      }
      win.close()
    },
  }
}

test("dashboard retains four cards, shows available voltage and uses server ETA with cycle-specific cancellation", async () => {
  const fixture = await dashboardFixture()
  const { container, fixtures, failed, writes } = fixture
  try {
    assert.equal(container.querySelectorAll(".tile-grid > *").length, 4)
    assert.match(container.textContent, /5V supply5\.10 V/)
    assert.match(container.textContent, /About 1\.7 h remaining/)
    assert.match(container.textContent, /Storage managed automatically/)
    assert.equal(container.querySelector(".halo-red"), null, "Managed snapshot usage is not a storage failure")
    assert.doesNotMatch(container.textContent, /Wi-Fi firmware update/)
    let cancel = [...container.querySelectorAll("button")].find(button => button.textContent === "Cancel archive")!
    assert.equal(cancel.disabled, false)
    await act(async () => cancel.click())
    assert.deepEqual(writes, [{ path: "/api/archive/cancel", body: { cycle_id: "fixture-cycle" } }])
    assert.match(container.textContent, /Stopping this archive safely/)
    fixtures["/api/status"] = { ...fixtures["/api/status"] as object, supply_voltage: null }
    fixtures["/api/archive/status"] = { phase: "archiving", current: 0, total: 200 }
    await fixture.refresh()
    assert.doesNotMatch(container.textContent, /5V supply/)
    assert.match(container.textContent, /Estimate unavailable/)
    cancel = [...container.querySelectorAll("button")].find(button => button.textContent === "Cancel archive")!
    assert.equal(cancel.disabled, true, "Older backend progress cannot cancel an unidentified cycle")
    failed.add("/api/status")
    failed.add("/api/archive/status")
    await fixture.refresh()
    assert.match(container.textContent, /Reconnecting · showing the last update/)
    assert.match(container.textContent, /Storage managed automatically/)
    assert.match(container.textContent, /Waiting for connection/)
  } finally { await fixture.cleanup() }
})

test("eligible Pi shows firmware review and failed rollback stays actionable", async () => {
  const firmware = {
      eligible: true, supported_board: true, model: "Raspberry Pi 5 Model B", up_to_date: false,
      target_version: "7.45.286", running_version: "7.45.265", installed_version: "7.45.265",
      symptom_detected: false, symptom_detail: null, can_rollback: true, reboot_pending: false, pinned: false,
      install: { state: "failed", step: "reload", progress: 100, message: "Prior attempt interrupted", updated_at: 1 },
    }
  const fixture = await dashboardFixture({ "/api/system/wifi-firmware": firmware })
  const { container, win, failed, writes } = fixture
  try {
    assert.match(container.textContent, /Wi-Fi firmware update available/)
    assert.ok([...container.querySelectorAll("button")].some(button => button.textContent.includes("Review")))
    // Exercise the actionable error without starting a real firmware operation.
    const { createRoot } = await import("react-dom/client")
    const { WifiFirmwareModal } = await import("../components/dashboard/WifiFirmwareModal.tsx")
    const modalHost = win.document.createElement("div"); win.document.body.append(modalHost)
    const modalRoot = createRoot(modalHost as unknown as HTMLElement)
    failed.add("/api/system/wifi-firmware/rollback")
    try {
      await act(async () => modalRoot.render(createElement(WifiFirmwareModal, { status: firmware as import("../components/dashboard/WifiFirmwareModal.tsx").WifiFirmwareStatus, onClose: () => {}, onRefresh: () => {} })))
      const restore = [...win.document.querySelectorAll("button")].find(button => button.textContent === "Revert to the previous firmware")!
      assert.ok(restore)
      await act(async () => restore.click())
      assert.match(win.document.body.textContent, /Fixture request failed/)
      assert.doesNotMatch(win.document.body.textContent, /Restoring…/)
      assert.deepEqual(writes, [{ path: "/api/system/wifi-firmware/rollback", body: null }])
      assert.doesNotMatch(win.document.body.textContent, /keep-awake/)
      assert.equal(container.querySelectorAll(".tile-grid > *").length, 4)
    } finally { await act(async () => modalRoot.unmount()) }
  } finally { await fixture.cleanup() }
})
