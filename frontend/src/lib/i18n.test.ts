/// <reference types="node" />
import { afterEach, describe, expect, it } from 'vitest'
import fs from 'node:fs'
import path from 'node:path'
import ts from 'typescript'
import { getLang, localized, setLang, tr } from '@/lib/i18n'
import { en } from '@/lib/i18n/en'
import { ja } from '@/lib/i18n/ja'
import { zhTW } from '@/lib/i18n/zh-TW'

const CJK = /[\u3400-\u9fff\u3001-\u303f\uff00-\uffef]/
const SRC = path.resolve(__dirname, '..')

/** Every text the page can put through `tr`: the first argument of each call,
 *  and every Chinese string in a table wrapped in `localized(...)`. Read off
 *  the sources' syntax trees, so a text added anywhere is found here. */
function texts(): string[] {
  const found = new Set<string>()
  const files = (dir: string): string[] => fs.readdirSync(dir, { withFileTypes: true }).flatMap(entry => {
    const full = path.join(dir, entry.name)
    if (entry.isDirectory()) return entry.name === 'i18n' ? [] : files(full)
    return /\.tsx?$/.test(entry.name) && !/\.test\./.test(entry.name) && entry.name !== 'i18n.ts' ? [full] : []
  })
  for (const file of files(SRC)) {
    const source = ts.createSourceFile(file, fs.readFileSync(file, 'utf8'), ts.ScriptTarget.Latest, true, file.endsWith('x') ? ts.ScriptKind.TSX : ts.ScriptKind.TS)
    const walk = (node: ts.Node, inTable: boolean): void => {
      if (ts.isCallExpression(node) && ts.isIdentifier(node.expression)) {
        const first = node.arguments[0]
        if (node.expression.text === 'tr' && first && ts.isStringLiteralLike(first)) found.add(first.text)
        if (node.expression.text === 'localized') { node.arguments.forEach(a => walk(a, true)); return }
      }
      if (inTable && ts.isStringLiteralLike(node) && CJK.test(node.text) && !(ts.isPropertyAssignment(node.parent) && node.parent.name === node)) found.add(node.text)
      ts.forEachChild(node, child => walk(child, inTable))
    }
    walk(source, false)
  }
  return [...found]
}

const places = (text: string) => (text.match(/\{\d+\}/g) ?? []).sort().join()

describe('the words on screen', () => {
  const keys = texts()
  afterEach(() => setLang('zh-CN'))

  it('finds the page\'s texts', () => {
    expect(keys.length).toBeGreaterThan(300)
    expect(keys).toContain('对局开始')
  })

  it.each([['English', en], ['日本語', ja], ['繁體中文', zhTW]] as const)('%s has every text the page can show, and nothing the page does not', (_name, table) => {
    expect(keys.filter(key => !(key in table))).toEqual([])
    expect(Object.keys(table).filter(key => !keys.includes(key))).toEqual([])
  })

  it.each([['English', en], ['日本語', ja], ['繁體中文', zhTW]] as const)('%s puts every value where a text takes one', (_name, table) => {
    expect(Object.entries(table).filter(([key, value]) => places(key) !== places(value)).map(([key]) => key)).toEqual([])
  })

  it('leaves no Chinese in English', () => {
    expect(Object.entries(en).filter(([, value]) => CJK.test(value)).map(([key]) => key)).toEqual([])
  })

  it('answers in the language of the moment, values in place, and as written where it has no words', () => {
    expect(getLang()).toBe('zh-CN')
    expect(tr('另有 {0} 项', 2)).toBe('另有 2 项')
    const table = localized({ winds: { E: '东' }, tabs: [['game', '对局']] as Array<[string, string]> })
    expect(table.winds.E).toBe('东')
    setLang('en')
    expect(tr('另有 {0} 项', 2)).toBe(en['另有 {0} 项'].replace('{0}', '2'))
    expect(table.winds.E).toBe(en['东'])
    expect(table.tabs.map(([key, label]) => `${key}:${label}`)).toEqual([`game:${en['对局']}`])
    expect(tr('不在表里的话')).toBe('不在表里的话')
  })
})
