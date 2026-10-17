import { readFileSync, writeFileSync } from 'node:fs'
import { pathToFileURL } from 'node:url'

type Main = (params: unknown) => unknown

const env = process.env
const out = (name: string): string => `${env.SHUTTLE_RUN}/${name}`

let main: Main | undefined
try {
  Object.assign(globalThis, { desk: await import(pathToFileURL(`${import.meta.dirname}/desk.mts`).href) })
  const mod = await import(pathToFileURL(env.SHUTTLE_SCRIPT ?? '').href) as { main?: unknown }
  if (typeof mod.main === 'function') main = mod.main as Main
  else fail('no_main', `${env.SHUTTLE_SCRIPT} exports no main function`)
} catch (e: unknown) {
  fail('import', e)
}
if (main) {
  try {
    const params: unknown = JSON.parse(readFileSync(out('params.json'), 'utf8'))
    writeFileSync(out('result.json'), JSON.stringify(await main(params)) ?? 'null')
  } catch (e: unknown) {
    fail('exception', e)
  }
}
setTimeout(() => process.exit(), 500).unref()

function fail(kind: string, e: unknown): void {
  const message = e instanceof Error ? e.stack ?? `${e.name}: ${e.message}` : String(e)
  writeFileSync(out('error.json'), JSON.stringify({ kind, message }))
  process.exitCode = 1
}
