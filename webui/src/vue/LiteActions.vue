<script setup lang="ts">
import { computed, ref } from 'vue'
import {
  MiuixArrowPreference,
  MiuixButton,
  MiuixCard,
  MiuixIcon,
  MiuixIconButton,
  MiuixProgressIndicator,
  MiuixSmallTitle,
  showSnackbar,
} from 'miuix-vue'
import { AddCircle, Close, Download, Photos, Reset, Update } from 'miuix-vue/icons'
import MiuixBottomSheet from './StableBottomSheet.vue'
import type { Cli } from '../cli'
import { i18n } from '../i18n'
import { isSecurityPatchDate } from '../security_patch'

// The lite variant renders a single page, so the actions that normally live on
// the Tools and Settings pages are collected here instead.
type BusyPatch = 'sync' | 'restore' | null

const props = defineProps<{
  cli: Cli
  securityPatch: string | null
  securityPatchBusy: BusyPatch
  maintenanceBusy: 'gms' | 'rkp' | null
  cloudBusy?: boolean
}>()

const emit = defineEmits<{
  'open-app-targets': []
  'cloud-update': []
  'clear-gms-cache': []
  'sync-security-patch': []
  'restore-security-patch': []
  'apply-security-patch': [date: string]
  'wallpaper': []
  'overlay-open': []
  'overlay-close': []
}>()

function tr(key: string, fallback: string): string {
  const value = i18n.t(key)
  return value === key ? fallback : value
}

const patchSheetOpen = ref(false)
const customPatchOpen = ref(false)
const customPatchDate = ref('')
const logExportBusy = ref(false)
// The lite build has no Settings page, so the polling period is edited here.
const cloudInterval = ref<number | null>(null)
const cloudIntervalBusy = ref(false)

async function loadCloudInterval(): Promise<void> {
  try { cloudInterval.value = await props.cli.getCloudInterval() } catch { cloudInterval.value = null }
}

async function promptCloudInterval(): Promise<void> {
  if (cloudIntervalBusy.value) return
  const current = cloudInterval.value === null ? '' : String(cloudInterval.value)
  const answer = window.prompt(
    tr('settings_cloud_interval_prompt', 'Minutes between cloud updates (1-1440)'),
    current,
  )
  if (answer === null) return
  const minutes = Number(answer.trim())
  cloudIntervalBusy.value = true
  try {
    // The helper validates the range as well; this only avoids a pointless call
    // when the entry is obviously not a whole number.
    if (!Number.isInteger(minutes)) throw new Error(tr('settings_cloud_interval_invalid', 'Enter 1 to 1440 minutes'))
    cloudInterval.value = await props.cli.setCloudInterval(minutes)
    void showSnackbar({
      message: tr('settings_cloud_interval_saved', 'Cloud update every %s minutes').replace('%s', String(cloudInterval.value)),
      duration: 'long',
      withDismissAction: true,
    })
  } catch (error) {
    void showSnackbar({
      message: error instanceof Error ? error.message : String(error),
      duration: 6000,
      withDismissAction: true,
    })
  } finally { cloudIntervalBusy.value = false }
}

// Read once: the helper owns the stored value.
void loadCloudInterval()

const customPatchValid = computed(() => isSecurityPatchDate(customPatchDate.value.trim()))
const customPatchInvalid = computed(
  () => customPatchDate.value.trim() !== '' && !customPatchValid.value,
)

function openPatchSheet(): void {
  patchSheetOpen.value = true
  customPatchOpen.value = false
}

function closePatchSheet(): void {
  patchSheetOpen.value = false
  customPatchOpen.value = false
}

function applyCustomPatch(): void {
  if (!customPatchValid.value) return
  const date = customPatchDate.value.trim()
  closePatchSheet()
  emit('apply-security-patch', date)
}

function runPatchAction(event: 'sync-security-patch' | 'restore-security-patch'): void {
  closePatchSheet()
  // The typed emit rejects a union argument, so dispatch both variants explicitly.
  if (event === 'sync-security-patch') emit('sync-security-patch')
  else emit('restore-security-patch')
}

async function exportLogs(): Promise<void> {
  if (logExportBusy.value) return
  logExportBusy.value = true
  try {
    const path = await props.cli.exportLogs()
    void showSnackbar({
      message: i18n.t('prompt_logs_exported', path),
      duration: 'long',
      withDismissAction: true,
    })
  } catch (error) {
    void showSnackbar({
      message: error instanceof Error ? error.message : String(error),
      duration: 6000,
      withDismissAction: true,
    })
  } finally {
    logExportBusy.value = false
  }
}

const gmsBusy = computed(() => props.maintenanceBusy === 'gms')
const patchBusy = computed(() => props.securityPatchBusy !== null)
</script>

<template>
  <section class="preference-section lite-actions" aria-labelledby="lite-actions-title">
    <MiuixSmallTitle
      id="lite-actions-title"
      :text="tr('lite_actions_title', '快捷操作')"
    />
    <MiuixCard class="preference-card" press-feedback="none">
      <MiuixArrowPreference
        :title="tr('app_targets_title', 'Add package names')"
        :summary="tr('tools_app_targets_desc', 'Choose which installed apps are routed through Oh My Keymint.')"
        @click="emit('open-app-targets')"
      >
        <template #start>
          <span class="lite-actions__icon"><MiuixIcon :icon="AddCircle" :size="23" /></span>
        </template>
      </MiuixArrowPreference>

      <MiuixArrowPreference
        :title="tr('tools_cloud_keybox', '从云端获取keybox')"
        :summary="tr('tools_cloud_keybox_desc', '从浅笑云端更新keybox')"
        :disabled="!!cloudBusy"
        :hold-down="!!cloudBusy"
        @click="emit('cloud-update')"
      >
        <template #start>
          <span class="lite-actions__icon">
            <MiuixProgressIndicator v-if="cloudBusy" type="circular" :size="22" :stroke-width="2.5" />
            <MiuixIcon v-else :icon="Update" :size="23" />
          </span>
        </template>
      </MiuixArrowPreference>

      <MiuixArrowPreference
        :title="tr('tools_security_patch', 'Security patch settings')"
        :summary="securityPatch ?? tr('tools_security_patch_desc', 'Sync or restore the reported security patch date.')"
        :disabled="patchBusy"
        :hold-down="patchBusy"
        @click="openPatchSheet"
      >
        <template #start>
          <span class="lite-actions__icon">
            <MiuixProgressIndicator v-if="patchBusy" type="circular" :size="22" :stroke-width="2.5" />
            <MiuixIcon v-else :icon="Update" :size="23" />
          </span>
        </template>
      </MiuixArrowPreference>

      <MiuixArrowPreference
        :title="tr('tools_clear_gms_cache', 'Clear GMS cache')"
        :summary="tr('tools_clear_gms_cache_desc', 'Clear Play services and Play Store caches, keep app data, then stop the specified processes.')"
        :disabled="gmsBusy"
        :hold-down="gmsBusy"
        @click="emit('clear-gms-cache')"
      >
        <template #start>
          <span class="lite-actions__icon">
            <MiuixProgressIndicator v-if="gmsBusy" type="circular" :size="22" :stroke-width="2.5" />
            <MiuixIcon v-else :icon="Reset" :size="23" />
          </span>
        </template>
      </MiuixArrowPreference>

      <MiuixArrowPreference
        :title="tr('tools_wallpaper', '背景壁纸')"
        summary=""
        @click="emit('wallpaper')"
      >
        <template #start>
          <span class="lite-actions__icon"><MiuixIcon :icon="Photos" :size="23" /></span>
        </template>
      </MiuixArrowPreference>

      <MiuixArrowPreference
        :title="tr('settings_cloud_interval', 'Cloud update interval')"
        :summary="cloudInterval === null
          ? tr('settings_cloud_interval_loading', 'Reading the current period')
          : tr('settings_cloud_interval_value', 'Every %s minutes').replace('%s', String(cloudInterval))"
        :disabled="cloudIntervalBusy"
        :hold-down="cloudIntervalBusy"
        @click="promptCloudInterval"
      >
        <template #start>
          <span class="lite-actions__icon"><MiuixIcon :icon="Update" :size="23" /></span>
        </template>
      </MiuixArrowPreference>

      <MiuixArrowPreference
        :title="tr('settings_export_logs', 'Export logs')"
        :summary="tr('settings_export_logs_desc', 'Collect OMK logs into a support archive on shared storage')"
        :disabled="logExportBusy"
        :hold-down="logExportBusy"
        @click="exportLogs"
      >
        <template #start>
          <span class="lite-actions__icon">
            <MiuixProgressIndicator v-if="logExportBusy" type="circular" :size="22" :stroke-width="2.5" />
            <MiuixIcon v-else :icon="Download" :size="23" />
          </span>
        </template>
      </MiuixArrowPreference>
    </MiuixCard>

    <MiuixBottomSheet
      v-model="patchSheetOpen"
      :title="tr('tools_security_patch', 'Security patch settings')"
    >
      <template #start-action>
        <MiuixIconButton
          :aria-label="tr('functional_button_cancel', 'Cancel')"
          @click="closePatchSheet"
        >
          <MiuixIcon :icon="Close" :size="22" />
        </MiuixIconButton>
      </template>
      <div class="lite-actions__sheet">
        <MiuixCard class="preference-card" press-feedback="none">
          <MiuixArrowPreference
            :title="tr('tools_sync_patch', 'Sync security patch')"
            :summary="tr('tools_sync_patch_desc', 'Fetch the latest published Android security patch date.')"
            :disabled="patchBusy"
            :hold-down="securityPatchBusy === 'sync'"
            @click="runPatchAction('sync-security-patch')"
          >
            <template #start>
              <span class="lite-actions__icon">
                <MiuixProgressIndicator
                  v-if="securityPatchBusy === 'sync'"
                  type="circular"
                  :size="22"
                  :stroke-width="2.5"
                />
                <MiuixIcon v-else :icon="Update" :size="23" />
              </span>
            </template>
          </MiuixArrowPreference>
          <MiuixArrowPreference
            :title="tr('tools_restore_patch', 'Restore security patch')"
            :summary="tr('tools_restore_patch_desc', 'Restore the security patch date originally reported by the device.')"
            :disabled="patchBusy"
            :hold-down="securityPatchBusy === 'restore'"
            @click="runPatchAction('restore-security-patch')"
          >
            <template #start>
              <span class="lite-actions__icon">
                <MiuixProgressIndicator
                  v-if="securityPatchBusy === 'restore'"
                  type="circular"
                  :size="22"
                  :stroke-width="2.5"
                />
                <MiuixIcon v-else :icon="Reset" :size="23" />
              </span>
            </template>
          </MiuixArrowPreference>
          <MiuixArrowPreference
            :title="tr('tools_custom_patch', 'Custom security patch')"
            :summary="tr('tools_custom_patch_desc', 'Enter a date such as 2026-05-01. A reboot is required.')"
            @click="customPatchOpen = !customPatchOpen"
          >
            <template #start>
              <span class="lite-actions__icon"><MiuixIcon :icon="Photos" :size="23" /></span>
            </template>
          </MiuixArrowPreference>
        </MiuixCard>
        <div v-if="customPatchOpen" class="custom-patch">
          <label class="custom-patch__field">
            <span>{{ tr('tools_custom_patch_label', 'Patch date') }}</span>
            <input
              v-model="customPatchDate"
              type="text"
              inputmode="numeric"
              maxlength="10"
              spellcheck="false"
              autocomplete="off"
              :placeholder="tr('tools_custom_patch_placeholder', '2026-05-01')"
            >
          </label>
          <p v-if="customPatchInvalid" class="custom-patch__error" role="alert">
            {{ tr('tools_custom_patch_invalid', 'Enter a valid date (YYYY-MM-DD).') }}
          </p>
          <MiuixButton type="primary" :disabled="!customPatchValid || patchBusy" @click="applyCustomPatch">
            {{ tr('tools_custom_patch_apply', 'Apply') }}
          </MiuixButton>
        </div>
      </div>
    </MiuixBottomSheet>
  </section>
</template>

<style scoped>
.lite-actions__icon { display: inline-flex; align-items: center; justify-content: center; }
.lite-actions__sheet { display: grid; gap: 4px; }
.custom-patch { display: grid; gap: 10px; padding: 12px 16px 4px; }
.custom-patch__field { display: grid; gap: 6px; color: var(--m-color-on-surface-variant-summary); font-size: 13px; }
.custom-patch__field input {
  box-sizing: border-box;
  width: 100%;
  min-height: 48px;
  padding: 12px 14px;
  border: 1px solid var(--m-color-outline);
  border-radius: 14px;
  background: var(--m-color-surface-container);
  color: var(--m-color-on-surface);
  font: inherit;
  font-size: 16px;
  font-variant-numeric: tabular-nums;
  letter-spacing: .02em;
}
.custom-patch__field input:focus-visible { outline: 2px solid var(--m-color-primary); outline-offset: -2px; }
.custom-patch__error { margin: 0; color: var(--m-color-error); font-size: 13px; }
</style>
