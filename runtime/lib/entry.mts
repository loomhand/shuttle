import { readFileSync, writeFileSync } from 'node:fs'
import { pathToFileURL } from 'node:url'

const env = process.env
const out = (name: string): string => `${env.SHUTTLE_RUN}/${name}`

try {
  Object.assign(globalThis, { desk: await import(pathToFileURL(`${import.meta.dirname}/desk.mts`).href) })
  const mod = await import(pathToFileURL(env.SHUTTLE_SCRIPT ?? '').href) as { main?: (params: unknown) => unknown }
  if (typeof mod.main === 'function') {
    const params: unknown = JSON.parse(readFileSync(out('params.json'), 'utf8'))
    writeFileSync(out('result.json'), JSON.stringify((await mod.main(params)) ?? null) ?? 'null')
  }
} catch (e: unknown) {
  writeFileSync(out('error.txt'), e instanceof Error ? `${e.name}: ${e.message}` : String(e))
  process.exitCode = 1
}
setTimeout(() => process.exit(), 500).unref()
