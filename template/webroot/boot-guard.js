/**
 * Boot guard.
 *
 * Runs as a classic script before the module entry, so it survives when the
 * module graph itself fails to load. Its job is to make a failed boot visible:
 * a WebView that blocks the module script renders an empty page, and an empty
 * page carries no information. The fallback node inside `#app` is replaced as
 * soon as Vue mounts, so this text only survives when the app never started.
 */
(function () {
  var reported = false

  function report(message) {
    var node = document.getElementById('boot-fallback')
    if (!node) return
    reported = true
    node.setAttribute('data-error', 'true')
    node.textContent = '界面加载失败：' + message
  }

  window.addEventListener('error', function (event) {
    // Resource failures arrive without a message; name the tag instead.
    if (event && event.message) {
      report(event.message)
    } else if (event && event.target && event.target.tagName) {
      report('无法加载 ' + String(event.target.tagName).toLowerCase())
    }
  })

  window.addEventListener('unhandledrejection', function (event) {
    var reason = event ? event.reason : null
    report(reason && reason.message ? reason.message : String(reason))
  })

  // The module graph resolves within a second on device. If the fallback is
  // still on screen after this long the entry never executed at all.
  window.setTimeout(function () {
    if (reported) return
    if (document.getElementById('boot-fallback')) {
      report('前端脚本未执行（可能被管理器容器拦截）')
    }
  }, 6000)
})()
