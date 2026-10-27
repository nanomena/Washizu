const { contextBridge, ipcRenderer } = require('electron')
contextBridge.exposeInMainWorld('akagi', {
  /** @param {string} method @param {Record<string, unknown>} [args] */
  async invoke(method, args = {}) {
    const reply = await ipcRenderer.invoke('washizu:profiles', method, args)
    if (!reply.ok) throw new Error(reply.error)
    return reply.value
  },
  /** @param {string} name @param {(state: unknown) => void} listener */
  listen(name, listener) {
    /** @param {Electron.IpcRendererEvent} _event @param {string} eventName @param {unknown} state */
    const receive = (_event, eventName, state) => { if (eventName === name) listener(state) }
    ipcRenderer.on('akagi:event', receive)
    return () => ipcRenderer.removeListener('akagi:event', receive)
  },
})
