// A stand-in for `akagi-core engine-host --engine <dir>`: it records each start
// in the package folder, exits at once while a `crash` file is there, and
// otherwise holds until its stdin closes, as the real host does.
import fs from 'node:fs'
import path from 'node:path'

const dir = process.argv[process.argv.indexOf('--engine') + 1]
fs.appendFileSync(path.join(dir, 'starts'), `${process.argv.slice(2).join(' ')}\n`)
if (fs.existsSync(path.join(dir, 'crash'))) process.exit(3)
process.stdin.resume()
process.stdin.on('end', () => {
  fs.writeFileSync(path.join(dir, 'stopped'), 'stopped')
  process.exit(0)
})
