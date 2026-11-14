import { createApp } from 'vue'
import { enableEdgeToEdge, isKsuWebui } from 'kernelsu-alt'
import 'miuix-vue/style.css'
import { isSupported, renderBlockingPage } from './webview/webview'
import App from './vue/App.vue'
import { i18n } from './i18n'
import { restoreWallpaper } from './wallpaper'

const root = document.querySelector<HTMLDivElement>('#app')!

if (!isSupported()) {
  root.replaceChildren(renderBlockingPage())
} else {
  try {
    if (isKsuWebui()) {
      // Older hosts enable this when insets.css loads; retain that fallback.
      await enableEdgeToEdge(true).catch(() => {})
    }
    // Resolve the saved wallpaper before the first paint. Doing it here rather
    // than in a mounted component means the first frame already carries the
    // right background, so the bundled default never flashes past a custom one.
    await restoreWallpaper()
    await i18n.init()
    createApp(App).mount(root)
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error)
    const errorElement = document.createElement('p')
    errorElement.id = 'load-error'
    errorElement.textContent = `Failed to load app: ${message}`
    root.replaceChildren(errorElement)
  }
}
