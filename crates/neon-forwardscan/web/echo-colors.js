(function (root, factory) {
  if (typeof module === 'object' && module.exports) module.exports = factory()
  else root.EchoColors = factory()
})(typeof self !== 'undefined' ? self : this, function () {
  'use strict'
  // How the Navico Echo app (NEON 2.6, SRX) turns one ping into one column of pixels, verified
  // against the running app (test/echo-colors.js):
  //
  //   1. Level: level = 100 - ceil(100 * b / 255), b = ping header byte 79 (the sounder's noise floor).
  //   2. Gain: the gain picks a threshold index (0..245):
  //        Auto:   ((100 - level) * 245) / 100                (clamped 1..245, integer division)
  //        Manual: 245 - floor(245 * s / 100), s = the UI gain - 10   (UI gain 10..100)
  //   3. Colour table: a 256 x 256 table of RGB565 pixels [threshold][cell].
  //      Cells at or below the threshold are the background colour; above it the cell is stretched
  //      by k = 256 / (1 + 399 * (100 - colorLine) / 100) and looks up the palette (256 RGB entries).
  //   4. Scaling: one pixel row steps `cellsPerFt * 65536 / pixelsPerFt` in 16.16
  //      fixed point through the samples and shows the MAXIMUM of the samples it skipped (peak hold).
  //   5. The white line: walking down the column, a row whose amplitude rises above
  //      threshold + 10 (having been at or below the threshold before, or at the top) is drawn white (0xffff).
  //      That marks the surface and the top of the bottom return (and strong fish / thermoclines).
  // Not reproduced: Color Erase, Fish ID, the bottom-region colouring (same table here).
  const PALETTE_DATA = {
    2: 'AAA/AAA/AAA/AAA/AAA/AAA/AAA/AAA/AAA/AAA/AAA/AAA/AAA/AAA/AAA/AAA/AAA/AAA/AAA/AAA/AAA/AAA/AAA/AAA/AAA/AAA/AABMAABaAABoAAB1AACDAACRAACfAACsAAC6AADIAADVAADjAADxAAD/AA3/ABr/ACj/ADX/AEP/AFD/AF3/AGv/AHj/AIb/AJP/AKH/AK7/ALv/AMn/ANb/AOT/APH/AP//AP/xAP/kAP/WAP/JAP+7AP+uAP+hAP+TAP+GAP94AP9rAP9dAP9QAP9DAP81AP8oAP8aAP8NAP8ADv8AHP8AKv8AOP8ARv8AVf8AY/8Acf8Af/8Ajf8Am/8Aqv8AuP8Axv8A1P8A4v8A8P8A//8A//YA/+0A/+QA/9sA/9MA/8oA/8EA/7gA/68A/6cA/54A/5UA/4wA/4MA/3sA/3IA/2kA/2AA/1cA/08A/0YA/z0A/zQA/ysA/yMA/xoA/xEA/wgA/wAA+gAA9gAA8gAA7QAA6QAA5QAA4QAA3AAA2AAA1AAA0AAAywAAxwAAwwAAvwAAugAAtgAAsgAArQAAqQAApQAAoQAAnAAAmAAAlAAAkAAAiwAAhwAAgwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAA',
    3: 'AAB/AAB/AAB/AAB/AAB/AAB/AAB/AAB/AAB/AAB/AAB/AAB/AAB/AAB/AAB/AAB/AAB/AAB/AAB/AAB/AAB/AAB/AAB/AAB/AAB/AAB/AACIAACRAACaAACjAACsAAC1AAC/AADIAADRAADaAADjAADsAAD1AAD/AA3/ABr/ACj/ADX/AEP/AFD/AF3/AGv/AHj/AIb/AJP/AKH/AK7/ALv/AMn/ANb/AOT/APH/AP//AP/xAP/kAP/WAP/JAP+7AP+uAP+hAP+TAP+GAP94AP9rAP9dAP9QAP9DAP81AP8oAP8aAP8NAP8ADv8AHP8AKv8AOP8ARv8AVf8AY/8Acf8Af/8Ajf8Am/8Aqv8AuP8Axv8A1P8A4v8A8P8A//8A//YA/+0A/+QA/9sA/9MA/8oA/8EA/7gA/68A/6cA/54A/5UA/4wA/4MA/3sA/3IA/2kA/2AA/1cA/08A/0YA/z0A/zQA/ysA/yMA/xoA/xEA/wgA/wAA+gAA9gAA8gAA7QAA6QAA5QAA4QAA3AAA2AAA1AAA0AAAywAAxwAAwwAAvwAAugAAtgAAsgAArQAAqQAApQAAoQAAnAAAmAAAlAAAkAAAiwAAhwAAgwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAA',
    1: '////////////////////////////////////////////////////////////////////////////////////////////////////////7Oz/2tr/yMj/trb/o6P/kZH/f3//bW3/W1v/SEj/Njb/JCT/EhL/AAD/AA3/ABr/ACj/ADX/AEP/AFD/AF3/AGv/AHj/AIb/AJP/AKH/AK7/ALv/AMn/ANb/AOT/APH/AP//AP/xAP/kAP/WAP/JAP+7AP+uAP+hAP+TAP+GAP94AP9rAP9dAP9QAP9DAP81AP8oAP8aAP8NAP8ADv8AHP8AKv8AOP8ARv8AVf8AY/8Acf8Af/8Ajf8Am/8Aqv8AuP8Axv8A1P8A4v8A8P8A//8A//YA/+0A/+QA/9sA/9MA/8oA/8EA/7gA/68A/6cA/54A/5UA/4wA/4MA/3sA/3IA/2kA/2AA/1cA/08A/0YA/z0A/zQA/ysA/yMA/xoA/xEA/wgA/wAA+gAA9gAA8gAA7QAA6QAA5QAA4QAA3AAA2AAA1AAA0AAAywAAxwAAwwAAvwAAugAAtgAAsgAArQAAqQAApQAAoQAAnAAAmAAAlAAAkAAAiwAAhwAAgwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAAfwAA'
  }
  // Palette 1..3 as the Echo app numbers them (eSonarColorMode 16 / 14 / 15). The background is
  // palette entry 0; verified on palette 2, assumed the same rule for 1 and 3.
  const palettes = {}
  function palette (n) {
    if (palettes[n]) return palettes[n]
    const data = PALETTE_DATA[n]
    if (!data) throw new Error('palette must be 1, 2 or 3')
    const bin = typeof atob === 'function' ? atob(data) : Buffer.from(data, 'base64').toString('binary')
    const rgb = new Uint8Array(768)
    for (let i = 0; i < 768; i++) rgb[i] = bin.charCodeAt(i)
    return (palettes[n] = { rgb, bg: rgb565(rgb[0], rgb[1], rgb[2]) })
  }
  function rgb565 (r, g, b) { return ((r & 0xf8) << 8) | ((g & 0xfc) << 3) | (b >> 3) }

  const f = Math.fround
  function levelFromNoiseFloor (b) { return Math.max(0, 100 - Math.ceil(100 * b / 255)) }

  // auto: true = Automatic gain (level from levelFromNoiseFloor, bias normally 0); else gain = the UI value 10..100
  function thresholdIndex (opts) {
    let idx
    if (opts.auto) {
      idx = Math.trunc((100 - ((opts.bias || 0) + opts.level)) * 245 / 100)
      idx = Math.min(245, Math.max(1, idx))
    } else {
      idx = 245 - Math.trunc(245 * (opts.gain - 10) / 100)
      idx = Math.min(245, Math.max(0, idx))
    }
    return idx
  }

  const rowCache = new Map()
  // The 256 pixels (RGB565) of table row `idx` for palette 1..3 and Color 0..100.
  function colorRow (paletteNo, colorLine, idx) {
    const key = paletteNo * 1e6 + colorLine * 1e3 + idx
    let row = rowCache.get(key)
    if (row) return row
    const p = palette(paletteNo)
    const k = f(256 / f(1 + f(399 * f(f(100 - colorLine) / 100))))
    row = new Uint16Array(256)
    for (let c = 0; c < 256; c++) {
      let v = p.bg
      if (idx < c) {
        let x = Math.trunc(f(k * (c - idx)))
        x = x > 254 ? 255 : Math.max(x, 0)
        if (x > 0) {
          const i = Math.trunc(f(f(x) / 256) * 256)
          v = rgb565(p.rgb[i * 3], p.rgb[i * 3 + 1], p.rgb[i * 3 + 2])
        }
      }
      row[c] = v
    }
    if (rowCache.size > 2000) rowCache.clear()
    rowCache.set(key, row)
    return row
  }

  // Peak-hold resampling of `samples` (first n) into `rows` pixel rows. cellsPerFt = n / (range in ft
  // the samples cover), pixelsPerFt = rows / (range in ft shown). Returns [cells, rowsUsed]; rows
  // beyond the samples are not filled (the app leaves them in the background colour).
  function scaleColumn (samples, n, rows, cellsPerFt, pixelsPerFt, out) {
    const cells = out || new Uint8Array(rows)
    const step = Math.trunc(f(f(cellsPerFt * 65536) / pixelsPerFt))
    let acc = 0
    let prev = Math.max(0, (acc - step) >> 16)
    let used = 0
    for (; used < rows; used++) {
      const idx = acc >> 16
      if (idx >= n) break
      let v = samples[idx]
      for (let j = prev + 1; j < idx; j++) if (samples[j] > v) v = samples[j]
      cells[used] = v
      acc += step; prev = idx
    }
    return [cells, used]
  }

  // Rows drawn white by the white line rule for a scaled column (see 5. above).
  function whiteRows (cells, used, idx, offset) {
    const white = new Uint8Array(used)
    const limit = idx + (offset === undefined ? 10 : offset)
    let pending = false
    for (let r = 0; r < used; r++) {
      const c = cells[r]
      if (c > limit && !pending) { white[r] = 1; pending = true } else if (c <= idx) pending = false
    }
    return white
  }

  return { whiteRows, palette, rgb565, levelFromNoiseFloor, thresholdIndex, colorRow, scaleColumn }
})
