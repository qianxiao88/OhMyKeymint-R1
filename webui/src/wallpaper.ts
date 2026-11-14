export const MAX_WALLPAPER_BYTES = 8 * 1024 * 1024
export const WALLPAPER_TYPES = ['image/jpeg', 'image/png', 'image/webp'] as const
export interface WallpaperRecord { enabled: boolean, blob: Blob | null }

export function wallpaperDimensions(bytes: Uint8Array, type: string): [number, number] {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
  const tag = (offset: number, value: string) => value.split('').every((char, i) => bytes[offset + i] === char.charCodeAt(0))
  if (type === 'image/png' && bytes.length >= 24
      && [137, 80, 78, 71, 13, 10, 26, 10].every((byte, i) => bytes[i] === byte)
      && view.getUint32(8) === 13 && tag(12, 'IHDR')) {
    return [view.getUint32(16), view.getUint32(20)]
  }
  if (type === 'image/jpeg' && bytes[0] === 0xff && bytes[1] === 0xd8) {
    let offset = 2
    while (offset + 4 <= bytes.length) {
      if (bytes[offset++] !== 0xff) break
      while (bytes[offset] === 0xff) offset++
      const marker = bytes[offset++]
      if (marker === 0xda || marker === 0xd9) break
      if (marker === 0x01 || marker >= 0xd0 && marker <= 0xd7) continue
      if (offset + 2 > bytes.length) break
      const length = view.getUint16(offset)
      if (length < 2 || offset + length > bytes.length) break
      if ([0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf].includes(marker)
          && length >= 8) return [view.getUint16(offset + 5), view.getUint16(offset + 3)]
      offset += length
    }
  }
  if (type === 'image/webp' && bytes.length >= 20 && tag(0, 'RIFF') && tag(8, 'WEBP')) {
    for (let offset = 12; offset + 8 <= bytes.length;) {
      const length = view.getUint32(offset + 4, true)
      const start = offset + 8
      if (start + length > bytes.length) break
      if (tag(offset, 'VP8X') && length >= 10) {
        const uint24 = (at: number) => bytes[at] + (bytes[at + 1] << 8) + (bytes[at + 2] << 16)
        return [uint24(start + 4) + 1, uint24(start + 7) + 1]
      }
      if (tag(offset, 'VP8 ') && length >= 10 && bytes[start + 3] === 0x9d
          && bytes[start + 4] === 1 && bytes[start + 5] === 0x2a) {
        return [view.getUint16(start + 6, true) & 0x3fff, view.getUint16(start + 8, true) & 0x3fff]
      }
      if (tag(offset, 'VP8L') && length >= 5 && bytes[start] === 0x2f) {
        const bits = view.getUint32(start + 1, true)
        return [(bits & 0x3fff) + 1, ((bits >>> 14) & 0x3fff) + 1]
      }
      offset = start + length + (length % 2)
    }
  }
  throw new Error('Invalid or unsupported image header')
}

function validateDimensions(width: number, height: number): void {
  if (!width || !height || width > 8192 || height > 8192 || width * height > 24_000_000) {
    throw new Error('Wallpaper must be at most 8192 px per side and 24 megapixels')
  }
}

export async function validateWallpaper(blob: Blob): Promise<void> {
  if (!(blob instanceof Blob) || !WALLPAPER_TYPES.includes(blob.type as typeof WALLPAPER_TYPES[number])
      || blob.size === 0 || blob.size > MAX_WALLPAPER_BYTES) {
    throw new Error('Choose a JPEG, PNG or WebP image up to 8 MiB')
  }
  const bytes = new Uint8Array(await blob.arrayBuffer())
  // Check encoded dimensions before allocating a decoded full-resolution bitmap.
  validateDimensions(...wallpaperDimensions(bytes, blob.type))
  const bitmap = await createImageBitmap(blob)
  try {
    validateDimensions(bitmap.width, bitmap.height)
  } finally { bitmap.close() }
}

export function wallpaperStorage(record?: WallpaperRecord): Promise<WallpaperRecord> {
  return new Promise((resolve, reject) => {
    if (typeof indexedDB === 'undefined') {
      reject(new Error('This WebView does not provide wallpaper storage'))
      return
    }
    let blocked = false
    const request = indexedDB.open('omk-r1-wallpaper', 1)
    request.onupgradeneeded = () => { request.result.createObjectStore('wallpaper') }
    request.onerror = () => reject(new Error('Unable to open wallpaper storage'))
    request.onblocked = () => {
      blocked = true
      reject(new Error('Wallpaper storage is busy; close other WebUI windows'))
    }
    request.onsuccess = () => {
      const db = request.result
      if (blocked) { db.close(); return }
      db.onversionchange = () => db.close()
      let value: WallpaperRecord = { enabled: false, blob: null }
      let transaction: IDBTransaction
      try {
        transaction = db.transaction('wallpaper', record ? 'readwrite' : 'readonly')
        const store = transaction.objectStore('wallpaper')
        if (record) { store.put(record, 'current'); value = record }
        else {
          const read = store.get('current')
          read.onsuccess = () => {
            const saved: unknown = read.result
            if (saved === undefined) return
            if (typeof saved !== 'object' || saved === null
                || typeof (saved as WallpaperRecord).enabled !== 'boolean'
                || (!((saved as WallpaperRecord).blob instanceof Blob)
                  && (saved as WallpaperRecord).blob !== null)
                || ((saved as WallpaperRecord).enabled && !(saved as WallpaperRecord).blob)) {
              transaction.abort()
              return
            }
            value = saved as WallpaperRecord
          }
        }
      } catch {
        db.close()
        reject(new Error('Unable to access wallpaper storage'))
        return
      }
      transaction.oncomplete = () => { db.close(); resolve(value) }
      transaction.onabort = transaction.onerror = () => {
        db.close()
        reject(new Error('Unable to save or read wallpaper; check WebView storage space'))
      }
    }
  })
}
