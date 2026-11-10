import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'
import { test } from 'node:test'
import vm from 'node:vm'

const source = readFileSync(new URL('../src/cli.ts', import.meta.url), 'utf8')
const start = source.indexOf('export class Cli {')
const end = source.indexOf('  async getSoterLocal()', start)
const quoteStart = source.indexOf('function shellQuote(')
const quote = source.slice(quoteStart, source.indexOf('\n}', quoteStart) + 2)
function harness(result) {
  const calls = []
  const code = stripTypeScriptTypes(`${quote}\n${source.slice(start, end).replace('export ', '')}}\nnew Cli()`)
  const cli = vm.runInNewContext(code, {
    exec: async command => { calls.push(command); return result },
    isRecord: value => value !== null && typeof value === 'object' && !Array.isArray(value),
  })
  return { cli, calls }
}

test('cloud update invokes the fixed arm64 helper and returns unchanged boolean', async () => {
  const { cli, calls } = harness({ errno: 0, stdout: '{"ok":true,"unchanged":true}\n', stderr: '' })
  assert.equal(await cli.cloudUpdate(), true)
  assert.equal(await harness({ errno: 0, stdout: '{"ok":true,"unchanged":false}', stderr: '' }).cli.cloudUpdate(), false)
  assert.deepEqual(calls, ["'/data/adb/modules/oh_my_keymint/arm64-v8a/r1-helper' 'cloud-update'"])
  for (const stdout of ['{}', '{"ok":false,"unchanged":true}', '{"ok":"true","unchanged":true}',
    '{"ok":true}', 'not json', '[]', 'null']) {
    await assert.rejects(harness({ errno: 0, stdout, stderr: '' }).cli.cloudUpdate())
  }
  for (const unchanged of [null, 0, 1, 'true', 'false', [], {}]) {
    await assert.rejects(harness({ errno: 0, stdout: JSON.stringify({ ok: true, unchanged }), stderr: '' }).cli.cloudUpdate(),
      /Invalid R1 cloud update response/)
  }
  await assert.rejects(harness({ errno: 1, stdout: '', stderr: 'Cloud temporarily unavailable' }).cli.cloudUpdate(),
    /Cloud temporarily unavailable/)
})

test('cloud update shows both success messages, refreshes state, and preserves failure handling', async () => {
  const app = readFileSync(new URL('../src/vue/App.vue', import.meta.url), 'utf8')
  const start = app.indexOf('async function updateCloudKeybox(')
  const end = app.indexOf('\nasync function openFeedback(', start)
  for (const result of [true, false, new Error('offline')]) {
    const notifications = []
    const refreshes = []
    const cloudBusy = { value: false }
    const update = vm.runInNewContext(stripTypeScriptTypes(`${app.slice(start, end)}\nupdateCloudKeybox`), {
      cloudBusy, keyboxBusy: { value: false }, keyboxOpen: { value: false },
      isDev: () => false,
      afterPaint: async () => {},
      cli: { cloudUpdate: async () => { if (result instanceof Error) throw result; return result } },
      notify: (...args) => notifications.push(args),
      refreshIdentity: async force => refreshes.push(['identity', force]),
      refreshDiagnostics: async () => refreshes.push(['diagnostics']),
    })
    await update()
    assert.deepEqual(notifications, result instanceof Error
      ? [['请检查你的网络哟', true]]
      : [[result ? '你的keybox是最新的啦' : '获取成功，正在载入']])
    assert.deepEqual(refreshes, result instanceof Error ? [] : [['identity', true], ['diagnostics']])
    assert.equal(cloudBusy.value, false)
  }
})

test('status validates every field and keeps cloud connectivity separate from running state', async () => {
  const result = { ok: true, reason: 'running', cloud_connected: false }
  const { cli, calls } = harness({ errno: 0, stdout: JSON.stringify(result), stderr: '' })
  assert.equal(JSON.stringify(await cli.getR1Status()), JSON.stringify(result))
  assert.match(calls[0], /r1-helper' 'status'$/)
  await assert.rejects(harness({ errno: 0, stdout: '{"ok":true}', stderr: '' }).cli.getR1Status(), /Invalid R1 status/)
})

test('feedback group uses a shell-quoted Android intent and reports unresolved activities', async () => {
  const { cli, calls } = harness({ errno: 0, stdout: 'Starting: Intent', stderr: '' })
  await cli.openFeedbackGroup()
  assert.match(calls[0], /^\/system\/bin\/am start --user current -a android.intent.action.VIEW -d 'mqqapi:\/\//)
  assert.match(calls[0], /uin=608960009&card_type=group&source=qrcode'$/)
  await assert.rejects(harness({ errno: 0, stdout: 'Error: unable to resolve Intent', stderr: '' }).cli.openFeedbackGroup(),
    /unable to resolve Intent/)
})
