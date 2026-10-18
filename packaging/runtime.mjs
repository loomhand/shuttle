// usage: node packaging/runtime.mjs SRC DST — writes each SRC/*.mts as DST/*.mjs with its types stripped and its relative .mts imports renamed
import { mkdirSync, readdirSync, readFileSync, writeFileSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'

const [src, dst] = process.argv.slice(2)
mkdirSync(dst, { recursive: true })
for (const name of readdirSync(src).filter((n) => n.endsWith('.mts'))) {
  const js = stripTypeScriptTypes(readFileSync(`${src}/${name}`, 'utf8')).replace(/(['"])(\.\.?\/[^'"\n]*)\.mts\1/g, '$1$2.mjs$1')
  if (js.includes('.mts')) throw new Error(`${name} still names a .mts file`)
  writeFileSync(`${dst}/${name.replace(/\.mts$/, '.mjs')}`, js)
}
