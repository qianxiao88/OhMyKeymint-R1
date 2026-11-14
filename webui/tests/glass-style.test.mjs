import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'

const style = readFileSync(new URL('../src/vue/r1.scss', import.meta.url), 'utf8')

function luminance(hex) {
  const rgb = hex.match(/[a-f\d]{2}/gi).map(channel => parseInt(channel, 16) / 255)
  return rgb.map(value => value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4)
    .reduce((sum, value, index) => sum + value * [0.2126, 0.7152, 0.0722][index], 0)
}

test('light and dark text palettes retain contrast against their reference paper colors', () => {
  const palette = name => [...style.matchAll(new RegExp(`--r1-${name}: (#[a-f\\d]{6});`, 'gi'))].map(match => match[1])
  const papers = palette('paper')
  assert.equal(papers.length, 2)
  for (const name of ['ink', 'muted', 'accent', 'error']) {
    const colors = palette(name)
    assert.equal(colors.length, 2)
    colors.forEach((color, index) => {
      const values = [luminance(color), luminance(papers[index])].sort((a, b) => b - a)
      assert.ok((values[0] + 0.05) / (values[1] + 0.05) >= 4.5, `${name}: ${color} on ${papers[index]}`)
    })
  }
  assert.match(style, /\.m-basic-component__center[\s\S]*?background: transparent !important/)
  assert.doesNotMatch(style, /background: var\(--r1-paper\)/)
  assert.match(style, /text-shadow:[^;]*var\(--r1-halo\)/)
})

test('wallpaper fills the viewport with an unfiltered cover layer', () => {
  const wallpaper = style.match(/html::before\s*\{([^}]+)\}/)[1]
  for (const rule of ['position: fixed', 'inset: 0', 'z-index: 0', 'background-size: cover', 'background-repeat: no-repeat']) {
    assert.ok(wallpaper.includes(rule), rule)
  }
  assert.doesNotMatch(wallpaper, /filter|mask|opacity|contain|gradient/)
  assert.match(style, /#app \{ position: relative; z-index: 1; \}/)
  assert.match(style, /--r1-canvas: linear-gradient/)
})

test('glass uses small blur, compact transparent headers and translucent fallback', () => {
  assert.match(style, /border-radius: 24px/)
  assert.match(style, /box-shadow: inset 0 1px 0 var\(--r1-edge\)/)
  for (const match of style.matchAll(/blur\((\d+)px\)/g)) assert.ok(Number(match[1]) <= 6)
  assert.match(style, /\.m-top-app-bar, \.targets-top-bar[^}]*backdrop-filter: none !important/s)
  assert.match(style, /@supports not[^}]*background: var\(--r1-control\) !important/s)
  assert.match(style, /\.app-page > \.m-top-app-bar, \.settings-view > \.m-top-app-bar \{ display: none; \}/)
  assert.match(style, /\.page-large-title \{ display: block;/)
  assert.match(style, /prefers-reduced-motion: reduce/)
  assert.match(style, /transition: none !important/)
  assert.match(style, /\.m-navigation-bar__item \{ opacity: 1 !important/)
})
