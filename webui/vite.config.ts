import { defineConfig, loadEnv } from 'vite'
import vue from '@vitejs/plugin-vue'

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, '.', 'VITE_R1_')
  const variant = env.VITE_R1_VARIANT || 'full'
  if (!['full', 'no-soter', 'lite'].includes(variant)) {
    throw new Error(`Invalid VITE_R1_VARIANT: ${variant}`)
  }
  return {
    base: '',
    plugins: [vue()],
    server: {
      host: '0.0.0.0',
      allowedHosts: ['.monkeycode-ai.online'],
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
