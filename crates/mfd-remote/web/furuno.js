// A Furuno TimeZero unit's remote: the keys, cursor pad and knob of a Furuno MCU-006, played by canboat on
// the unit's NMEA 2000 bus. Our server passes each command to canboat's control port and its answer back:
//   {"key":"F5","down":true}  {"pointer":{"dx":40,"dy":-10}}  {"knob":-1}  {"click":true}  {"keypad":true}
//   {"status":true} -> { ok, host, keypad, … }: host is the MFD the remote is paired with, null before that
// A key is held for as long as it is pressed here, so the unit itself tells a tap from a long press.
// CURS/SCRL switches what the joystick does, moving the cursor or panning: it is the same joystick report,
// with keypad mode on.

const $ = (id) => document.getElementById(id)
const statusEl = $('status')
const remote = $('remote')
const pad = $('pad')

const serial = new URLSearchParams(location.search).get('plotter')
// remote units per CSS pixel of a drag
const SPEED = 1.5
// a long press, as the unit counts it (past its threshold)
const LONG_MS = 1000
// how far a pad touch may wander and still be a tap
const TAP_SLOP = 6
// two-finger drag per knob detent
const DETENT_PX = 24
// an arrow held in SCROLL mode: so far, this often
const PAN_STEP = 12
const PAN_MS = 60

let ws = null
let retry = null
let statusTimer = null

function setStatus(s) {
  statusEl.textContent = s
}

function send(cmd) {
  if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify(cmd))
}

async function plotter() {
  const r = await fetch('api/plotters')
  // these pages may have a password: its login is on the first page, which comes back here after it
  if (r.status === 401) {
    location.href = `../?next=${encodeURIComponent(location.pathname + location.search)}`
    throw new Error('login needed')
  }
  return (await r.json()).find((p) => p.serial === serial)
}

function scheduleRetry() {
  clearTimeout(retry)
  retry = setTimeout(connect, 3000)
}

async function connect() {
  clearTimeout(retry)
  remote.classList.add('off')
  let p
  try {
    p = await plotter()
  } catch {
    setStatus('our server does not answer')
    return scheduleRetry()
  }
  if (!p) {
    setStatus('not found')
    return scheduleRetry()
  }
  $('name').textContent = p.name
  document.title = p.name
  if (!p.online) {
    setStatus('not answering')
    return scheduleRetry()
  }
  if (!p.canControl) {
    setStatus('no remote: run canboat with --quirk mcu006 on its NMEA 2000 bus')
    return scheduleRetry()
  }
  setStatus('connecting…')
  const scheme = location.protocol === 'https:' ? 'wss:' : 'ws:'
  const base = location.pathname.replace(/[^/]*$/, '')
  ws = new WebSocket(`${scheme}//${location.host}${base}signal/furuno?plotter=${encodeURIComponent(serial)}`)
  ws.onopen = () => {
    remote.classList.remove('off')
    send({ status: true })
    clearInterval(statusTimer)
    statusTimer = setInterval(() => send({ status: true }), 5000)
  }
  ws.onmessage = (e) => {
    let r
    try {
      r = JSON.parse(e.data)
    } catch {
      return
    }
    if (r.ok === false) setStatus(r.error ?? 'refused')
    else if ('host' in r) setStatus(r.host == null ? 'waiting for the chartplotter to pair with the remote' : 'connected')
    if ('keypad' in r) setKeypad(r.keypad)
  }
  ws.onclose = (e) => {
    ws = null
    clearInterval(statusTimer)
    remote.classList.add('off')
    setStatus(e.reason || 'disconnected')
    scheduleRetry()
  }
}

// --- keys: down while pressed ---

for (const b of document.querySelectorAll('[data-key]')) {
  let timer = null
  const up = () => {
    if (!b.classList.contains('held')) return
    clearTimeout(timer)
    b.classList.remove('held', 'long')
    send({ key: b.dataset.key, down: false })
  }
  b.addEventListener('pointerdown', (e) => {
    e.preventDefault()
    b.setPointerCapture(e.pointerId)
    b.classList.add('held')
    send({ key: b.dataset.key, down: true })
    timer = setTimeout(() => b.classList.add('long'), LONG_MS)
  })
  b.addEventListener('pointerup', up)
  b.addEventListener('pointercancel', up)
  b.addEventListener('lostpointercapture', up)
  b.addEventListener('contextmenu', (e) => e.preventDefault())
}

// --- the knob ---

for (const b of document.querySelectorAll('[data-knob]')) {
  b.addEventListener('click', () => send({ knob: Number(b.dataset.knob) }))
}
$('push').addEventListener('click', () => send({ click: true }))

let keypad = false
function setKeypad(on) {
  keypad = !!on
  $('keypad').setAttribute('aria-pressed', String(keypad))
  $('keypad-mode').textContent = keypad ? 'Scroll' : 'Cursor'
  remote.classList.toggle('scroll', keypad)
  $('hint').textContent = keypad
    ? 'Drag or hold an arrow to pan · tap to push · two fingers to zoom'
    : 'Drag to move the cursor · tap to push · two fingers to zoom'
}
$('keypad').addEventListener('click', () => {
  setKeypad(!keypad)
  send({ keypad })
})

// SCROLL mode's arrows: the joystick held over, for as long as one is pressed
for (const b of document.querySelectorAll('.arrow')) {
  let timer = null
  const step = () => send({ pointer: { dx: Number(b.dataset.dx) * PAN_STEP, dy: Number(b.dataset.dy) * PAN_STEP } })
  const up = () => {
    clearInterval(timer)
    timer = null
    b.classList.remove('held')
  }
  b.addEventListener('pointerdown', (e) => {
    e.preventDefault()
    e.stopPropagation() // not a touch on the pad itself
    b.setPointerCapture(e.pointerId)
    b.classList.add('held')
    step()
    clearInterval(timer)
    timer = setInterval(step, PAN_MS)
  })
  for (const ev of ['pointerup', 'pointercancel', 'lostpointercapture']) b.addEventListener(ev, up)
  b.addEventListener('contextmenu', (e) => e.preventDefault())
}

// --- the cursor pad ---

const touches = new Map() // pointerId -> { x, y }
let moved = 0
let pending = { dx: 0, dy: 0 }
let frame = null
let twoFingerY = null

function flush() {
  frame = null
  const dx = Math.trunc(pending.dx)
  const dy = Math.trunc(pending.dy)
  if (!dx && !dy) return
  pending.dx -= dx
  pending.dy -= dy
  send({ pointer: { dx, dy } })
}

function midY() {
  let y = 0
  for (const t of touches.values()) y += t.y
  return y / touches.size
}

pad.addEventListener('pointerdown', (e) => {
  e.preventDefault()
  pad.setPointerCapture(e.pointerId)
  touches.set(e.pointerId, { x: e.clientX, y: e.clientY })
  pad.classList.add('active')
  if (touches.size === 1) moved = 0
  twoFingerY = touches.size === 2 ? midY() : null
})

pad.addEventListener('pointermove', (e) => {
  const t = touches.get(e.pointerId)
  if (!t) return
  const dx = e.clientX - t.x
  const dy = e.clientY - t.y
  t.x = e.clientX
  t.y = e.clientY
  moved += Math.abs(dx) + Math.abs(dy)
  if (touches.size === 1) {
    pending.dx += dx * SPEED
    pending.dy += dy * SPEED
    frame ??= requestAnimationFrame(flush)
  } else if (touches.size === 2 && twoFingerY != null) {
    // fingers moving up zoom in, as the knob turned right does
    const y = midY()
    const detents = Math.trunc((twoFingerY - y) / DETENT_PX)
    if (detents) {
      send({ knob: detents })
      twoFingerY -= detents * DETENT_PX
    }
  }
})

function padUp(e) {
  if (!touches.delete(e.pointerId)) return
  if (touches.size === 0) {
    pad.classList.remove('active')
    // a tap that did not wander, with one finger all along: select
    if (moved <= TAP_SLOP && twoFingerY == null && e.type === 'pointerup') send({ click: true })
    twoFingerY = null
  } else {
    // one finger left of two: it moves the cursor again, but this gesture is no tap
    moved = Infinity
    twoFingerY = touches.size === 2 ? midY() : twoFingerY
  }
}
pad.addEventListener('pointerup', padUp)
pad.addEventListener('pointercancel', padUp)

// a mouse wheel turns the knob, a detent per notch
let wheel = 0
pad.addEventListener('wheel', (e) => {
  e.preventDefault()
  wheel -= e.deltaY
  const detents = Math.trunc(wheel / 50)
  if (detents) {
    wheel -= detents * 50
    send({ knob: detents })
  }
}, { passive: false })

connect()
