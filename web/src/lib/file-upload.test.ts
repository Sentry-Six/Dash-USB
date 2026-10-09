import assert from 'node:assert/strict'
import test from 'node:test'
import { uploadRelativePath } from './file-upload.ts'

test('relative folder paths preserve spaces and nested duplicate basenames', () => {
  assert.equal(uploadRelativePath({ name: 'clip.mp4', webkitRelativePath: 'Road trip/front/clip.mp4' }), 'Road trip/front/clip.mp4')
  assert.equal(uploadRelativePath({ name: 'clip.mp4', webkitRelativePath: 'Road trip/rear/clip.mp4' }), 'Road trip/rear/clip.mp4')
  assert.equal(uploadRelativePath({ name: 'single.mp4', webkitRelativePath: '' }), 'single.mp4')
})
test('invalid picked paths are rejected before requests are created', () => {
  for (const path of ['', '/absolute.mp4', '../escape.mp4', 'folder/../escape.mp4', 'folder//clip.mp4', './clip.mp4', 'folder\\clip.mp4', 'bad\0.mp4', 'bad\n.mp4']) {
    assert.throws(() => uploadRelativePath({ name: path, webkitRelativePath: '' }), /Invalid upload path/)
  }
})
