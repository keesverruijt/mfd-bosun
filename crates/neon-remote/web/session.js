// A session with a chartplotter's screen-mirroring service: signalling through our server (which adds the
// Bearer token and the trust-all TLS a browser cannot), and the WebRTC connection straight to the
// chartplotter. Used by the remote screen page, and by the first page to send a file.
//
// The chartplotter is the WebRTC *offerer* (it sends its screen); we are the *answerer*.
// Signalling envelope, both ways:  { command, clientKey?, dataString? }, dataString itself a JSON string:
//   SDP: { type: "offer"|"answer", sdp }     ICE: { candidate, sdpMLineIndex, sdpMid, usernameFragment }

// close codes from our server (see neon-remote's lib.rs)
export const CLOSE_PASSWORD = 4001
export const CLOSE_UNKNOWN = 4004

// The channels we open for the chartplotter.
const CHANNELS = ['tap', 'drag', 'pinch', 'hardkey', 'file', 'file-metadata']

export function newClientKey() {
  return Math.floor(Math.random() * 2_000_000_000) + 1
}

/** The signalling URL for a chartplotter; `base` is the path of the remote screen pages ("/remote/"). */
export function signalUrl(base, serial, clientKey) {
  const scheme = location.protocol === 'https:' ? 'wss:' : 'ws:'
  return `${scheme}//${location.host}${base}signal?plotter=${encodeURIComponent(serial)}&key=${clientKey}`
}

export class Session {
  /**
   * @param {object} o
   * @param {string} o.url            signalling URL (see signalUrl)
   * @param {number} o.clientKey
   * @param {Function} [o.log]
   * @param {Function} [o.onTrack]            (RTCTrackEvent)
   * @param {Function} [o.onPcState]          (connectionState)
   * @param {Function} [o.onSignalOpen]
   * @param {Function} [o.onSignalClose]      (CloseEvent)
   * @param {Function} [o.onOffer]            after the answer went out
   * @param {Function} [o.onDeviceOffline]
   * @param {Function} [o.onPairingResponse]  (string)
   * @param {Function} [o.onDataChannel]      (RTCDataChannel) the chartplotter's own channels
   */
  constructor(o) {
    this.o = o
    this.clientKey = o.clientKey
    this.log = o.log ?? (() => {})
    this.channels = {}

    const pc = (this.pc = new RTCPeerConnection({ iceServers: [] })) // the boat's network: host candidates only
    pc.ondatachannel = (e) => o.onDataChannel?.(e.channel)
    pc.ontrack = (e) => {
      this.log('track', e.track.kind)
      o.onTrack?.(e)
    }
    pc.onicecandidate = (e) => {
      if (!e.candidate) return
      this.send({
        command: 'onIceCandidate',
        clientKey: this.clientKey,
        dataString: JSON.stringify({
          candidate: e.candidate.candidate,
          sdpMLineIndex: e.candidate.sdpMLineIndex,
          sdpMid: e.candidate.sdpMid,
          usernameFragment: e.candidate.usernameFragment,
        }),
      })
    }
    pc.oniceconnectionstatechange = () => this.log('ice', pc.iceConnectionState)
    pc.onconnectionstatechange = () => {
      this.log('pc', pc.connectionState)
      o.onPcState?.(pc.connectionState)
    }
    for (const label of CHANNELS) {
      const c = pc.createDataChannel(label)
      c.onopen = () => this.log('data channel open:', label)
      this.channels[label] = c
    }

    const ws = (this.ws = new WebSocket(o.url))
    ws.onopen = () => {
      this.log('signalling open, clientKey', this.clientKey)
      this.send({ command: 'receiver-online', clientKey: this.clientKey })
      o.onSignalOpen?.()
    }
    ws.onerror = () => this.log('signalling error')
    ws.onmessage = (e) => this.onSignal(JSON.parse(e.data))
    ws.onclose = (e) => {
      this.log('signalling closed', e.code, e.reason)
      this.ws = null
      this.closeCode = e.code
      this.closeReason = e.reason || `signalling closed (${e.code})`
      o.onSignalClose?.(e)
    }
  }

  send(env) {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify(env))
  }

  /** One of our channels, when it is open. */
  channel(label) {
    const c = this.channels[label]
    return c?.readyState === 'open' ? c : null
  }

  /** Resolves when all these channels are open; rejects after ms, or when signalling closes first. */
  whenOpen(labels, ms) {
    return new Promise((resolve, reject) => {
      const t0 = performance.now()
      const check = () => {
        if (labels.every((l) => this.channel(l))) return resolve()
        if (this.closed || !this.ws) return reject(new Error(this.closeReason ?? 'closed'))
        if (performance.now() - t0 > ms) return reject(new Error('the chartplotter did not answer'))
        setTimeout(check, 100)
      }
      check()
    })
  }

  close() {
    this.closed = true
    if (this.ws) {
      this.ws.onclose = null
      this.ws.close()
      this.ws = null
    }
    this.pc.close()
  }

  async onSignal(env) {
    switch (env.command) {
      case 'device-online':
        this.log('device-online')
        // ask for the offer again, in case we were first
        this.send({ command: 'receiver-online', clientKey: this.clientKey })
        break
      case 'device-offline':
        this.o.onDeviceOffline?.()
        break
      case 'offer': {
        const { sdp } = JSON.parse(env.dataString ?? '{}')
        this.log('offer; data channels:', /m=application/.test(sdp))
        await this.pc.setRemoteDescription({ type: 'offer', sdp })
        const answer = await this.pc.createAnswer()
        await this.pc.setLocalDescription(answer)
        this.send({
          command: 'answer',
          clientKey: this.clientKey,
          dataString: JSON.stringify({ type: 'answer', sdp: answer.sdp }),
        })
        this.o.onOffer?.()
        break
      }
      case 'onIceCandidate': {
        const c = JSON.parse(env.dataString ?? '{}')
        await this.pc
          .addIceCandidate({ candidate: c.candidate, sdpMid: c.sdpMid, sdpMLineIndex: c.sdpMLineIndex })
          .catch((err) => this.log('addIceCandidate failed', String(err)))
        break
      }
      case 'pairing-response':
        this.o.onPairingResponse?.(env.dataString ?? '')
        break
      default:
        this.log('unhandled command', env.command)
    }
  }
}

// --- sending a file -------------------------------------------------------------------------------------
// First { name, size, progress, destinationPath } on "file-metadata", then the bytes on "file". The
// chartplotter keeps them in memory until `size` bytes are in, then saves the file to Download.
export const FILE_CHUNK = 16 * 1024 // what the chartplotter sends in, and what any SCTP stack takes
export const FILE_WARN_BYTES = 100 * 1024 * 1024

/** Sends `f`; onProgress(0..1). Resolves once everything has left this browser. */
export async function sendFile(session, f, onProgress) {
  const meta = session.channel('file-metadata')
  const data = session.channel('file')
  if (!meta || !data) throw new Error('not connected')
  if (f.size === 0) throw new Error('the file is empty')
  meta.send(JSON.stringify({ name: f.name, size: f.size, progress: 0, destinationPath: 'Download' }))
  data.bufferedAmountLowThreshold = 4 * FILE_CHUNK
  const drained = () =>
    new Promise((r) => data.addEventListener('bufferedamountlow', r, { once: true }))
  for (let at = 0; at < f.size; at += FILE_CHUNK) {
    if (data.readyState !== 'open') throw new Error('the connection went')
    if (data.bufferedAmount > 16 * FILE_CHUNK) await drained()
    data.send(await f.slice(at, at + FILE_CHUNK).arrayBuffer())
    onProgress?.(Math.min(1, (at + FILE_CHUNK) / f.size))
  }
  // all of it on the wire before anyone closes the connection
  while (data.bufferedAmount > 0) {
    if (data.readyState !== 'open') throw new Error('the connection went')
    await new Promise((r) => setTimeout(r, 100))
  }
}
