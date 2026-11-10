<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { MiuixArrowPreference, MiuixButton, MiuixDialog, MiuixIcon, MiuixSwitchPreference } from 'miuix-vue'
import { Photos } from 'miuix-vue/icons'
import { validateWallpaper, wallpaperStorage, type WallpaperRecord } from '../wallpaper'

const open = defineModel<boolean>({ default: false })
const emit = defineEmits<{ notify: [message: string, error: boolean] }>()
const input = ref<HTMLInputElement | null>(null)
const busy = ref(true)
const enabled = ref(false)
const hasImage = ref(false)
let saved: WallpaperRecord = { enabled: false, blob: null }
let objectUrl: string | null = null
let disposed = false

function apply(record: WallpaperRecord): void {
  if (disposed) return
  const previous = objectUrl
  objectUrl = record.enabled && record.blob ? URL.createObjectURL(record.blob) : null
  document.documentElement.style.setProperty('--r1-wallpaper', objectUrl ? `url("${objectUrl}")` : 'none')
  document.documentElement.dataset.r1Wallpaper = String(!!objectUrl)
  if (previous) URL.revokeObjectURL(previous)
  saved = record
  enabled.value = record.enabled
  hasImage.value = !!record.blob
}

function report(error: unknown): void {
  if (!disposed) emit('notify', error instanceof Error ? error.message : String(error), true)
}

async function upload(event: Event): Promise<void> {
  const element = event.target as HTMLInputElement
  const file = element.files?.[0]
  element.value = ''
  if (!file || busy.value) return
  busy.value = true
  try {
    await validateWallpaper(file)
    apply(await wallpaperStorage({ enabled: true, blob: file }))
  } catch (error) { report(error) }
  finally { busy.value = false }
}

async function toggle(value: boolean): Promise<void> {
  if (busy.value) return
  busy.value = true
  try {
    if (value && saved.blob) await validateWallpaper(saved.blob)
    apply(await wallpaperStorage({ ...saved, enabled: value && !!saved.blob }))
  } catch (error) { report(error) }
  finally { busy.value = false }
}

async function removeWallpaper(): Promise<void> {
  if (busy.value || !hasImage.value) return
  busy.value = true
  try {
    apply(await wallpaperStorage({ enabled: false, blob: null }))
  } catch (error) { report(error) }
  finally { busy.value = false }
}

onMounted(async () => {
  try {
    const record = await wallpaperStorage()
    if (record.enabled && record.blob) await validateWallpaper(record.blob)
    apply(record)
  } catch (error) { report(error) }
  finally { busy.value = false }
})
onBeforeUnmount(() => {
  disposed = true
  if (objectUrl) URL.revokeObjectURL(objectUrl)
  document.documentElement.style.removeProperty('--r1-wallpaper')
  delete document.documentElement.dataset.r1Wallpaper
})
</script>

<template>
  <input ref="input" type="file" accept="image/jpeg,image/png,image/webp" hidden @change="upload">
  <MiuixDialog v-model="open" title="背景壁纸" :close-on-click-modal="!busy">
    <MiuixSwitchPreference :model-value="enabled" title="启用壁纸" :disabled="busy || !hasImage"
      @update:model-value="toggle" />
    <MiuixArrowPreference title="上传图片" summary="JPEG / PNG / WebP · 8 MiB · 24 MP"
      :disabled="busy" @click="input?.click()">
      <template #start><MiuixIcon :icon="Photos" :size="23" /></template>
    </MiuixArrowPreference>
    <div class="confirm-actions">
      <MiuixButton :disabled="busy || !hasImage" @click="removeWallpaper">移除壁纸</MiuixButton>
      <MiuixButton :disabled="busy" @click="open = false">关闭</MiuixButton>
    </div>
  </MiuixDialog>
</template>
