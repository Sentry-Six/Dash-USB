import assert from 'node:assert/strict'
import test from 'node:test'
import { Readable } from 'node:stream'
import { createPreviewFiles } from './preview-files.mjs'

async function call(handle, path, method = 'GET', form) {
  const response = form ? new Response(form) : null
  const req = Readable.from(response ? [Buffer.from(await response.arrayBuffer())] : [])
  req.method = method
  req.headers = { 'content-type': response?.headers.get('content-type') || '' }
  const result = {}
  const res = {
    writeHead(status, headers) { result.status = status; result.headers = headers },
    end(data) { result.body = Buffer.isBuffer(data) ? data.toString() : data },
  }
  assert.equal(await handle(req, res, new URL(path, 'http://localhost')), true)
  return result
}
function upload(relative, content, overwrite = false) {
  const form = new FormData()
  form.set('file', new Blob([content]), 'same.txt')
  form.set('path', '/mutable')
  form.set('relative_path', relative)
  form.set('overwrite', String(overwrite))
  return form
}

test('preview retains separate folders and requires explicit replacement without changing old bytes', async () => {
  const route = createPreviewFiles()
  assert.equal((await call(route, '/api/files/upload', 'POST', upload('trip/front/same.txt', 'original'))).status, 200)
  assert.equal((await call(route, '/api/files/upload', 'POST', upload('trip/rear/same.txt', 'rear'))).status, 200)
  assert.equal((await call(route, '/api/files/upload', 'POST', upload('trip/front/same.txt', 'replacement'))).status, 409)
  assert.equal((await call(route, '/api/files/download?path=/mutable/trip/front/same.txt')).body, 'original')
  assert.equal((await call(route, '/api/files/upload', 'POST', upload('trip/front/same.txt', 'replacement', true))).status, 200)
  assert.equal((await call(route, '/api/files/download?path=/mutable/trip/front/same.txt')).body, 'replacement')
  assert.equal((await call(route, '/api/files/download?path=/mutable/trip/rear/same.txt')).body, 'rear')
  const listing = JSON.parse((await call(route, '/api/files/ls?path=/mutable/trip')).body)
  assert.deepEqual(listing.entries.map(e => [e.name, e.is_dir]), [['front', true], ['rear', true]])
})

test('preview rejects traversal and duplicate-file multipart without publishing', async () => {
  const route = createPreviewFiles()
  assert.equal((await call(route, '/api/files/upload', 'POST', upload('../escape.txt', 'unsafe'))).status, 400)
  const form = upload('partial.txt', 'one')
  form.append('file', new Blob(['two']), 'two.txt')
  assert.equal((await call(route, '/api/files/upload', 'POST', form)).status, 400)
  assert.equal((await call(route, '/api/files/download?path=/mutable/partial.txt')).status, 404)
})
