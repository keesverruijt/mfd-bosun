// The control bar (bar.css): each .seg[data-for=id] gets a button per option of the <select id> after it and
// drives it (firing input and change, as if the select had been used); each slider with an <output id="<id>v">
// shows its value. Call Bar.init({ sliderId: unit }) after the page has set its controls' saved values.
// The bar is closed behind the #menu button, opens over the view, and closes again on a tap on the view,
// Escape, or after IDLE_MS without using it.
// Every other <select> in the bar is shown as a .pick button opening a .picklist drawn by the page: the
// chartplotters' browser flies its own drop-down list in from the side of the screen (a flash). The select
// stays the model: choosing sets its value and fires input and change. After the page changes a select
// itself (options or value), it calls Bar.refresh(select); Bar.picking(select) is true while its list is open.
const IDLE_MS = 15000
const pickers = new Map() // select -> { refresh, close, list }
const Bar = {
  refresh (sel) { const p = pickers.get(sel); if (p) p.refresh() },
  picking (sel) { const p = pickers.get(sel); return !!p && !p.list.hidden },
  closePickers () { for (const p of pickers.values()) p.close() },
  init (units) {
    const bar = document.getElementById('bar'), menu = document.getElementById('menu')
    let idle = null
    const setOpen = (open) => {
      bar.classList.toggle('open', open); menu.setAttribute('aria-expanded', open)
      if (!open) Bar.closePickers()
      clearTimeout(idle)
      if (open) idle = setTimeout(() => setOpen(false), IDLE_MS)
    }
    menu.onclick = () => setOpen(!bar.classList.contains('open'))
    for (const ev of ['pointerdown', 'input', 'wheel', 'scroll']) {
      bar.addEventListener(ev, () => setOpen(true), { passive: true, capture: true })
    }
    document.getElementById('wrap').addEventListener('pointerdown', () => setOpen(false))
    document.addEventListener('keydown', (e) => { if (e.key === 'Escape') { Bar.closePickers(); setOpen(false) } })
    // a tap anywhere but on a picker closes its list
    document.addEventListener('pointerdown', (e) => { if (!e.target.closest('.pick, .picklist')) Bar.closePickers() }, true)

    for (const sel of bar.querySelectorAll('select')) {
      if (sel.previousElementSibling?.classList.contains('seg')) continue
      const btn = document.createElement('button'); btn.type = 'button'; btn.className = 'pick'
      const list = document.createElement('div'); list.className = 'picklist'; list.hidden = true
      sel.after(btn, list); sel.classList.add('picked')
      const refresh = () => { const o = sel.selectedOptions[0]; btn.textContent = o ? o.textContent : '' }
      const close = () => { list.hidden = true }
      btn.onclick = () => {
        if (!list.hidden) return close()
        Bar.closePickers()
        list.replaceChildren(...[...sel.options].map(o => {
          const b = document.createElement('button'); b.type = 'button'; b.textContent = o.textContent
          if (o.value === sel.value) b.className = 'on'
          b.onclick = () => {
            sel.value = o.value; close(); refresh()
            sel.dispatchEvent(new Event('input')); sel.dispatchEvent(new Event('change'))
          }
          return b
        }))
        list.hidden = false
        list.querySelector('.on')?.scrollIntoView({ block: 'nearest' })
      }
      pickers.set(sel, { refresh, close, list })
      refresh()
    }

    for (const seg of document.querySelectorAll('.seg')) {
      const sel = document.getElementById(seg.dataset.for)
      const sync = () => { for (const b of seg.children) b.classList.toggle('on', b.dataset.v === sel.value) }
      for (const o of sel.options) {
        const b = document.createElement('button'); b.type = 'button'; b.textContent = o.textContent; b.dataset.v = o.value
        if (o.title) b.title = o.title
        b.onclick = () => { sel.value = o.value; sel.dispatchEvent(new Event('input')); sel.dispatchEvent(new Event('change')); sync() }
        seg.appendChild(b)
      }
      sel.addEventListener('change', sync)
      sync()
    }
    for (const out of document.querySelectorAll('#bar output')) {
      const el = document.getElementById(out.id.replace(/v$/, ''))
      const show = () => { out.textContent = el.value + ((units || {})[el.id] || '') }
      el.addEventListener('input', show); show()
    }
  }
}
