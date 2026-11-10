import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'
import { test } from 'node:test'
import vm from 'node:vm'

const source = stripTypeScriptTypes(readFileSync(new URL('../src/wallpaper.ts', import.meta.url), 'utf8').replaceAll('export ', ''))

test('WebView content policy permits persisted Blob wallpaper images', () => {
  const config = JSON.parse(readFileSync(new URL('../public/config.json', import.meta.url), 'utf8'))
  assert.match(JSON.stringify(config), /img-src[^;]*blob:/)
})

function harness(globals = {}) {
  return vm.runInNewContext(`${source}\n({ validateWallpaper, wallpaperStorage, wallpaperDimensions })`, {
    Blob, Uint8Array, DataView, ...globals,
  })
}
function png(width = 1920, height = 1080) {
  const bytes = Buffer.alloc(24)
  bytes.set([137, 80, 78, 71, 13, 10, 26, 10])
  bytes.writeUInt32BE(13, 8)
  bytes.write('IHDR', 12)
  bytes.writeUInt32BE(width, 16)
  bytes.writeUInt32BE(height, 20)
  return new Blob([bytes], { type: 'image/png' })
}

test('wallpaper rejects SVG, MIME spoofing, oversize and excessive original dimensions before decoding', async () => {
  let decodes = 0
  const { validateWallpaper } = harness({ createImageBitmap: async () => { decodes++; throw new Error('unexpected decode') } })
  for (const blob of [new Blob(['<svg/>'], { type: 'image/svg+xml' }), new Blob(['<svg/>'], { type: 'image/png' }),
    new Blob([new Uint8Array(8 * 1024 * 1024 + 1)], { type: 'image/jpeg' }), png(10000, 100), png(6000, 6000), png(0, 100)]) {
    await assert.rejects(validateWallpaper(blob))
  }
  assert.equal(decodes, 0)
})

test('wallpaper validates decoding and closes full-size bitmaps on success and dimension errors', async () => {
  let closed = 0
  let width = 1920
  const { validateWallpaper } = harness({ createImageBitmap: async () => ({ width, height: 1080, close() { closed++ } }) })
  await validateWallpaper(png())
  width = 9000
  await assert.rejects(validateWallpaper(png()), /8192/)
  assert.equal(closed, 2)
  await assert.rejects(harness({ createImageBitmap: async () => { throw new Error('Corrupt image') } }).validateWallpaper(png()), /Corrupt image/)
})

test('original dimensions are read from JPEG and every supported WebP header', () => {
  const { wallpaperDimensions } = harness()
  const jpeg = Buffer.from([0xff, 0xd8, 0xff, 0xc0, 0, 8, 8, 0x04, 0x38, 0x07, 0x80, 0])
  assert.deepEqual([...wallpaperDimensions(jpeg, 'image/jpeg')], [1920, 1080])
  for (const kind of ['VP8X', 'VP8 ', 'VP8L']) {
    const bytes = Buffer.alloc(30)
    bytes.write('RIFF', 0)
    bytes.writeUInt32LE(22, 4)
    bytes.write('WEBP', 8)
    bytes.write(kind, 12)
    bytes.writeUInt32LE(10, 16)
    if (kind === 'VP8X') {
      bytes.writeUIntLE(1919, 24, 3)
      bytes.writeUIntLE(1079, 27, 3)
    } else if (kind === 'VP8 ') {
      bytes.set([0x9d, 1, 0x2a], 23)
      bytes.writeUInt16LE(1920, 26)
      bytes.writeUInt16LE(1080, 28)
    } else {
      bytes[20] = 0x2f
      bytes.writeUInt32LE(1919 | (1079 << 14), 21)
    }
    assert.deepEqual([...wallpaperDimensions(bytes, 'image/webp')], [1920, 1080])
    assert.throws(() => wallpaperDimensions(bytes.subarray(0, 23), 'image/webp'), /header/)
  }
})

function database({ fail = false } = {}) {
  let saved
  let closes = 0
  let mode
  const indexedDB = { open() {
    const request = {}
    queueMicrotask(() => {
      request.result = {
        close() { closes++ },
        transaction(_store, requestedMode) {
          mode = requestedMode
          const tx = { objectStore: () => ({
            put(value) { if (!fail) saved = structuredClone(value) },
            get() {
              const read = {}
              queueMicrotask(() => { read.result = saved; read.onsuccess() })
              return read
            },
          }), abort() { queueMicrotask(() => tx.onabort()) } }
          setImmediate(() => fail ? tx.onerror() : tx.oncomplete())
          return tx
        },
      }
      request.onsuccess()
    })
    return request
  } }
  return { indexedDB, get closes() { return closes }, get mode() { return mode } }
}

test('IndexedDB persists Blob and enabled flag across reopen, replacement and disable', async () => {
  const db = database()
  const { wallpaperStorage } = harness({ indexedDB: db.indexedDB })
  assert.equal((await wallpaperStorage()).enabled, false)
  const blob = png()
  await wallpaperStorage({ enabled: true, blob })
  assert.equal(db.mode, 'readwrite')
  let restored = await wallpaperStorage()
  assert.equal(restored.enabled, true)
  assert.ok(restored.blob instanceof Blob)
  assert.equal(restored.blob.size, blob.size)
  await wallpaperStorage({ ...restored, enabled: false })
  restored = await wallpaperStorage()
  assert.equal(restored.enabled, false)
  assert.ok(restored.blob instanceof Blob)
  await wallpaperStorage({ enabled: true, blob: png(1280, 720) })
  assert.equal((await wallpaperStorage()).enabled, true)
  assert.equal(db.closes, 7)
})

test('wallpaper storage failures reject and close database handles', async () => {
  const db = database({ fail: true })
  await assert.rejects(harness({ indexedDB: db.indexedDB }).wallpaperStorage({ enabled: true, blob: png() }), /storage space/)
  assert.equal(db.closes, 1)
  await assert.rejects(harness().wallpaperStorage(), /does not provide wallpaper storage/)
})

test('wallpaper replacement and disable update the live surface and release previous Blob URLs', () => {
  const component = readFileSync(new URL('../src/vue/WallpaperDialog.vue', import.meta.url), 'utf8')
  const start = component.indexOf('function apply(')
  const apply = stripTypeScriptTypes(component.slice(start, component.indexOf('\n}', start) + 2))
  let serial = 0
  const revoked = []
  const properties = new Map()
  const document = { documentElement: {
    style: { setProperty: (key, value) => properties.set(key, value) }, dataset: {},
  } }
  const surface = vm.runInNewContext(`
    let disposed = false, objectUrl = null, saved;
    const enabled = { value: false }, hasImage = { value: false };
    ${apply}
    ({ apply, enabled, hasImage })`, {
    document,
    URL: { createObjectURL: () => `blob:wallpaper-${++serial}`, revokeObjectURL: url => revoked.push(url) },
  })
  surface.apply({ enabled: true, blob: png() })
  assert.equal(properties.get('--r1-wallpaper'), 'url("blob:wallpaper-1")')
  surface.apply({ enabled: true, blob: png(1280, 720) })
  assert.equal(properties.get('--r1-wallpaper'), 'url("blob:wallpaper-2")')
  surface.apply({ enabled: false, blob: png() })
  assert.equal(properties.get('--r1-wallpaper'), 'none')
  assert.equal(document.documentElement.dataset.r1Wallpaper, 'false')
  assert.equal(surface.enabled.value, false)
  assert.equal(surface.hasImage.value, true)
  assert.deepEqual(revoked, ['blob:wallpaper-1', 'blob:wallpaper-2'])
})
