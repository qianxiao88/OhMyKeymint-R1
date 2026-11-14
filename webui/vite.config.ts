import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

type Variant = 'full' | 'no-soter' | 'lite'

const VARIANT = (process.env.VITE_R1_VARIANT ?? 'full') as Variant

// Components that a reduced variant drops outright. A runtime `v-if` is not
// enough: the import would still pull the whole module (and its styles) into the
// bundle. Swapping the module at resolve time removes it for real.
const LITE_REMOVED = ['SoterIdentityDialog.vue', 'PifFingerprintDialog.vue', 'SettingsView.vue', 'ToolsView.vue']
const NO_SOTER_REMOVED = ['SoterIdentityDialog.vue']

function removedFor(variant: Variant): string[] {
  if (variant === 'lite') return LITE_REMOVED
  if (variant === 'no-soter') return NO_SOTER_REMOVED
  return []
}

// The stub keeps the two instance methods App.vue reaches for through an
// optional chain, so the reduced bundles stay type- and runtime-safe.
const STUB_SOURCE = `
import { defineComponent } from 'vue'

export default defineComponent({
  name: 'OmkRemovedComponent',
  methods: {
    dismissOverlay() { return false },
    requestClose() {},
  },
  render() { return null },
})
`

function variantStubs(variant: Variant) {
  const removed = removedFor(variant)
  if (removed.length === 0) return null

  const VIRTUAL_PREFIX = '\0omk-removed:'

  return {
    name: 'omk-variant-stubs',
    enforce: 'pre' as const,
    resolveId(source: string, importer: string | undefined) {
      if (!importer || source.startsWith('\0')) return null
      const target = removed.find(name => source === `./${name}` || source.endsWith(`/${name}`))
      return target ? `${VIRTUAL_PREFIX}${target}` : null
    },
    load(id: string) {
      return id.startsWith(VIRTUAL_PREFIX) ? STUB_SOURCE : null
    },
  }
}

const stubPlugin = variantStubs(VARIANT)

export default defineConfig({
  base: '',
  plugins: [...(stubPlugin ? [stubPlugin] : []), vue()],
  build: {
    target: 'chrome94',
    cssTarget: 'chrome94',
    outDir: process.env.VITE_R1_OUT_DIR ?? '../template/webroot',
    emptyOutDir: true,
    cssCodeSplit: false,
    chunkSizeWarningLimit: 2048,
  },
})
