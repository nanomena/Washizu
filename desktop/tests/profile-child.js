// A process-only account stand-in. No Electron, browser, core or network.
process.on('message', message => {
  if (message && typeof message === 'object' && 'type' in message) {
    if (message.type === 'focus') process.send?.({ type: 'focused' })
    if (message.type === 'close') setTimeout(() => process.exit(0), 30)
  }
})
process.send?.({ type: 'profile-ready', args: process.argv.slice(2) })
