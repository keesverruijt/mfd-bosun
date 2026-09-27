// The control bar (bar.css): each .seg[data-for=id] gets a button per option of the <select id> after it and
// drives it (firing input and change, as if the select had been used); each slider with an <output id="<id>v">
// shows its value. Call Bar.init({ sliderId: unit }) after the page has set its controls' saved values.
// The bar is closed behind the #menu button, opens over the view, and closes again on a tap on the view,
// Escape, or after IDLE_MS without using it.
const IDLE_MS = 15000
const Bar = {
  init (units) {
    const bar = document.getElementById('bar'), menu = document.getElementById('menu')
    let idle = null
    const setOpen = (open) => {
      bar.classList.toggle('open', open); menu.setAttribute('aria-expanded', open)
      clearTimeout(idle); if (open) idle = setTimeout(() => setOpen(false), IDLE_MS)
    }
    menu.onclick = () => setOpen(!bar.classList.contains('open'))
    for (const ev of ['pointerdown', 'input', 'wheel', 'scroll']) {
      bar.addEventListener(ev, () => setOpen(true), { passive: true, capture: true })
    }
    document.getElementById('wrap').addEventListener('pointerdown', () => setOpen(false))
    document.addEventListener('keydown', (e) => { if (e.key === 'Escape') setOpen(false) })

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
