import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'
import { test } from 'node:test'
import vm from 'node:vm'

const source = readFileSync(new URL('../src/r1.ts', import.meta.url), 'utf8')
function variantHarness(value, name) {
  const code = stripTypeScriptTypes(source.replaceAll('export ', '').replaceAll('import.meta.env', 'R1_ENV'))
  return vm.runInNewContext(`${code}\n({ variant, IS_LITE, HAS_SOTER, R1_MODULE_NAME, R1_AUTHOR, toolAvailable })`, {
    R1_ENV: { VITE_R1_VARIANT: value, VITE_R1_NAME: name },
  })
}

test('build variants retain full by default and reject invalid variants', () => {
  for (const value of [undefined, '', 'full', 'no-soter', 'lite']) {
    const state = variantHarness(value)
    assert.equal(state.variant, value || 'full')
    assert.equal(state.IS_LITE, value === 'lite')
    assert.equal(state.HAS_SOTER, !value || value === 'full')
    assert.equal(state.R1_MODULE_NAME, 'OhMyKeymint-R1-1.3.8-302-full-dev')
    assert.equal(state.R1_AUTHOR, '\u6d45\u7b11\u5450')
  }
  assert.equal(variantHarness('full', 'custom').R1_MODULE_NAME, 'custom')
  assert.throws(() => variantHarness('invalid'), /Invalid VITE_R1_VARIANT/)
})

test('lite accepts only its five tool families; no-soter preserves other tools', () => {
  const lite = variantHarness('lite')
  for (const event of ['cloudUpdate', 'syncSecurityPatch', 'restoreSecurityPatch', 'clearGmsCache',
    'openAppTargets', 'wallpaper']) assert.equal(lite.toolAvailable(event), true)
  for (const event of ['installKeybox', 'spoofPif', 'repairRkp', 'issueRkp', 'openSoterLocal', 'feedback']) {
    assert.equal(lite.toolAvailable(event), false)
    assert.equal(variantHarness('full').toolAvailable(event), true)
    assert.equal(variantHarness('no-soter').toolAvailable(event), event !== 'openSoterLocal')
  }
})

test('lite tool rows exclude feedback and retain only requested actions', () => {
  const tools = readFileSync(new URL('../src/vue/ToolsView.vue', import.meta.url), 'utf8')
  const rows = tools.slice(tools.indexOf('const liteRows ='), tools.indexOf('const groups ='))
  assert.doesNotMatch(rows, /feedbackRow|installKeybox|spoofPif|repairRkp|issueRkp|openSoterLocal/)
  for (const action of ['cloudRow', "sheet: 'patch'", "event: 'clearGmsCache'", "event: 'openAppTargets'", 'wallpaperRow']) {
    assert.ok(rows.includes(action), action)
  }
})

test('build resolver excludes optional component sources before Vue compilation', () => {
  const config = readFileSync(new URL('../vite.config.ts', import.meta.url), 'utf8')
  const code = stripTypeScriptTypes(config.slice(config.indexOf('export function variantComponents'), config.indexOf('export default defineConfig')).replace('export ', ''))
  const create = vm.runInNewContext(`${code}\nvariantComponents`)
  assert.equal(create('full'), undefined)
  for (const variant of ['no-soter', 'lite']) {
    const plugin = create(variant)
    assert.equal(plugin.apply, 'build')
    assert.equal(plugin.enforce, 'pre')
    for (const component of ['SoterIdentityDialog.vue', 'PifFingerprintDialog.vue', 'SettingsView.vue']) {
      const excluded = variant === 'lite' || component === 'SoterIdentityDialog.vue'
      for (const path of [`./${component}`, `/repo/webui/src/vue/${component}`, `C:\\repo\\${component}?vue`]) {
        const resolved = plugin.resolveId(path)
        if (excluded) {
          assert.ok(resolved.startsWith('\0r1-empty-component:'))
          assert.ok(resolved.endsWith('.js'))
          assert.equal(plugin.load(resolved), 'export default {}')
        } else assert.equal(resolved, null)
      }
    }
    assert.equal(plugin.resolveId('./HomeView.vue'), null)
    assert.equal(plugin.load('/real/component.vue'), null)
  }
  assert.match(config, /plugins: \[variantComponents\(variant\), vue\(\)\]/)
})

const app = readFileSync(new URL('../src/vue/App.vue', import.meta.url), 'utf8')
function appFunction(name) {
  const start = app.indexOf(`async function ${name}(`)
  assert.ok(start >= 0)
  return stripTypeScriptTypes(app.slice(start, app.indexOf('\n}', start) + 2))
}

test('lite startup avoids PIF, RKP diagnostics and certificate revocation calls', async () => {
  const calls = []
  const ref = () => ({ value: null })
  const globals = { IS_LITE: true, isDev: () => false, afterPaint: async () => {}, console }
  for (const key of ['keyboxStatus', 'keyboxSource', 'keyboxLevel', 'keyboxRevocation', 'teeStatus',
    'securityPatch', 'spoofedDevice', 'diagnosticsStatus', 'moduleStatus', 'liteReason', 'diagnostics']) globals[key] = ref()
  globals.cli = new Proxy({}, { get: (_, method) => async () => {
    calls.push(method)
    switch (method) {
      case 'getKeyboxState': return { valid: true, bundled: false, source: 'unknown', level: 'tee', revocation: 'not_checked' }
      case 'getSystemSecurityPatch': return '2026-11-01'
      case 'getTeeStatus': return true
      case 'getR1Status': return { ok: true, reason: 'running', cloud_connected: false }
      default: throw new Error(`Unexpected startup call: ${method}`)
    }
  } })
  const run = vm.runInNewContext(`${appFunction('refreshIdentity')}\n${appFunction('refreshDiagnostics')}
    async () => { await refreshIdentity(); await refreshDiagnostics() }`, globals)
  await run()
  assert.deepEqual(calls.sort(), ['getKeyboxState', 'getR1Status', 'getSystemSecurityPatch', 'getTeeStatus'].sort())
  assert.equal(globals.moduleStatus.value, 'ready')
  assert.equal(globals.keyboxRevocation.value, 'not_checked')
})

test('Soter entry points and optional component imports are compile-time gated', () => {
  const targets = readFileSync(new URL('../src/vue/TargetsView.vue', import.meta.url), 'utf8')
  const row = readFileSync(new URL('../src/vue/TargetAppRow.vue', import.meta.url), 'utf8')
  for (const text of [app, targets]) {
    assert.match(text, /HAS_SOTER \? defineAsyncComponent/)
    assert.match(text, /<SoterIdentityDialog v-if="HAS_SOTER"/)
  }
  assert.match(targets, /MiuixButton v-if="HAS_SOTER"[^\n]+@click="openBulkIdentity"/)
  assert.match(row, /<button v-if="HAS_SOTER"/)
  assert.match(app, /IS_LITE \? undefined : defineAsyncComponent\(\(\) => import\('\.\/PifFingerprintDialog.vue'\)/)
})
