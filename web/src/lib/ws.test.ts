import assert from 'node:assert/strict'
import test from 'node:test'

test('subscriptions share a connecting socket; stale callbacks and intentional closes cannot reconnect it', async t => {
  const previous = ['window', 'WebSocket'].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  class Socket {
    static CONNECTING = 0; static OPEN = 1
    static instances: Socket[] = []
    readyState = 0; closes = 0; sent: string[] = []
    onopen: (() => void) | null = null; onclose: (() => void) | null = null
    onmessage: ((event: { data: string }) => void) | null = null; onerror: (() => void) | null = null
    constructor() { Socket.instances.push(this) }
    open() { this.readyState = 1; this.onopen?.() }
    close() { this.readyState = 3; this.closes++; this.onclose?.() }
    send(value: string) { this.sent.push(value) }
  }
  Object.defineProperty(globalThis, 'window', { configurable: true, value: { location: { protocol: 'http:', host: 'localhost' } } })
  Object.defineProperty(globalThis, 'WebSocket', { configurable: true, value: Socket })
  t.mock.timers.enable({ apis: ['setTimeout', 'setInterval'] })
  const { WebSocketClient } = await import('./ws.ts')
  const client = new WebSocketClient()
  const states: boolean[] = [], messages: unknown[] = []
  try {
    client.onStatusChange(state => states.push(state))
    client.subscribe('status', value => messages.push(value))
    client.subscribe('progress', () => {})
    assert.equal(Socket.instances.length, 1)
    const first = Socket.instances[0]
    const stale = { open: first.onopen!, close: first.onclose!, error: first.onerror!, message: first.onmessage! }
    first.open()
    t.mock.timers.tick(25000)
    assert.deepEqual(first.sent, ['{"type":"ping"}'])
    client.reconnect()
    const second = Socket.instances[1]
    stale.open(); stale.close(); stale.error(); stale.message({ data: '{"type":"status","data":"stale"}' })
    assert.equal(second.closes, 0)
    assert.equal(client.isConnected, false)
    second.open()
    second.onmessage!({ data: '{"type":"status","data":"fresh"}' })
    assert.deepEqual(messages, ['fresh'])
    assert.deepEqual(states, [true, false, true])
    client.disconnect()
    t.mock.timers.tick(120000)
    assert.equal(Socket.instances.length, 2)
    assert.equal(client.isConnected, false)

    // A genuine outage retries once with bounded backoff. Manual connect clears
    // a pending retry; successful open resets the next delay to three seconds.
    client.connect()
    Socket.instances.at(-1)!.close()
    t.mock.timers.tick(2999); assert.equal(Socket.instances.length, 3)
    t.mock.timers.tick(1); assert.equal(Socket.instances.length, 4)
    Socket.instances.at(-1)!.close()
    t.mock.timers.tick(5999); assert.equal(Socket.instances.length, 4)
    t.mock.timers.tick(1); assert.equal(Socket.instances.length, 5)
    Socket.instances.at(-1)!.close()
    client.connect()
    t.mock.timers.tick(12000); assert.equal(Socket.instances.length, 6)
    Socket.instances.at(-1)!.open()
    Socket.instances.at(-1)!.close()
    t.mock.timers.tick(3000); assert.equal(Socket.instances.length, 7)
  } finally {
    client.disconnect()
    t.mock.timers.reset()
    for (const [key, descriptor] of previous) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
  }
})
