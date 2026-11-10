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

test('cloud update invokes the fixed arm64 helper and requires ok:true', async () => {
  const { cli, calls } = harness({ errno: 0, stdout: '{"ok":true}\n', stderr: '' })
  await cli.cloudUpdate()
  assert.deepEqual(calls, ["'/data/adb/modules/oh_my_keymint/arm64-v8a/r1-helper' 'cloud-update'"])
  for (const stdout of ['{}', '{"ok":false}', 'not json', '[]']) {
    await assert.rejects(harness({ errno: 0, stdout, stderr: '' }).cli.cloudUpdate())
  }
  await assert.rejects(harness({ errno: 1, stdout: '', stderr: 'Cloud temporarily unavailable' }).cli.cloudUpdate(),
    /Cloud temporarily unavailable/)
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
