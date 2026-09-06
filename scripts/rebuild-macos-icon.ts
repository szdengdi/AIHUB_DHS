import type { Buffer } from 'node:buffer'
// 重建 macOS 图标资产，与 favicon.svg 保持同步（修复 #88：Dock 图标比相邻应用大一圈；
// 并为 macOS 状态栏生成半透明玻璃模板图标）。
//
// `tauri icon public/favicon.svg` 生成的是全出血（full-bleed）图标：白色圆角方块
// 填满整个画布，无透明边距。macOS Dock 按图标的不透明像素边界缩放，因此这类图标
// 会比带安全边距的相邻图标（如 ChatGPT/Firefox）渲染得更大。
//
// 本脚本在 `npm run icons` 之后执行：
//   1) 把 favicon 图形缩放到 Apple 官方 1024 图标网格（内容 824px、四周 100px 透明边距），
//      仅重生成 macOS 使用的 icon.icns（Windows/Linux 图标保持全出血不变，见
//      QwenLM/qwen-code PR #8987 的同类修复）。参考 Apple HIG.app-icons 网格。
//   2) 从 assets/macos-tray.svg 生成透明状态栏模板 PNG（src-tauri/icons/macos-tray.png），
//      供 builder.rs 在 macOS 上用 `icon_as_template(true)` 按 NSImage template 渲染，
//      由系统按菜单栏深浅/半透明材质自动着色（半透明玻璃观感）。
//
// 用法：tsx scripts/rebuild-macos-icon.ts
import { spawnSync } from 'node:child_process'
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { join, relative, resolve } from 'node:path'
import process from 'node:process'

const root = resolve(import.meta.dirname, '..')
const masterSvg = join(root, '.tmp', 'macos.svg')
const traySvg = join(root, 'assets', 'macos-tray.svg')
const tmpOut = join(root, '.tmp', 'macos-icons')
const targetIcns = join(root, 'src-tauri', 'icons', 'icon.icns')
const targetTrayPng = join(root, 'src-tauri', 'icons', 'macos-tray.png')

function fail(step: string, result?: { stderr?: Buffer | string }): never {
  const detail = result?.stderr ? result.stderr.toString() : ''
  console.error(`[rebuild-macos-icon] ${step} failed:\n${detail}`)
  process.exit(1)
}

// tauri CLI 的 JS 入口（避免 Windows 下 shell 展开 .cmd 的转义问题）
const cliJs = join(root, 'node_modules', '@tauri-apps', 'cli', 'tauri.js')

// 1. 由 favicon.svg 生成 Apple 网格母版（824px 内容 + 100px 透明边距）
const favicon = join(root, 'public', 'favicon.svg')
if (!existsSync(favicon))
  fail('read public/favicon.svg')
const src = readFileSync(favicon, 'utf8')
const match = src.match(/<svg[^>]*>([\s\S]*)<\/svg>/)
if (!match)
  fail('parse public/favicon.svg structure')
// Apple HIG：1024 画布，内容 824px（64px -> 824px = 12.875），边距 (1024-824)/2 = 100
const DOCK_SCALE = 12.875
const DOCK_GUTTER = 100
const master = `<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">
  <g transform="translate(${DOCK_GUTTER} ${DOCK_GUTTER}) scale(${DOCK_SCALE})">
${match[1]}
  </g>
</svg>
`
writeFileSync(masterSvg, master)

// 2. 用 tauri icon 生成整套图标到临时目录（.tmp 已在 .gitignore 中）
function genIcons(svgFile: string, outDir: string) {
  rmSync(outDir, { recursive: true, force: true })
  mkdirSync(outDir, { recursive: true })
  return spawnSync(process.execPath, [cliJs, 'icon', svgFile, '--output', outDir], {
    cwd: root,
    stdio: 'inherit',
  })
}

const gen = genIcons(masterSvg, tmpOut)
if (gen.status !== 0)
  fail('tauri icon (icns)', gen)

// 3. 只回写 macOS 的 icon.icns（其余平台图标保持不变）
const generatedIcns = join(tmpOut, 'icon.icns')
if (!existsSync(generatedIcns))
  fail('find generated icon.icns')
copyFileSync(generatedIcns, targetIcns)
console.log(`[rebuild-macos-icon] updated ${relative(root, targetIcns)}`)

// 4. 生成透明状态栏模板 PNG（128x128@2x = 256px，用于 retina）
const trayTmp = join(tmpOut, '..', 'tray-icons')
const tgen = genIcons(traySvg, trayTmp)
if (tgen.status !== 0)
  fail('tauri icon (tray)', tgen)
const traySrc = join(trayTmp, '128x128@2x.png')
if (!existsSync(traySrc))
  fail('find generated tray template png')
copyFileSync(traySrc, targetTrayPng)
console.log(`[rebuild-macos-icon] updated ${relative(root, targetTrayPng)}`)
