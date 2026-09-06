#!/usr/bin/env node
// The authored SVG is the source of truth. Tauri's installed icon renderer keeps
// every platform's exported assets reproducible without additional dependencies.
import { spawnSync } from 'node:child_process'
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { deflateSync, inflateSync } from 'node:zlib'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const source = readFileSync(join(root, 'design/vibestudio-logo.svg'), 'utf8')
const viewBox = source.match(/viewBox="([^"]+)"/)?.[1].split(/\s+/).map(Number)
const background = source.match(/<rect\b[^>]*\bid="Lime-background"[^>]*\bfill="([^"]+)"/)?.[1]
if (!viewBox || viewBox.length !== 4 || !viewBox.every(Number.isFinite) || !background) {
  throw new Error('Expected a valid viewBox and the Lime-background rectangle in the authored logo.')
}
const [x, y, width, height] = viewBox
const size = Math.max(width, height)
const left = x - (size - width) / 2
const top = y - (size - height) / 2
const centerX = left + size / 2
const centerY = top + size / 2
const body = source.replace(/^[\s\S]*?<svg\b[^>]*>/, '').replace(/<\/svg>\s*$/, '')
  .replace(/\s*<title>[\s\S]*?<\/title>/, '')
  .replace(/\s*<desc>[\s\S]*?<\/desc>/, '')
const mark = body.replace(/\s*<rect\b[^>]*\bid="Lime-background"[^>]*\/>/, '')
const svg = (content) => `<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="${left} ${top} ${size} ${size}" fill="none">\n  <title>VibeStudio</title>${content}\n</svg>\n`
const fill = `<rect x="${left}" y="${top}" width="${size}" height="${size}" fill="${background}"/>`
const square = svg(body)
const opaque = svg(fill + mark)
const scaledMark = (scale) => `<g transform="translate(${centerX} ${centerY}) scale(${scale}) translate(${-centerX} ${-centerY})">${mark}</g>`
// Keep the complete mark inside the maskable icon's central 80% safe circle.
const maskable = svg(fill + scaledMark(0.85))
const temp = mkdtempSync(join(tmpdir(), 'vibestudio-brand-assets-'))
const cli = join(root, 'node_modules/@tauri-apps/cli/tauri.js')

function runIcon(input, output, args = []) {
  const result = spawnSync(process.execPath, [cli, 'icon', input, '--output', output, ...args], {
    cwd: temp,
    encoding: 'utf8',
  })
  if (result.error || result.status !== 0) {
    throw new Error(result.error?.message || result.stderr || result.stdout)
  }
}

function copy(sourcePath, destination) {
  const target = join(root, destination)
  mkdirSync(dirname(target), { recursive: true })
  copyFileSync(sourcePath, target)
}

function copyDirectory(sourcePath, destination) {
  for (const entry of readdirSync(sourcePath, { withFileTypes: true })) {
    if (entry.isDirectory()) copyDirectory(join(sourcePath, entry.name), join(destination, entry.name))
    else copy(join(sourcePath, entry.name), join(destination, entry.name))
  }
}

// Apple catalogs must not carry an alpha channel. Composite against the tile
// color and encode RGB, including the renderer's nearly opaque edge pixels.
function flattenPng(path) {
  const png = readFileSync(path)
  const header = Buffer.from(png.subarray(16, 29))
  if (header[8] !== 8 || header[9] !== 6 || header[12] !== 0) {
    throw new Error(`Expected an 8-bit, non-interlaced RGBA PNG: ${path}`)
  }
  const width = header.readUInt32BE(0)
  const height = header.readUInt32BE(4)
  const compressed = []
  for (let offset = 8; offset < png.length;) {
    const length = png.readUInt32BE(offset)
    if (png.toString('ascii', offset + 4, offset + 8) === 'IDAT') {
      compressed.push(png.subarray(offset + 8, offset + 8 + length))
    }
    offset += length + 12
  }
  const raw = inflateSync(Buffer.concat(compressed))
  const rgbaStride = width * 4
  const rgbStride = width * 3 + 1
  const rgb = Buffer.alloc(rgbStride * height)
  const backdrop = background.slice(1).match(/../g).map((channel) => parseInt(channel, 16))
  let previous = Buffer.alloc(rgbaStride)
  const paeth = (a, b, c) => {
    const p = a + b - c
    const [pa, pb, pc] = [Math.abs(p - a), Math.abs(p - b), Math.abs(p - c)]
    return pa <= pb && pa <= pc ? a : pb <= pc ? b : c
  }
  for (let row = 0; row < height; row++) {
    const filter = raw[row * (rgbaStride + 1)]
    const pixels = Buffer.from(raw.subarray(row * (rgbaStride + 1) + 1, (row + 1) * (rgbaStride + 1)))
    if (filter > 4) throw new Error(`Unsupported PNG row filter: ${filter}`)
    for (let i = 0; i < rgbaStride; i++) {
      const a = i < 4 ? 0 : pixels[i - 4]
      const b = previous[i]
      const c = i < 4 ? 0 : previous[i - 4]
      pixels[i] = (pixels[i] + [0, a, b, Math.floor((a + b) / 2), paeth(a, b, c)][filter]) & 255
    }
    for (let column = 0; column < width; column++) {
      const alpha = pixels[column * 4 + 3] / 255
      for (let channel = 0; channel < 3; channel++) {
        rgb[row * rgbStride + 1 + column * 3 + channel] = Math.round(
          pixels[column * 4 + channel] * alpha + backdrop[channel] * (1 - alpha),
        )
      }
    }
    previous = pixels
  }
  const chunk = (type, data) => {
    const content = Buffer.concat([Buffer.from(type), data])
    let crc = 0xffffffff
    for (const byte of content) {
      crc ^= byte
      for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0)
    }
    const result = Buffer.alloc(data.length + 12)
    result.writeUInt32BE(data.length, 0)
    content.copy(result, 4)
    result.writeUInt32BE((crc ^ 0xffffffff) >>> 0, result.length - 4)
    return result
  }
  header[9] = 2
  writeFileSync(path, Buffer.concat([
    png.subarray(0, 8), chunk('IHDR', header), chunk('IDAT', deflateSync(rgb)), chunk('IEND', Buffer.alloc(0)),
  ]))
}

function sortIcnsChunks(path) {
  // The CLI collects ICNS representations in a hash map. Stabilize their order
  // so regenerating unchanged artwork produces byte-for-byte identical files.
  const icns = readFileSync(path)
  const chunks = []
  for (let offset = 8; offset < icns.length;) {
    const length = icns.readUInt32BE(offset + 4)
    if (length < 8 || offset + length > icns.length) throw new Error('Invalid ICNS chunk')
    chunks.push(icns.subarray(offset, offset + length))
    offset += length
  }
  chunks.sort((a, b) => Buffer.compare(a.subarray(0, 4), b.subarray(0, 4)))
  writeFileSync(path, Buffer.concat([icns.subarray(0, 8), ...chunks]))
}

try {
  // Android's 108dp foreground has a smaller, 66dp central safe circle.
  const inputs = { square, opaque, maskable, foreground: svg(scaledMark(0.65)) }
  for (const [name, content] of Object.entries(inputs)) writeFileSync(join(temp, `${name}.svg`), content)
  // Use the same SVG rasterizer for browser icons and the Android foreground.
  runIcon(join(temp, 'square.svg'), join(temp, 'web'), ['--png', '192', '--png', '512'])
  runIcon(join(temp, 'opaque.svg'), join(temp, 'touch'), ['--png', '180'])
  runIcon(join(temp, 'maskable.svg'), join(temp, 'maskable'), ['--png', '512'])
  runIcon(join(temp, 'foreground.svg'), join(temp, 'foreground'), ['--png', '1024'])
  writeFileSync(join(temp, 'icon.json'), JSON.stringify({
    default: 'square.svg',
    bg_color: background,
    android_fg: 'foreground/1024x1024.png',
  }))
  runIcon(join(temp, 'icon.json'), join(temp, 'native'))
  sortIcnsChunks(join(temp, 'native/icon.icns'))
  for (const name of readdirSync(join(temp, 'native/ios'))) {
    if (name.endsWith('.png')) flattenPng(join(temp, 'native/ios', name))
  }
  flattenPng(join(temp, 'touch/180x180.png'))
  flattenPng(join(temp, 'maskable/512x512.png'))

  writeFileSync(join(root, 'app-icon.svg'), square)
  writeFileSync(join(root, 'public/favicon.svg'), square)
  copy(join(root, 'design/vibestudio-logo.svg'), 'docs/assets/vibestudio-logo.svg')
  copy(join(temp, 'web/192x192.png'), 'public/icons/icon-192.png')
  copy(join(temp, 'web/512x512.png'), 'public/icons/icon-512.png')
  copy(join(temp, 'touch/180x180.png'), 'public/apple-touch-icon.png')
  copy(join(temp, 'maskable/512x512.png'), 'public/icons/maskable-512.png')
  copyDirectory(join(temp, 'native'), 'client/desktop/icons')

  // Tauri's explicit output directory is also our portable icon catalog. Copy
  // the iOS exports into the checked-in Xcode catalog used by native builds.
  const appleCatalog = 'client/desktop/gen/apple/Assets.xcassets/AppIcon.appiconset'
  copyDirectory(join(temp, 'native/ios'), appleCatalog)
  const androidResources = 'client/desktop/gen/android/app/src/main/res'
  if (existsSync(join(root, androidResources))) {
    copyDirectory(join(temp, 'native/android'), androidResources)
  }
  console.log('Generated desktop, iOS, Android, PWA, favicon, and documentation logo assets.')
} finally {
  rmSync(temp, { recursive: true, force: true })
}
