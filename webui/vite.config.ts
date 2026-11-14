import { defineConfig, loadEnv, type Plugin } from 'vite'
import vue from '@vitejs/plugin-vue'

export function variantComponents(variant: string): Plugin | undefined {
  const excluded = variant === 'lite'
    ? ['SoterIdentityDialog.vue', 'PifFingerprintDialog.vue', 'SettingsView.vue']
    : variant === 'no-soter' ? ['SoterIdentityDialog.vue'] : []
  if (excluded.length === 0) return undefined
  const names = new Set(excluded)
  const prefix = '\0r1-empty-component:'
  return {
    name: 'r1-variant-components',
    apply: 'build',
    enforce: 'pre',
    resolveId(source) {
      const path = source.split('?', 1)[0]!.replace(/\\/g, '/')
      const name = path.slice(path.lastIndexOf('/') + 1)
      return names.has(name) ? prefix + 'stub.js' : null
    },
    load(id) {
      return id.startsWith(prefix) ? 'export default {}' : null
    },
  }
}


export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, '.', 'VITE_R1_')
  const variant = env.VITE_R1_VARIANT || 'full'
  if (!['full', 'no-soter', 'lite'].includes(variant)) {
    throw new Error(`Invalid VITE_R1_VARIANT: ${variant}`)
  }
  return {
    base: '',
    plugins: [variantComponents(variant), vue(), stripCrossorigin()],
    server: {
      host: '0.0.0.0',
    },
    build: {
      target: 'chrome94',
      cssTarget: 'chrome94',
      outDir: env.VITE_R1_OUT_DIR || '../template/webroot',
      emptyOutDir: true,
      cssCodeSplit: false,
    },
  }
})

/**
 * Drop the `crossorigin` attribute Vite adds to the emitted module script and
 * preload links.
 *
 * Module scripts are fetched in CORS mode when the attribute is present, so the
 * manager container must answer with `Access-Control-Allow-Origin` for the page
 * to run at all. KernelSU-derived managers serve the module webroot themselves
 * and several of them do not, which leaves a completely blank page with no
 * console output the user can see. These files are always same-origin, so the
 * attribute buys nothing and only adds a failure mode.
 */
export function stripCrossorigin(): Plugin {
  return {
    name: 'r1-strip-crossorigin',
    apply: 'build',
    enforce: 'post',
    transformIndexHtml(html) {
      return html.replace(/ crossorigin(?:="[^"]*")?/g, '')
    },
  }
}
