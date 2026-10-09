import assert from 'node:assert/strict'
import test, { type TestContext } from 'node:test'
import { act, createElement, StrictMode } from 'react'
import { Window } from 'happy-dom'

interface Request {
  signal: AbortSignal; ignoreAbort: boolean
  finish(status?: number): void
}
async function withConnection(t: TestContext, run: (h: {
  requests: Request[]; state(): string | null
  tick(ms: number): Promise<void>; visibility(hidden: boolean): Promise<void>
  retry(): Promise<void>; unmount(): Promise<void>
}) => Promise<void>, strict = false) {
  const win = new Window({ url: 'http://localhost/' })
  let hidden = false, unmounted = false
  Object.defineProperty(win.document, 'hidden', { configurable: true, get: () => hidden })
  const requests: Request[] = []
  class Socket {
    static OPEN = 1; static CONNECTING = 0
    readyState = 0
    close() { this.readyState = 3 }
  }
  const values = {
    window: win, document: win.document, navigator: win.navigator,
    WebSocket: Socket, IS_REACT_ACT_ENVIRONMENT: true,
    fetch: (input: unknown, init: RequestInit) => new Promise<Response>((resolve, reject) => {
      assert.equal(String(input), '/api/health')
      const signal = init.signal as AbortSignal
      const request = { signal, ignoreAbort: false, finish: (status = 200) => resolve(Response.json({ ok: status === 200 }, { status })) }
      requests.push(request)
      signal.addEventListener('abort', () => { if (!request.ignoreAbort) reject(new Error('aborted')) }, { once: true })
    }),
  }
  const previous = Object.keys(values).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries(values)) Object.defineProperty(globalThis, key, { configurable: true, value })
  const { createRoot } = await import('react-dom/client')
  const { ConnectionProvider, useConnectionStatus } = await import('./useConnectionStatus.tsx')
  function Consumer() {
    const connection = useConnectionStatus()
    return createElement('button', { onClick: connection.retry }, connection.state)
  }
  const container = win.document.createElement('div'); win.document.body.append(container)
  const root = createRoot(container as unknown as HTMLElement)
  t.mock.timers.enable({ apis: ['setTimeout', 'setInterval'] })
  const unmount = async () => { if (!unmounted) { await act(async () => root.unmount()); unmounted = true } }
  try {
    const provider = createElement(ConnectionProvider, { children: createElement(Consumer) })
    await act(async () => root.render(strict ? createElement(StrictMode, null, provider) : provider))
    await run({ requests, state: () => container.textContent,
      tick: async ms => { await act(async () => t.mock.timers.tick(ms)) },
      visibility: async value => { hidden = value; await act(async () => win.document.dispatchEvent(new win.Event('visibilitychange'))) },
      retry: async () => { await act(async () => container.querySelector('button')!.click()) }, unmount,
    })
  } finally {
    await unmount()
    t.mock.timers.reset()
    win.close()
    for (const [key, descriptor] of previous) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
  }
}

test('health failures escalate, slow probes never overlap, and timed-out requests recover', async t => {
  await withConnection(t, async h => {
    assert.equal(h.requests.length, 1)
    await act(async () => h.requests[0].finish(503))
    assert.equal(h.state(), 'connected')
    await h.tick(8000)
    await act(async () => h.requests[1].finish(503))
    assert.equal(h.state(), 'reconnecting')
    await h.tick(8000)
    await h.tick(8000)
    assert.equal(h.requests.length, 3, 'do not overlap an unresolved probe')
    await h.tick(7000)
    assert.equal(h.requests[2].signal.aborted, true)
    assert.equal(h.state(), 'disconnected')
    await h.tick(1000)
    await act(async () => h.requests[3].finish())
    assert.equal(h.state(), 'connected')
  })
})

test('hidden tabs abort and stop probes; retry and unmount ignore stale completions', async t => {
  await withConnection(t, async h => {
    const initial = h.requests[0]; initial.ignoreAbort = true
    await h.visibility(true)
    assert.equal(initial.signal.aborted, true)
    await h.tick(40000)
    assert.equal(h.requests.length, 1)
    await h.visibility(false)
    const visible = h.requests[1]; visible.ignoreAbort = true
    await h.retry()
    assert.equal(visible.signal.aborted, true)
    assert.equal(h.requests.length, 3)
    await act(async () => h.requests[2].finish())
    assert.equal(h.state(), 'connected')
    await act(async () => { initial.finish(503); visible.finish(503) })
    assert.equal(h.state(), 'connected', 'superseded results must not change connection state')
    await h.tick(8000)
    const pending = h.requests.at(-1)!
    await h.unmount()
    assert.equal(pending.signal.aborted, true)
    const count = h.requests.length
    await h.tick(40000)
    assert.equal(h.requests.length, count)
  })
})

test('StrictMode aborts the discarded mount and retains a single polling loop', async t => {
  await withConnection(t, async h => {
    assert.equal(h.requests.length, 2)
    assert.equal(h.requests[0].signal.aborted, true)
    await act(async () => h.requests[1].finish())
    await h.tick(8000)
    assert.equal(h.requests.length, 3)
    await act(async () => h.requests[2].finish())
    assert.equal(h.state(), 'connected')
  }, true)
})
