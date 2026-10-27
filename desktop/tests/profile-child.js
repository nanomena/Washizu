// A process-only account stand-in. No Electron, browser, core or network.
process.on('message', message => {
  if (message && typeof message === 'object' && 'type' in message) {
    if (message.type === 'focus') process.send?.({ type: 'focused' })
    if (message.type === 'release') process.send?.({ type: 'profile-released' })
    if (message.type === 'exit') process.exit(0)
    if (message.type === 'close') process.send?.({ type: 'profile-released' }, () => setTimeout(() => process.exit(0), 30))
    if (message.type === 'unreleased-close') process.exit(1)
  }
})
process.send?.({ type: 'profile-ready', args: process.argv.slice(2) })
