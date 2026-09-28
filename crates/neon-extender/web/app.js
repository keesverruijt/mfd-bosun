// The first page: the chartplotters found, each opening its remote screen, and the sonars with ForwardScan.

import { Session, sendFile, signalUrl, newClientKey, CLOSE_PASSWORD, CLOSE_UNKNOWN, FILE_WARN_BYTES } from './remote/session.js'

const $ = (id) => document.getElementById(id)

const esc = (s) => String(s).replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`)
const brands = { bandg: 'B&G', simrad: 'Simrad', lowrance: 'Lowrance' }

function plotterCard(p) {
  const brand = brands[p.brand] ?? p.brand
  const detail = [p.name !== p.model ? `${brand} ${p.model}` : brand, p.address, `v${p.osVersion}`].filter(Boolean)
  const s = encodeURIComponent(p.serial)
  // waypoints and files need its password known, and it on
  const actions =
    p.online && p.hasPassword
      ? `<div class="actions">
      <a class="action" href="remote/api/plotters/${s}/export.gpx" download title="All its waypoints and routes">Waypoints &amp; routes (GPX)</a>
      <button class="action" type="button" data-send="${esc(p.serial)}" title="To its Download folder; or drop a file here">Send a file</button>
      <span class="upload" id="up-${esc(p.serial)}">${esc(uploads[p.serial]?.text ?? '')}</span>
    </div>`
      : ''
  return `<li class="plotter${p.online ? '' : ' offline'}" data-serial="${esc(p.serial)}">
    <a class="card" href="remote/?plotter=${s}">
      <span class="dot" title="${p.online ? 'on' : 'not answering'}"></span>
      <span class="what"><b>${esc(p.name)}</b><small>${esc(detail.join(' · '))}</small></span>
      <span class="go">${p.hasPassword ? 'Screen' : 'Connect'} ›</span>
    </a>${actions}</li>`
}

// --- sending a file to a chartplotter, from here: a session of its own, without the video ---

const uploads = {} // serial -> { text, busy }
let pickFor = null

function setUpload(serial, text, busy = false) {
  uploads[serial] = { text, busy }
  const el = document.getElementById(`up-${serial}`)
  if (el) el.textContent = text
}

async function upload(serial, f) {
  if (uploads[serial]?.busy) return
  if (f.size > FILE_WARN_BYTES && !confirm(`${f.name} is ${Math.round(f.size / 1048576)} MB. The chartplotter holds all of it in memory until it is in. Send it?`)) return
  setUpload(serial, `Connecting for ${f.name}…`, true)
  const base = new URL('remote/', location.href).pathname
  const s = new Session({ url: signalUrl(base, serial, newClientKey()) })
  try {
    await s.whenOpen(['file', 'file-metadata'], 20000).catch((err) => {
      if (s.closeCode === CLOSE_PASSWORD) throw new Error('open its screen once, to give its password')
      if (s.closeCode === CLOSE_UNKNOWN) throw new Error('it does not answer')
      throw err
    })
    await sendFile(s, f, (p) => setUpload(serial, `Sending ${f.name}: ${Math.round(p * 100)}%`, true))
    setUpload(serial, `${f.name} sent, in its Download folder.`)
  } catch (err) {
    setUpload(serial, `${f.name} not sent: ${err.message}`)
  } finally {
    setTimeout(() => s.close(), 1000) // the last pieces reach it first
  }
}

$('plotters').addEventListener('click', (e) => {
  const b = e.target.closest('[data-send]')
  if (!b) return
  pickFor = b.dataset.send
  $('pick').value = ''
  $('pick').click()
})
$('pick').addEventListener('change', () => {
  const f = $('pick').files[0]
  if (f && pickFor) upload(pickFor, f)
})
// drop a file on a chartplotter's card
$('plotters').addEventListener('dragover', (e) => {
  const li = e.target.closest('li.plotter')
  if (!li?.querySelector('[data-send]') || ![...(e.dataTransfer?.types ?? [])].includes('Files')) return
  e.preventDefault()
  for (const x of document.querySelectorAll('li.dropping')) if (x !== li) x.classList.remove('dropping')
  li.classList.add('dropping')
})
$('plotters').addEventListener('dragleave', (e) => {
  const li = e.target.closest('li.plotter')
  if (li && !li.contains(e.relatedTarget)) li.classList.remove('dropping')
})
$('plotters').addEventListener('drop', (e) => {
  const li = e.target.closest('li.plotter')
  li?.classList.remove('dropping')
  const f = e.dataTransfer?.files?.[0]
  if (!li || !f || !li.querySelector('[data-send]')) return
  e.preventDefault()
  upload(li.dataset.serial, f)
})

function sonarCard(s) {
  return `<li>
    <a class="card${s.live ? '' : ' offline'}" href="forwardscan/">
      <span class="dot" title="${s.live ? 'sending' : 'paused'}"></span>
      <span class="what"><b>ForwardScan</b><small>${esc(s.name)} · ${esc(s.model)} · ${esc(s.address)}</small></span>
      <span class="go">Open ›</span>
    </a></li>`
}

let shown = ''
async function refresh() {
  let st
  try {
    const r = await fetch('api/status')
    if (r.status === 401) return showLogin()
    st = await r.json()
    $('state').textContent = ''
  } catch {
    $('state').textContent = 'not connected'
    return
  }
  $('login').hidden = true
  $('list').hidden = false
  const html = JSON.stringify(st)
  if (html === shown) return
  shown = html
  $('plotters').innerHTML = st.plotters.map(plotterCard).join('')
  $('none').hidden = st.plotters.length > 0
  const sources = st.forwardScan?.sources ?? []
  $('fs').hidden = !st.forwardScan
  $('sonars').innerHTML = sources.length
    ? sources.map(sonarCard).join('')
    : '<li class="empty">No ForwardScan seen yet. Start ForwardScan on the sonar.</li>'
  $('version').textContent = `navico-neon-extender ${st.version}`
}

// --- the password of these pages, when they have one ---

function showLogin() {
  shown = ''
  $('list').hidden = true
  $('fs').hidden = true
  if ($('login').hidden) {
    $('login').hidden = false
    $('password').focus()
  }
}

$('login').addEventListener('submit', async (e) => {
  e.preventDefault()
  $('login-error').hidden = true
  const r = await fetch('api/login', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ password: $('password').value, remember: $('remember').checked }),
  }).catch(() => null)
  if (!r?.ok) {
    $('login-error').textContent = r?.status === 401 ? 'Wrong password.' : 'The server does not answer.'
    $('login-error').hidden = false
    return
  }
  $('password').value = ''
  // back to the page that sent us here, if any (only a path of our own)
  const next = new URLSearchParams(location.search).get('next')
  if (next && next.startsWith('/') && !next.startsWith('//')) location.href = next
  else refresh()
})

$('logout').addEventListener('click', async (e) => {
  e.preventDefault()
  await fetch('api/logout', { method: 'POST' })
  showLogin()
})

fetch('api/auth')
  .then((r) => r.json())
  .then((a) => ($('logout').hidden = !a.required))
  .catch(() => {})

refresh()
setInterval(refresh, 3000)
