// The remote screen of a NEON chartplotter.
//
// The chartplotter is the WebRTC *offerer* (it sends its screen); this page is the *answerer*. Signalling
// goes through our server at ./signal (it adds the Bearer token and the trust-all TLS a browser cannot);
// the video and the input data channels go peer to peer to the chartplotter.
//
// Signalling envelope, both ways:  { command, clientKey?, dataString? }, dataString itself a JSON string:
//   SDP: { type: "offer"|"answer", sdp }     ICE: { candidate, sdpMLineIndex, sdpMid, usernameFragment }

import { Session, sendFile as sendFileTo, signalUrl, newClientKey, CLOSE_PASSWORD, CLOSE_UNKNOWN, FILE_WARN_BYTES } from './session.js'

const $ = (id) => document.getElementById(id)
const video = $('video')
const center = $('center')
const touch = $('touch')
const startBtn = $('start')
const statusEl = $('status')
const logEl = $('log')
const panel = $('panel')
const login = $('login')

const serial = new URLSearchParams(location.search).get('plotter')
const clientKey = newClientKey()
let session = null
let retry = null
let streaming = false
let lostTimer = null
// an older NOS unit: our server plays its part (no files, its own keys, drags end explicitly)
let nos = false
// what our server last said about it (a NOS unit waiting for the user to allow input)
let note = null

function connectedText() {
  return note ? `connected — ${note}` : 'connected'
}

function log(...a) {
  const line = a.map((x) => (typeof x === 'string' ? x : JSON.stringify(x))).join(' ')
  logEl.textContent = `${new Date().toLocaleTimeString()}  ${line}\n` + logEl.textContent.slice(0, 20000)
  console.log(...a)
}

function setStatus(s, big) {
  statusEl.textContent = s
  center.hidden = !big
  center.textContent = big ? s : ''
}

function send(env) {
  session?.send(env)
}

async function plotter() {
  const r = await fetch('api/plotters')
  if (r.status === 401) toLogin()
  const list = await r.json()
  return list.find((p) => p.serial === serial)
}

// These pages may have a password (mfd-bosun --password): its login is on the first page,
// which comes back here after it.
function toLogin() {
  location.href = `../?next=${encodeURIComponent(location.pathname + location.search)}`
  throw new Error('login needed')
}

function teardown() {
  note = null
  clearTimeout(retry)
  retry = null
  clearTimeout(lostTimer)
  lostTimer = null
  startBtn.hidden = true
  session?.close()
  session = null
  streaming = false
}

function connect() {
  teardown()
  setStatus('connecting…', true)

  const base = location.pathname.replace(/[^/]*$/, '')
  const me = (session = new Session({
    url: signalUrl(base, serial, clientKey),
    clientKey,
    log,
    // the chartplotter's own channels: it sends files on its "file" / "file-metadata"
    onDataChannel: receiveChannel,
    onTrack: (e) => {
      video.srcObject = e.streams[0] ?? new MediaStream([e.track])
      streaming = true
      setStatus(connectedText())
      play()
    },
    onPcState: (s) => {
      if (session !== me) return
      clearTimeout(lostTimer)
      lostTimer = null
      if (s === 'failed') {
        setStatus('connection lost', true)
        scheduleRetry()
      } else if (s === 'disconnected') {
        // often recovers by itself (a missed ICE check): give it time before starting over
        setStatus('connection interrupted…', false)
        lostTimer = setTimeout(() => {
          if (session?.pc.connectionState !== 'connected') {
            setStatus('connection lost', true)
            connect()
          }
        }, 8000)
      } else if (s === 'connected' && streaming) {
        setStatus(connectedText())
      }
    },
    onSignalOpen: () => setStatus('waiting for the chartplotter…', true),
    onOffer: () => {
      if (!streaming) setStatus('starting video…', true) // the track may already have come with the offer
    },
    onDeviceOffline: () => setStatus('chartplotter offline', !streaming),
    onStatus: (s) => {
      log(s)
      note = s === 'connected' ? null : s
      setStatus(streaming ? connectedText() : s, !streaming)
    },
    onSignalClose: (e) => {
      if (session !== me) return
      if (e.code === CLOSE_PASSWORD) {
        teardown()
        askPassword(e.reason === 'password needed' ? '' : e.reason)
      } else if (e.code === CLOSE_UNKNOWN) {
        setStatus('chartplotter not found — is it on?', true)
        scheduleRetry()
      } else if (!streaming) {
        setStatus(e.reason || `disconnected (${e.code})`, true)
        // a refused login shows as a WebSocket that never opened
        fetch('api/plotters')
          .then((r) => (r.status === 401 ? toLogin() : scheduleRetry()))
          .catch(() => scheduleRetry())
      }
      // while streaming, the video goes on without signalling; a lost connection shows in pc state
    },
  }))
}

function scheduleRetry() {
  if (retry) return
  retry = setTimeout(() => {
    retry = null
    connect()
  }, 5000)
}

// --- the password -----------------------------------------------------------------------------------

function askPassword(error) {
  setStatus('password needed', false)
  $('login-error').hidden = !error
  $('login-error').textContent = error || ''
  $('password').value = ''
  if (!login.open) login.showModal()
  $('password').focus()
}

$('login-form').addEventListener('submit', async (e) => {
  e.preventDefault()
  const ok = $('login-ok')
  ok.disabled = true
  $('login-error').hidden = true
  try {
    const r = await fetch(`api/plotters/${encodeURIComponent(serial)}/password`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ password: $('password').value }),
    })
    const j = await r.json().catch(() => ({}))
    if (!r.ok) throw new Error(j.error || `HTTP ${r.status}`)
    login.close()
    connect()
  } catch (err) {
    $('login-error').textContent = err.message === 'wrong password' ? 'Wrong password.' : err.message
    $('login-error').hidden = false
  } finally {
    ok.disabled = false
  }
})

$('forget').addEventListener('click', async () => {
  await fetch(`api/plotters/${encodeURIComponent(serial)}/password`, { method: 'DELETE' })
  log('password forgotten')
  teardown()
  showPanel(false)
  askPassword('')
})

// --- playing ---
// Safari does not always start a stream by itself, even muted: start it, and when the browser wants a
// user gesture for that, ask for one.

function play() {
  video
    .play()
    .then(() => (startBtn.hidden = true))
    .catch((err) => {
      log('video does not start by itself:', String(err))
      startBtn.hidden = false
    })
}

startBtn.addEventListener('click', play)
video.addEventListener('pause', () => streaming && play())

// --- input -------------------------------------------------------------------------------------------
// The chartplotter maps our coordinates from the video box it is shown in (letterbox-aware): we send the
// raw offset within the <video> element and the element's size.

function videoPoint(ev) {
  const r = video.getBoundingClientRect()
  return { x: ev.clientX - r.left, y: ev.clientY - r.top, clientWidth: r.width, clientHeight: r.height }
}

function open(label) {
  return session?.channel(label) ?? null
}

// One finger (or the mouse): drag while down, tap on release, but only when it did not move (else the
// chartplotter gets a click where a drag ended). A second finger makes it a pinch, and then neither the
// tap nor more drag of the first finger is sent.
const TAP_SLOP_PX = 10
const pointers = new Map() // pointerId -> the last PointerEvent
let downAt = 0
let downPos = null
let dragged = false
let pinching = false

touch.addEventListener('pointerdown', (ev) => {
  touch.setPointerCapture(ev.pointerId)
  pointers.set(ev.pointerId, ev)
  if (pointers.size === 1) {
    downAt = performance.now()
    downPos = { x: ev.clientX, y: ev.clientY }
    dragged = false
    pinching = false
  } else if (pointers.size === 2) {
    pinching = true
    pinch('start', ...pointers.values())
  }
})
touch.addEventListener('pointermove', (ev) => {
  if (!pointers.has(ev.pointerId)) return
  pointers.set(ev.pointerId, ev)
  if (pinching) {
    if (pointers.size >= 2) pinch('move', ...[...pointers.values()].slice(0, 2))
    return
  }
  const c = open('drag')
  if (ev.buttons !== 1 || !c) return
  if (!dragged && Math.hypot(ev.clientX - downPos.x, ev.clientY - downPos.y) < TAP_SLOP_PX) return
  // a NOS unit is told where the finger went down, or a quick swipe would be a tap where it ended
  if (!dragged && nos) c.send(JSON.stringify(videoPoint({ clientX: downPos.x, clientY: downPos.y })))
  dragged = true
  c.send(JSON.stringify(videoPoint(ev)))
})
function pointerEnd(ev) {
  if (!pointers.has(ev.pointerId)) return
  if (pinching) {
    if (pointers.size === 2) pinch('end', ...pointers.values())
  } else if (dragged && nos) {
    open('drag')?.send(JSON.stringify({ ...videoPoint(ev), end: true }))
  } else if (ev.type === 'pointerup' && !dragged) {
    const c = open('tap')
    const isLongPress = performance.now() - downAt > 500
    c?.send(JSON.stringify({ ...videoPoint(ev), isLongPress }))
  }
  pointers.delete(ev.pointerId)
}
touch.addEventListener('pointerup', pointerEnd)
touch.addEventListener('pointercancel', pointerEnd)

// A pinch: { phase: start|move|end, p1: {x, y}, p2: {x, y}, clientWidth, clientHeight }, the points as a
// tap's. The chartplotter drops a pinch with a finger in the letterbox, so points are kept on the picture.
// It may handle messages sent close together out of order: moves go at most every PINCH_GAP_MS, and the end
// after the last move.
const PINCH_GAP_MS = 40
let pinchLastAt = 0
let pinchPending = null

function pinch(phase, a, b) {
  const c = open('pinch')
  if (!c) return
  const r = video.getBoundingClientRect()
  const msg = JSON.stringify({
    phase,
    p1: onPicture(a.clientX - r.left, a.clientY - r.top, r),
    p2: onPicture(b.clientX - r.left, b.clientY - r.top, r),
    clientWidth: r.width,
    clientHeight: r.height,
  })
  const now = performance.now()
  if (phase === 'move' && now - pinchLastAt < PINCH_GAP_MS) {
    // keep only the latest move; it goes when the gap is over
    if (!pinchPending) setTimeout(flushPinch, PINCH_GAP_MS - (now - pinchLastAt))
    pinchPending = msg
    return
  }
  if (phase === 'end' && pinchPending) flushPinch()
  const send = () => {
    c.send(msg)
    pinchLastAt = performance.now()
  }
  if (phase === 'end') setTimeout(send, PINCH_GAP_MS)
  else send()
}

function flushPinch() {
  const c = open('pinch')
  if (pinchPending && c) c.send(pinchPending)
  pinchPending = null
  pinchLastAt = performance.now()
}

/** A point clamped onto the picture inside the <video> box (object-fit: contain). */
function onPicture(x, y, r) {
  const vw = video.videoWidth || r.width
  const vh = video.videoHeight || r.height
  const scale = Math.min(r.width / vw, r.height / vh)
  const w = vw * scale
  const h = vh * scale
  const left = (r.width - w) / 2
  const top = (r.height - h) / 2
  const m = 2 // stay just inside
  return {
    x: Math.min(Math.max(x, left + m), left + w - m),
    y: Math.min(Math.max(y, top + m), top + h - m),
  }
}

// A trackpad pinch on a Mac (Chrome and Firefox: wheel events with ctrlKey; Safari: gesture events), and
// Ctrl + mouse wheel, as two fingers around the pointer.
const SYNTH_SPREAD = 120 // px between the fingers at the start
const SYNTH_IDLE_MS = 200 // no more events: the pinch ends
let synth = null // { x, y, scale, timer }

function synthPoints() {
  const d = (SYNTH_SPREAD * synth.scale) / 2
  return [
    { clientX: synth.x - d, clientY: synth.y },
    { clientX: synth.x + d, clientY: synth.y },
  ]
}

function synthPinch(x, y, scale) {
  if (!synth) {
    synth = { x, y, scale: 1, timer: null }
    pinch('start', ...synthPoints())
  }
  // two fingers wider than the picture would be clamped anyway: keep the scale sensible
  synth.scale = Math.min(Math.max(scale, 0.1), 8)
  pinch('move', ...synthPoints())
  clearTimeout(synth.timer)
  synth.timer = setTimeout(() => {
    pinch('end', ...synthPoints())
    synth = null
  }, SYNTH_IDLE_MS)
}

touch.addEventListener(
  'wheel',
  (ev) => {
    if (!ev.ctrlKey) return // plain scrolling: not ours
    ev.preventDefault() // not the browser's page zoom
    const scale = (synth?.scale ?? 1) * Math.exp(-ev.deltaY / 100)
    synthPinch(ev.clientX, ev.clientY, scale)
  },
  { passive: false },
)

// iOS sends gesture events for a real two-finger pinch too: that one goes as the fingers themselves
let gestureScale = 1
touch.addEventListener('gesturestart', (ev) => {
  ev.preventDefault()
  gestureScale = synth?.scale ?? 1
})
touch.addEventListener('gesturechange', (ev) => {
  ev.preventDefault()
  if (pointers.size < 2) synthPinch(ev.clientX, ev.clientY, gestureScale * ev.scale)
})
touch.addEventListener('gestureend', (ev) => ev.preventDefault())
touch.addEventListener('contextmenu', (e) => e.preventDefault())

// The hardkey channel takes { key }: "KEYCODE_<name>" is pressed as that Android key,
// "KEYCODE_SHIFT + KEYCODE_TAB" as Shift+Tab, and anything else is set into the focused field as text.
//
// Characters go as text: capitals and any other character work that way (a key cannot carry Shift).
// Enter is the knob press, as on a Zeus SR: with the on-screen keyboard up, it presses the key that is
// highlighted on it (move the highlight with Tab / Shift+Tab or the arrows, or click ✓ instead).
//
// The chartplotter may handle messages sent close together out of order, so they are queued here and
// sent KEY_GAP_MS apart.
const KEY_GAP_MS = 40

const keyQueue = []
let keyTimer = null

function pumpKeys() {
  const c = open('hardkey')
  if (!c) keyQueue.length = 0 // the connection went: the rest is not for the next one
  if (!keyQueue.length) {
    keyTimer = null
    return
  }
  const key = keyQueue.shift()
  c.send(JSON.stringify({ key }))
  log('key', key)
  keyTimer = setTimeout(pumpKeys, KEY_GAP_MS)
}

function sendHardKey(key) {
  if (!key || !open('hardkey')) return false
  keyQueue.push(key)
  if (!keyTimer) pumpKeys()
  return true
}

/** Types a string, character by character, into the focused field. */
function typeText(s) {
  let ok = false
  for (const ch of s) ok = sendHardKey(ch) || ok
  return ok
}


$('keypad').addEventListener('click', (e) => {
  const b = e.target.closest('[data-key]')
  if (!b) return
  sendHardKey(b.dataset.key)
  b.blur() // or Enter/Space would press it again
})

// A NOS unit's keys, as it lists them.
const NOS_KEYS = {
  page: 'Pages', menu: 'Menu', enter: 'Enter', cancel: 'Cancel', zoomin: '+', zoomout: '−', goto: 'Go to',
  mark: 'Mark', waypoint: 'Waypoint', mob: 'MOB', user: 'User', power: 'Power',
}

function nosKeypad(keys) {
  $('keypad').replaceChildren(
    ...keys.map((k) => {
      const b = Object.assign(document.createElement('button'), { type: 'button', textContent: NOS_KEYS[k] ?? k })
      b.dataset.key = `NOS:${k}`
      return b
    }),
  )
  $('keyhelp').textContent = keys.length
    ? 'The keys this chartplotter has. A pinch zooms with its zoom keys, when it has them.'
    : 'This chartplotter has no keys to press from here.'
}

// A keyboard on the remote screen itself: keys as the chartplotter's, characters typed.
const KEYS = {
  ArrowUp: 'KEYCODE_DPAD_UP',
  ArrowDown: 'KEYCODE_DPAD_DOWN',
  ArrowLeft: 'KEYCODE_DPAD_LEFT',
  ArrowRight: 'KEYCODE_DPAD_RIGHT',
  Enter: 'KEYCODE_ENTER',
  Escape: 'KEYCODE_BACK',
  Backspace: 'KEYCODE_DEL',
  Delete: 'KEYCODE_FORWARD_DEL',
  Tab: 'KEYCODE_TAB',
  Home: 'KEYCODE_MOVE_HOME',
  End: 'KEYCODE_MOVE_END',
  PageUp: 'KEYCODE_PAGE_UP',
  PageDown: 'KEYCODE_PAGE_DOWN',
}

document.addEventListener('keydown', (e) => {
  // not while typing in our own fields, or with a shortcut of the browser's
  if (e.target.closest?.('input, textarea, dialog') || login.open) return
  if (e.ctrlKey || e.metaKey || e.altKey || e.isComposing) return
  let key = KEYS[e.key]
  if (e.key === 'Tab' && e.shiftKey) key = 'KEYCODE_SHIFT + KEYCODE_TAB'
  if (key) {
    if (sendHardKey(key)) e.preventDefault()
  } else if (e.key.length === 1 && typeText(e.key)) {
    e.preventDefault()
  }
})

// --- files ------------------------------------------------------------------------------------------
let sendingFile = false

function fileStatus(text, progress) {
  $('filestatus').textContent = text
  $('fileprogress').hidden = progress == null
  if (progress != null) $('fileprogress').value = progress
}

async function sendFile(f) {
  if (nos) return
  if (!session) return fileStatus('Not connected.')
  if (sendingFile) return fileStatus('Still sending the last file.')
  if (f.size > FILE_WARN_BYTES && !confirm(`${f.name} is ${Math.round(f.size / 1048576)} MB. The chartplotter holds all of it in memory until it is in. Send it?`)) return
  sendingFile = true
  showPanel(true)
  log('sending file', f.name, f.size)
  try {
    await sendFileTo(session, f, (p) => fileStatus(`Sending ${f.name}…`, p))
    fileStatus(`${f.name} sent: in Download on the chartplotter.`)
    log('file sent', f.name)
  } catch (err) {
    fileStatus(`${f.name} not sent: ${err.message}`)
    log('file failed', String(err))
  } finally {
    sendingFile = false
  }
}

$('sendfile').addEventListener('click', () => {
  const f = $('file').files[0]
  if (f) sendFile(f).then(() => ($('file').value = ''))
})

// drop a file on the screen
for (const t of [document.body]) {
  t.addEventListener('dragover', (e) => {
    if (![...(e.dataTransfer?.types ?? [])].includes('Files')) return
    e.preventDefault()
    document.body.classList.add('dropping')
  })
  t.addEventListener('dragleave', (e) => {
    if (e.target === document.body || e.relatedTarget == null) document.body.classList.remove('dropping')
  })
  t.addEventListener('drop', (e) => {
    document.body.classList.remove('dropping')
    const f = e.dataTransfer?.files?.[0]
    if (!f) return
    e.preventDefault()
    sendFile(f)
  })
}

// From the chartplotter (another app on it asks the screen-mirroring service to send a file): the same
// metadata, then 16 KB pieces, on channels it opens itself. Offered as a download when complete.
let incoming = null // { name, size, parts: [], got }

function receiveChannel(ch) {
  ch.binaryType = 'arraybuffer'
  log('chartplotter opened channel', ch.label)
  if (ch.label === 'file-metadata') {
    ch.onmessage = (e) => {
      try {
        const m = JSON.parse(typeof e.data === 'string' ? e.data : new TextDecoder().decode(e.data))
        incoming = { name: m.name || 'file', size: m.size || 0, parts: incoming?.parts ?? [], got: incoming?.got ?? 0 }
        log('file coming', incoming.name, incoming.size)
        showPanel(true)
        maybeFileDone()
      } catch (err) {
        log('bad file metadata', String(err))
      }
    }
  } else if (ch.label === 'file') {
    ch.onmessage = (e) => {
      const b = typeof e.data === 'string' ? new TextEncoder().encode(e.data).buffer : e.data
      incoming ??= { name: 'file', size: 0, parts: [], got: 0 }
      incoming.parts.push(b)
      incoming.got += b.byteLength
      if (incoming.size) fileStatus(`Receiving ${incoming.name}…`, incoming.got / incoming.size)
      maybeFileDone()
    }
  }
}

function maybeFileDone() {
  if (!incoming?.size || incoming.got < incoming.size) return
  const { name, parts } = incoming
  incoming = null
  const url = URL.createObjectURL(new Blob(parts))
  const a = Object.assign(document.createElement('a'), { href: url, download: name })
  document.body.append(a)
  a.click()
  a.remove()
  setTimeout(() => URL.revokeObjectURL(url), 60_000)
  fileStatus(`Received ${name}.`)
  log('file received', name)
}

// --- the panel -----------------------------------------------------------------------------------------

function showPanel(show) {
  panel.hidden = !show
  $('more').setAttribute('aria-expanded', String(show))
}

$('more').addEventListener('click', () => showPanel(panel.hidden))
$('reconnect').addEventListener('click', () => {
  showPanel(false)
  connect()
})

// --- start -------------------------------------------------------------------------------------------

async function start() {
  if (!serial) {
    location.href = '../'
    return
  }
  let p
  try {
    p = await plotter()
  } catch {
    setStatus('the server does not answer', true)
    setTimeout(start, 5000)
    return
  }
  if (!p) {
    setStatus('chartplotter not found — is it on?', true)
    setTimeout(start, 5000)
    return
  }
  nos = p.kind === 'nos'
  if (nos) {
    for (const e of document.querySelectorAll('.neon-only')) e.hidden = true
    nosKeypad(p.keys ?? [])
  }
  $('name').textContent = p.name
  $('gpx').href = `api/plotters/${encodeURIComponent(serial)}/export.gpx`
  $('login-title').textContent = `Password for ${p.name}`
  document.title = p.name
  if (!p.hasPassword) askPassword('')
  else connect()
}

start()
