import assert from 'node:assert/strict'
import { EventEmitter } from 'node:events'
import { readFileSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'
import { test } from 'node:test'
import vm from 'node:vm'

const component = readFileSync(new URL('../src/vue/PifFingerprintDialog.vue', import.meta.url), 'utf8')
const cliSource = readFileSync(new URL('../src/cli.ts', import.meta.url), 'utf8')
const withoutImports = source => source.replace(/^import[\s\S]*? from ['"][^'"]+['"]\s*$/gm, '')
const script = withoutImports(component.split('<script setup lang="ts">')[1].split('</script>')[0])
const template = component.slice(component.indexOf('\n<template>'), component.indexOf('\n<style'))
const profile = (overrides = {}) => ({
  enabled: true,
  model: 'Pixel 6a',
  product: 'bluejay_beta',
  fingerprint: 'google/bluejay_beta/bluejay:16/EXAMPLE/1:user/release-keys',
  security_patch: '2026-10-05',
  device_initial_sdk_int: 32,
  spoof_build: true,
  spoof_props: true,
  spoof_vending_build: false,
  ...overrides,
})

function dialogHarness({ state = profile(), preview = false, applyResult } = {}) {
  const calls = [], events = [], errors = [], reads = [], watchers = []
  const cli = {
    getPifFingerprintState: async () => {
      reads.push('state')
      if (state instanceof Error) throw state.message
      return state
    },
    // The dialog prefers the live Pixel Canary resolver and falls back to the
    // mirrored catalog, so both entry points must be present in the mock.
    listCanaryDevices: async () => { throw new Error('canary unavailable in tests') },
    listPifDevices: async () => { reads.push('catalog'); return [{ model: 'Pixel 6a', product: 'bluejay_beta' }] },
    applyCanaryFingerprint: async () => { throw new Error('canary catalog is unavailable') },
    applyPifFingerprint: async (...args) => {
      calls.push(args)
      return applyResult
        ? applyResult(...args)
        : profile({
            product: args[0],
            spoof_build: args[1],
            spoof_props: args[2],
            spoof_vending_build: args[3],
          })
    },
    disablePifFingerprint: async () => { calls.push(['disable']); return { enabled: false } },
  }
  const props = { modelValue: true, cli }
  const dialog = vm.runInNewContext(`${stripTypeScriptTypes(script)}\n;({
    loadState, loadCatalog, retryState, retryCatalog, apply, requestClose,
    currentState, desiredEnabled, selectedProduct, stateStatus, catalogStatus,
    stateError, catalogError, canApply, busy, devices, previewDevices,
    spoofBuild, spoofProps, spoofVendingBuild,
  })`, {
    defineProps: () => props,
    defineEmits: () => (...args) => events.push(args),
    defineExpose: () => {},
    ref: value => ({ value }),
    shallowRef: value => ({ value }),
    computed: get => ({ get value() { return get() } }),
    watch: (sources, callback) => { if (Array.isArray(sources)) watchers.push([sources, callback]) },
    onBeforeUnmount: () => {},
    afterPaint: async () => {},
    isDev: () => preview,
    i18n: { t: key => key },
    console: { error: (...args) => errors.push(args), warn: () => {} },
  })
  const updateCatalog = async () => {
    for (const [sources, callback] of watchers) {
      await callback(sources.map(source => typeof source === 'function' ? source() : source.value), [], () => {})
    }
    await new Promise(resolve => setImmediate(resolve))
  }
  return { ...dialog, calls, events, errors, reads, updateCatalog }
}

function cliHarness(result = args => profile({
  product: args[1],
  spoof_build: args[2] === '1',
  spoof_props: args[3] === '1',
  spoof_vending_build: args[4] === '1',
})) {
  const calls = [], execCalls = []
  const securityPatch = withoutImports(readFileSync(new URL('../src/security_patch.ts', import.meta.url), 'utf8'))
  const code = stripTypeScriptTypes(`${securityPatch.replaceAll('export ', '')}\n${withoutImports(cliSource).replaceAll('export ', '')}\nnew Cli()`)
  const cli = vm.runInNewContext(code, {
    TextEncoder, TextDecoder,
    // The helper runner arms a wall-clock guard with window.setTimeout, which
    // the browser always provides; mirror it here so the harness matches.
    window: { setTimeout, clearTimeout },
    setTimeout, clearTimeout,
    exec: async command => {
      execCalls.push(command)
      return { errno: 0, stdout: 'arm64-v8a\naarch64\n', stderr: '' }
    },
    spawn: (binary, args) => {
      calls.push([binary, Array.from(args)])
      const process = new EventEmitter()
      process.stdout = new EventEmitter()
      process.stderr = new EventEmitter()
      queueMicrotask(() => {
        process.stdout.emit('data', JSON.stringify(typeof result === 'function' ? result(Array.from(args)) : result))
        process.emit('exit', 0)
      })
      return process
    },
  })
  return { cli, calls, execCalls }
}

test('PIF exposes the master switch plus three independent spoof switches', () => {
  assert.equal((template.match(/role="switch"/g) ?? []).length, 1)
  assert.equal((template.match(/<MiuixSwitchPreference\b/g) ?? []).length, 3)
  for (const control of ['spoofBuild', 'spoofProps', 'spoofVendingBuild', 'pif_spoof_build',
    'pif_spoof_props', 'pif_spoof_vending']) {
    assert.ok(component.includes(control), control)
  }
  const master = template.slice(template.indexOf('<MiuixBasicComponent'), template.indexOf('</MiuixBasicComponent>'))
  for (const control of ['current-config', 'device-list', '@click="retryCatalog"', '@click="retryState"',
    '@click="requestClose"', '@click="apply"', 'stateError', 'catalogError']) assert.ok(template.includes(control), control)
  assert.ok(master.includes('pif_enable_spoofing'))
})

test('stored profiles restore the three switches and apply the selected flags', async () => {
  for (const flags of [
    { spoof_build: true, spoof_props: true, spoof_vending_build: false },
    { spoof_build: true, spoof_props: false, spoof_vending_build: false },
    { spoof_build: false, spoof_props: true, spoof_vending_build: true },
  ]) {
    const dialog = dialogHarness({ state: profile(flags) })
    await dialog.loadState(0)
    await dialog.loadCatalog(0)
    assert.equal(dialog.desiredEnabled.value, true)
    assert.equal(dialog.selectedProduct.value, 'bluejay_beta')
    assert.equal(dialog.spoofBuild.value, flags.spoof_build)
    assert.equal(dialog.spoofProps.value, flags.spoof_props)
    assert.equal(dialog.spoofVendingBuild.value, flags.spoof_vending_build)
    assert.equal(dialog.canApply.value, true)
    await dialog.apply()
    assert.deepEqual(dialog.calls, [[
      'bluejay_beta', flags.spoof_build, flags.spoof_props, flags.spoof_vending_build,
    ]])
    assert.equal(dialog.currentState.value.spoof_build, flags.spoof_build)
    assert.equal(dialog.currentState.value.spoof_props, flags.spoof_props)
    assert.equal(dialog.currentState.value.spoof_vending_build, flags.spoof_vending_build)
    assert.ok(dialog.events.some(event => event[0] === 'changed'))
    assert.ok(dialog.events.some(event => event[0] === 'update:modelValue' && event[1] === false))
  }
})

test('master disable avoids catalog loading and cancel remains guarded while applying', async () => {
  let finish
  const dialog = dialogHarness({ applyResult: () => new Promise(resolve => { finish = resolve }) })
  await dialog.loadState(0)
  await dialog.loadCatalog(0)
  const pending = dialog.apply()
  await Promise.resolve()
  assert.equal(dialog.busy.value, true)
  assert.equal(dialog.requestClose(), false)
  await dialog.apply()
  assert.equal(dialog.calls.length, 1)
  finish(profile())
  await pending
  assert.equal(dialog.busy.value, false)
  assert.equal(dialog.requestClose(), true)

  const disabled = dialogHarness()
  await disabled.loadState(0)
  disabled.desiredEnabled.value = false
  assert.equal(disabled.catalogStatus.value, 'loading')
  assert.equal(disabled.canApply.value, true)
  await disabled.apply()
  assert.deepEqual(disabled.calls, [['disable']])
  assert.equal(disabled.currentState.value.enabled, false)
})

test('apply errors keep the sheet open and restore its action controls', async () => {
  const dialog = dialogHarness({ applyResult: () => { throw new Error('offline') } })
  await dialog.loadState(0)
  await dialog.loadCatalog(0)
  await dialog.apply()
  assert.equal(dialog.busy.value, false)
  assert.equal(dialog.errors.length, 1)
  assert.ok(dialog.events.some(event => event[0] === 'notify' && event[2] === true))
  assert.ok(!dialog.events.some(event => event[0] === 'update:modelValue'))
})

test('development preview restores multiple Pixel examples and changes no device settings', async () => {
  const dialog = dialogHarness({ preview: true })
  await dialog.loadState(0)
  await dialog.loadCatalog(0)
  assert.deepEqual(Array.from(dialog.previewDevices, item => item.model), ['Pixel 6a', 'Pixel 9 Pro', 'Pixel 11 Pro XL'])
  assert.match(template, /Development preview: example devices and configuration only, not real device data\./)
  dialog.selectedProduct.value = 'caiman_beta'
  dialog.spoofBuild.value = false
  dialog.spoofProps.value = true
  dialog.spoofVendingBuild.value = true
  await dialog.apply()
  assert.equal(dialog.currentState.value.model, 'Pixel 9 Pro')
  assert.equal(dialog.currentState.value.spoof_build, false)
  assert.equal(dialog.currentState.value.spoof_props, true)
  assert.equal(dialog.currentState.value.spoof_vending_build, true)
  assert.deepEqual(dialog.calls, [])
})

test('unreadable saved profiles remain unchanged until the user applies disable', async () => {
  const dialog = dialogHarness({ state: new Error('unsupported profile schema 3') })
  assert.equal(dialog.canApply.value, false)
  await dialog.loadState(0)
  await dialog.updateCatalog()
  assert.equal(dialog.stateStatus.value, 'error')
  assert.equal(dialog.stateError.value, 'unsupported profile schema 3')
  assert.equal(dialog.currentState.value, null)
  assert.equal(dialog.desiredEnabled.value, false)
  assert.equal(dialog.canApply.value, true)
  assert.deepEqual(dialog.reads, ['state'])
  assert.deepEqual(dialog.calls, [])
  assert.match(template, /Current configuration could not be loaded\. Select a device to replace it, or switch off and apply to clear it\./)
  assert.match(template, /:aria-disabled="busy \|\| stateStatus === 'loading'"/)
  assert.doesNotMatch(template, /:disabled="busy \|\| stateStatus !== 'ready'"/)
  await dialog.apply()
  assert.deepEqual(dialog.calls, [['disable']])
  assert.equal(dialog.currentState.value.enabled, false)
})

test('unreadable saved profiles can load a catalog and explicitly apply a replacement', async () => {
  const dialog = dialogHarness({ state: new Error('unsupported profile schema 3') })
  await dialog.loadState(0)
  dialog.desiredEnabled.value = true
  assert.equal(dialog.canApply.value, false)
  await dialog.updateCatalog()
  assert.deepEqual(dialog.reads, ['state', 'catalog'])
  assert.equal(dialog.catalogStatus.value, 'ready')
  assert.equal(dialog.stateStatus.value, 'error')
  assert.equal(dialog.stateError.value, 'unsupported profile schema 3')
  assert.deepEqual(dialog.calls, [])
  dialog.selectedProduct.value = 'bluejay_beta'
  assert.equal(dialog.canApply.value, true)
  assert.match(template, /<template v-if="stateStatus !== 'loading'">/)
  assert.match(template, /v-if="stateStatus === 'ready' && currentState !== null"/)
  await dialog.apply()
  assert.deepEqual(dialog.calls, [['bluejay_beta', true, true, false]])
  assert.equal(dialog.currentState.value.product, 'bluejay_beta')
  assert.ok(dialog.events.some(event => event[0] === 'changed'))
})

test('CLI sends the selected positional spoof flags and validates the response', async () => {
  const { cli, calls } = cliHarness()
  const state = await cli.applyPifFingerprint('bluejay_beta')
  assert.equal(state.spoof_build, true)
  assert.equal(state.spoof_props, true)
  assert.equal(state.spoof_vending_build, false)
  assert.deepEqual(calls, [[
    '/data/adb/omk/keymint',
    ['--webui-apply-pif-fingerprint', 'bluejay_beta', '1', '1', '0'],
  ]])

  const explicit = cliHarness()
  await explicit.cli.applyPifFingerprint('bluejay_beta', false, true, true)
  assert.deepEqual(explicit.calls, [[
    '/data/adb/omk/keymint',
    ['--webui-apply-pif-fingerprint', 'bluejay_beta', '0', '1', '1'],
  ]])

  for (const result of [
    { enabled: false }, profile({ product: 'caiman_beta' }),
    profile({ spoof_build: false }), profile({ spoof_props: false }), profile({ spoof_vending_build: true }),
  ]) {
    await assert.rejects(cliHarness(result).cli.applyPifFingerprint('bluejay_beta'), /unexpected PIF fingerprint state/)
  }
})

test('CLI accepts every boolean flag combination and rejects non-boolean flags', async () => {
  const { cli, calls, execCalls } = cliHarness()
  for (const flags of [
    [true, true, false], [true, false, false], [false, true, true], [false, false, false],
  ]) {
    await cli.applyPifFingerprint('bluejay_beta', ...flags)
  }
  for (const flags of [['false', false, true], [false, 'true', true], [false, false, 1]]) {
    await assert.rejects(cli.applyPifFingerprint('bluejay_beta', ...flags), /Invalid PIF spoofing options/)
  }
  await assert.rejects(cli.applyPifFingerprint('not a product'), /Invalid PIF product/)
  assert.equal(calls.length, 4)
})
