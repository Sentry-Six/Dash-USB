import assert from 'node:assert/strict'
import test from 'node:test'
import { createLivenessProbe } from './liveness.ts'

test('liveness uses no-store health probes and preserves failures instead of masking them', async () => {
  const original = globalThis.fetch
  const calls: string[] = []
  const controller = new AbortController()
  let status = 200
  globalThis.fetch = async (input, init) => {
    calls.push(String(input))
    assert.equal(init?.cache, 'no-store')
    assert.equal(init?.signal, controller.signal)
    return Response.json({ ok: status === 200 }, { status })
  }
  try {
    const probe = createLivenessProbe()
    assert.equal((await probe(controller.signal)).ok, true)
    status = 503
    assert.equal((await probe(controller.signal)).ok, false)
    assert.deepEqual(calls, ['/api/health', '/api/health'])
  } finally { globalThis.fetch = original }
})

test('older-device 401, 404 and SPA responses fall back once; a web page is never healthy', async () => {
  const original = globalThis.fetch
  try {
    for (const legacy of ['401', '404', 'html']) {
      const calls: string[] = []
      let statusIsHtml = false
      globalThis.fetch = async input => {
        const path = String(input); calls.push(path)
        if (path === '/api/health' && legacy !== 'html') return new Response(null, { status: Number(legacy) })
        if (path === '/api/health' || statusIsHtml) return new Response('<html>SPA</html>', { headers: { 'content-type': 'text/html' } })
        return Response.json({ uptime: '60' })
      }
      const probe = createLivenessProbe()
      assert.equal((await probe()).ok, true)
      assert.equal((await probe()).ok, true)
      assert.deepEqual(calls, ['/api/health', '/api/status', '/api/status'])
      statusIsHtml = true
      await assert.rejects(probe(), /web page/)
    }
  } finally { globalThis.fetch = original }
})
