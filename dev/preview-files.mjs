// Memory-only file API for the actual UI preview. No host paths are accessed.
import { posix } from 'node:path'

const MAX_UPLOAD = 16 * 1024 * 1024
const MAX_STORED = 64 * 1024 * 1024
const allowed = path => ['/mutable', '/mnt/cam'].some(base => path === base || path.startsWith(`${base}/`))
function destination(value) {
  if (typeof value !== 'string' || !value.startsWith('/') || value.includes('\\') || value.includes('\0') || value.split('/').includes('..')) throw new Error('Invalid destination')
  const path = posix.normalize(value)
  if (!allowed(path)) throw new Error('Access denied')
  return path
}
function relativePath(value) {
  if (typeof value !== 'string' || !value || value.includes('\\') || value.includes('\0') || value.split('/').some(p => !p || p === '.' || p === '..')) throw new Error('Invalid upload path')
  return value
}
const json = (res, status, value) => {
  res.writeHead(status, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' })
  res.end(JSON.stringify(value))
}
async function requestBody(req, limit) {
  const chunks = []
  let size = 0
  for await (const chunk of req) {
    size += chunk.length
    if (size > limit) throw new Error('Local preview uploads are limited to 16 MiB per request.')
    chunks.push(chunk)
  }
  return Buffer.concat(chunks)
}

export function createPreviewFiles() {
  const entries = new Map()
  const add = (path, data = null) => entries.set(path, { data, modified: new Date().toISOString() })
  for (const path of ['/mutable', '/mutable/Recordings', '/mnt/cam']) add(path)
  add('/mutable/upload-demo.txt', Buffer.from('Simulated file: download this and upload it again to try replacement confirmation.\n'))
  const exists = path => entries.has(path)
  const folder = path => entries.get(path)?.data === null
  function ensureDirectories(path) {
    if (exists(path)) {
      if (!folder(path)) throw new Error('A file occupies the requested folder path.')
      return
    }
    const parent = posix.dirname(path)
    if (allowed(parent)) ensureDirectories(parent)
    add(path)
  }
  return async function handle(req, res, url) {
    if (!url.pathname.startsWith('/api/files')) return false
    const method = req.method || 'GET'
    try {
      if (url.pathname === '/api/files/ls' && method === 'GET') {
        const path = destination(url.searchParams.get('path') || '/mutable')
        if (!folder(path)) { json(res, 404, { error: 'Folder unavailable in this preview.' }); return true }
        const search = (url.searchParams.get('search') || '').toLowerCase()
        const listed = [...entries].filter(([p]) => p !== path && posix.dirname(p) === path && posix.basename(p).toLowerCase().includes(search)).map(([p, item]) => ({
          name: posix.basename(p), path: p, is_dir: item.data === null,
          size: item.data?.length || 0, mod_time: item.modified,
        })).sort((a,b) => Number(b.is_dir) - Number(a.is_dir) || a.name.localeCompare(b.name))
        json(res, 200, { path, entries: listed }); return true
      }
      if (url.pathname === '/api/files/upload' && method === 'POST') {
        const data = await requestBody(req, MAX_UPLOAD)
        const form = await new Request('http://localhost/api/files/upload', { method: 'POST', headers: { 'Content-Type': req.headers['content-type'] || '' }, body: data }).formData()
        if (form.getAll('file').length !== 1 || form.getAll('path').length !== 1 || form.getAll('relative_path').length > 1 || form.getAll('overwrite').length > 1) throw new Error('Expected one file and one destination per upload.')
        const file = form.get('file')
        if (!file || typeof file.arrayBuffer !== 'function') throw new Error('Missing file in upload')
        const base = destination(form.get('path'))
        const relative = relativePath(form.get('relative_path') ?? file.name)
        const path = posix.join(base, relative)
        if (form.has('overwrite') && !['true', 'false'].includes(form.get('overwrite'))) throw new Error('overwrite must be true or false')
        const overwrite = form.get('overwrite') === 'true'
        const bytes = Buffer.from(await file.arrayBuffer())
        // Check again only after the complete body is available, matching the
        // real API's publication-time no-clobber behavior.
        if (exists(path) && !overwrite) { json(res, 409, { error: 'A file with this name already exists. Choose Replace to overwrite it.' }); return true }
        if (folder(path)) throw new Error('Cannot replace a folder with an uploaded file.')
        const total = [...entries.values()].reduce((n,e) => n + (e.data?.length || 0), 0) - (entries.get(path)?.data?.length || 0) + bytes.length
        if (total > MAX_STORED) { json(res, 507, { error: 'The preview file store is full. Delete preview files or restart the preview.' }); return true }
        ensureDirectories(posix.dirname(path))
        add(path, bytes)
        json(res, 200, { name: file.name, path, size: String(bytes.length) }); return true
      }
      if (url.pathname === '/api/files/mkdir' && method === 'POST') {
        const data = JSON.parse((await requestBody(req, 65536)).toString())
        ensureDirectories(destination(data.path))
        json(res, 200, { success: true }); return true
      }
      if (url.pathname === '/api/files' && method === 'DELETE') {
        const path = destination(url.searchParams.get('path'))
        if (['/mutable', '/mnt/cam'].includes(path)) throw new Error('Cannot delete root directory')
        for (const p of entries.keys()) if (p === path || p.startsWith(`${path}/`)) entries.delete(p)
        json(res, 200, { success: true }); return true
      }
      if (url.pathname === '/api/files/download' && method === 'GET') {
        const path = destination(url.searchParams.get('path')), data = entries.get(path)?.data
        if (!data) { json(res, 404, { error: 'File unavailable in this preview.' }); return true }
        const name = posix.basename(path).replace(/[\r\n"\\]/g, '_')
        res.writeHead(200, { 'Content-Type': 'application/octet-stream', 'Content-Disposition': `attachment; filename="${name}"`, 'Cache-Control': 'no-store' })
        res.end(data); return true
      }
      return false
    } catch (error) {
      json(res, 400, { error: error.message }); return true
    }
  }
}
